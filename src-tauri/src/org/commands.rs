//! Tauri commands for organizations. Blocking network work runs on the
//! blocking pool; results are plain JSON for the WebView. Tokens never leave Rust.
use super::{
    api::{Api, ApiError},
    auth, cache, library,
    upload::{Job, Phase, Step},
    worker, Connection, Destination,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{fs, path::Path};

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("errors.operationFailed|{e}"))?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgState {
    connections: Vec<Connection>,
    settings: super::OrgSettings,
    uploads: Vec<worker::JobView>,
    cache: cache::Usage,
}

fn state() -> Result<OrgState, String> {
    let _lock = super::lock();
    let mut jobs = super::read_queue()?;
    // Finished uploads stay visible for an hour, then the synced recording replaces them.
    let now = super::now_ms();
    let before = jobs.len();
    jobs.retain(|job| {
        job.phase != Phase::Done || now.saturating_sub(job.finished_at.unwrap_or(now)) < 3_600_000
    });
    if jobs.len() != before {
        super::write_queue(&jobs)?;
    }
    Ok(OrgState {
        connections: super::read_connections()?,
        settings: super::read_settings()?,
        uploads: jobs.iter().map(worker::view).collect(),
        cache: cache::usage(&super::cache_dir()?),
    })
}

#[tauri::command]
pub async fn org_state() -> Result<OrgState, String> {
    blocking(state).await
}

#[tauri::command]
pub async fn org_connect_start(
    app: tauri::AppHandle,
    server: String,
    replaces: Option<String>,
) -> Result<Value, String> {
    blocking(move || auth::start(&app, &server, replaces)).await
}

#[tauri::command]
pub fn org_connect_cancel(state: String) {
    auth::cancel(&state);
}

#[tauri::command]
pub async fn org_disconnect(connection_id: String) -> Result<OrgState, String> {
    blocking(move || {
        super::valid_id(&connection_id)?;
        let connection = {
            let _lock = super::lock();
            let mut connections = super::read_connections()?;
            let index = connections
                .iter()
                .position(|c| c.id == connection_id)
                .ok_or("errors.orgMissing")?;
            let connection = connections.remove(index);
            super::write_connections(&connections)?;
            let mut settings = super::read_settings()?;
            if settings
                .destination
                .as_ref()
                .is_some_and(|d| d.connection_id == connection_id)
            {
                settings.destination = None;
                super::write_settings(&settings)?;
            }
            connection
        };
        // Revoking is best effort: offline or already revoked still removes the token here.
        if let Ok(token) = super::secrets::load(&connection.id) {
            let _ = super::api::revoke(&connection.revoke_url, &token);
        }
        super::secrets::delete(&connection.id);
        // Mirrors and cached audio of that account must not outlive it here.
        forget_organizations(&connection.id, &[]);
        // Unsent audio is kept: its upload shows "Disconnected" with "Keep on device".
        worker::fail_connection(&connection.id);
        state()
    })
    .await
}

fn connection(connection_id: &str) -> Result<(Connection, String), String> {
    super::valid_id(connection_id)?;
    let connection = super::read_connections()?
        .into_iter()
        .find(|c| c.id == connection_id)
        .ok_or("errors.orgMissing")?;
    let token = match super::secrets::load(&connection.id) {
        Ok(token) => token,
        Err(error) => {
            // The keychain lost it (restored backup, revoked keychain access): sign in again.
            if error == "errors.orgUnauthorized" {
                worker::mark_reauth(connection_id);
            }
            return Err(error);
        }
    };
    Ok((connection, token))
}
fn organization_key(connection: &Connection, organization_id: &str) -> Result<String, String> {
    connection
        .organizations
        .iter()
        .find(|org| org.id == organization_id)
        .map(|org| org.key.clone())
        .ok_or_else(|| "errors.orgMissing".into())
}
fn api_error(connection_id: &str, error: ApiError) -> String {
    if error == ApiError::Status(401) {
        worker::mark_reauth(connection_id);
    }
    error.key()
}

