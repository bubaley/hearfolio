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
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
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
pub async fn start_audio_recording(
    app: tauri::AppHandle,
    sample_rate: Option<u32>,
) -> Result<(), String> {
    let job = JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let extension = if cfg!(target_os = "android") {
            "m4a"
        } else {
            "wav"
        };
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
            name: Some(format!(
                "Recording-{}.{}",
                crate::clock(),
                if cfg!(target_os = "android") {
                    "m4a"
                } else {
                    "wav"
                }
            )),
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
