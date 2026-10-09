use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_fs::FsExt;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePlatform {
    os: &'static str,
    transfer_mvp: bool,
    mobile: bool,
    local_recognition: bool,
    custom_storage: bool,
    native_audio: bool,
    audio_recording: bool,
}
#[tauri::command]
pub fn get_runtime_platform() -> RuntimePlatform {
    RuntimePlatform {
        os: std::env::consts::OS,
        transfer_mvp: cfg!(feature = "transfer-mvp"),
        mobile: cfg!(mobile),
        local_recognition: !cfg!(mobile),
        custom_storage: !cfg!(mobile),
        native_audio: true,
        audio_recording: !cfg!(target_os = "ios"),
    }
}

pub fn validate_configuration(
    configuration: &crate::cloud::RecognitionConfig,
) -> Result<(), String> {
    validate_configuration_for(configuration, !cfg!(mobile))
}
pub(crate) fn validate_configuration_for(
    configuration: &crate::cloud::RecognitionConfig,
    local_recognition: bool,
) -> Result<(), String> {
    if !local_recognition && configuration.provider == "local" {
        return Err("errors.localUnavailableMobile".into());
    }
    Ok(())
}

#[derive(Serialize)]
pub struct AudioSelection {
    pub path: String,
    pub name: Option<String>,
}

pub fn open_audio(app: &AppHandle, path: &str) -> Result<fs::File, String> {
    let file_path: tauri_plugin_fs::FilePath = path.parse().unwrap();
    #[cfg(not(target_os = "android"))]
    if matches!(&file_path, tauri_plugin_fs::FilePath::Url(url) if url.scheme() != "file") {
        return Err("errors.audioPathInvalid".into());
    }
    #[cfg(target_os = "android")]
    if matches!(&file_path, tauri_plugin_fs::FilePath::Url(url) if !["content", "file"].contains(&url.scheme()))
    {
        return Err("errors.audioPathInvalid".into());
    }
    let mut options = tauri_plugin_fs::OpenOptions::new();
    options.read(true);
    app.fs()
        .open(file_path, options)
        .map_err(|error| format!("errors.audioRead|{error}"))
}

fn source_name(path: &str) -> Option<String> {
    if path.starts_with("content://") {
        return None;
    }
    let file: tauri_plugin_fs::FilePath = path.parse().ok()?;
    file.into_path()
        .ok()?
        .file_name()?
        .to_str()
        .map(str::to_owned)
}

#[tauri::command]
pub async fn pick_audio_file(app: AppHandle) -> Result<Option<AudioSelection>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = app
            .dialog()
            .file()
            .add_filter("Audio", &["m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = file.to_string();
        #[cfg(not(target_os = "android"))]
        let name = source_name(&path);
        #[cfg(target_os = "android")]
        let mut name = source_name(&path);
        #[cfg(target_os = "android")]
        if name.is_none() {
            use std::os::fd::AsRawFd;
            if let Ok(file) = open_audio(&app, &path) {
                if let Ok(resolved) = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())) {
                    name = resolved
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| Path::new(name).extension().is_some())
                        .map(str::to_owned);
                }
            }
        }
        Ok(Some(AudioSelection { path, name }))
    })
    .await
    .map_err(|error| format!("errors.operationFailed|{error}"))?
}

#[tauri::command]
pub async fn export_text(app: AppHandle, name: String, content: String) -> Result<bool, String> {
    let name = Path::new(&name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Transcript.txt")
        .to_owned();
    let name = if name.to_lowercase().ends_with(".txt") {
        name
    } else {
        format!("{name}.txt")
    };
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = app
            .dialog()
            .file()
            .add_filter("Text", &["txt"])
            .set_file_name(name)
            .blocking_save_file()
        else {
            return Ok(false);
        };
        let mut options = tauri_plugin_fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        let mut file = app
            .fs()
            .open(path, options)
            .map_err(|error| format!("errors.exportWrite|{error}"))?;
        file.write_all(content.as_bytes())
            .and_then(|_| file.flush())
            .map_err(|error| format!("errors.exportWrite|{error}"))?;
        Ok(true)
    })
    .await
    .map_err(|error| format!("errors.operationFailed|{error}"))?
}

// Resolve by immutable history id; no media command accepts a frontend path.
fn tracked_audio_at(
    root: &Path,
    entries: &[crate::records::HistoryEntry],
    id: &str,
) -> Result<PathBuf, String> {
    if !id.starts_with("hearing-")
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("errors.recordIdInvalid".into());
    }
    let entry = entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or("errors.recordMissing")?;
    let input = root
        .join("input")
        .canonicalize()
        .map_err(|_| "errors.audioPathInvalid")?;
    let path = Path::new(&entry.input_path)
        .canonicalize()
        .map_err(|_| "errors.audioRead")?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .ok_or("errors.audioPathInvalid")?;
    if !["wav", "m4a", "mp4", "mp3", "aac", "flac", "ogg"].contains(&extension)
        || path != input.join(format!("{id}.{extension}"))
        || !path.is_file()
    {
        return Err("errors.audioPathInvalid".into());
    }
    Ok(path)
}
fn tracked_audio(id: &str) -> Result<(PathBuf, String), String> {
    let root = crate::base()?;
    let entries = crate::read_history()?;
    let path = tracked_audio_at(&root, &entries, id)?;
    let name = entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or("errors.recordMissing")?
        .name
        .clone();
    Ok((path, name))
}