/// Refresh the organizations of a connection (roles, names, new memberships).
#[tauri::command]
pub async fn org_refresh_connection(connection_id: String) -> Result<OrgState, String> {
    blocking(move || {
        let (connection, token) = connection(&connection_id)?;
        let api = Api {
            base: &connection.server,
            token: &token,
            organization_key: None,
        };
        let list: Value = api
            .get("/me/organizations")
            .map_err(|e| api_error(&connection_id, e))?;
        let organizations = auth::organizations_from(&list);
        let keep: Vec<String> = organizations.iter().map(|org| org.id.clone()).collect();
        {
            let _lock = super::lock();
            let mut connections = super::read_connections()?;
            if let Some(stored) = connections.iter_mut().find(|c| c.id == connection_id) {
                stored.organizations = organizations;
                stored.needs_reauth = false;
            }
            super::write_connections(&connections)?;
        }
        forget_organizations(&connection_id, &keep);
        worker::retry_connection(&connection_id);
        state()
    })
    .await
}

#[tauri::command]
pub async fn org_set_destination(destination: Option<Destination>) -> Result<OrgState, String> {
    blocking(move || {
        {
            let _lock = super::lock();
            if let Some(destination) = &destination {
                super::validate_destination(&super::read_connections()?, destination)?;
            }
            let mut settings = super::read_settings()?;
            settings.destination = destination;
            super::write_settings(&settings)?;
        }
        state()
    })
    .await
}

#[tauri::command]
pub async fn org_set_retention(days: Option<u32>) -> Result<OrgState, String> {
    blocking(move || {
        if !cache::valid_retention(days) {
            return Err("errors.orgInvalid".into());
        }
        {
            let _lock = super::lock();
            let mut settings = super::read_settings()?;
            settings.retention_days = days;
            super::write_settings(&settings)?;
        }
        cache::evict(&super::cache_dir()?, days, std::time::SystemTime::now());
        state()
    })
    .await
}

#[tauri::command]
pub async fn org_clear_cache() -> Result<OrgState, String> {
    blocking(|| {
        cache::clear(&super::cache_dir()?);
        state()
    })
    .await
}

#[tauri::command]
pub async fn org_projects(
    connection_id: String,
    organization_id: String,
) -> Result<Vec<Value>, String> {
    blocking(move || {
        let (connection, token) = connection(&connection_id)?;
        let key = organization_key(&connection, &organization_id)?;
        let api = Api {
            base: &connection.server,
            token: &token,
            organization_key: Some(&key),
        };
        let page: Value = api
            .get("/projects?page_size=500&archived=false")
            .map_err(|e| api_error(&connection_id, e))?;
        Ok(page["data"].as_array().cloned().unwrap_or_default())
    })
    .await
}

pub(crate) fn store_recording(
    connection_id: &str,
    organization_id: &str,
    recording: Value,
) -> Result<bool, String> {
    let dir = library::dir(connection_id, organization_id)?;
    let _lock = library::lock();
    let mut index = library::read_index(&dir);
    let mut change = library::Change::default();
    library::upsert(&mut index, &recording, &mut change);
    if change.is_empty() {
        return Ok(false);
    }
    library::apply_change(&dir, &change)?;
    library::write_index(&dir, &index)?;
    Ok(true)
}

