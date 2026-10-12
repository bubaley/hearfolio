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
struct Session {
    path: PathBuf,
    writer: Option<Writer>,
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
fn native(app: &tauri::AppHandle, command: &str, path: Option<&Path>) -> Result<(), String> {
    app.state::<tauri_plugin_recording::Recording<tauri::Wry>>()
        .run(
            command,
            path.map(|path| serde_json::json!({"path":path}))
                .unwrap_or_else(|| serde_json::json!({})),
        )
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
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let extension = "wav";
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = crate::base()?
            .join("temp")
            .join(format!("hearfolio-recording-{unique}.{extension}"));
        let writer = if cfg!(target_os = "android") {
            None
        } else {
            Some(create_writer(
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
        if let Err(error) = native(&app, "start", Some(&path)) {
            let mut active = SESSION.lock().map_err(|_| "errors.recordingStart")?;
            if active.as_ref().is_some_and(|session| session.path == path) {
                active.take();
            }
            return Err(error);
        }
        #[cfg(target_os = "android")]
        {
            if let Ok(mut result) = LAST_RESULT.lock() {
                *result = None;
            }
            let _ = fs::remove_file(crate::base()?.join("recording-result.json"));
            monitor(app.clone(), path);
        }
        #[cfg(not(target_os = "android"))]
        let _ = app;
        Ok(())
    })
    .await
    .map_err(|_| "errors.recordingStart")?
}

#[tauri::command]
pub fn append_audio_recording(samples: Vec<i16>) -> Result<(), String> {
    if samples.len() > 65536 {
        return Err("errors.recordingStop".into());
    }
    let mut state = SESSION.lock().map_err(|_| "errors.recordingStop")?;
    let writer = state
        .as_mut()
        .and_then(|session| session.writer.as_mut())
        .ok_or("errors.recordingUnavailable")?;
    for sample in samples {
        writer
            .write_sample(sample)
            .map_err(|_| "errors.recordingStop")?;
    }
    Ok(())
}

#[tauri::command]
pub async fn stop_audio_recording(app: tauri::AppHandle) -> Result<AudioSelection, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = SESSION
            .lock()
            .map_err(|_| "errors.recordingStop")?
            .take()
            .ok_or("errors.recordingUnavailable")?;
        #[cfg(target_os = "android")]
        native(&app, "stop", None)?;
        #[cfg(not(target_os = "android"))]
        let _ = app;
        if let Some(writer) = session.writer.take() {
            if writer.duration() == 0 {
                return Err("errors.recordingEmpty".into());
            }
            writer.finalize().map_err(|_| "errors.recordingStop")?;
        }
        if fs::metadata(&session.path)
            .map_err(|_| "errors.recordingStop")?
            .len()
            <= 44
        {
            return Err("errors.recordingEmpty".into());
        }
        let path = session.path.clone();
        FINISHED
            .lock()
            .map_err(|_| "errors.recordingStop")?
            .insert(path.clone());
        Ok(AudioSelection {
            name: Some(format!("Recording-{}.{}", crate::clock(), "wav")),
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
        let result = native(&app, "cancel", None);
        #[cfg(not(target_os = "android"))]
        let result = {
            let _ = app;
            Ok(())
        };
        SESSION.lock().map_err(|_| "errors.recordingStop")?.take();
        result
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
            writer: Some(create_writer(&cancelled, 16000).unwrap()),
            path: cancelled.clone(),
            _job: job,
        };
        drop(session);
        assert!(!cancelled.exists());
        drop(JobGuard::acquire().unwrap());
        fs::remove_dir_all(directory).unwrap();
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

#[cfg(target_os = "android")]
fn monitor(app: tauri::AppHandle, path: PathBuf) {
    use tauri::Emitter;
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
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
                    let payload = serde_json::json!({"error":"errors.recordingInterrupted","selection":{"path":path,"name":"Recording.wav"}});
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
            let _ = app.emit(
            "recording-native-state",
            serde_json::json!({"state":state,"elapsedSeconds":status["elapsedSeconds"].as_u64().unwrap_or(0)+if state=="recording" {crate::clock().saturating_sub(status["updatedAt"].as_u64().unwrap_or(crate::clock()))/1000} else {0}}),
        );
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
                    let payload = archive_native_recording(&app, &path, status["error"].as_str());
                    drop(session);
                    store_recording_result(&payload);
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
                let payload = serde_json::json!({"error":status["error"].as_str().unwrap_or("errors.recordingEmpty"),"selection":if state=="error" && path.exists() {Some(serde_json::json!({"path":path,"name":"Recording.wav"}))} else {None}});
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
            let bridge=app.state::<tauri_plugin_recording::Recording<tauri::Wry>>();
            let mut status:serde_json::Value=bridge.transfer("recordingStatus",serde_json::json!({}))?;
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
                    archive_native_recording(&app,&path,status["error"].as_str())
                } else {
                    serde_json::json!({"error":status["error"].as_str().unwrap_or("errors.recordingInterrupted"),"selection":if path.exists(){Some(serde_json::json!({"path":path,"name":"Recording.wav"}))}else{None}})
                };
                let success=payload["entry"].is_object();
                store_recording_result(&payload);drop(session);
                if success { bridge.transfer::<()>("acknowledgeRecording",serde_json::json!({}))?;status["state"]=serde_json::json!("idle"); }
            } else if status["state"]=="finished" && status["quick"]!=true {
                // Recover a completed service recording after process recreation.
                let _job=JobGuard::acquire()?;
                if let Some(path)=terminal_path {
                    let base=crate::base()?.join("temp").canonicalize().map_err(|_|"errors.recordingStop")?;
                    let file=path.canonicalize().map_err(|_|"errors.recordingStop")?;
                    if file.parent()==Some(base.as_path()) && native_recording_id(&file).is_some() {
                        FINISHED.lock().map_err(|_|"errors.recordingStop")?.insert(file.clone());
                        let payload=archive_native_recording(&app,&file,status["error"].as_str());
                        let success=payload["entry"].is_object();store_recording_result(&payload);
                        if success { bridge.transfer::<()>("acknowledgeRecording",serde_json::json!({}))?;status["state"]=serde_json::json!("idle"); }
                    }
                }
            }
            let result=LAST_RESULT.lock().map_err(|_|"errors.recordingStop")?.clone().or_else(||fs::read(crate::base().ok()?.join("recording-result.json")).ok().and_then(|b|serde_json::from_slice(&b).ok()));
            Ok(serde_json::json!({"state":status["state"],"quick":status["quick"],"level":status["level"],"elapsedSeconds":status["elapsedSeconds"],"path":status["path"],"error":status["error"],"interrupted":status["interrupted"],"result":result}))
        }
        #[cfg(not(target_os="android"))]
        {let _=app; Ok(serde_json::json!({"state":"idle"}))}
    }).await.map_err(|e|e.to_string())?
}

