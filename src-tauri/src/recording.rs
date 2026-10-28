use crate::{platform::AudioSelection, JobGuard};
use std::{
    collections::HashSet,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
#[cfg(target_os = "android")]
use tauri::Manager;

type Writer = hound::WavWriter<BufWriter<fs::File>>;
// Desktop capture: WebView PCM at the device rate is decimated to 16 kHz in
// Rust (independent of WebView AudioContext resampling support) and the WAV
// header is committed every two seconds so a crash leaves a playable file.
struct Capture {
    writer: Writer,
    resampler: crate::audio::Resampler,
    input: Vec<f32>,
    output: Vec<f32>,
    unflushed: u32,
}
impl Capture {
    fn open(path: &Path, input_rate: u32) -> Result<Self, String> {
        if !(8000..=192000).contains(&input_rate) {
            return Err("errors.recordingStart".into());
        }
        let rate = input_rate.min(crate::audio::SPEECH_SAMPLE_RATE);
        Ok(Self {
            writer: create_writer(path, rate)?,
            resampler: crate::audio::Resampler::new(input_rate, rate),
            input: Vec::new(),
            output: Vec::new(),
            unflushed: 0,
        })
    }
    fn write_output(&mut self) -> Result<(), String> {
        for value in &self.output {
            let sample = (value * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            self.writer
                .write_sample(sample)
                .map_err(|_| "errors.recordingStop")?;
        }
        self.unflushed += self.output.len() as u32;
        self.output.clear();
        Ok(())
    }
    fn append(&mut self, samples: impl Iterator<Item = i16>) -> Result<(), String> {
        self.input.clear();
        self.input
            .extend(samples.map(|sample| sample as f32 / 32768.0));
        self.resampler.process(&self.input, &mut self.output);
        self.write_output()?;
        if self.unflushed >= self.writer.spec().sample_rate * 2 {
            // Rewrites the RIFF/data sizes and flushes buffered PCM.
            self.writer.flush().map_err(|_| "errors.recordingStop")?;
            self.unflushed = 0;
        }
        Ok(())
    }
    fn frames(&self) -> u32 {
        self.writer.duration()
    }
    fn finalize(mut self) -> Result<(), String> {
        self.resampler.finish(&mut self.output);
        let tail = self.write_output();
        let finalized = self
            .writer
            .finalize()
            .map_err(|_| "errors.recordingStop".to_owned());
        tail.and(finalized)
    }
}
struct Session {
    path: PathBuf,
    writer: Option<Capture>,
    _job: JobGuard,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.writer.take();
        // The successful stop moves ownership to FINISHED before dropping us.
        if !FINISHED
            .lock()
            .is_ok_and(|paths| paths.contains(&self.path))
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
#[cfg(target_os = "android")]
static LAST_RESULT: Mutex<Option<serde_json::Value>> = Mutex::new(None);
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
// A terminal journal and the response to an explicit stop describe the same
// operation. Give exactly one lifecycle operation ownership of that result.
#[cfg(any(target_os = "android", test))]
static NATIVE_LIFECYCLE: Mutex<()> = Mutex::new(());

#[cfg(test)]
static JOB_TEST_LOCK: Mutex<()> = Mutex::new(());
static FINISHED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn create_writer(path: &Path, sample_rate: u32) -> Result<Writer, String> {
    if !(8000..=192000).contains(&sample_rate) {
        return Err("errors.recordingStart".into());
    }
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| "errors.recordingStart")?;
    hound::WavWriter::new(
        BufWriter::new(file),
        hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|_| {
        let _ = fs::remove_file(path);
        "errors.recordingStart".into()
    })
}

#[cfg(target_os = "android")]
fn native_call<T: serde::de::DeserializeOwned + Send + 'static>(
    app: &tauri::AppHandle,
    command: &str,
    payload: serde_json::Value,
) -> Result<T, String> {
    // Bound lifecycle ownership without keeping an OS thread for a missing
    // Activity callback. The detached async bridge remains alive after timeout:
    // Tauri's callback requires its oneshot receiver for a safe late response.
    let timeout = if command == "start" { 120 } else { 15 };
    let app = app.clone();
    let command = command.to_owned();
    let (send, receive) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let result = app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .run_async::<T>(&command, payload)
            .await;
        let _ = send.send(result);
    });
    await_native_response(receive, std::time::Duration::from_secs(timeout))
}
#[cfg(any(target_os = "android", test))]
fn await_native_response<T>(
    receive: std::sync::mpsc::Receiver<Result<T, String>>,
    timeout: std::time::Duration,
) -> Result<T, String> {
    receive
        .recv_timeout(timeout)
        .map_err(|_| "errors.recordingInterrupted".to_owned())?
}
#[cfg(any(target_os = "android", test))]
fn retire_absent_session(
    active: &mut Option<Session>,
    status: &serde_json::Value,
) -> Option<Session> {
    // Native idle includes the absence of pending permission/start work, not
    // merely absence of an AudioRecord instance. Never infer it from a timeout.
    if status["state"] == "idle" {
        active.take()
    } else {
        None
    }
}
#[cfg(target_os = "android")]
fn native(app: &tauri::AppHandle, command: &str, path: Option<&Path>) -> Result<(), String> {
    native_call(
        app,
        command,
        path.map(|path| serde_json::json!({"path":path}))
            .unwrap_or_else(|| serde_json::json!({})),
    )
}
#[cfg(any(target_os = "android", test))]
fn validated_native_path(path: &Path, directory: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || native_recording_id(path).is_none() {
        return None;
    }
    if path.parent()?.canonicalize().ok()? != directory.canonicalize().ok()? {
        return None;
    }
    Some(directory.canonicalize().ok()?.join(path.file_name()?))
}
#[cfg(any(target_os = "android", test))]
fn restore_session(active: &mut Option<Session>, path: &Path) -> Result<bool, String> {
    if active.is_some() {
        return Ok(false);
    }
    *active = Some(Session {
        path: path.to_owned(),
        writer: None,
        _job: JobGuard::acquire()?,
    });
    Ok(true)
}