/// Remove everything this device keeps for organizations of a connection:
/// mirrors and the cached audio of their recordings. `keep` lists
/// organizations to leave alone (still present after a refresh).
pub(crate) fn forget_organizations(connection_id: &str, keep: &[String]) {
    let Ok(root) = super::root().map(|root| root.join("library").join(connection_id)) else {
        return;
    };
    let cache = super::cache_dir().ok();
    let _lock = library::lock();
    for entry in fs::read_dir(&root).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) || !entry.path().is_dir() {
            continue;
        }
        if let Some(cache) = &cache {
            for id in library::read_index(&entry.path()).items.keys() {
                if let Some(file) = cache::find(cache, id) {
                    let _ = fs::remove_file(file);
                }
            }
        }
        let _ = fs::remove_dir_all(entry.path());
    }
    if keep.is_empty() {
        let _ = fs::remove_dir_all(&root);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryView {
    items: Vec<Value>,
    synced_at: Option<u64>,
}

/// Stored recordings without the network: instant and offline. Rows carry no
/// transcripts; those are read by `org_recording`.
#[tauri::command]
pub async fn org_library(
    connection_id: String,
    organization_id: String,
) -> Result<LibraryView, String> {
    blocking(move || {
        let dir = library::dir(&connection_id, &organization_id)?;
        let index = {
            let _lock = library::lock();
            library::read_index(&dir)
        };
        Ok(LibraryView {
            items: library::summaries(&index),
            synced_at: index.synced_at,
        })
    })
    .await
}

/// Pull changes since the stored cursor (all pages) without holding any lock
/// over the network, then store them in one go and return the list.
#[tauri::command]
pub async fn org_sync(
    connection_id: String,
    organization_id: String,
) -> Result<LibraryView, String> {
    blocking(move || {
        let (connection, token) = connection(&connection_id)?;
        let key = organization_key(&connection, &organization_id)?;
        let api = Api {
            base: &connection.server,
            token: &token,
            organization_key: Some(&key),
        };
        let dir = library::dir(&connection_id, &organization_id)?;
        let mut cursor = {
            let _lock = library::lock();
            library::read_index(&dir).cursor
        };
        let mut pages = Vec::new();
        for _ in 0..50 {
            let query = match &cursor {
                Some(cursor) => format!("/recordings/sync?limit=200&cursor={}", urlencode(cursor)),
                None => "/recordings/sync?limit=200".to_owned(),
            };
            let page: library::SyncPage = match api.get(&query) {
                Ok(page) => page,
                // Keep what earlier pages brought; the cursor resumes from there.
                Err(e) if !pages.is_empty() => {
                    let _ = api_error(&connection_id, e);
                    break;
                }
                Err(e) => return Err(api_error(&connection_id, e)),
            };
            let more = page.has_more;
            if let Some(next) = page.next_cursor.clone().filter(|c| !c.is_empty()) {
                cursor = Some(next);
            }
            pages.push(page);
            if !more {
                break;
            }
        }
        let index = {
            let _lock = library::lock();
            let mut index = library::read_index(&dir);
            let mut change = library::Change::default();
            let now = super::now_ms();
            for page in pages {
                library::merge(&mut index, page, now, &mut change);
            }
            library::apply_change(&dir, &change)?;
            library::write_index(&dir, &index)?;
            if !change.is_empty() {
                super::emit(
                    "org-library",
                    json!({"connectionId": connection_id, "organizationId": organization_id}),
                );
            }
            index
        };
        Ok(LibraryView {
            items: library::summaries(&index),
            synced_at: index.synced_at,
        })
    })
    .await
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// A recording with its transcript and summaries. `refresh` asks the server
/// (detail + results); offline, the stored copy is returned.
#[tauri::command]
pub async fn org_recording(
    connection_id: String,
    organization_id: String,
    recording_id: String,
    refresh: bool,
) -> Result<Value, String> {
    blocking(move || {
        super::valid_id(&recording_id)?;
        let dir = library::dir(&connection_id, &organization_id)?;
        let mut error = None;
        if refresh {
            match connection(&connection_id) {
                Ok((connection, token)) => {
                    let key = organization_key(&connection, &organization_id)?;
                    let api = Api {
                        base: &connection.server,
                        token: &token,
                        organization_key: Some(&key),
                    };
                    match api.get::<Value>(&format!("/recordings/{recording_id}")) {
                        Ok(detail) => {
                            let results = api
                                .get::<Value>(&format!("/recordings/{recording_id}/results"))
                                .ok();
                            let _lock = library::lock();
                            let mut index = library::read_index(&dir);
                            let mut change = library::Change::default();
                            library::upsert(&mut index, &detail, &mut change);
                            if change.is_empty() {
                                // Same version as the list row: still store the full detail.
                                change.upserts.push(detail.clone());
                            }
                            library::apply_change(&dir, &change)?;
                            if let Some(Value::Array(results)) = results {
                                library::store_results(&dir, &mut index, &recording_id, results)?;
                            }
                            library::write_index(&dir, &index)?;
                        }
                        Err(ApiError::Status(404)) => {
                            let _lock = library::lock();
                            let mut index = library::read_index(&dir);
                            index.items.remove(&recording_id);
                            library::apply_change(
                                &dir,
                                &library::Change {
                                    upserts: vec![],
                                    removed: vec![recording_id.clone()],
                                },
                            )?;
                            library::write_index(&dir, &index)?;
                            return Ok(Value::Null);
                        }
                        Err(e) => error = Some(api_error(&connection_id, e)),
                    }
                }
                Err(e) => error = Some(e),
            }
        }
        let (row, detail) = {
            let _lock = library::lock();
            (
                library::read_index(&dir).items.get(&recording_id).cloned(),
                library::read_detail(&dir, &recording_id),
            )
        };
        let Some(row) = row else {
            return match error {
                Some(error) => Err(error),
                None => Ok(Value::Null),
            };
        };
        let detail = detail.unwrap_or_default();
        let recording = if detail.recording.is_object() {
            detail.recording
        } else {
            row
        };
        let cached = cache::find(&super::cache_dir()?, &recording_id).is_some();
        Ok(json!({
            "recording": recording,
            "results": detail.results,
            "audioCached": cached,
            "offline": error,
        }))
    })
    .await
}

/// Ask the server to run a failed transcription again.
#[tauri::command]
pub async fn org_retry_recording(
    connection_id: String,
    organization_id: String,
    recording_id: String,
) -> Result<Value, String> {
    blocking(move || {
        super::valid_id(&recording_id)?;
        let (connection, token) = connection(&connection_id)?;
        let key = organization_key(&connection, &organization_id)?;
        let api = Api {
            base: &connection.server,
            token: &token,
            organization_key: Some(&key),
        };
        let detail: Value = api
            .post(&format!("/recordings/{recording_id}/retry"), &json!({}))
            .map_err(|e| api_error(&connection_id, e))?;
        store_recording(&connection_id, &organization_id, detail.clone())?;
        Ok(detail)
    })
    .await
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| ["m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg"].contains(&e.as_str()))
        .unwrap_or_else(|| "m4a".into())
}

/// Queue a local recording for an organization. `move_local`: the recording
/// was made for the organization, so it leaves the local archive (its audio
/// becomes the pending upload). Otherwise it is a copy and, if it has a
/// transcript, the transcript goes along so the server skips transcription.
#[tauri::command]
pub async fn org_enqueue_record(
    id: String,
    destination: Destination,
    move_local: bool,
    title: Option<String>,
    source: Option<String>,
) -> Result<worker::JobView, String> {
    blocking(move || {
        super::validate_destination(&super::read_connections()?, &destination)?;
        let root = crate::base()?;
        let (entry, transcript) = {
            let _history = crate::history_lock()?;
            let entry = crate::records::read_history_at(&root)?
                .into_iter()
                .find(|entry| entry.id == id)
                .ok_or("errors.recordMissing")?;
            let transcript = if move_local {
                None
            } else {
                entry
                    .output_path
                    .as_ref()
                    .and_then(|path| fs::read_to_string(path).ok())
                    .filter(|text| !text.trim().is_empty())
            };
            (entry, transcript)
        };
        // Copying twice to the same organization returns the first copy.
        if !move_local {
            let _lock = super::lock();
            if let Some(existing) = super::read_queue()?.iter().find(|job| {
                job.local_record_id.as_deref() == Some(id.as_str())
                    && job.organization_id == destination.organization_id
                    && job.phase != Phase::Failed
            }) {
                return Ok(worker::view(existing));
            }
        }
        let source_path = Path::new(&entry.input_path);
        let size = fs::metadata(source_path)
            .map_err(|_| "errors.audioRead")?
            .len();
        if size == 0 {
            return Err("errors.audioRead".into());
        }
        let extension = extension_of(source_path);
        let client_id = uuid::Uuid::new_v4().to_string();
        let pending = super::pending_dir()?.join(format!("{client_id}.{extension}"));
        if fs::hard_link(source_path, &pending).is_err() {
            if let Err(error) = fs::copy(source_path, &pending) {
                let _ = fs::remove_file(&pending);
                return Err(format!("errors.orgStore|{error}"));
            }
        }
        let display = crate::record_name_title(&entry.name);
        let job = Job {
            client_id: client_id.clone(),
            connection_id: destination.connection_id.clone(),
            organization_id: destination.organization_id.clone(),
            title: title
                .filter(|t| !t.trim().is_empty())
                .map(|t| t.trim().chars().take(200).collect()),
            display_name: display.clone(),
            recorded_at: entry.created_at,
            duration_ms: entry
                .duration_seconds
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(|s| (s * 1000.0) as u64),
            source: match source.as_deref() {
                Some("import") => "import".into(),
                _ if cfg!(target_os = "android") => "android".into(),
                _ => "desktop".into(),
            },
            original_filename: format!("{display}.{extension}"),
            mime: super::mime_for(&extension).into(),
            size,
            file: pending.clone(),
            project_id: destination.project_id.clone(),
            visibility: destination.visibility.clone(),
            transcription_model: transcript.as_ref().and(entry.model.clone()),
            transcript_text: transcript,
            local_record_id: (!move_local).then(|| id.clone()),
            phase: Phase::Queued,
            remote_id: None,
            multipart: None,
            single_done: false,
            parts: Default::default(),
            attempts: 0,
            next_attempt_at: 0,
            error: None,
            waiting_network: false,
            created_at: super::now_ms(),
            finished_at: None,
            omit_parts: false,
        };
        {
            let _lock = super::lock();
            let mut jobs = super::read_queue()?;
            jobs.push(job.clone());
            if let Err(error) = super::write_queue(&jobs) {
                let _ = fs::remove_file(&pending);
                return Err(error);
            }
        }
        if move_local {
            // The queued copy now owns the audio; a failure here only leaves a duplicate.
            if let Ok(_history) = crate::history_lock() {
                if crate::records::delete_entries_at(&root, Some(&id)).is_ok() {
                    crate::resume::remove(&root, &id);
                }
            }
        }
        let view = worker::view(&job);
        super::emit("org-upload", view.clone());
        worker::kick();
        Ok(view)
    })
    .await
}

#[tauri::command]
pub async fn org_retry_upload(client_id: String) -> Result<Option<worker::JobView>, String> {
    blocking(move || {
        let job = worker::transition(&client_id, Step::Retry);
        worker::kick();
        Ok(job.as_ref().map(worker::view))
    })
    .await
}

/// Wake the queue now (app focus, network back): waiting jobs try immediately.
#[tauri::command]
pub fn org_wake() {
    // Network back or app focused: connections pooled before are likely dead.
    super::api::reset_client();
    {
        let _lock = super::lock();
        if let Ok(mut jobs) = super::read_queue() {
            let mut changed = false;
            // Back online or back in the app: everything waiting tries now.
            for job in jobs
                .iter_mut()
                .filter(|job| job.active() && job.next_attempt_at > 0)
            {
                job.next_attempt_at = 0;
                changed = true;
            }
            if changed {
                let _ = super::write_queue(&jobs);
            }
        }
    }
    worker::kick();
}

fn remove_job(client_id: &str) -> Result<Option<Job>, String> {
    let _lock = super::lock();
    let mut jobs = super::read_queue()?;
    let Some(index) = jobs.iter().position(|job| job.client_id == client_id) else {
        return Ok(None);
    };
    let job = jobs.remove(index);
    super::write_queue(&jobs)?;
    Ok(Some(job))
}

/// The server already holds an empty recording waiting for this audio: delete
/// it so the organization never sees «waiting for audio» forever.
fn discard_remote(job: &Job) {
    let Some(remote) = &job.remote_id else { return };
    let Ok((connection, token)) = connection(&job.connection_id) else {
        return;
    };
    let Ok(key) = organization_key(&connection, &job.organization_id) else {
        return;
    };
    let api = Api {
        base: &connection.server,
        token: &token,
        organization_key: Some(&key),
    };
    let _ = api.delete(&format!("/recordings/{remote}"));
    if let Ok(dir) = library::dir(&job.connection_id, &job.organization_id) {
        let _lock = library::lock();
        let mut index = library::read_index(&dir);
        if index.items.remove(remote).is_some() {
            let _ = library::apply_change(
                &dir,
                &library::Change {
                    upserts: vec![],
                    removed: vec![remote.clone()],
                },
            );
            let _ = library::write_index(&dir, &index);
        }
    }
}

/// Cancel and delete an upload together with its unsent audio.
#[tauri::command]
pub async fn org_cancel_upload(client_id: String) -> Result<(), String> {
    blocking(move || {
        worker::cancel(&client_id);
        let Some(job) = remove_job(&client_id)? else {
            return Ok(());
        };
        let _ = fs::remove_file(&job.file);
        discard_remote(&job);
        super::emit(
            "org-library",
            json!({"connectionId": job.connection_id, "organizationId": job.organization_id}),
        );
        Ok(())
    })
    .await
}

/// Give up on uploading and keep the audio in the local archive instead.
#[tauri::command]
pub async fn org_keep_local(
    app: tauri::AppHandle,
    client_id: String,
) -> Result<crate::records::HistoryEntry, String> {
    blocking(move || {
        let job = {
            let _lock = super::lock();
            super::read_queue()?
                .into_iter()
                .find(|job| job.client_id == client_id)
                .ok_or("errors.recordMissing")?
        };
        // Stop an in-flight request before anything else.
        worker::cancel(&client_id);
        if job.local_record_id.is_some() {
            // A copy: the original is still in the archive.
            remove_job(&client_id)?;
            let _ = fs::remove_file(&job.file);
            discard_remote(&job);
            return crate::records::read_history_at(&crate::base()?)?
                .into_iter()
                .find(|entry| Some(&entry.id) == job.local_record_id.as_ref())
                .ok_or_else(|| "errors.recordMissing".into());
        }
        let configuration = crate::cloud::load()?.last_configuration;
        let name = format!("{}.{}", job.display_name, extension_of(&job.file));
        let entry = crate::import_audio_with_id(
            &app,
            job.file.to_string_lossy().into_owned(),
            Some(name),
            configuration,
            None,
        )?;
        remove_job(&client_id)?;
        let _ = fs::remove_file(&job.file);
        discard_remote(&job);
        Ok(entry)
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSource {
    path: Option<String>,
    url: Option<String>,
}

fn remote_audio_url(
    connection_id: &str,
    organization_id: &str,
    recording_id: &str,
) -> Result<String, String> {
    let (connection, token) = connection(connection_id)?;
    let key = organization_key(&connection, organization_id)?;
    let api = Api {
        base: &connection.server,
        token: &token,
        organization_key: Some(&key),
    };
    let audio: Value = api
        .get(&format!("/recordings/{recording_id}/audio"))
        .map_err(|e| api_error(connection_id, e))?;
    audio["url"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "errors.orgResponse".into())
}

/// Cached file if present; otherwise a presigned URL for streaming.
#[tauri::command]
pub async fn org_audio(
    connection_id: String,
    organization_id: String,
    recording_id: String,
) -> Result<AudioSource, String> {
    blocking(move || {
        super::valid_id(&recording_id)?;
        if let Some(path) = cache::find(&super::cache_dir()?, &recording_id) {
            cache::touch(&path);
            return Ok(AudioSource {
                path: Some(path.to_string_lossy().into_owned()),
                url: None,
            });
        }
        Ok(AudioSource {
            path: None,
            url: Some(remote_audio_url(
                &connection_id,
                &organization_id,
                &recording_id,
            )?),
        })
    })
    .await
}

/// Download cloud audio into the cache (progress: `org-audio` events).
#[tauri::command]
pub async fn org_cache_audio(
    connection_id: String,
    organization_id: String,
    recording_id: String,
) -> Result<String, String> {
    blocking(move || {
        super::valid_id(&recording_id)?;
        let dir = super::cache_dir()?;
        if let Some(path) = cache::find(&dir, &recording_id) {
            cache::touch(&path);
            return Ok(path.to_string_lossy().into_owned());
        }
        let library_dir = library::dir(&connection_id, &organization_id)?;
        let mime = {
            let _lock = library::lock();
            library::read_index(&library_dir)
                .items
                .get(&recording_id)
                .and_then(|item| item["audio_mime"].as_str().map(str::to_owned))
        };
        let extension = match mime.as_deref() {
            Some("audio/mpeg") => "mp3",
            Some("audio/wav" | "audio/x-wav") => "wav",
            Some("audio/flac") => "flac",
            Some("audio/ogg") => "ogg",
            _ => "m4a",
        };
        let url = remote_audio_url(&connection_id, &organization_id, &recording_id)?;
        let name = cache::file_name(&recording_id, extension).ok_or("errors.orgInvalid")?;
        let target = dir.join(&name);
        let partial = dir.join(format!("{name}.part"));
        let mut response = super::api::client()
            .get(url)
            .timeout(std::time::Duration::from_secs(3600))
            .send()
            .map_err(|e| super::api::from_reqwest(&e).key())?;
        if !response.status().is_success() {
            return Err(ApiError::Status(response.status().as_u16()).key());
        }
        let total = response.content_length().unwrap_or(0);
        let mut file = fs::File::create(&partial).map_err(|e| format!("errors.orgStore|{e}"))?;
        let mut buffer = vec![0u8; 256 * 1024];
        let (mut done, mut last) = (0u64, std::time::Instant::now());
        loop {
            use std::io::{Read, Write};
            let read = response.read(&mut buffer).map_err(|_| {
                let _ = fs::remove_file(&partial);
                "errors.orgOffline".to_owned()
            })?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read]).map_err(|e| {
                let _ = fs::remove_file(&partial);
                format!("errors.orgStore|{e}")
            })?;
            done += read as u64;
            if last.elapsed().as_millis() > 250 {
                last = std::time::Instant::now();
                super::emit(
                    "org-audio",
                    json!({"recordingId": recording_id, "current": done, "total": total}),
                );
            }
        }
        file.sync_all()
            .map_err(|e| format!("errors.orgStore|{e}"))?;
        drop(file);
        fs::rename(&partial, &target).map_err(|e| format!("errors.orgStore|{e}"))?;
        super::emit(
            "org-audio",
            json!({"recordingId": recording_id, "current": done, "total": done, "done": true}),
        );
        Ok(target.to_string_lossy().into_owned())
    })
    .await
}

/// Android: play a cached cloud file with the system media player.
#[tauri::command]
pub async fn org_prepare_native_playback(
    app: tauri::AppHandle,
    path: String,
) -> Result<Value, String> {
    blocking(move || {
        let dir = super::cache_dir()?;
        let file = Path::new(&path);
        let inside = file.parent().is_some_and(|parent| parent == dir) && file.is_file();
        if !inside {
            return Err("errors.audioPlayback".into());
        }
        cache::touch(file);
        #[cfg(target_os = "android")]
        {
            use tauri::Manager;
            return app
                .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .run("preparePlayback", json!({"path": path}));
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = app;
            Err("errors.audioPlayback".into())
        }
    })
    .await
}
