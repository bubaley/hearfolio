//! Hearfolio Cloud organizations: connections (OAuth + PKCE), the recording
//! destination, a persistent upload queue, library sync and the audio cache.
//! Network and persistence live here; the WebView only renders.
//!
//! Files under `<config>/organizations/` (owner-only on Unix):
//! `connections.json`, `settings.json`, `queue.json`, `flows.json`,
//! `library/<connection>/<organization>.json`, `pending/` (audio waiting for
//! upload, never evicted) and `cache/` (uploaded or played audio, evicted).
pub mod api;
mod auth;
pub mod cache;
pub mod commands;
#[cfg(test)]
mod e2e_tests;
pub mod library;
pub mod pkce;
pub mod secrets;
pub mod server;
pub mod upload;
mod worker;

use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};
use tauri::Emitter;

static STATE_LOCK: Mutex<()> = Mutex::new(());
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

pub(crate) fn lock() -> MutexGuard<'static, ()> {
    STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub(crate) static TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn root() -> Result<PathBuf, String> {
    #[cfg(test)]
    let dir = TEST_ROOT
        .get_or_init(|| {
            std::env::temp_dir().join(format!("hearfolio-org-{}", uuid::Uuid::new_v4()))
        })
        .clone();
    #[cfg(not(test))]
    let dir = crate::storage::config_dir()?.join("organizations");
    fs::create_dir_all(&dir).map_err(|e| format!("errors.storageUnavailable|{e}"))?;
    Ok(dir)
}
fn subdir(name: &str) -> Result<PathBuf, String> {
    let dir = root()?.join(name);
    fs::create_dir_all(&dir).map_err(|e| format!("errors.storageUnavailable|{e}"))?;
    Ok(dir)
}
pub fn pending_dir() -> Result<PathBuf, String> {
    subdir("pending")
}
pub fn cache_dir() -> Result<PathBuf, String> {
    subdir("cache")
}

/// Identifiers that become file names or URL path segments (UUIDs and our own ids).
pub fn valid_id(id: &str) -> Result<(), String> {
    if !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Ok(())
    } else {
        Err("errors.orgInvalid".into())
    }
}

pub(crate) fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("errors.orgStore|{e}"))?;
    }
    let temp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|e| format!("errors.orgStore|{e}"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("errors.orgStore|{e}"))?;
    fs::rename(&temp, path).map_err(|e| format!("errors.orgStore|{e}"))
}
pub(crate) fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| format!("errors.orgStore|{e}"))?;
    write_private_bytes(path, &bytes)
}
fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Organization {
    pub id: String,
    pub name: String,
    pub key: String,
    pub role: String,
}
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserBrief {
    pub id: String,
    pub email: String,
    pub name: String,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    /// API base URL (the server's published public URL).
    pub server: String,
    pub server_name: String,
    #[serde(default)]
    pub server_version: String,
    pub revoke_url: String,
    pub user: UserBrief,
    pub organizations: Vec<Organization>,
    pub created_at: u64,
    /// Token held by the OS keychain/keystore (false: owner-only file).
    pub secure: bool,
    /// The server rejected the token: reconnect to continue.
    #[serde(default)]
    pub needs_reauth: bool,
}

fn default_visibility() -> String {
    "organization".into()
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Destination {
    pub connection_id: String,
    pub organization_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default = "default_visibility")]
    pub visibility: String,
}
fn default_retention() -> Option<u32> {
    Some(cache::DEFAULT_RETENTION_DAYS)
}
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgSettings {
    /// Where new recordings go; `None` keeps them on this device.
    #[serde(default)]
    pub destination: Option<Destination>,
    /// Days unused cloud audio stays on the device; `None` keeps it forever.
    #[serde(default = "default_retention")]
    pub retention_days: Option<u32>,
}
impl Default for OrgSettings {
    fn default() -> Self {
        Self {
            destination: None,
            retention_days: default_retention(),
        }
    }
}

