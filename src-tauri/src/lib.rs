mod audio;
mod cloud;
mod platform;
mod records;
mod storage;
use cloud::RecognitionConfig;
use records::{
    delete_entries_at, read_history_at, rename_entry_at, save_transcript_at,
    update_configuration_at, write_history_at, HistoryEntry,
};
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, MutexGuard,
};
use std::{
    fs,
    io::{BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager};

static HISTORY_LOCK: Mutex<()> = Mutex::new(());
static JOB_ACTIVE: AtomicBool = AtomicBool::new(false);
fn history_lock() -> Result<MutexGuard<'static, ()>, String> {
    HISTORY_LOCK.lock().map_err(|_| "errors.historyLock".into())
}
struct JobGuard;
impl JobGuard {
    fn acquire() -> Result<Self, String> {
        JOB_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "errors.operationBusy".to_owned())?;
        Ok(Self)
    }
}
impl Drop for JobGuard {
    fn drop(&mut self) {
        JOB_ACTIVE.store(false, Ordering::Release);
    }
}

#[derive(Serialize)]
struct ModelStatus {
    id: &'static str,
    installed: bool,
}
#[derive(Serialize)]
struct RuntimeStatus {
    ffmpeg: bool,
    whisper: bool,
    nemo: bool,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    kind: &'static str,
    stage: &'static str,
    current: u64,
    total: u64,
    elapsed: u64,
}
#[derive(Serialize)]
struct HistoryDetail {
    entry: HistoryEntry,
    text: String,
    warning: Option<String>,
}