#[cfg(target_os = "android")]
fn adopt_native_session(app: &tauri::AppHandle, status: &serde_json::Value) -> Result<(), String> {
    if !matches!(
        status["state"].as_str(),
        Some("starting" | "recording" | "paused" | "stopping")
    ) || status["quick"] == true
    {
        return Ok(());
    }
    let Some(path) = status["path"]
        .as_str()
        .and_then(|path| validated_native_path(Path::new(path), &crate::base().ok()?.join("temp")))
    else {
        return Ok(());
    };
    let mut active = SESSION.lock().map_err(|_| "errors.recordingUnavailable")?;
    match restore_session(&mut active, &path) {
        Ok(true) => monitor(app.clone(), path),
        Ok(false) => {}
        Err(error) if error == "errors.operationBusy" => {}
        Err(error) => return Err(error),
    }
    Ok(())
}
#[cfg(target_os = "android")]
fn native_control_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if let Some(path) = SESSION
        .lock()
        .map_err(|_| "errors.recordingUnavailable")?
        .as_ref()
        .map(|session| session.path.clone())
    {
        return Ok(path);
    }
    let status: serde_json::Value = native_call(app, "recordingStatus", serde_json::json!({}))?;
    adopt_native_session(app, &status)?;
    SESSION
        .lock()
        .map_err(|_| "errors.recordingUnavailable")?
        .as_ref()
        .map(|session| session.path.clone())
        .ok_or_else(|| "errors.recordingUnavailable".into())
}

#[tauri::command]
pub async fn audio_recording_level(app: tauri::AppHandle) -> Result<f64, String> {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Deserialize)]
        struct Level {
            level: f64,
        }
        let level = tauri::async_runtime::spawn_blocking(move || {
            app.state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .run::<Level>("recordingLevel", serde_json::json!({}))
        })
        .await
        .map_err(|_| "errors.recordingUnavailable")??;
        return Ok(level.level.clamp(0.0, 1.0));
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(0.0)
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn hearfolio_request_microphone_permission() -> std::ffi::c_int;
}
fn permission_result(status: i32) -> Result<(), String> {
    match status {
        0 => Ok(()),
        1 | 2 => Err("errors.microphonePermissionDenied".into()),
        _ => Err("errors.recordingUnavailable".into()),
    }
}
#[tauri::command]
pub async fn request_microphone_permission() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return tauri::async_runtime::spawn_blocking(|| {
        // Apple native authorization is requested before WKWebView getUserMedia.
        permission_result(unsafe { hearfolio_request_microphone_permission() })
    })
    .await
    .map_err(|_| "errors.recordingUnavailable")?;
    #[cfg(not(target_os = "macos"))]
    permission_result(0)
}

#[tauri::command]
pub async fn start_audio_recording(
    app: tauri::AppHandle,
    sample_rate: Option<u32>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        let _lifecycle = NATIVE_LIFECYCLE
            .lock()
            .map_err(|_| "errors.recordingStart")?;
        let job = JobGuard::acquire()?;
        let extension = "wav";
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = crate::base()?.join("temp");
        let path = directory.join(format!("hearfolio-recording-{unique}.{extension}"));
        // Android dataDir may use /data/data while File.canonicalPath uses
        // /data/user/0. Allocate one identity before SESSION, ticket or journal
        // references it; the WAV itself does not exist until native capture.
        #[cfg(target_os = "android")]
        let path = validated_native_path(&path, &directory).ok_or("errors.recordingStart")?;
        let writer = if cfg!(target_os = "android") {
            None
        } else {
            Some(Capture::open(
                &path,
                sample_rate.ok_or("errors.recordingStart")?,
            )?)
        };
        *SESSION.lock().map_err(|_| "errors.recordingStart")? = Some(Session {
            path: path.clone(),
            writer,
            _job: job,
        });
        #[cfg(target_os = "android")]
        monitor(app.clone(), path.clone());
        #[cfg(target_os = "android")]
        if let Err(error) = native(&app, "start", Some(&path)) {
            // Any bridge failure can race capture already claimed by the
            // service. An error string is not evidence that native ownership
            // ended. The monitor or authoritative idle/terminal snapshot will
            // retire this session; never delete a possible active WAV here.
            return Err(error);
        }
        #[cfg(target_os = "android")]
        {
            if let Ok(mut result) = LAST_RESULT.lock() {
                *result = None;
            }
            let _ = fs::remove_file(crate::base()?.join("recording-result.json"));
        }
        #[cfg(not(target_os = "android"))]
        let _ = app;
        Ok(())
    })
    .await
    .map_err(|_| "errors.recordingStart")?
}

// The body is raw little-endian i16 PCM (no JSON number array). The frontend
// serializes calls, so running off the main thread preserves sample order.
#[tauri::command]
pub async fn append_audio_recording(request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("errors.recordingStop".into());
    };
    if bytes.len() % 2 != 0 || bytes.len() > 65536 * 2 {
        return Err("errors.recordingStop".into());
    }
    let bytes = bytes.clone();
    tauri::async_runtime::spawn_blocking(move || append_pcm(&bytes))
        .await
        .map_err(|_| "errors.recordingStop".to_owned())?
}
fn append_pcm(bytes: &[u8]) -> Result<(), String> {
    let mut state = SESSION.lock().map_err(|_| "errors.recordingStop")?;
    let capture = state
        .as_mut()
        .and_then(|session| session.writer.as_mut())
        .ok_or("errors.recordingUnavailable")?;
    capture.append(
        bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]])),
    )
}

/// Rewrites the RIFF and data sizes of a 44-byte-header PCM WAV from its actual
/// length. Returns the number of PCM bytes, or None when it is not our WAV.
fn repair_wav(path: &Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok()?;
    let mut header = [0u8; 44];
    file.read_exact(&mut header).ok()?;
    let le = |offset: usize, size: usize| {
        (0..size).fold(0u64, |value, index| {
            value | (header[offset + index] as u64) << (8 * index)
        })
    };
    if &header[0..4] != b"RIFF"
        || &header[8..16] != b"WAVEfmt "
        || &header[36..40] != b"data"
        || le(16, 4) != 16
        || le(20, 2) != 1
        || le(34, 2) != 16
    {
        return None;
    }
    let block = le(32, 2).max(1);
    let length = file.metadata().ok()?.len();
    let data = (length.checked_sub(44)? / block * block).min(0xffff_ffff - 36);
    if data == 0 {
        return None;
    }
    file.set_len(44 + data).ok()?;
    file.seek(SeekFrom::Start(4)).ok()?;
    file.write_all(&((data + 36) as u32).to_le_bytes()).ok()?;
    file.seek(SeekFrom::Start(40)).ok()?;
    file.write_all(&(data as u32).to_le_bytes()).ok()?;
    file.sync_all().ok()?;
    Some(data)
}

fn recording_name(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("wav");
    format!("Recording-{}.{extension}", crate::clock())
}