// A stable private filename makes import idempotent if the process exits after
// committing history but before deleting the quick-recording source.
#[cfg(any(target_os = "android", test))]
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
            for path in pending["paths"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str())
            {
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
                    if crate::transfer::hash_file(source)?
                        != crate::transfer::hash_file(Path::new(&entry.input_path))?
                    {
                        return Err("Archived quick recording differs; source preserved".into());
                    }
                    entry
                } else {
                    let configuration = crate::cloud::load()?.last_configuration;
                    crate::import_audio_with_id(
                        &app,
                        path.into(),
                        Some(format!("Recording-{}.wav", crate::clock())),
                        configuration,
                        Some(id),
                    )?
                };
                fs::remove_file(source).map_err(|e| format!("errors.audioRead|{e}"))?;
                imported.push(entry);
            }
            Ok(imported)
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
pub async fn recording_quick_control(app: tauri::AppHandle, action: String) -> Result<(), String> {
    if !["stop", "cancel"].contains(&action.as_str()) {
        return Err("errors.recordingUnavailable".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        {
            app.state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .transfer::<()>(
                    "quickControl",
                    serde_json::json!({"id":"quick","stage":action}),
                )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (app, action);
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
            if crate::transfer::hash_file(path)?
                != crate::transfer::hash_file(Path::new(&entry.input_path))?
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
        Ok(entry) => serde_json::json!({"entry":entry,"warning":warning}),
        Err(error) => {
            serde_json::json!({"error":error,"warning":warning,"selection":{"path":path,"name":"Recording.wav"}})
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
        return app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .run(
                "recordingControl",
                serde_json::json!({"id":"recording","stage":_stage}),
            );
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