pub fn read_connections() -> Result<Vec<Connection>, String> {
    Ok(read_json(&root()?.join("connections.json")))
}
pub fn write_connections(connections: &[Connection]) -> Result<(), String> {
    write_private_json(&root()?.join("connections.json"), &connections)
}
pub fn read_settings() -> Result<OrgSettings, String> {
    Ok(read_json(&root()?.join("settings.json")))
}
pub fn write_settings(settings: &OrgSettings) -> Result<(), String> {
    write_private_json(&root()?.join("settings.json"), settings)
}
pub fn read_queue() -> Result<Vec<upload::Job>, String> {
    Ok(read_json(&root()?.join("queue.json")))
}
pub fn write_queue(jobs: &[upload::Job]) -> Result<(), String> {
    write_private_json(&root()?.join("queue.json"), &jobs)
}
pub fn library_path(connection: &str, organization: &str) -> Result<PathBuf, String> {
    valid_id(connection)?;
    valid_id(organization)?;
    Ok(subdir("library")?
        .join(connection)
        .join(format!("{organization}.json")))
}

/// Destination checks shared by the picker and the queue.
pub fn validate_destination(
    connections: &[Connection],
    destination: &Destination,
) -> Result<(), String> {
    let connection = connections
        .iter()
        .find(|connection| connection.id == destination.connection_id)
        .ok_or("errors.orgMissing")?;
    if !connection
        .organizations
        .iter()
        .any(|organization| organization.id == destination.organization_id)
    {
        return Err("errors.orgMissing".into());
    }
    if !matches!(destination.visibility.as_str(), "organization" | "personal") {
        return Err("errors.orgInvalid".into());
    }
    if let Some(project) = &destination.project_id {
        valid_id(project)?;
    }
    Ok(())
}

pub(crate) fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(app) = APP.get() {
        let _ = app.emit(event, payload);
    }
}

pub fn now_ms() -> u64 {
    crate::clock()
}

/// `2026-11-13T09:05:01.250Z` from Unix milliseconds (proleptic Gregorian, UTC).
pub fn iso8601(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = (seconds / 86_400) as i64;
    let rem = seconds % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

pub fn mime_for(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "m4a" | "mp4" | "aac" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

/// Called from `setup`: remembers the app handle, allows cached audio for
/// playback, evicts stale audio and starts the upload worker.
pub fn install(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
    secrets::install(app);
    if let Ok(dir) = cache_dir() {
        use tauri::Manager;
        let _ = app.asset_protocol_scope().allow_directory(&dir, false);
        let days = read_settings()
            .map(|s| s.retention_days)
            .unwrap_or(default_retention());
        cache::evict(&dir, days, std::time::SystemTime::now());
    }
    auth::install(app);
    worker::start();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamps_and_types_for_the_api() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_794_560_701_250), "2026-11-13T09:05:01.250Z");
        assert_eq!(iso8601(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(mime_for("M4A"), "audio/mp4");
        assert_eq!(mime_for("flac"), "audio/flac");
        assert!(valid_id("3f2a-11").is_ok() && valid_id("../x").is_err() && valid_id("").is_err());
        let settings: OrgSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.retention_days, Some(30));
        let forever: OrgSettings = serde_json::from_str(r#"{"retentionDays":null}"#).unwrap();
        assert_eq!(forever.retention_days, None);
    }

    #[test]
    fn destinations_must_name_a_connected_organization() {
        let connection = Connection {
            id: "c".into(),
            server: "https://x".into(),
            server_name: "X".into(),
            server_version: String::new(),
            revoke_url: "https://x/api/v1/oauth/revoke".into(),
            user: UserBrief::default(),
            organizations: vec![Organization {
                id: "o".into(),
                name: "Org".into(),
                key: "k".into(),
                role: "member".into(),
            }],
            created_at: 0,
            secure: true,
            needs_reauth: false,
        };
        let mut destination = Destination {
            connection_id: "c".into(),
            organization_id: "o".into(),
            project_id: Some("p-1".into()),
            visibility: "personal".into(),
        };
        let connections = [connection];
        assert!(validate_destination(&connections, &destination).is_ok());
        destination.visibility = "public".into();
        assert!(validate_destination(&connections, &destination).is_err());
        destination.visibility = "organization".into();
        destination.organization_id = "other".into();
        assert_eq!(
            validate_destination(&connections, &destination),
            Err("errors.orgMissing".into())
        );
    }
}