/// Desktop: WAVs left in temp by a crash or a failed stop. Their headers are
/// repaired (periodic flushes keep at most ~2 s unaccounted) and they are
/// registered as finished so they can be archived or discarded.
#[tauri::command]
pub async fn recover_audio_recordings() -> Result<Vec<AudioSelection>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        if cfg!(target_os = "android") {
            return Ok(vec![]);
        }
        let directory = crate::base()?.join("temp");
        let active = SESSION
            .lock()
            .map_err(|_| "errors.recordingUnavailable")?
            .as_ref()
            .map(|session| session.path.clone());
        let mut found = Vec::new();
        for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if Some(&path) == active.as_ref() || native_recording_id(&path).is_none() {
                continue;
            }
            let registered = FINISHED
                .lock()
                .map_err(|_| "errors.recordingUnavailable")?
                .contains(&path);
            if !registered && repair_wav(&path).is_none() {
                continue;
            }
            FINISHED
                .lock()
                .map_err(|_| "errors.recordingUnavailable")?
                .insert(path.clone());
            found.push(AudioSelection {
                name: Some(recording_name(&path)),
                path: path.to_string_lossy().into_owned(),
            });
        }
        Ok(found)
    })
    .await
    .map_err(|_| "errors.recordingUnavailable".to_owned())?
}

/// Captured audio the archive never took in. A failed or interrupted import
/// keeps the source in temp (or the quick-recording folder); after a restart
/// nothing else lists it, so the archive screen offers retry and export.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LooseRecording {
    path: String,
    size_bytes: u64,
    created_at: u64,
    duration_seconds: Option<f64>,
}

fn loose_recording_paths(app: &tauri::AppHandle) -> Result<Vec<PathBuf>, String> {
    let active = SESSION
        .lock()
        .map_err(|_| "errors.recordingUnavailable")?
        .as_ref()
        .map(|session| session.path.clone());
    // The service may still be writing a file the WebView has not adopted yet.
    #[cfg(target_os = "android")]
    let active = {
        let status: serde_json::Value = native_call(app, "recordingStatus", serde_json::json!({}))?;
        let terminal = ["idle", "finished", "error", "cancelled"]
            .contains(&status["state"].as_str().unwrap_or("idle"));
        match status["path"].as_str() {
            Some(path) if !terminal => Some(PathBuf::from(path)),
            _ => active,
        }
    };
    let archived: HashSet<String> = {
        let _lock = crate::history_lock()?;
        crate::read_history_at(&crate::base()?)?
            .into_iter()
            .map(|entry| entry.id)
            .collect()
    };
    let mut found = Vec::new();
    for entry in fs::read_dir(crate::base()?.join("temp"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        // Compare names: Android reports /data/user/0 where temp may read /data/data.
        let recording = active
            .as_ref()
            .is_some_and(|active| active.file_name() == path.file_name());
        if recording || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if native_recording_id(&path).is_some_and(|id| !archived.contains(&id)) {
            found.push(path);
        }
    }
    #[cfg(target_os = "android")]
    {
        let pending: serde_json::Value = app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .transfer("quickRecordings", serde_json::json!({}))?;
        for path in pending["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|path| path.as_str())
        {
            let path = PathBuf::from(path);
            if quick_recording_id(&path).is_ok_and(|id| !archived.contains(&id)) {
                found.push(path);
            }
        }
    }
    #[cfg(not(target_os = "android"))]
    let _ = app;
    Ok(found)
}

/// Accepts only a path the archive scan itself reported.
pub(crate) fn loose_recording(app: &tauri::AppHandle, path: &str) -> Result<PathBuf, String> {
    loose_recording_paths(app)?
        .into_iter()
        .find(|candidate| candidate == Path::new(path))
        .ok_or_else(|| "errors.recordMissing".into())
}

#[tauri::command]
pub async fn list_loose_recordings(app: tauri::AppHandle) -> Result<Vec<LooseRecording>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(loose_recording_paths(&app)?
            .into_iter()
            .filter_map(|path| {
                let metadata = fs::metadata(&path).ok()?;
                let created_at = metadata
                    .modified()
                    .ok()?
                    .duration_since(UNIX_EPOCH)
                    .ok()?
                    .as_millis() as u64;
                Some(LooseRecording {
                    duration_seconds: crate::audio::duration(&path)
                        .or_else(|| crate::platform::native_audio_duration(&app, &path)),
                    path: path.to_string_lossy().into_owned(),
                    size_bytes: metadata.len(),
                    created_at,
                })
            })
            .collect())
    })
    .await
    .map_err(|_| "errors.recordingUnavailable".to_owned())?
}

/// Registers a loose recording as finished so `import_audio` compacts it,
/// keeps it on failure and removes the source once archived.
#[tauri::command]
pub async fn adopt_loose_recording(app: tauri::AppHandle, path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _job = JobGuard::acquire()?;
        let path = loose_recording(&app, &path)?;
        FINISHED
            .lock()
            .map_err(|_| "errors.recordingUnavailable")?
            .insert(path);
        Ok(())
    })
    .await
    .map_err(|_| "errors.recordingUnavailable".to_owned())?
}

/// Recordings (not user imports) are compacted before archiving.
pub(crate) fn is_recording_source(path: &Path) -> bool {
    FINISHED.lock().is_ok_and(|paths| paths.contains(path)) || quick_recording_id(path).is_ok()
}

#[tauri::command]
pub async fn stop_audio_recording(app: tauri::AppHandle) -> Result<AudioSelection, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        let _lifecycle = NATIVE_LIFECYCLE
            .lock()
            .map_err(|_| "errors.recordingStop")?;
        #[cfg(target_os = "android")]
        let path = native_control_path(&app)?;
        // Retain the registered session during the bridge call. If the Activity
        // loses its response, the journal monitor/snapshot can still complete
        // and archive it. Never take a newer recording after a delayed response.
        #[cfg(target_os = "android")]
        native(&app, "stop", Some(&path))?;
        let mut active = SESSION.lock().map_err(|_| "errors.recordingStop")?;
        #[cfg(target_os = "android")]
        let mut session = take_terminal_session(&mut active, "finished", &path)
            .ok_or("errors.recordingUnavailable")?;
        #[cfg(not(target_os = "android"))]
        let mut session = active.take().ok_or("errors.recordingUnavailable")?;
        drop(active);
        #[cfg(not(target_os = "android"))]
        let _ = app;
        let path = session.path.clone();
        if let Some(capture) = session.writer.take() {
            if capture.frames() == 0 {
                // Dropping the unregistered session removes the empty file.
                return Err("errors.recordingEmpty".into());
            }
            // Register before finalizing: a failed header write must never let
            // Session::drop delete captured audio. Repair the header instead.
            FINISHED
                .lock()
                .map_err(|_| "errors.recordingStop")?
                .insert(path.clone());
            if capture.finalize().is_err() && repair_wav(&path).is_none() {
                return Err("errors.recordingStop".into());
            }
        }
        if fs::metadata(&path)
            .map_err(|_| "errors.recordingStop")?
            .len()
            <= 44
        {
            if let Ok(mut paths) = FINISHED.lock() {
                paths.remove(&path);
            }
            return Err("errors.recordingEmpty".into());
        }
        FINISHED
            .lock()
            .map_err(|_| "errors.recordingStop")?
            .insert(path.clone());
        Ok(AudioSelection {
            name: Some(recording_name(&path)),
            path: path.to_string_lossy().into_owned(),
        })
    })
    .await
    .map_err(|_| "errors.recordingStop")?
}

