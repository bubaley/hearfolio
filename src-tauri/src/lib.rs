mod cloud;
mod records;
use serde::Serialize;
use std::{fs, io::{Read, Write, BufReader}, net::{TcpListener, TcpStream}, path::{Path, PathBuf}, process::{Command, Stdio}, thread, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use tauri::Emitter;
use std::sync::{Mutex, MutexGuard, atomic::{AtomicBool, Ordering}};
use records::{HistoryEntry, read_history_at, write_history_at, delete_entries_at, save_transcript_at, rename_entry_at};

static HISTORY_LOCK: Mutex<()> = Mutex::new(());
static JOB_ACTIVE: AtomicBool = AtomicBool::new(false);
fn history_lock() -> Result<MutexGuard<'static, ()>, String> { HISTORY_LOCK.lock().map_err(|_| "Не удалось открыть историю".into()) }
struct JobGuard;
impl JobGuard {
    fn acquire() -> Result<Self, String> {
        JOB_ACTIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).map_err(|_| "Дождитесь завершения текущей операции".to_owned())?;
        Ok(Self)
    }
}
impl Drop for JobGuard { fn drop(&mut self) { JOB_ACTIVE.store(false, Ordering::Release); } }

#[derive(Serialize)]
struct ModelStatus { id: &'static str, installed: bool }
#[derive(Serialize)]
struct RuntimeStatus { ffmpeg: bool, whisper: bool, nemo: bool }
#[derive(Serialize, Clone)]
#[serde(rename_all="camelCase")]
struct Progress { kind: &'static str, stage: &'static str, current: u64, total: u64, elapsed: u64 }
#[derive(Serialize)]
struct HistoryDetail { entry: HistoryEntry, text: String, warning: Option<String> }

const MODELS: [(&str, &str, u64); 4] = [
    ("whisper-tiny", "ggml-tiny.bin", 77_691_713),
    ("whisper-base", "ggml-base.bin", 147_951_465),
    ("whisper-small", "ggml-small.bin", 487_601_967),
    ("nemotron-3.5", "nemotron-3.5.ready", 742_090_464),
];
fn clock() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
fn bin(name: &str) -> Option<PathBuf> {
    let mut paths = vec![PathBuf::from(name)];
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] { paths.push(PathBuf::from(dir).join(name)); }
    if let Some(home) = std::env::var_os("HOME") { paths.push(PathBuf::from(home).join(".local/bin").join(name)); }
    paths.into_iter().find(|path| Command::new(path).arg("--help").output().is_ok())
}
fn base() -> Result<PathBuf, String> {
    let dir = PathBuf::from(std::env::var_os("HOME").ok_or("Не найдена домашняя папка")?).join(".hearing");
    for name in ["input", "output", "models", "temp"] { fs::create_dir_all(dir.join(name)).map_err(|e| e.to_string())?; }
    Ok(dir)
}
fn model_path(id: &str) -> Result<PathBuf, String> {
    let file = MODELS.iter().find(|(key, _, _)| *key == id).ok_or("Неизвестная модель")?.1;
    Ok(base()?.join("models").join(file))
}
fn read_history() -> Result<Vec<HistoryEntry>, String> {
    let _lock = history_lock()?;
    read_history_at(&base()?)
}
#[cfg(test)]
fn write_history(entries: &[HistoryEntry]) -> Result<(), String> {
    let _lock = history_lock()?;
    write_history_at(&base()?, entries)
}
fn audio_duration(path: &Path) -> Option<f64> {
    let mut probe = Command::new(bin("ffprobe")?);
    probe.args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"]).arg(path);
    let value = command_result(probe, "Длительность аудио").ok()?.parse::<f64>().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}