const MODELS: [(&str, &str, u64); 4] = [
    ("whisper-tiny", "ggml-tiny.bin", 77_691_713),
    ("whisper-base", "ggml-base.bin", 147_951_465),
    ("whisper-small", "ggml-small.bin", 487_601_967),
    ("nemotron-3.5", "nemotron-3.5.ready", 742_090_464),
];
fn clock() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn bin(name: &str) -> Option<PathBuf> {
    if cfg!(mobile) {
        return None;
    }
    let mut paths = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for dir in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/snap/bin",
    ] {
        paths.push(PathBuf::from(dir).join(name));
    }
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".local/bin").join(name));
    }
    paths
        .into_iter()
        .find(|path| path.is_file() && Command::new(path).arg("--help").output().is_ok())
}
fn base() -> Result<PathBuf, String> {
    storage::active_root()
}
fn model_path(id: &str) -> Result<PathBuf, String> {
    let file = MODELS
        .iter()
        .find(|(key, _, _)| *key == id)
        .ok_or("errors.localModelUnknown")?
        .1;
    Ok(base()?.join("models").join(file))
}
fn read_history() -> Result<Vec<HistoryEntry>, String> {
    let _lock = history_lock()?;
    read_history_at(&base()?)
}
fn audio_duration(path: &Path) -> Option<f64> {
    audio::duration(path).or_else(|| {
        let mut probe = Command::new(bin("ffprobe")?);
        probe
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(path);
        let value = command_result(probe, "progress.audioDuration")
            .ok()?
            .parse::<f64>()
            .ok()?;
        (value.is_finite() && value > 0.0).then_some(value)
    })
}
fn save_transcript(id: &str, model: String, text: &str) -> Result<(), String> {
    let _lock = history_lock()?;
    save_transcript_at(&base()?, id, model, text)
}
fn emit(
    app: &tauri::AppHandle,
    kind: &'static str,
    stage: &'static str,
    current: u64,
    total: u64,
    elapsed: u64,
) {
    let _ = app.emit(
        "task-progress",
        Progress {
            kind,
            stage,
            current,
            total,
            elapsed,
        },
    );
}
fn command_result(mut c: Command, _action: &str) -> Result<String, String> {
    let out = c
        .output()
        .map_err(|e| format!("errors.processFailed|{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "errors.processFailed|{}",
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(700)
                .collect::<String>()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
fn tail(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .chars()
        .rev()
        .take(700)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}
fn nemo_downloaded_bytes() -> u64 {
    let root = std::env::var_os("NEMO_SPEECH_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            #[cfg(target_os = "macos")]
            {
                home.join("Library/Caches/NeMoSpeech/models")
            }
            #[cfg(not(target_os = "macos"))]
            {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".cache"))
                    .join("nemo-speech/models")
            }
        })
        .join("nvidia/nemotron-3.5-asr-streaming-0.6b");
    let Ok(revisions) = fs::read_dir(root) else {
        return 0;
    };
    revisions
        .filter_map(Result::ok)
        .filter_map(|revision| {
            fs::metadata(
                revision
                    .path()
                    .join("nemotron-3.5-asr-streaming-0.6b.q8_0.gguf.partial"),
            )
            .ok()
            .map(|m| m.len())
        })
        .max()
        .unwrap_or(0)
}
fn monitored(
    mut c: Command,
    app: &tauri::AppHandle,
    kind: &'static str,
    stage: &'static str,
    log: &Path,
    total: u64,
    partial: Option<&Path>,
    meter: bool,
) -> Result<(), String> {
    c.stdout(Stdio::null())
        .stderr(fs::File::create(log).map_err(|e| format!("errors.operationFailed|{e}"))?);
    let mut child = c.spawn().map_err(|e| format!("errors.processFailed|{e}"))?;
    let start = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("errors.operationFailed|{e}"))?
        {
            if !status.success() {
                return Err(format!("errors.processFailed|{}", tail(log)));
            }
            emit(app, kind, stage, total, total, start.elapsed().as_secs());
            return Ok(());
        }
        let current = if let Some(p) = partial {
            fs::metadata(p).map(|m| m.len()).unwrap_or(0)
        } else {
            let s = fs::read_to_string(log).unwrap_or_default();
            let percent = if meter {
                s.split(['\r', '\n'])
                    .filter_map(|line| {
                        let fields: Vec<_> = line.split_whitespace().collect();
                        if fields.len() > 3 && fields[0].parse::<u64>().is_ok() {
                            fields[2].parse::<u64>().ok()
                        } else {
                            None
                        }
                    })
                    .last()
                    .unwrap_or(0)
            } else {
                s.split("progress = ")
                    .skip(1)
                    .filter_map(|p| p.split('%').next()?.trim().parse::<u64>().ok())
                    .last()
                    .unwrap_or(0)
            };
            if meter {
                let bytes = nemo_downloaded_bytes();
                if bytes > 0 {
                    bytes
                } else {
                    percent.min(99) * total / 100
                }
            } else {
                percent.min(99) * total / 100
            }
        };
        emit(
            app,
            kind,
            stage,
            current.min(total.saturating_sub(1)),
            total,
            start.elapsed().as_secs(),
        );
        thread::sleep(Duration::from_millis(350));
    }
}
fn monitored_whisper(
    mut c: Command,
    app: &tauri::AppHandle,
    log: &Path,
    stdout_path: &Path,
    configuration: &RecognitionConfig,
) -> Result<(), String> {
    c.stdout(fs::File::create(stdout_path).map_err(|e| format!("errors.operationFailed|{e}"))?)
        .stderr(fs::File::create(log).map_err(|e| format!("errors.operationFailed|{e}"))?);
    let mut child = c
        .spawn()
        .map_err(|e| format!("errors.operationFailed|{e}"))?;
    let start = Instant::now();
    let mut last = String::new();
    let mut promoted = false;
    loop {
        let lines = fs::read_to_string(stdout_path).unwrap_or_default();
        let text = lines
            .lines()
            .filter_map(|line| {
                if line.starts_with("[00:") {
                    line.split_once(']').map(|(_, body)| body.trim())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        if !text.is_empty() && text != last {
            if !promoted {
                promoted = cloud::mark_working(configuration).is_ok();
            }
            partial(app, &text);
            last = text;
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("errors.operationFailed|{e}"))?
        {
            if !status.success() {
                return Err(format!("errors.whisperTranscribe|{}", tail(log)));
            }
            emit(
                app,
                "transcribe",
                "progress.recognizeWhisper",
                100,
                100,
                start.elapsed().as_secs(),
            );
            return Ok(());
        }
        let log_text = fs::read_to_string(log).unwrap_or_default();
        let percent = log_text
            .split("progress = ")
            .skip(1)
            .filter_map(|p| p.split('%').next()?.trim().parse::<u64>().ok())
            .last()
            .unwrap_or(0)
            .min(99);
        emit(
            app,
            "transcribe",
            "progress.recognizeWhisper",
            percent,
            100,
            start.elapsed().as_secs(),
        );
        thread::sleep(Duration::from_millis(350));
    }
}
#[tauri::command]
fn runtime_status() -> RuntimeStatus {
    RuntimeStatus {
        ffmpeg: bin("ffmpeg").is_some(),
        whisper: bin("whisper-cli").is_some(),
        nemo: bin("nemo-speech").is_some(),
    }
}
#[tauri::command]
fn model_status() -> Result<Vec<ModelStatus>, String> {
    if cfg!(mobile) {
        return Ok(vec![]);
    }
    MODELS
        .iter()
        .map(|(id, _, _)| {
            Ok(ModelStatus {
                id,
                installed: model_path(id)?.exists(),
            })
        })
        .collect()
}
#[tauri::command]
fn list_history() -> Result<Vec<HistoryEntry>, String> {
    let mut entries = read_history()?;
    entries.reverse();
    Ok(entries)
}
#[tauri::command]
async fn import_audio(
    app: tauri::AppHandle,
    path: String,
    name: Option<String>,
    mut configuration: RecognitionConfig,
) -> Result<HistoryEntry, String> {
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        configuration.validate()?;
        platform::validate_configuration(&configuration)?;
        let source = Path::new(&path);
        let is_uri = path.starts_with("content://");
        let requested_name = name
            .as_deref()
            .and_then(|name| Path::new(name).file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.chars().any(char::is_control));
        let original_name = requested_name.or_else(|| {
            if is_uri {
                None
            } else {
                source.file_name().and_then(|name| name.to_str())
            }
        });
        let extension = original_name
            .and_then(|name| Path::new(name).extension())
            .and_then(|extension| extension.to_str())
            .map(str::to_lowercase);
        if !is_uri
            && !extension.as_ref().is_some_and(|extension| {
                ["m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg"].contains(&extension.as_str())
            })
        {
            return Err("errors.audioFormatUnsupported".into());
        }
        let id = format!(
            "hearing-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let staging = base()?.join("temp").join(format!("{id}-import.audio"));
        let mut input = platform::open_audio(&app, &path)?;
        let total = input.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let mut output =
            fs::File::create(&staging).map_err(|e| format!("errors.operationFailed|{e}"))?;
        let start = Instant::now();
        let mut copied = 0u64;
        let mut buffer = [0u8; 1024 * 1024];
        let copy_result = (|| -> Result<(), String> {
            loop {
                let n = input
                    .read(&mut buffer)
                    .map_err(|e| format!("errors.operationFailed|{e}"))?;
                if n == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..n])
                    .map_err(|e| format!("errors.operationFailed|{e}"))?;
                copied += n as u64;
                emit(
                    &app,
                    "import",
                    "progress.copyAudio",
                    copied,
                    total,
                    start.elapsed().as_secs(),
                );
            }
            Ok(())
        })();
        if let Err(e) = copy_result {
            drop(output);
            let _ = fs::remove_file(&staging);
            return Err(e);
        }
        if let Err(e) = output.sync_all() {
            drop(output);
            let _ = fs::remove_file(&staging);
            return Err(format!("errors.audioRead|{e}"));
        }
        drop(output);
        let extension = match extension {
            Some(extension)
                if ["m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg"]
                    .contains(&extension.as_str()) =>
            {
                extension
            }
            _ => match audio::detect_extension(&staging) {
                Ok(extension) => extension.to_owned(),
                Err(error) => {
                    let _ = fs::remove_file(&staging);
                    return Err(error);
                }
            },
        };
        let name = original_name
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Recording-{}.{extension}", clock()));
        let target = base()?.join("input").join(format!("{id}.{extension}"));
        if let Err(error) = fs::rename(&staging, &target) {
            let _ = fs::remove_file(&staging);
            return Err(format!("errors.audioRead|{error}"));
        }
        let entry = HistoryEntry {
            id,
            name,
            input_path: target.to_string_lossy().into_owned(),
            output_path: None,
            model: None,
            configuration: Some(configuration),
            created_at: clock(),
            completed_at: None,
            size_bytes: Some(copied),
            duration_seconds: audio_duration(&target),
        };
        let _lock = history_lock()?;
        let dir = base()?;
        let mut entries = match read_history_at(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                let _ = fs::remove_file(&target);
                return Err(e);
            }
        };
        entries.push(entry.clone());
        if let Err(e) = write_history_at(&dir, &entries) {
            let _ = fs::remove_file(target);
            return Err(e);
        }
        Ok(entry)
    })
    .await
    .map_err(|e| format!("errors.operationFailed|{e}"))?
}
#[tauri::command]
fn get_history(id: String) -> Result<HistoryDetail, String> {
    let _lock = history_lock()?;
    let entry = read_history_at(&base()?)?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or("errors.recordMissing")?;
    let (text, warning) = match &entry.output_path {
        Some(path) => match fs::read_to_string(path) {
            Ok(text) => (text, None),
            Err(_) => (
                String::new(),
                Some("errors.transcriptUnavailable".to_owned()),
            ),
        },
        None => (String::new(), None),
    };
    Ok(HistoryDetail {
        entry,
        text,
        warning,
    })
}
#[tauri::command]
fn update_record_configuration(
    id: String,
    configuration: RecognitionConfig,
) -> Result<HistoryEntry, String> {
    let _job = JobGuard::acquire()?;
    let _lock = history_lock()?;
    update_configuration_at(&base()?, &id, configuration)
}
#[tauri::command]
async fn set_storage_parent(
    app: tauri::AppHandle,
    parent: String,
) -> Result<cloud::PublicSettings, String> {
    if cfg!(mobile) {
        return Err("errors.storageChangeUnavailableMobile".into());
    }
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        let _history = history_lock()?;
        let source = base()?;
        cloud::preserve_settings()?;
        let started = Instant::now();
        let root = storage::migrate(&source, Path::new(&parent), |current, total| {
            emit(
                &app,
                "storage",
                "progress.copyArchive",
                current,
                total,
                started.elapsed().as_secs(),
            )
        })?;
        app.asset_protocol_scope()
            .allow_directory(root.join("input"), false)
            .map_err(|e| format!("errors.storageAccess|{e}"))?;
        emit(
            &app,
            "storage",
            "progress.done",
            1,
            1,
            started.elapsed().as_secs(),
        );
        cloud::load()?.public()
    })
    .await
    .map_err(|e| format!("errors.operationFailed|{e}"))?
}
#[tauri::command]
fn rename_history(id: String, name: String) -> Result<HistoryEntry, String> {
    let _job = JobGuard::acquire()?;
    let _lock = history_lock()?;
    rename_entry_at(&base()?, &id, &name)
}
#[tauri::command]
fn delete_history(id: String) -> Result<(), String> {
    let _job = JobGuard::acquire()?;
    let _lock = history_lock()?;
    delete_entries_at(&base()?, Some(&id))
}
#[tauri::command]
fn clear_history() -> Result<(), String> {
    let _job = JobGuard::acquire()?;
    let _lock = history_lock()?;
    delete_entries_at(&base()?, None)
}
#[tauri::command]
async fn download_model(app: tauri::AppHandle, id: String) -> Result<(), String> {
    if cfg!(mobile) {
        return Err("errors.localUnavailableMobile".into());
    }
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        let target = model_path(&id)?;
        let (_, file, total) = *MODELS
            .iter()
            .find(|(key, _, _)| *key == id)
            .ok_or("errors.localModelUnknown")?;
        if target.exists() {
            emit(&app, "download", "progress.done", total, total, 0);
            return Ok(());
        }
        let log = base()?.join("temp").join(format!("{id}-download.log"));
        if id == "nemotron-3.5" {
            let exe = bin("nemo-speech").ok_or("errors.nemotronMissing")?;
            let mut c = Command::new(exe);
            c.args(["pull", "nemotron-3.5"]);
            monitored(
                c,
                &app,
                "download",
                "progress.downloadNemotron",
                &log,
                total,
                None,
                true,
            )?;
            fs::write(target, "downloaded").map_err(|e| format!("errors.operationFailed|{e}"))?;
        } else {
            let partial = target.with_extension("part");
            let mut c = Command::new("curl");
            c.args(["-fsSL", "--retry", "3", "--continue-at", "-", "-o"])
                .arg(&partial)
                .arg(format!(
                    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}"
                ));
            monitored(
                c,
                &app,
                "download",
                "progress.downloadWhisper",
                &log,
                total,
                Some(&partial),
                false,
            )?;
            if fs::metadata(&partial)
                .map_err(|e| format!("errors.operationFailed|{e}"))?
                .len()
                != total
            {
                return Err("errors.modelSizeMismatch".into());
            }
            fs::rename(partial, target).map_err(|e| format!("errors.operationFailed|{e}"))?;
        }
        let _ = fs::remove_file(log);
        Ok(())
    })
    .await
    .map_err(|e| format!("errors.operationFailed|{e}"))?
}