#[tauri::command]
pub async fn cancel_audio_recording(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        let _lifecycle = NATIVE_LIFECYCLE
            .lock()
            .map_err(|_| "errors.recordingStop")?;
        #[cfg(target_os = "android")]
        {
            let path = match native_control_path(&app) {
                Ok(path) => Some(path),
                Err(error) if error == "errors.recordingUnavailable" => None,
                Err(error) => return Err(error),
            };
            if let Some(path) = path {
                native(&app, "cancel", Some(&path))?;
                let mut active = SESSION.lock().map_err(|_| "errors.recordingStop")?;
                take_terminal_session(&mut active, "cancelled", &path);
            }
            return Ok(());
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = app;
            SESSION.lock().map_err(|_| "errors.recordingStop")?.take();
            Ok(())
        }
    })
    .await
    .map_err(|_| "errors.recordingStop")?
}

// Only files allocated and successfully stopped in this process may be removed.
// Failed imports retain their recording so the user can retry.
pub fn imported(path: &Path) {
    let _ = discard(path);
    #[cfg(target_os = "android")]
    let _ = fs::remove_file(format!("{}.state.json", path.to_string_lossy()));
}
fn discard(path: &Path) -> Result<(), String> {
    let mut paths = FINISHED.lock().map_err(|_| "errors.recordingDiscard")?;
    if paths.contains(path) {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("errors.recordingDiscard".into()),
        }
        paths.remove(path);
    }
    Ok(())
}
#[tauri::command]
pub fn discard_audio_recording(path: String) -> Result<(), String> {
    let _job = JobGuard::acquire()?;
    discard(Path::new(&path))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_recording_survives_retry_and_cancel_releases_job_without_deleting_foreign_file() {
        let _serial = JOB_TEST_LOCK.lock().unwrap();
        let directory = std::env::temp_dir().join(format!(
            "hearfolio-recording-lifecycle-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("recording.wav");
        let foreign = directory.join("foreign.wav");
        fs::write(&foreign, b"unrelated").unwrap();
        let mut writer = create_writer(&path, 16000).unwrap();
        writer.write_sample(100i16).unwrap();
        writer.finalize().unwrap();
        let session = Session {
            path: path.clone(),
            writer: None,
            _job: JobGuard::acquire().unwrap(),
        };
        assert!(JobGuard::acquire().is_err());
        FINISHED.lock().unwrap().insert(path.clone());
        drop(session);
        assert!(path.is_file());
        imported(&foreign);
        assert!(foreign.is_file());
        let job = JobGuard::acquire().unwrap();
        imported(&path);
        assert!(!path.exists());
        let cancelled = directory.join("cancelled.wav");
        let session = Session {
            writer: Some(Capture::open(&cancelled, 16000).unwrap()),
            path: cancelled.clone(),
            _job: job,
        };
        drop(session);
        assert!(!cancelled.exists());
        drop(JobGuard::acquire().unwrap());
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn desktop_capture_decimates_to_16_khz_and_stays_recoverable_between_flushes() {
        let path = std::env::temp_dir().join(format!(
            "hearfolio-capture-test-{}.wav",
            uuid::Uuid::new_v4()
        ));
        let mut capture = Capture::open(&path, 48000).unwrap();
        let tone = |index: usize| {
            (f64::sin(index as f64 * 2.0 * std::f64::consts::PI * 440.0 / 48000.0) * 12000.0) as i16
        };
        // 3 s of 48 kHz input in 4096-sample WebView chunks.
        let mut sent = 0;
        while sent < 48000 * 3 {
            let count = 4096.min(48000 * 3 - sent);
            capture.append((sent..sent + count).map(tone)).unwrap();
            sent += count;
        }
        // Simulated crash: the periodic flush already committed a valid header.
        let crashed = hound::WavReader::open(&path).unwrap();
        assert_eq!(crashed.spec().sample_rate, 16000);
        assert!(crashed.duration() >= 16000 * 2);
        drop(crashed);
        capture.finalize().unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().sample_rate, 16000);
        assert_eq!(reader.duration(), 16000 * 3);
        let energy: f64 = reader
            .samples::<i16>()
            .map(|sample| (sample.unwrap() as f64 / 32768.0).powi(2))
            .sum::<f64>()
            / (16000.0 * 3.0);
        let expected = (12000.0f64 / 32768.0).powi(2) / 2.0;
        assert!((energy / expected - 1.0).abs() < 0.05, "energy {energy}");
        // A stale header (e.g. failed finalize) is repaired from file length.
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(40)).unwrap();
            file.write_all(&0u32.to_le_bytes()).unwrap();
        }
        assert_eq!(repair_wav(&path), Some(16000 * 3 * 2));
        assert_eq!(hound::WavReader::open(&path).unwrap().duration(), 16000 * 3);
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn native_permission_contract_never_treats_denied_or_unknown_as_granted() {
        assert!(permission_result(0).is_ok());
        for status in [1, 2] {
            assert_eq!(
                permission_result(status).unwrap_err(),
                "errors.microphonePermissionDenied"
            );
        }
        assert_eq!(
            permission_result(3).unwrap_err(),
            "errors.recordingUnavailable"
        );
    }
    #[test]
    fn recording_writer_produces_decodable_pcm_and_rejects_invalid_rate() {
        let path = std::env::temp_dir().join(format!(
            "hearfolio-recording-test-{}.wav",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(create_writer(&path, 0).is_err());
        assert!(!path.exists());
        let mut writer = create_writer(&path, 16000).unwrap();
        for sample in [0i16, 32767, -32768, 1024] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().sample_rate, 16000);
        assert_eq!(
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![0, 32767, -32768, 1024]
        );
        fs::remove_file(path).unwrap();
    }
}

#[cfg(any(target_os = "android", test))]
fn take_terminal_session(
    session: &mut Option<Session>,
    state: &str,
    path: &Path,
) -> Option<Session> {
    if matches!(state, "finished" | "error" | "cancelled")
        && session.as_ref().is_some_and(|session| session.path == path)
    {
        session.take()
    } else {
        None
    }
}

#[cfg(any(target_os = "android", test))]
fn native_state_payload(status: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"state":status["state"],"stateRevision":status["stateRevision"],"path":status["path"],"elapsedSeconds":status["elapsedSeconds"],"elapsedMillis":status["elapsedMillis"].as_u64().unwrap_or_else(||status["elapsedSeconds"].as_u64().unwrap_or(0)*1000),"envelope":status["envelope"],"envelopeSequence":status["envelopeSequence"],"envelopeIntervalMs":status["envelopeIntervalMs"]})
}