fn save_transcript(id: &str, model: String, text: &str) -> Result<(), String> {
    let _lock = history_lock()?;
    save_transcript_at(&base()?, id, model, text)
}
fn emit(app: &tauri::AppHandle, kind: &'static str, stage: &'static str, current: u64, total: u64, elapsed: u64) {
    let _ = app.emit("task-progress", Progress { kind, stage, current, total, elapsed });
}
fn command_result(mut c: Command, action: &str) -> Result<String, String> {
    let out = c.output().map_err(|e| format!("{action}: {e}"))?;
    if !out.status.success() { return Err(format!("{action}: {}", String::from_utf8_lossy(&out.stderr).trim().chars().take(700).collect::<String>())); }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
fn tail(path: &Path) -> String { fs::read_to_string(path).unwrap_or_default().chars().rev().take(700).collect::<String>().chars().rev().collect() }
fn nemo_downloaded_bytes() -> u64 {
    let root = std::env::var_os("NEMO_SPEECH_MODEL_DIR").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Library/Caches/NeMoSpeech/models")
    }).join("nvidia/nemotron-3.5-asr-streaming-0.6b");
    let Ok(revisions) = fs::read_dir(root) else { return 0 };
    revisions.filter_map(Result::ok).filter_map(|revision| {
        fs::metadata(revision.path().join("nemotron-3.5-asr-streaming-0.6b.q8_0.gguf.partial")).ok().map(|m| m.len())
    }).max().unwrap_or(0)
}
fn monitored(mut c: Command, app: &tauri::AppHandle, kind: &'static str, stage: &'static str, log: &Path, total: u64, partial: Option<&Path>, meter: bool) -> Result<(), String> {
    c.stdout(Stdio::null()).stderr(fs::File::create(log).map_err(|e| e.to_string())?);
    let mut child = c.spawn().map_err(|e| format!("{stage}: {e}"))?; let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() { return Err(format!("{stage}: {}", tail(log))); }
            emit(app, kind, stage, total, total, start.elapsed().as_secs()); return Ok(());
        }
        let current = if let Some(p) = partial { fs::metadata(p).map(|m| m.len()).unwrap_or(0) } else {
            let s = fs::read_to_string(log).unwrap_or_default();
            let percent = if meter {
                s.split(['\r','\n']).filter_map(|line| { let fields: Vec<_> = line.split_whitespace().collect(); if fields.len() > 3 && fields[0].parse::<u64>().is_ok() { fields[2].parse::<u64>().ok() } else { None } }).last().unwrap_or(0)
            } else { s.split("progress = ").skip(1).filter_map(|p| p.split('%').next()?.trim().parse::<u64>().ok()).last().unwrap_or(0) };
            if meter { let bytes = nemo_downloaded_bytes(); if bytes > 0 { bytes } else { percent.min(99) * total / 100 } } else { percent.min(99) * total / 100 }
        };
        emit(app, kind, stage, current.min(total.saturating_sub(1)), total, start.elapsed().as_secs());
        thread::sleep(Duration::from_millis(350));
    }
}
fn monitored_whisper(mut c: Command, app: &tauri::AppHandle, log: &Path, stdout_path: &Path) -> Result<(), String> {
    c.stdout(fs::File::create(stdout_path).map_err(|e| e.to_string())?)
        .stderr(fs::File::create(log).map_err(|e| e.to_string())?);
    let mut child = c.spawn().map_err(|e| e.to_string())?; let start = Instant::now();
    let mut last = String::new();
    loop {
        let lines = fs::read_to_string(stdout_path).unwrap_or_default();
        let text = lines.lines().filter_map(|line| {
            if line.starts_with("[00:") { line.split_once(']').map(|(_, body)| body.trim()) } else { None }
        }).collect::<Vec<_>>().join(" ");
        if !text.is_empty() && text != last { partial(app, &text); last = text; }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() { return Err(format!("Распознавание Whisper: {}", tail(log))); }
            emit(app, "transcribe", "Распознавание Whisper", 100, 100, start.elapsed().as_secs()); return Ok(());
        }
        let log_text = fs::read_to_string(log).unwrap_or_default();
        let percent = log_text.split("progress = ").skip(1).filter_map(|p| p.split('%').next()?.trim().parse::<u64>().ok()).last().unwrap_or(0).min(99);
        emit(app, "transcribe", "Распознавание Whisper", percent, 100, start.elapsed().as_secs());
        thread::sleep(Duration::from_millis(350));
    }
}
#[tauri::command]
fn runtime_status() -> RuntimeStatus { RuntimeStatus { ffmpeg: bin("ffmpeg").is_some(), whisper: bin("whisper-cli").is_some(), nemo: bin("nemo-speech").is_some() } }
#[tauri::command]
fn model_status() -> Result<Vec<ModelStatus>, String> { MODELS.iter().map(|(id, _, _)| Ok(ModelStatus { id, installed: model_path(id)?.exists() })).collect() }
#[tauri::command]
fn list_history() -> Result<Vec<HistoryEntry>, String> { let mut entries = read_history()?; entries.reverse(); Ok(entries) }
#[tauri::command]
async fn import_audio(app: tauri::AppHandle, path: String) -> Result<HistoryEntry, String> {
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
    let _job = job;
    let source = Path::new(&path);
    if !source.is_file() { return Err("Аудиофайл не найден".into()); }
    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if !["m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg"].contains(&ext.as_str()) { return Err("Неподдерживаемый аудиоформат".into()); }
    let id = format!("hearing-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());
    let name = source.file_name().and_then(|s| s.to_str()).unwrap_or("audio").to_string();
    let target = base()?.join("input").join(format!("{id}.{ext}"));
    let total = fs::metadata(source).map_err(|e| e.to_string())?.len();
    let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut output = fs::File::create(&target).map_err(|e| e.to_string())?;
    let start = Instant::now(); let mut copied = 0u64; let mut buffer = [0u8; 1024 * 1024];
    let copy_result = (|| -> Result<(), String> { loop {
        let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        copied += n as u64;
        emit(&app, "import", "Копирование записи", copied, total, start.elapsed().as_secs());
    } Ok(()) })();
    if let Err(e) = copy_result { let _ = fs::remove_file(&target); return Err(e); }
    drop(output);
    let entry = HistoryEntry { id, name, input_path: target.to_string_lossy().into_owned(), output_path: None, model: None, created_at: clock(), size_bytes: Some(total), duration_seconds: audio_duration(&target) };
    let _lock = history_lock()?;
    let dir = base()?;
    let mut entries = match read_history_at(&dir) { Ok(entries) => entries, Err(e) => { let _ = fs::remove_file(&target); return Err(e); } };
    entries.push(entry.clone());
    if let Err(e) = write_history_at(&dir, &entries) { let _ = fs::remove_file(target); return Err(e); }
    Ok(entry)
    }).await.map_err(|e| e.to_string())?
}
#[tauri::command]
fn get_history(id: String) -> Result<HistoryDetail, String> {
    let _lock = history_lock()?;
    let entry = read_history_at(&base()?)?.into_iter().find(|e| e.id == id).ok_or("Запись не найдена")?;
    let (text, warning) = match &entry.output_path {
        Some(path) => match fs::read_to_string(path) {
            Ok(text) => (text, None),
            Err(_) => (String::new(), Some("Сохранённый текст недоступен. Проверьте файл или повторите распознавание.".to_owned())),
        },
        None => (String::new(), None),
    };
    Ok(HistoryDetail { entry, text, warning })
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
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        let target = model_path(&id)?;
        let (_, file, total) = *MODELS.iter().find(|(key, _, _)| *key == id).ok_or("Неизвестная модель")?;
        if target.exists() { emit(&app, "download", "Готово", total, total, 0); return Ok(()); }
        let log = base()?.join("temp").join(format!("{id}-download.log"));
        if id == "nemotron-3.5" {
            let exe = bin("nemo-speech").ok_or("Установите NeMo-Speech.cpp: см. README.md")?;
            let mut c = Command::new(exe); c.args(["pull", "nemotron-3.5"]);
            monitored(c, &app, "download", "Скачивание Nemotron", &log, total, None, true)?;
            fs::write(target, "downloaded").map_err(|e| e.to_string())?;
        } else {
            let partial = target.with_extension("part");
            let mut c = Command::new("curl"); c.args(["-fsSL", "--retry", "3", "--continue-at", "-", "-o"]).arg(&partial).arg(format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}"));
            monitored(c, &app, "download", "Скачивание Whisper", &log, total, Some(&partial), false)?;
            if fs::metadata(&partial).map_err(|e| e.to_string())?.len() != total { return Err("Размер модели не совпадает с ожидаемым; повторите загрузку".into()); }
            fs::rename(partial, target).map_err(|e| e.to_string())?;
        }
        let _ = fs::remove_file(log); Ok(())
    }).await.map_err(|e| e.to_string())?
}