#[derive(Serialize, Clone)]
struct PartialText {
    text: String,
}
fn partial(app: &tauri::AppHandle, text: &str) {
    let _ = app.emit(
        "partial-text",
        PartialText {
            text: text.to_owned(),
        },
    );
}

fn stream_nemotron(
    app: &tauri::AppHandle,
    pcm: &Path,
    log: &Path,
    configuration: &RecognitionConfig,
) -> Result<String, String> {
    use tungstenite::{connect, stream::MaybeTlsStream, Error as WsError, Message};
    let exe = bin("nemo-speech").ok_or("errors.nemotronMissing")?;
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("errors.operationFailed|{e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("errors.operationFailed|{e}"))?
        .port();
    drop(listener);
    let mut server = Command::new(exe)
        .args([
            "serve",
            "--asr-model",
            "nemotron-3.5",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--no-ui",
        ])
        .stdout(Stdio::null())
        .stderr(fs::File::create(log).map_err(|e| format!("errors.operationFailed|{e}"))?)
        .spawn()
        .map_err(|e| format!("errors.operationFailed|{e}"))?;
    let result = (|| {
        let start = Instant::now();
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if server
                .try_wait()
                .map_err(|e| format!("errors.operationFailed|{e}"))?
                .is_some()
            {
                return Err(format!("errors.nemotronStart|{}", tail(log)));
            }
            if start.elapsed() > Duration::from_secs(120) {
                return Err("errors.nemotronTimeout".into());
            }
            emit(
                app,
                "transcribe",
                "progress.loadNemotron",
                0,
                0,
                start.elapsed().as_secs(),
            );
            thread::sleep(Duration::from_millis(400));
        }
        let (mut socket, _) = connect(format!(
            "ws://127.0.0.1:{port}/v1/audio/transcriptions/realtime"
        ))
        .map_err(|e| format!("errors.operationFailed|{e}"))?;
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_millis(30)))
                .map_err(|e| format!("errors.operationFailed|{e}"))?;
        }
        socket
            .send(Message::Text(
                r#"{"type":"session.update","session":{"sample_rate":16000,"language":"auto"}}"#
                    .into(),
            ))
            .map_err(|e| format!("errors.operationFailed|{e}"))?;
        let total = fs::metadata(pcm)
            .map_err(|e| format!("errors.operationFailed|{e}"))?
            .len();
        let mut reader =
            BufReader::new(fs::File::open(pcm).map_err(|e| format!("errors.operationFailed|{e}"))?);
        let mut finals: Vec<String> = Vec::new();
        let mut pending = String::new();
        let finished = std::cell::Cell::new(false);
        let mut promoted = false;
        let mut on_event = |message: Message| -> Result<(), String> {
            if let Message::Text(text) = message {
                let value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("errors.operationFailed|{e}"))?;
                match value["type"].as_str().unwrap_or("") {
                    "conversation.item.input_audio_transcription.delta" => {
                        pending.push_str(value["delta"].as_str().unwrap_or(""));
                        if !promoted && !pending.trim().is_empty() {
                            promoted = cloud::mark_working(configuration).is_ok();
                        }
                        partial(
                            app,
                            &format!(
                                "{}{}{}",
                                finals.join(" "),
                                if finals.is_empty() { "" } else { " " },
                                pending
                            ),
                        );
                    }
                    "conversation.item.input_audio_transcription.completed" => {
                        let text = value["transcript"].as_str().unwrap_or("").trim();
                        if !text.is_empty() {
                            finals.push(text.to_string());
                        }
                        pending.clear();
                        partial(app, &finals.join(" "));
                    }
                    "input_audio_buffer.committed" => {
                        finished.set(true);
                    }
                    "error" => {
                        return Err(value["error"]["message"]
                            .as_str()
                            .unwrap_or("errors.nemotronStream")
                            .to_string());
                    }
                    _ => {}
                }
            }
            Ok(())
        };
        let mut chunk = [0u8; 32_000];
        let mut sent = 0u64;
        let transfer = Instant::now();
        loop {
            let n = reader
                .read(&mut chunk)
                .map_err(|e| format!("errors.operationFailed|{e}"))?;
            if n == 0 {
                break;
            }
            socket
                .send(Message::Binary(chunk[..n].to_vec().into()))
                .map_err(|e| format!("errors.operationFailed|{e}"))?;
            sent += n as u64;
            emit(
                app,
                "transcribe",
                "progress.streamRecognition",
                sent,
                total,
                transfer.elapsed().as_secs(),
            );
            loop {
                match socket.read() {
                    Ok(message) => on_event(message)?,
                    Err(WsError::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        break
                    }
                    Err(e) => return Err(format!("errors.operationFailed|{e}")),
                }
            }
        }
        socket
            .send(Message::Text(
                r#"{"type":"input_audio_buffer.commit"}"#.into(),
            ))
            .map_err(|e| format!("errors.operationFailed|{e}"))?;
        let deadline = Instant::now();
        while !finished.get() && deadline.elapsed() < Duration::from_secs(180) {
            match socket.read() {
                Ok(message) => on_event(message)?,
                Err(WsError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("errors.operationFailed|{e}")),
            }
            emit(
                app,
                "transcribe",
                "progress.finishRecognition",
                0,
                0,
                transfer.elapsed().as_secs(),
            );
        }
        if !finished.get() {
            return Err("errors.nemotronIncomplete".into());
        }
        Ok(finals.join(" "))
    })();
    let _ = server.kill();
    let _ = server.wait();
    result
}