#[cfg(target_os = "android")]
fn monitor(app: tauri::AppHandle, path: PathBuf) {
    use tauri::Emitter;
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let Ok(_lifecycle) = NATIVE_LIFECYCLE.lock() else {
                break;
            };
            if !SESSION
                .lock()
                .is_ok_and(|s| s.as_ref().is_some_and(|s| s.path == path))
            {
                break;
            }
            if started.elapsed() >= std::time::Duration::from_secs(4 * 60 * 60 + 60) {
                let session = SESSION.lock().ok().and_then(|mut session| {
                    if session.as_ref().is_some_and(|session| session.path == path) {
                        session.take()
                    } else {
                        None
                    }
                });
                if let Some(session) = session {
                    // The service has a four hour deadline. Preserve the source if
                    // its final journal was unavailable, then release Rust ownership.
                    if let Ok(mut paths) = FINISHED.lock() {
                        paths.insert(path.clone());
                    }
                    let payload = serde_json::json!({"path":path,"error":"errors.recordingInterrupted","selection":{"path":path,"name":"Recording.wav"}});
                    store_recording_result(&payload);
                    drop(session);
                    let _ = app.emit("recording-native-finished", payload);
                }
                break;
            }
            let status: serde_json::Value =
                match fs::read(format!("{}.state.json", path.to_string_lossy()))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                {
                    Some(status) => status,
                    None => continue,
                };
            if status["path"].as_str() != path.to_str() {
                continue;
            }
            let state = status["state"].as_str().unwrap_or("");
            let _ = app.emit("recording-native-state", native_state_payload(&status));
            if state == "finished" {
                let session = SESSION.lock().ok().and_then(|mut s| {
                    if s.as_ref().is_some_and(|s| s.path == path) {
                        s.take()
                    } else {
                        None
                    }
                });
                if let Some(session) = session {
                    // Preserve the source on a failed archive import.
                    if let Ok(mut paths) = FINISHED.lock() {
                        paths.insert(path.clone());
                    }
                    let payload = archive_native_recording(
                        &app,
                        &path,
                        status["error"].as_str(),
                        &status["stateRevision"],
                    );
                    drop(session);
                    store_recording_result(&payload);
                    if payload["entry"].is_object() {
                        let _ = native_call::<()>(
                            &app,
                            "acknowledgeRecording",
                            serde_json::json!({"path":path}),
                        );
                    }
                    let _ = app.emit("recording-native-finished", payload);
                    let _ = fs::remove_file(format!("{}.state.json", path.to_string_lossy()));
                }
                break;
            }
            if state == "error" || state == "cancelled" {
                let mut session = match SESSION.lock() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                if session.as_ref().is_some_and(|s| s.path == path) {
                    if state == "error" && path.exists() {
                        if let Ok(mut paths) = FINISHED.lock() {
                            paths.insert(path.clone());
                        }
                    }
                    session.take();
                }
                let payload = serde_json::json!({"path":path,"stateRevision":status["stateRevision"],"error":status["error"].as_str().unwrap_or("errors.recordingEmpty"),"selection":if state=="error" && path.exists() {Some(serde_json::json!({"path":path,"name":"Recording.wav"}))} else {None}});
                store_recording_result(&payload);
                let _ = app.emit("recording-native-finished", payload);
                break;
            }
        }
    });
}

#[tauri::command]
pub async fn recording_activity_snapshot(
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os="android")]
        {
            let _lifecycle=NATIVE_LIFECYCLE.lock().map_err(|_|"errors.recordingUnavailable")?;
            let mut status:serde_json::Value=native_call(&app,"recordingStatus",serde_json::json!({}))?;
            let absent_session={
                let mut active=SESSION.lock().map_err(|_|"errors.recordingUnavailable")?;
                retire_absent_session(&mut active,&status)
            };
            if let Some(session)=absent_session {
                let path=session.path.clone();
                if path.exists() {
                    FINISHED.lock().map_err(|_|"errors.recordingStop")?.insert(path.clone());
                    store_recording_result(&serde_json::json!({"path":path,"error":"errors.recordingInterrupted","selection":{"path":path,"name":"Recording.wav"}}));
                }
                drop(session);
            }
            // The service survives Activity/WebView recreation. Re-register its
            // active regular recording before exposing controls to the frontend.
            adopt_native_session(&app,&status)?;
            // A visible native snapshot is authoritative when a final journal
            // write failed. Claim only the matching terminal session so its job
            // guard is released and its private source survives failed import.
            let terminal_state=status["state"].as_str().unwrap_or("");
            let terminal_path=status["path"].as_str().map(PathBuf::from);
            let terminal_session=if let Some(path)=terminal_path.as_deref() {
                let mut session=SESSION.lock().map_err(|_|"errors.recordingStop")?;
                take_terminal_session(&mut session,terminal_state,path)
            } else { None };
            if let Some(session)=terminal_session {
                let path=session.path.clone();
                if terminal_state=="finished" || (terminal_state=="error" && path.exists()) {
                    FINISHED.lock().map_err(|_|"errors.recordingStop")?.insert(path.clone());
                }
                let payload=if terminal_state=="finished" {
                    archive_native_recording(&app,&path,status["error"].as_str(),&status["stateRevision"])
                } else {
                    serde_json::json!({"path":path,"stateRevision":status["stateRevision"],"error":status["error"].as_str().unwrap_or("errors.recordingInterrupted"),"selection":if path.exists(){Some(serde_json::json!({"path":path,"name":"Recording.wav"}))}else{None}})
                };
                let success=payload["entry"].is_object();
                store_recording_result(&payload);drop(session);
                if success { native_call::<()>(&app,"acknowledgeRecording",serde_json::json!({"path":path}))?;status["state"]=serde_json::json!("idle"); }
            } else if status["state"]=="finished" && status["quick"]!=true {
                // Recover a completed service recording after process recreation.
                // A foreground import may already own this completed result.
                // Keep returning the authoritative snapshot while it finishes.
                if let (Ok(_job),Some(path))=(JobGuard::acquire(),terminal_path) {
                    if let Some(file)=validated_native_path(&path,&crate::base()?.join("temp")) {
                        FINISHED.lock().map_err(|_|"errors.recordingStop")?.insert(file.clone());
                        let payload=archive_native_recording(&app,&file,status["error"].as_str(),&status["stateRevision"]);
                        let success=payload["entry"].is_object();store_recording_result(&payload);
                        if success { native_call::<()>(&app,"acknowledgeRecording",serde_json::json!({"path":file}))?;status["state"]=serde_json::json!("idle"); }
                    }
                }
            }
            let result=LAST_RESULT.lock().map_err(|_|"errors.recordingStop")?.clone().or_else(||fs::read(crate::base().ok()?.join("recording-result.json")).ok().and_then(|b|serde_json::from_slice(&b).ok()));
            let mut snapshot=native_state_payload(&status);
            snapshot["quick"]=status["quick"].clone();snapshot["level"]=status["level"].clone();snapshot["error"]=status["error"].clone();snapshot["interrupted"]=status["interrupted"].clone();snapshot["result"]=serde_json::json!(result);
            Ok(snapshot)
        }
        #[cfg(not(target_os="android"))]
        {let _=app; Ok(serde_json::json!({"state":"idle"}))}
    }).await.map_err(|e|e.to_string())?
}