#[derive(Serialize, Clone)]
struct PartialText { text: String }
fn partial(app: &tauri::AppHandle, text: &str) { let _ = app.emit("partial-text", PartialText { text: text.to_owned() }); }

fn stream_nemotron(app: &tauri::AppHandle, pcm: &Path, log: &Path) -> Result<String, String> {
    use tungstenite::{connect, Message, stream::MaybeTlsStream, Error as WsError};
    let exe = bin("nemo-speech").ok_or("Не найден nemo-speech; см. README.md")?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port(); drop(listener);
    let mut server = Command::new(exe).args(["serve", "--asr-model", "nemotron-3.5", "--host", "127.0.0.1", "--port", &port.to_string(), "--no-ui"])
        .stdout(Stdio::null()).stderr(fs::File::create(log).map_err(|e| e.to_string())?).spawn().map_err(|e| e.to_string())?;
    let result = (|| {
        let start = Instant::now();
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() { break; }
            if server.try_wait().map_err(|e| e.to_string())?.is_some() { return Err(format!("Запуск Nemotron: {}", tail(log))); }
            if start.elapsed() > Duration::from_secs(120) { return Err("Nemotron не запустился за 2 минуты".into()); }
            emit(app, "transcribe", "Загрузка Nemotron", 0, 0, start.elapsed().as_secs());
            thread::sleep(Duration::from_millis(400));
        }
        let (mut socket, _) = connect(format!("ws://127.0.0.1:{port}/v1/audio/transcriptions/realtime")).map_err(|e| e.to_string())?;
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() { stream.set_read_timeout(Some(Duration::from_millis(30))).map_err(|e| e.to_string())?; }
        socket.send(Message::Text(r#"{"type":"session.update","session":{"sample_rate":16000,"language":"auto"}}"#.into())).map_err(|e| e.to_string())?;
        let total = fs::metadata(pcm).map_err(|e| e.to_string())?.len();
        let mut reader = BufReader::new(fs::File::open(pcm).map_err(|e| e.to_string())?);
        let mut finals: Vec<String> = Vec::new(); let mut pending = String::new();
        let finished = std::cell::Cell::new(false);
        let mut on_event = |message: Message| -> Result<(), String> {
            if let Message::Text(text) = message {
                let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                match value["type"].as_str().unwrap_or("") {
                    "conversation.item.input_audio_transcription.delta" => {
                        pending.push_str(value["delta"].as_str().unwrap_or(""));
                        partial(app, &format!("{}{}{}", finals.join(" "), if finals.is_empty(){""}else{" "}, pending));
                    }
                    "conversation.item.input_audio_transcription.completed" => {
                        let text = value["transcript"].as_str().unwrap_or("").trim();
                        if !text.is_empty() { finals.push(text.to_string()); }
                        pending.clear(); partial(app, &finals.join(" "));
                    }
                    "input_audio_buffer.committed" => { finished.set(true); }
                    "error" => { return Err(value["error"]["message"].as_str().unwrap_or("Ошибка потокового распознавания").to_string()); }
                    _ => {}
                }
            }
            Ok(())
        };
        let mut chunk = [0u8; 32_000]; let mut sent = 0u64; let transfer = Instant::now();
        loop {
            let n = reader.read(&mut chunk).map_err(|e| e.to_string())?;
            if n == 0 { break; }
            socket.send(Message::Binary(chunk[..n].to_vec().into())).map_err(|e| e.to_string())?;
            sent += n as u64;
            emit(app, "transcribe", "Потоковое распознавание", sent, total, transfer.elapsed().as_secs());
            loop { match socket.read() {
                Ok(message) => on_event(message)?,
                Err(WsError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => break,
                Err(e) => return Err(e.to_string()),
            }}
        }
        socket.send(Message::Text(r#"{"type":"input_audio_buffer.commit"}"#.into())).map_err(|e| e.to_string())?;
        let deadline = Instant::now();
        while !finished.get() && deadline.elapsed() < Duration::from_secs(180) {
            match socket.read() {
                Ok(message) => on_event(message)?,
                Err(WsError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {},
                Err(e) => return Err(e.to_string()),
            }
            emit(app, "transcribe", "Завершение распознавания", 0, 0, transfer.elapsed().as_secs());
        }
        if !finished.get() { return Err("Nemotron не завершил потоковое распознавание".into()); }
        Ok(finals.join(" "))
    })();
    let _ = server.kill(); let _ = server.wait(); result
}

#[tauri::command]
async fn transcribe(app: tauri::AppHandle, id: String, model: Option<String>) -> Result<String, String> {
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        let settings = cloud::load()?;
        let model = model.unwrap_or_else(|| settings.local_model.clone());
        let entries = read_history()?;
        let index = entries.iter().position(|e| e.id == id).ok_or("Запись не найдена")?;
        let input = PathBuf::from(&entries[index].input_path);
        if !input.is_file() { return Err("Файл записи не найден в ~/.hearing/input".into()); }
        if settings.provider == "openrouter" {
            emit(&app, "transcribe", "Отправка аудио в OpenRouter", 0, 0, 0);
            let start = Instant::now();
            let ffmpeg = bin("ffmpeg").ok_or("Не найден FFmpeg; см. README.md")?;
            let chunk_dir = base()?.join("temp").join(format!("{id}-cloud-{}", clock()));
            fs::create_dir_all(&chunk_dir).map_err(|_| "Не удалось подготовить аудио")?;
            let result = (|| {
                emit(&app, "transcribe", "Подготовка аудио", 0, 0, 0);
                let mut c = Command::new(ffmpeg);
                c.args(["-y", "-v", "error", "-i"]).arg(&input).args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le", "-f", "segment", "-segment_time", "30", "-reset_timestamps", "1"]).arg(chunk_dir.join("chunk-%06d.wav"));
                command_result(c, "Подготовка аудио")?;
                let mut chunks = fs::read_dir(&chunk_dir).map_err(|_| "Не удалось прочитать аудио")?.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "wav")).collect::<Vec<_>>();
                chunks.sort();
                if chunks.is_empty() { return Err("Аудиофайл не содержит звука".into()); }
                let mut parts = Vec::new();
                for (index, chunk) in chunks.iter().enumerate() {
                    emit(&app, "transcribe", "Отправка фрагмента в OpenRouter", index as u64, chunks.len() as u64, start.elapsed().as_secs());
                    let prior = parts.join("\n");
                    let text = cloud::transcribe(&settings, chunk, |text| {
                        partial(&app, &format!("{}{}{}", prior, if prior.is_empty() {""} else {"\n"}, text));
                        emit(&app, "transcribe", "Распознавание OpenRouter", index as u64, chunks.len() as u64, start.elapsed().as_secs());
                    })?;
                    parts.push(text);
                    emit(&app, "transcribe", "Распознаны фрагменты", (index + 1) as u64, chunks.len() as u64, start.elapsed().as_secs());
                }
                Ok::<String, String>(parts.join("\n"))
            })();
            let _ = fs::remove_dir_all(chunk_dir);
            let text = result?;
            save_transcript(&id, format!("openrouter/{}", settings.openrouter_model), &text)?;
            emit(&app, "transcribe", "Готово", 1, 1, start.elapsed().as_secs());
            return Ok(text);
        }
        if !model_path(&model)?.exists() { return Err("Сначала скачайте выбранную модель".into()); }
        let ffmpeg = bin("ffmpeg").ok_or("Не найден FFmpeg; см. README.md")?;
        let temp = base()?.join("temp"); let wav = temp.join(format!("{id}.{}", if model == "nemotron-3.5" {"pcm"}else{"wav"})); let log = temp.join(format!("{id}.log")); let prefix = temp.join(format!("{id}-result"));
        let result = (|| {
            emit(&app, "transcribe", "Подготовка аудио", 0, 0, 0);
            let mut c = Command::new(ffmpeg); c.args(["-y", "-v", "error", "-i"]).arg(&input).args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"]); if model == "nemotron-3.5" { c.args(["-f", "s16le"]); } c.arg(&wav);
            let duration = audio_duration(&input).unwrap_or(0.0);
            let estimated = (duration * 32_000.0) as u64;
            monitored(c, &app, "transcribe", "Подготовка аудио", &log, estimated, Some(&wav), false)?;
            emit(&app, "transcribe", "Загрузка модели", 0, 0, 0);
            let text = if model == "nemotron-3.5" {
                stream_nemotron(&app, &wav, &log)?
            } else {
                let exe = bin("whisper-cli").ok_or("Не найден whisper-cli; см. README.md")?;
                let mut c = Command::new(exe); c.arg("-m").arg(model_path(&model)?).arg("-f").arg(&wav).arg("-otxt").arg("-of").arg(&prefix).arg("-pp");
                let live = temp.join(format!("{id}-live.log"));
                let run_result = monitored_whisper(c, &app, &log, &live);
                let _ = fs::remove_file(live); run_result?;
                let output = prefix.with_extension("txt");
                let text = fs::read_to_string(&output).map_err(|e| format!("Чтение текста: {e}"))?; let _ = fs::remove_file(output); text
            };
            save_transcript(&id, model, text.trim())?;
            emit(&app, "transcribe", "Готово", 1, 1, 0);
            Ok(text.trim().to_string())
        })();
        let _ = fs::remove_file(wav); let _ = fs::remove_file(log); result
    }).await.map_err(|e| e.to_string())?
}
#[tauri::command]
fn save_text(path: String, content: String) -> Result<(), String> {
    if !path.to_lowercase().ends_with(".txt") { return Err("Выберите файл с расширением .txt".into()); }
    fs::write(path, content).map_err(|e| e.to_string())
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default().plugin(tauri_plugin_opener::init()).plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![cloud::get_settings, cloud::save_settings, cloud::list_openrouter_models, runtime_status, model_status, list_history, import_audio, get_history, rename_history, delete_history, clear_history, download_model, transcribe, save_text])
        .run(tauri::generate_context!()).expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_clears_only_tracked_files() {
        let original = std::env::var_os("HOME");
        let root = std::env::temp_dir().join(format!("hearing-test-{}", clock()));
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("HOME", &root);
        let source = base().unwrap().join("input").join("hearing-test.m4a"); fs::write(&source, b"sample").unwrap();
        let entry = HistoryEntry { id: "hearing-test".into(), name: "sample.m4a".into(), input_path: source.to_string_lossy().into_owned(), output_path: None, model: None, created_at: clock(), size_bytes: None, duration_seconds: None };
        write_history(&[entry.clone()]).unwrap();
        let untouched = base().unwrap().join("input").join("my-own-file.m4a"); fs::write(&untouched, b"keep").unwrap();
        let output = base().unwrap().join("output").join(format!("{}.txt", entry.id)); fs::write(&output, "transcript").unwrap();
        let mut entries = read_history().unwrap(); entries[0].output_path = Some(output.to_string_lossy().into_owned()); write_history(&entries).unwrap();
        assert_eq!(get_history(entry.id.clone()).unwrap().text, "transcript");
        fs::remove_file(&output).unwrap();
        let missing = get_history(entry.id.clone()).unwrap();
        assert!(missing.text.is_empty()); assert!(missing.warning.is_some());
        assert_eq!(missing.entry.id, entry.id);
        clear_history().unwrap();
        assert!(read_history().unwrap().is_empty());
        assert!(!Path::new(&entry.input_path).exists()); assert!(!output.exists()); assert!(untouched.exists());
        if let Some(value) = original { std::env::set_var("HOME", value); } else { std::env::remove_var("HOME"); }
        fs::remove_dir_all(root).unwrap();
    }
}