#[tauri::command]
async fn transcribe(
    app: tauri::AppHandle,
    id: String,
    configuration: Option<RecognitionConfig>,
) -> Result<String, String> {
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        let settings = cloud::load()?;
        let entries = read_history()?;
        let index = entries
            .iter()
            .position(|e| e.id == id)
            .ok_or("errors.recordMissing")?;
        let mut configuration = configuration
            .or_else(|| entries[index].configuration.clone())
            .unwrap_or_else(|| {
                if let Some(model) = &entries[index].model {
                    if let Some(id) = model.strip_prefix("openrouter/") {
                        return RecognitionConfig {
                            provider: "openrouter".into(),
                            model: id.into(),
                            mode: "streaming".into(),
                        };
                    }
                    return RecognitionConfig {
                        provider: "local".into(),
                        model: model.clone(),
                        mode: "local".into(),
                    };
                }
                settings.last_configuration.clone()
            });
        platform::validate_configuration(&configuration)?;
        cloud::normalize_configuration(&settings, &mut configuration)?;
        {
            let _lock = history_lock()?;
            update_configuration_at(&base()?, &id, configuration.clone())?;
        }
        let model = configuration.model.clone();
        let input = PathBuf::from(&entries[index].input_path);
        if !input.is_file() {
            return Err("errors.audioMissing".into());
        }
        if configuration.provider == "openrouter" {
            emit(&app, "transcribe", "progress.sendCloudAudio", 0, 0, 0);
            let start = Instant::now();
            let chunk_dir = base()?.join("temp").join(format!("{id}-cloud-{}", clock()));
            fs::create_dir_all(&chunk_dir).map_err(|_| "errors.audioPrepare")?;
            let result = (|| {
                emit(&app, "transcribe", "progress.prepareAudio", 0, 0, 0);
                let chunks = audio::prepare_cloud_chunks(&input, &chunk_dir, |current, total| {
                    emit(
                        &app,
                        "transcribe",
                        "progress.prepareAudio",
                        current,
                        total,
                        start.elapsed().as_secs(),
                    )
                })?;
                if chunks.is_empty() {
                    return Err("errors.audioEmpty".into());
                }
                let mut parts = Vec::new();
                let mut promoted = false;
                for (index, chunk) in chunks.iter().enumerate() {
                    emit(
                        &app,
                        "transcribe",
                        "progress.sendCloudChunk",
                        index as u64,
                        chunks.len() as u64,
                        start.elapsed().as_secs(),
                    );
                    let prior = parts.join("\n");
                    let text = cloud::transcribe(&settings, &configuration, chunk, |text| {
                        if !promoted && !text.trim().is_empty() {
                            promoted = cloud::mark_working(&configuration).is_ok();
                        }
                        partial(
                            &app,
                            &format!(
                                "{}{}{}",
                                prior,
                                if prior.is_empty() { "" } else { "\n" },
                                text
                            ),
                        );
                        emit(
                            &app,
                            "transcribe",
                            "progress.recognizeCloud",
                            index as u64,
                            chunks.len() as u64,
                            start.elapsed().as_secs(),
                        );
                    })?;
                    parts.push(text);
                    emit(
                        &app,
                        "transcribe",
                        "progress.recognizedChunks",
                        (index + 1) as u64,
                        chunks.len() as u64,
                        start.elapsed().as_secs(),
                    );
                }
                Ok::<String, String>(parts.join("\n"))
            })();
            let _ = fs::remove_dir_all(chunk_dir);
            let text = result?;
            cloud::mark_working(&configuration)?;
            save_transcript(&id, format!("openrouter/{}", configuration.model), &text)?;
            emit(
                &app,
                "transcribe",
                "progress.done",
                1,
                1,
                start.elapsed().as_secs(),
            );
            return Ok(text);
        }
        if !model_path(&model)?.exists() {
            return Err("errors.modelDownloadRequired".into());
        }
        let ffmpeg = bin("ffmpeg").ok_or("errors.ffmpegMissing")?;
        let temp = base()?.join("temp");
        let wav = temp.join(format!(
            "{id}.{}",
            if model == "nemotron-3.5" {
                "pcm"
            } else {
                "wav"
            }
        ));
        let log = temp.join(format!("{id}.log"));
        let prefix = temp.join(format!("{id}-result"));
        let result = (|| {
            emit(&app, "transcribe", "progress.prepareAudio", 0, 0, 0);
            let mut c = Command::new(ffmpeg);
            c.args(["-y", "-v", "error", "-i"]).arg(&input).args([
                "-ar",
                "16000",
                "-ac",
                "1",
                "-c:a",
                "pcm_s16le",
            ]);
            if model == "nemotron-3.5" {
                c.args(["-f", "s16le"]);
            }
            c.arg(&wav);
            let duration = audio_duration(&input).unwrap_or(0.0);
            let estimated = (duration * 32_000.0) as u64;
            monitored(
                c,
                &app,
                "transcribe",
                "progress.prepareAudio",
                &log,
                estimated,
                Some(&wav),
                false,
            )?;
            emit(&app, "transcribe", "progress.loadModel", 0, 0, 0);
            let text = if model == "nemotron-3.5" {
                stream_nemotron(&app, &wav, &log, &configuration)?
            } else {
                let exe = bin("whisper-cli").ok_or("errors.whisperMissing")?;
                let mut c = Command::new(exe);
                c.arg("-m")
                    .arg(model_path(&model)?)
                    .arg("-f")
                    .arg(&wav)
                    .arg("-otxt")
                    .arg("-of")
                    .arg(&prefix)
                    .arg("-pp");
                let live = temp.join(format!("{id}-live.log"));
                let run_result = monitored_whisper(c, &app, &log, &live, &configuration);
                let _ = fs::remove_file(live);
                run_result?;
                let output = prefix.with_extension("txt");
                let text = fs::read_to_string(&output)
                    .map_err(|e| format!("errors.transcriptRead|{e}"))?;
                let _ = fs::remove_file(output);
                text
            };
            if text.trim().is_empty() {
                return Err("errors.emptyTranscript".into());
            }
            cloud::mark_working(&configuration)?;
            save_transcript(&id, model, text.trim())?;
            emit(&app, "transcribe", "progress.done", 1, 1, 0);
            Ok(text.trim().to_string())
        })();
        let _ = fs::remove_file(wav);
        let _ = fs::remove_file(log);
        result
    })
    .await
    .map_err(|e| format!("errors.operationFailed|{e}"))?
}
#[tauri::command]
fn save_text(path: String, content: String) -> Result<(), String> {
    if !path.to_lowercase().ends_with(".txt") {
        return Err("errors.exportExtension".into());
    }
    fs::write(path, content).map_err(|e| format!("errors.operationFailed|{e}"))
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init());
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());
    builder
        .setup(|app| {
            storage::initialize(app.handle()).map_err(std::io::Error::other)?;
            if let Ok(root) = base() {
                app.asset_protocol_scope()
                    .allow_directory(root.join("input"), false)?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            platform::get_runtime_platform,
            platform::pick_audio_file,
            platform::export_text,
            platform::get_latest_android_release,
            cloud::get_settings,
            cloud::save_settings,
            cloud::list_openrouter_models,
            runtime_status,
            model_status,
            list_history,
            import_audio,
            update_record_configuration,
            set_storage_parent,
            get_history,
            rename_history,
            delete_history,
            clear_history,
            download_model,
            transcribe,
            save_text
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