// A stable private filename makes import idempotent if the process exits after
// committing history but before deleting the quick-recording source.
fn quick_recording_id(path: &Path) -> Result<String, String> {
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        != Some("hearfolio-quick-recordings")
    {
        return Err("Invalid quick recording location".into());
    }
    let uuid = path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("hearfolio-recording-"))
        .and_then(|s| s.strip_suffix(".m4a").or_else(|| s.strip_suffix(".wav")))
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or("Invalid quick recording name")?;
    Ok(format!("hearing-quick-{uuid}"))
}
#[cfg(target_os = "android")]
fn import_quick_recording(
    app: &tauri::AppHandle,
    path: &str,
) -> Result<crate::records::HistoryEntry, String> {
    let source = Path::new(path);
    let id = quick_recording_id(source)?;
    let existing = {
        let _lock = crate::history_lock()?;
        crate::read_history_at(&crate::base()?)?
            .into_iter()
            .find(|e| e.id == id)
    };
    let entry = if let Some(entry) = existing {
        // Never discard the source if the archived copy is missing or damaged.
        if !crate::compact::archived_matches(source, Path::new(&entry.input_path))? {
            return Err("Archived quick recording differs; source preserved".into());
        }
        entry
    } else {
        let configuration = crate::cloud::load()?.last_configuration;
        crate::import_audio_with_id(
            app,
            path.into(),
            Some(format!("Recording-{}.wav", crate::clock())),
            configuration,
            Some(id),
        )?
    };
    fs::remove_file(source).map_err(|e| format!("errors.audioRead|{e}"))?;
    Ok(entry)
}
#[tauri::command]
pub async fn import_quick_recordings(
    app: tauri::AppHandle,
) -> Result<Vec<crate::records::HistoryEntry>, String> {
    #[cfg(target_os = "android")]
    {
        let job = JobGuard::acquire()?;
        tauri::async_runtime::spawn_blocking(move || {
            let _job = job;
            let pending: serde_json::Value = app
                .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .transfer("quickRecordings", serde_json::json!({}))?;
            let mut imported = Vec::new();
            let mut failure = None;
            for path in pending["paths"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str())
            {
                // One damaged file must not hold back the rest; it stays in
                // the archive screen's unprocessed list for retry or export.
                match import_quick_recording(&app, path) {
                    Ok(entry) => imported.push(entry),
                    Err(error) => failure = failure.or(Some(error)),
                }
            }
            match failure {
                Some(error) if imported.is_empty() => Err(error),
                _ => Ok(imported),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(vec![])
    }
}
#[cfg(test)]
mod quick_tests {
    use super::*;
    #[test]
    fn quick_recording_ids_survive_retries_and_reject_unfinished_or_foreign_files() {
        let uuid = uuid::Uuid::new_v4();
        let p = PathBuf::from(format!(
            "/private/hearfolio-quick-recordings/hearfolio-recording-{uuid}.m4a"
        ));
        assert_eq!(
            quick_recording_id(&p).unwrap(),
            format!("hearing-quick-{uuid}")
        );
        assert_eq!(
            quick_recording_id(&p).unwrap(),
            quick_recording_id(&p).unwrap()
        );
        assert!(quick_recording_id(&p.with_extension("m4a.partial")).is_err());
        assert_eq!(
            quick_recording_id(&p.with_extension("wav")).unwrap(),
            quick_recording_id(&p).unwrap()
        );
        assert!(quick_recording_id(&p.with_extension("wav.partial")).is_err());
        assert!(quick_recording_id(Path::new("/private/hearfolio-recording-invalid.m4a")).is_err());
        assert!(quick_recording_id(Path::new(
            "/private/hearfolio-quick-recordings/hearfolio-recording-invalid.m4a"
        ))
        .is_err());
    }
}

#[tauri::command]
pub async fn recording_quick_control(
    app: tauri::AppHandle,
    action: String,
    path: Option<String>,
) -> Result<(), String> {
    if !["stop", "cancel", "pause", "resume"].contains(&action.as_str()) {
        return Err("errors.recordingUnavailable".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        {
            let _lifecycle = NATIVE_LIFECYCLE
                .lock()
                .map_err(|_| "errors.recordingUnavailable")?;
            let path = if let Some(path) = path {
                path
            } else {
                let status: serde_json::Value =
                    native_call(&app, "recordingStatus", serde_json::json!({}))?;
                status["path"]
                    .as_str()
                    .ok_or("errors.recordingUnavailable")?
                    .to_owned()
            };
            native_call::<()>(
                &app,
                "quickControl",
                serde_json::json!({"id":"quick","stage":action,"path":path}),
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (app, action, path);
            Err("errors.recordingUnavailable".into())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// Stable native recording IDs prevent duplicate history entries when a process
// exits after archiving but before acknowledging/deleting the source.
pub(crate) fn allocated_recording_id(path: &Path) -> Option<String> {
    if FINISHED.lock().ok()?.contains(path) {
        native_recording_id(path)
    } else {
        None
    }
}
pub(crate) fn native_recording_id(path: &Path) -> Option<String> {
    if path.parent()?.file_name()?.to_str()? != "temp" {
        return None;
    }
    let value = path
        .file_name()?
        .to_str()?
        .strip_prefix("hearfolio-recording-")?
        .strip_suffix(".wav")?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("hearing-recording-{value}"))
}
#[cfg(target_os = "android")]
fn archive_native_recording(
    app: &tauri::AppHandle,
    path: &Path,
    warning: Option<&str>,
    state_revision: &serde_json::Value,
) -> serde_json::Value {
    let result = (|| {
        let id = native_recording_id(path).ok_or("errors.recordingStop")?;
        let existing = {
            let _lock = crate::history_lock()?;
            crate::read_history_at(&crate::base()?)?
                .into_iter()
                .find(|e| e.id == id)
        };
        if let Some(entry) = existing {
            if !Path::new(&entry.input_path).is_file()
                || (path.exists()
                    && !crate::compact::archived_matches(path, Path::new(&entry.input_path))?)
            {
                return Err("errors.recordingStop".into());
            }
            imported(path);
            return Ok(entry);
        }
        let configuration = crate::cloud::load()?.last_configuration;
        crate::import_audio_with_id(
            app,
            path.to_string_lossy().into_owned(),
            Some(format!("Recording-{}.wav", crate::clock())),
            configuration,
            Some(id),
        )
    })();
    match result {
        Ok(entry) => {
            serde_json::json!({"path":path,"stateRevision":state_revision,"entry":entry,"warning":warning,"error":warning})
        }
        Err(error) => {
            serde_json::json!({"path":path,"stateRevision":state_revision,"error":error,"warning":warning,"selection":{"path":path,"name":"Recording.wav"}})
        }
    }
}
#[cfg(target_os = "android")]
fn store_recording_result(payload: &serde_json::Value) {
    use std::io::Write;
    if let Ok(mut last) = LAST_RESULT.lock() {
        *last = Some(payload.clone());
    }
    let _ = (|| -> Result<(), String> {
        let path = crate::base()?.join("recording-result.json");
        let temp = path.with_extension("json.tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(|_| "errors.recordingStop")?;
        file.write_all(&serde_json::to_vec(payload).map_err(|_| "errors.recordingStop")?)
            .and_then(|_| file.sync_all())
            .map_err(|_| "errors.recordingStop")?;
        fs::rename(temp, path).map_err(|_| "errors.recordingStop".into())
    })();
}
#[tauri::command]
pub async fn pause_audio_recording(app: tauri::AppHandle) -> Result<(), String> {
    control_recording(app, "pause").await
}
#[tauri::command]
pub async fn resume_audio_recording(app: tauri::AppHandle) -> Result<(), String> {
    control_recording(app, "resume").await
}
async fn control_recording(app: tauri::AppHandle, _stage: &'static str) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        {
            let _lifecycle = NATIVE_LIFECYCLE
                .lock()
                .map_err(|_| "errors.recordingUnavailable")?;
            let path = native_control_path(&app)?;
            native_call::<()>(
                &app,
                "recordingControl",
                serde_json::json!({"stage":_stage,"path":path}),
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = app;
            Err("errors.recordingUnavailable".into())
        }
    })
    .await
    .map_err(|_| "errors.recordingUnavailable".to_owned())?
}
#[tauri::command]
pub async fn open_android_battery_settings(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        return app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .transfer("openBatterySettings", serde_json::json!({}));
        #[cfg(not(target_os = "android"))]
        {
            let _ = app;
            Ok(())
        }
    })
    .await
    .map_err(|_| "errors.operationFailed".to_owned())?
}

#[cfg(test)]
mod native_recovery_tests {
    use super::*;
    #[test]
    fn native_state_preserves_session_revision_and_measured_time_without_wall_clock_extrapolation()
    {
        let status = serde_json::json!({"state":"paused","stateRevision":7,"path":"/private/recording.wav","elapsedSeconds":3,"elapsedMillis":3125,"updatedAt":0,"envelope":[0.1,0.7],"envelopeSequence":25,"envelopeIntervalMs":125});
        let payload = native_state_payload(&status);
        assert_eq!(payload["elapsedMillis"], 3125);
        assert_eq!(payload["elapsedSeconds"], 3);
        assert_eq!(payload["stateRevision"], 7);
        assert_eq!(payload["path"], status["path"]);
        assert_eq!(payload["envelope"], status["envelope"]);
        let mut resumed = status.clone();
        resumed["state"] = serde_json::json!("recording");
        resumed["stateRevision"] = serde_json::json!(8);
        let next = native_state_payload(&resumed);
        assert_eq!(next["elapsedMillis"], payload["elapsedMillis"]);
        assert_eq!(next["envelopeSequence"], payload["envelopeSequence"]);
        assert!(next["stateRevision"].as_u64() > payload["stateRevision"].as_u64());
    }
    #[test]
    fn terminal_snapshot_releases_matching_session_after_journal_failure_and_preserves_source() {
        let _serial = JOB_TEST_LOCK.lock().unwrap();
        let path =
            std::env::temp_dir().join(format!("hearfolio-recording-{}.wav", uuid::Uuid::new_v4()));
        fs::write(&path, "recoverable bytes").unwrap();
        let mut session = Some(Session {
            path: path.clone(),
            writer: None,
            _job: JobGuard::acquire().unwrap(),
        });
        assert!(take_terminal_session(&mut session, "recording", &path).is_none());
        assert!(
            take_terminal_session(&mut session, "finished", Path::new("/foreign.wav")).is_none()
        );
        assert!(JobGuard::acquire().is_err());
        let terminal = take_terminal_session(&mut session, "finished", &path).unwrap();
        FINISHED.lock().unwrap().insert(path.clone());
        drop(terminal);
        assert!(session.is_none());
        assert!(path.exists());
        drop(JobGuard::acquire().unwrap());
        discard(&path).unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn lost_bridge_response_releases_lifecycle_and_authoritative_idle_releases_pending_job() {
        let _serial = JOB_TEST_LOCK.lock().unwrap();
        let path =
            std::env::temp_dir().join(format!("hearfolio-pending-{}.wav", uuid::Uuid::new_v4()));
        let mut active = None;
        restore_session(&mut active, &path).unwrap();
        {
            let _control = NATIVE_LIFECYCLE.lock().unwrap();
            let (_late_response, receive) = std::sync::mpsc::channel::<Result<(), String>>();
            assert_eq!(
                await_native_response(receive, std::time::Duration::from_millis(1)).unwrap_err(),
                "errors.recordingInterrupted"
            );
        }
        let _next_control = NATIVE_LIFECYCLE.try_lock().unwrap();
        assert!(retire_absent_session(
            &mut active,
            &serde_json::json!({"state":"starting","path":path})
        )
        .is_none());
        assert!(JobGuard::acquire().is_err());
        drop(retire_absent_session(&mut active, &serde_json::json!({"state":"idle"})).unwrap());
        assert!(restore_session(&mut active, &path).unwrap());
        drop(active);
        drop(JobGuard::acquire().unwrap());
    }
    #[test]
    fn timed_out_old_response_and_terminal_journal_cannot_claim_a_new_recording() {
        let _serial = JOB_TEST_LOCK.lock().unwrap();
        let old_path =
            std::env::temp_dir().join(format!("hearfolio-old-{}.wav", uuid::Uuid::new_v4()));
        let new_path =
            std::env::temp_dir().join(format!("hearfolio-new-{}.wav", uuid::Uuid::new_v4()));
        let (late, receive) = std::sync::mpsc::channel::<Result<(), String>>();
        assert!(await_native_response(receive, std::time::Duration::from_millis(1)).is_err());
        let mut active = None;
        restore_session(&mut active, &new_path).unwrap();
        // The detached bridge intentionally ignores its send failure after the
        // caller timed out. It does not mutate SESSION on a late callback.
        assert!(late.send(Ok(())).is_err());
        assert!(take_terminal_session(&mut active, "finished", &old_path).is_none());
        assert!(take_terminal_session(&mut active, "cancelled", &old_path).is_none());
        assert_eq!(active.as_ref().unwrap().path, new_path);
        assert!(JobGuard::acquire().is_err());
        drop(active);
        drop(JobGuard::acquire().unwrap());
    }
    #[test]
    fn recreated_session_reacquires_job_and_preserves_an_existing_owner() {
        let _serial = JOB_TEST_LOCK.lock().unwrap();
        let path =
            std::env::temp_dir().join(format!("hearfolio-restored-{}.wav", uuid::Uuid::new_v4()));
        let mut active = None;
        assert!(restore_session(&mut active, &path).unwrap());
        assert!(JobGuard::acquire().is_err());
        assert!(!restore_session(&mut active, Path::new("/foreign.wav")).unwrap());
        assert_eq!(active.as_ref().unwrap().path, path);
        let terminal = take_terminal_session(&mut active, "finished", &path).unwrap();
        drop(terminal);
        drop(JobGuard::acquire().unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn android_data_directory_alias_uses_one_identity_for_session_and_terminal_journal() {
        let _serial = JOB_TEST_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("hearfolio-path-alias-{}", uuid::Uuid::new_v4()));
        let actual = root.join("user-0");
        let alias = root.join("data-data");
        fs::create_dir_all(actual.join("temp")).unwrap();
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        let raw = alias.join("temp/hearfolio-recording-123.wav");
        let native = actual
            .canonicalize()
            .unwrap()
            .join("temp/hearfolio-recording-123.wav");
        assert_ne!(raw, native);
        assert!(!raw.exists());
        let allocated = validated_native_path(&raw, &alias.join("temp")).unwrap();
        assert_eq!(allocated, native);
        let mut active = None;
        restore_session(&mut active, &allocated).unwrap();
        assert_eq!(active.as_ref().unwrap().path, native);
        let stopped = take_terminal_session(&mut active, "finished", &native).unwrap();
        drop(stopped);
        drop(JobGuard::acquire().unwrap());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_recovery_accepts_only_private_allocated_paths_even_before_file_exists() {
        let root = std::env::temp_dir().join(format!("hearfolio-path-{}", uuid::Uuid::new_v4()));
        let private = root.join("temp");
        let foreign = root.join("foreign").join("temp");
        fs::create_dir_all(&private).unwrap();
        fs::create_dir_all(&foreign).unwrap();
        let path = private.join("hearfolio-recording-123.wav");
        assert_eq!(
            validated_native_path(&path, &private),
            Some(
                private
                    .canonicalize()
                    .unwrap()
                    .join("hearfolio-recording-123.wav")
            )
        );
        assert!(
            validated_native_path(&foreign.join("hearfolio-recording-123.wav"), &private).is_none()
        );
        assert!(validated_native_path(&private.join("unknown.wav"), &private).is_none());
        assert!(
            validated_native_path(Path::new("temp/hearfolio-recording-123.wav"), &private)
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn explicit_stop_claim_is_serialized_against_journal_monitor() {
        let _serial = JOB_TEST_LOCK.lock().unwrap();
        let path =
            std::env::temp_dir().join(format!("hearfolio-race-{}.wav", uuid::Uuid::new_v4()));
        let mut active = None;
        restore_session(&mut active, &path).unwrap();
        // A stop owns lifecycle while waiting for its native response. The
        // journal reader must not claim the just-finished session underneath it.
        let control = NATIVE_LIFECYCLE.lock().unwrap();
        assert!(NATIVE_LIFECYCLE.try_lock().is_err());
        let stopped = take_terminal_session(&mut active, "finished", &path).unwrap();
        drop(control);
        let _monitor = NATIVE_LIFECYCLE.lock().unwrap();
        assert!(take_terminal_session(&mut active, "finished", &path).is_none());
        drop(stopped);
        drop(JobGuard::acquire().unwrap());
    }
    #[test]
    fn only_allocated_native_wav_names_get_a_stable_archive_id() {
        assert_eq!(
            native_recording_id(Path::new("/private/temp/hearfolio-recording-123.wav")),
            Some("hearing-recording-123".into())
        );
        for path in [
            "/private/temp/hearfolio-recording-123.wav.partial",
            "/private/input/hearfolio-recording-123.wav",
            "/private/temp/hearfolio-recording-foreign.wav",
        ] {
            assert_eq!(native_recording_id(Path::new(path)), None);
        }
    }
}
