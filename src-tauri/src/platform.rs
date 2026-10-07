use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_fs::FsExt;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePlatform {
    os: &'static str,
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
    if cfg!(mobile) && configuration.provider == "local" {
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