#[tauri::command]
pub fn prepare_audio_playback(app: AppHandle, id: String) -> Result<String, String> {
    let (path, _) = tracked_audio(&id)?;
    app.asset_protocol_scope()
        .allow_file(&path)
        .map_err(|_| "errors.audioPlayback")?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(target_os = "android")]
fn android<T: serde::de::DeserializeOwned>(
    app: &AppHandle,
    command: &str,
    payload: serde_json::Value,
) -> Result<T, String> {
    app.state::<tauri_plugin_recording::Recording<tauri::Wry>>()
        .run(command, payload)
}

pub fn native_audio_duration(app: &AppHandle, path: &Path) -> Option<f64> {
    #[cfg(target_os = "android")]
    {
        #[derive(Deserialize)]
        struct Metadata {
            duration: Option<f64>,
        }
        let result: Metadata =
            android(app, "audioDuration", serde_json::json!({"path":path})).ok()?;
        result
            .duration
            .filter(|value| value.is_finite() && *value > 0.0)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, path);
        None
    }
}

#[tauri::command]
pub async fn save_audio(app: AppHandle, id: String) -> Result<(), String> {
    let job = crate::JobGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        let (source, name) = tracked_audio(&id)?;
        #[cfg(target_os = "android")]
        return android(
            &app,
            "saveAudio",
            serde_json::json!({"path":source,"name":name}),
        );
        #[cfg(not(target_os = "android"))]
        {
            let extension = source
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("wav");
            let name = Path::new(&name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Recording");
            let name = if name.to_lowercase().ends_with(&format!(".{extension}")) {
                name.to_owned()
            } else {
                format!("{name}.{extension}")
            };
            let Some(target) = app.dialog().file().set_file_name(name).blocking_save_file() else {
                return Ok(());
            };
            let mut options = tauri_plugin_fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            let mut target = app
                .fs()
                .open(target, options)
                .map_err(|_| "errors.exportWrite")?;
            let mut source = fs::File::open(source).map_err(|_| "errors.audioRead")?;
            std::io::copy(&mut source, &mut target)
                .and_then(|_| target.flush())
                .map_err(|_| "errors.exportWrite")?;
            Ok(())
        }
    })
    .await
    .map_err(|_| "errors.exportWrite")?
}
#[tauri::command]
pub async fn share_audio(app: AppHandle, id: String) -> Result<(), String> {
    #[cfg(target_os = "android")]
    return tauri::async_runtime::spawn_blocking(move || {
        let _job = crate::JobGuard::acquire()?;
        let (path, name) = tracked_audio(&id)?;
        android(
            &app,
            "shareAudio",
            serde_json::json!({"path":path,"name":name}),
        )
    })
    .await
    .map_err(|_| "errors.audioShare")?;
    #[cfg(not(target_os = "android"))]
    save_audio(app, id).await
}
#[tauri::command]
pub async fn share_text(app: AppHandle, name: String, text: String) -> Result<(), String> {
    #[cfg(target_os = "android")]
    return tauri::async_runtime::spawn_blocking(move || {
        android(
            &app,
            "shareText",
            serde_json::json!({"name":name,"text":text}),
        )
    })
    .await
    .map_err(|_| "errors.textShare")?;
    #[cfg(not(target_os = "android"))]
    export_text(app, name, text).await.map(|_| ())
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePlayerState {
    duration: f64,
    current_time: f64,
    playing: bool,
}
#[tauri::command]
pub async fn prepare_native_playback(
    app: AppHandle,
    id: String,
) -> Result<NativePlayerState, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _job = crate::JobGuard::acquire()?;
        let (path, _) = tracked_audio(&id)?;
        #[cfg(target_os = "android")]
        return android(&app, "preparePlayback", serde_json::json!({"path":path}));
        #[cfg(not(target_os = "android"))]
        {
            let _ = (app, path);
            Err("errors.audioPlayback".into())
        }
    })
    .await
    .map_err(|_| "errors.audioPlayback")?
}
#[tauri::command]
pub async fn native_playback_state(app: AppHandle) -> Result<NativePlayerState, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        return android(&app, "playbackState", serde_json::json!({}));
        #[cfg(not(target_os = "android"))]
        {
            let _ = app;
            Err("errors.audioPlayback".into())
        }
    })
    .await
    .map_err(|_| "errors.audioPlayback")?
}
async fn playback_action(
    app: AppHandle,
    command: &'static str,
    payload: serde_json::Value,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        return android(&app, command, payload);
        #[cfg(not(target_os = "android"))]
        {
            let _ = (app, command, payload);
            Ok(())
        }
    })
    .await
    .map_err(|_| "errors.audioPlayback")?
}
#[tauri::command]
pub async fn play_native_playback(app: AppHandle) -> Result<(), String> {
    playback_action(app, "playPlayback", serde_json::json!({})).await
}
#[tauri::command]
pub async fn pause_native_playback(app: AppHandle) -> Result<(), String> {
    playback_action(app, "pausePlayback", serde_json::json!({})).await
}
#[tauri::command]
pub async fn seek_native_playback(app: AppHandle, seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() {
        return Err("errors.audioPlayback".into());
    }
    playback_action(app, "seekPlayback", serde_json::json!({"seconds":seconds})).await
}
#[tauri::command]
pub async fn release_native_playback(app: AppHandle) -> Result<(), String> {
    playback_action(app, "releasePlayback", serde_json::json!({})).await
}

#[derive(Deserialize, Serialize)]
pub struct AndroidRelease {
    version: String,
    url: String,
    notes: Option<String>,
    sha256: Option<String>,
}
fn validate_release(release: AndroidRelease) -> Result<AndroidRelease, String> {
    let url = reqwest::Url::parse(&release.url).map_err(|_| "errors.androidReleaseInvalid")?;
    if release.version.trim().is_empty()
        || release.version.len() > 80
        || url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url
            .path()
            .starts_with("/bubaley/hearfolio/releases/download/")
        || !url.path().ends_with(".apk")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("errors.androidReleaseInvalid".into());
    }
    if release.sha256.as_ref().is_some_and(|hash| {
        hash.len() != 64 || !hash.bytes().all(|value| value.is_ascii_hexdigit())
    }) {
        return Err("errors.androidReleaseInvalid".into());
    }
    Ok(release)
}
#[tauri::command]
pub async fn get_latest_android_release() -> Result<Option<AndroidRelease>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(25))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|_| "errors.androidReleaseFetch")?;
        let response = client
            .get(
                "https://github.com/bubaley/hearfolio/releases/latest/download/latest-android.json",
            )
            .send()
            .map_err(|_| "errors.androidReleaseFetch")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err("errors.androidReleaseFetch".into());
        }
        let mut bytes = Vec::new();
        response
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "errors.androidReleaseFetch")?;
        if bytes.len() > 1024 * 1024 {
            return Err("errors.androidReleaseInvalid".into());
        }
        let release = serde_json::from_slice(&bytes).map_err(|_| "errors.androidReleaseInvalid")?;
        Ok(Some(validate_release(release)?))
    })
    .await
    .map_err(|_| "errors.androidReleaseFetch")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_resolver_rejects_foreign_missing_and_mismatched_history_paths() {
        let dir = std::env::temp_dir().join(format!("hearfolio-media-{}", crate::clock()));
        fs::create_dir_all(dir.join("input")).unwrap();
        let id = "hearing-synthetic";
        let path = dir.join("input").join(format!("{id}.wav"));
        fs::write(&path, b"synthetic").unwrap();
        let mut entry = crate::records::HistoryEntry {
            id: id.into(),
            transfer_source_id: None,
            name: "Fixture.wav".into(),
            input_path: path.to_string_lossy().into_owned(),
            output_path: None,
            model: None,
            configuration: None,
            created_at: 0,
            completed_at: None,
            size_bytes: None,
            duration_seconds: None,
            results: vec![],
        };
        assert_eq!(
            tracked_audio_at(&dir, &[entry.clone()], id).unwrap(),
            path.canonicalize().unwrap()
        );
        assert!(tracked_audio_at(&dir, &[entry.clone()], "../hearing-synthetic").is_err());
        assert!(tracked_audio_at(&dir, &[entry.clone()], "hearing-missing").is_err());
        let foreign = dir.join("secret.wav");
        fs::write(&foreign, b"foreign").unwrap();
        entry.input_path = foreign.to_string_lossy().into_owned();
        assert!(tracked_audio_at(&dir, &[entry.clone()], id).is_err());
        #[cfg(unix)]
        {
            let alias = dir.join("input/hearing-alias.wav");
            std::os::unix::fs::symlink(&foreign, &alias).unwrap();
            entry.id = "hearing-alias".into();
            entry.input_path = alias.to_string_lossy().into_owned();
            assert!(tracked_audio_at(&dir, &[entry], "hearing-alias").is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn android_release_rejects_foreign_downloads() {
        let valid = || AndroidRelease {
            version: "0.2.0".into(),
            url: "https://github.com/bubaley/hearfolio/releases/download/v0.2.0/Hearfolio.apk"
                .into(),
            notes: None,
            sha256: Some("a".repeat(64)),
        };
        assert!(validate_release(valid()).is_ok());
        let mut foreign = valid();
        foreign.url = "https://example.com/Hearfolio.apk".into();
        assert!(validate_release(foreign).is_err());
        let mut hash = valid();
        hash.sha256 = Some("broken".into());
        assert!(validate_release(hash).is_err());
    }
}
