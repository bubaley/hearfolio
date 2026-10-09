//! LAN relay protocol. Audio crosses HTTP as binary, with one acknowledged block in flight.
use crate::{records::HistoryEntry, storage};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
static CONFIG_LOCK: Mutex<()> = Mutex::new(());
const BLOCK: usize = 65536;
const MAX_SIZE: u64 = 1024 * 1024 * 1024;
#[derive(Serialize, Deserialize, Clone)]
struct Connection {
    server: String,
    id: String,
    token: String,
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: u8,
    source_id: String,
    name: String,
    extension: String,
    size: u64,
    sha256: String,
    transcript: String,
    created_at: u64,
    completed_at: Option<u64>,
    duration_seconds: Option<f64>,
    model: Option<String>,
    configuration: Option<crate::cloud::RecognitionConfig>,
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub fn cleanup_partial_files(dir: &Path) {
    if let Ok(files) = fs::read_dir(dir.join("temp")) {
        for entry in files.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = name
                .strip_prefix("transfer-")
                .and_then(|s| s.strip_suffix(".part"))
            {
                if uuid::Uuid::parse_str(id).is_ok()
                    && entry.file_type().is_ok_and(|kind| kind.is_file())
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
}
fn connection_path() -> Result<PathBuf, String> {
    Ok(storage::config_dir()?.join("transfer.json"))
}
fn load() -> Result<Connection, String> {
    serde_json::from_slice(&fs::read(connection_path()?).map_err(error)?).map_err(error)
}
fn client() -> Result<Client, String> {
    // Clones share the connection pool, including across polling and transfer jobs.
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .timeout(Duration::from_secs(35))
                .connect_timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(error)
        })
        .clone()
}
fn network_error(e: reqwest::Error) -> String {
    use std::error::Error;
    let mut message = e.to_string();
    let mut cause = e.source();
    while let Some(source) = cause {
        message.push_str(": ");
        message.push_str(&source.to_string());
        cause = source.source();
    }
    message
}
fn receive_block(c: &Connection, id: &str) -> Result<Option<(u64, Vec<u8>)>, String> {
    // Reading is repeatable until ack. Never retry PUT/ack blindly: they mutate offsets.
    for attempt in 0..3 {
        let result = (|| {
            let mut res = client()?
                .get(format!("{}/transfers/{id}/chunk", c.server))
                .bearer_auth(&c.token)
                .send()
                .map_err(network_error)?;
            if res.status() == reqwest::StatusCode::NO_CONTENT {
                return Ok(None);
            }
            if !res.status().is_success() {
                return Err(format!("Transfer interrupted (HTTP {})", res.status()));
            }
            let position = res
                .headers()
                .get("x-offset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or("Invalid block offset")?;
            let mut bytes = Vec::new();
            res.by_ref()
                .take((BLOCK + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("error reading transfer block: {e}"))?;
            if bytes.is_empty() || bytes.len() > BLOCK {
                return Err("Invalid block length".into());
            }
            Ok(Some((position, bytes.to_vec())))
        })();
        match result {
            Ok(block) => return Ok(block),
            Err(e)
                if attempt < 2
                    && (e.starts_with("error sending request")
                        || e.starts_with("error reading transfer block")) =>
            {
                std::thread::sleep(Duration::from_millis(250 * (attempt + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}
fn server_url(server: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(server.trim()).map_err(error)?;
    let local = match url.host_str().unwrap_or("").parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_private() || ip.is_loopback(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => url.host_str() == Some("localhost"),
    };
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("Use an HTTPS server or a local LAN HTTP address, without a path".into());
    }
    Ok(server.trim().trim_end_matches('/').into())
}
fn response(mut res: reqwest::blocking::Response) -> Result<Value, String> {
    let success = res.status().is_success();
    let mut bytes = vec![];
    res.by_ref()
        .take(5 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > 5 * 1024 * 1024 {
        return Err("Relay response too large".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(error)?;
    if !success {
        return Err(value["error"].as_str().unwrap_or("Relay error").into());
    }
    Ok(value)
}
fn rpc(c: &Connection, action: &str, payload: Value) -> Result<Value, String> {
    response(
        client()?
            .post(format!("{}/rpc", c.server))
            .bearer_auth(&c.token)
            .json(&json!({"action":action,"payload":payload}))
            .send()
            .map_err(network_error)?,
    )
}
fn join_relay(server: &str, name: &Value, code: &str) -> Result<Value, String> {
    let ticket = response(
        client()?
            .post(format!("{server}/join"))
            .json(&json!({"name":name,"code":code}))
            .send()
            .map_err(network_error)?,
    )?;
    let started = Instant::now();
    let mut failures = 0;
    while started.elapsed() < Duration::from_secs(180) {
        let result = client()?
            .post(format!("{server}/join/status"))
            .json(&ticket)
            .send()
            .map_err(network_error)
            .and_then(response);
        let status = match result {
            Ok(value) => {
                failures = 0;
                value
            }
            Err(e) => {
                failures += 1;
                if failures >= 3 {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        match status["status"].as_str() {
            Some("approved") => return Ok(status["device"].clone()),
            Some("rejected") => return Err("Device connection rejected".into()),
            Some("waiting") => std::thread::sleep(Duration::from_millis(1000)),
            _ => return Err("Invalid invitation response".into()),
        }
    }
    Err("Invitation confirmation timed out".into())
}
#[tauri::command]
pub async fn transfer_api(action: String, payload: Value) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if action == "validateServer" {
            return Ok(
                json!({"server":server_url(payload["server"].as_str().ok_or("Server required")?)?}),
            );
        }
        if action == "connect" {
            let _lock = CONFIG_LOCK.lock().map_err(error)?;
            let server = server_url(payload["server"].as_str().ok_or("Server required")?)?;
            let existing = load().ok().filter(|c| c.server == server);
            let c = if let Some(c) = existing {
                rpc(&c, "rename", json!({"name":payload["name"]}))?;
                if let Some(invite) = payload["invitationCode"].as_str().filter(|s| !s.is_empty()) {
                    rpc(&c, "pair", json!({"code":invite}))?;
                }
                c
            } else {
                let invite = payload["invitationCode"].as_str().unwrap_or("");
                let registered = if !invite.is_empty() {
                    join_relay(&server, &payload["name"], invite)?
                } else {
                    response(
                        client()?
                            .post(format!("{server}/register"))
                            .header(
                                "x-hearfolio-access-key",
                                payload["accessKey"].as_str().unwrap_or(""),
                            )
                            .json(&json!({"name":payload["name"]}))
                            .send()
                            .map_err(network_error)?,
                    )?
                };
                Connection {
                    server,
                    id: registered["id"].as_str().ok_or("Invalid device ID")?.into(),
                    token: registered["token"].as_str().ok_or("Invalid token")?.into(),
                }
            };
            let path = connection_path()?;
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts.open(path.with_extension("json.tmp")).map_err(error)?;
            file.write_all(&serde_json::to_vec(&c).map_err(error)?)
                .map_err(error)?;
            file.sync_all().map_err(error)?;
            drop(file);
            fs::rename(path.with_extension("json.tmp"), path).map_err(error)?;
            return rpc(&c, "poll", json!({}));
        }
        if action == "saved" {
            return Ok(load()
                .map(|c| json!({"server":c.server}))
                .unwrap_or(json!({})));
        }
        if !["poll", "code", "pair", "confirmPair", "cancel", "unpair"].contains(&action.as_str()) {
            return Err("Unsupported relay action".into());
        }
        rpc(&load()?, &action, payload)
    })
    .await
    .map_err(error)?
}
pub(crate) fn hash_file(path: &Path) -> Result<(String, u64), String> {
    let mut f = fs::File::open(path).map_err(error)?;
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buf = [0; BLOCK];
    loop {
        let n = f.read(&mut buf).map_err(error)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        size += n as u64;
    }
    Ok((format!("{:x}", hash.finalize()), size))
}
fn emit(app: &tauri::AppHandle, id: &str, status: &str, current: u64, total: u64) {
    let _ = app.emit(
        "transfer-progress",
        json!({"id":id,"status":status,"current":current,"total":total}),
    );
}
#[tauri::command]
pub async fn transfer_send(app: tauri::AppHandle, id: String, to: String) -> Result<(), String> {
    let job = crate::JobGuard::acquire()?;
    let background = crate::background::TransferGuard::start(&app, "send")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        let mut background = background;
        let c = load()?;
        let entry = crate::read_history()?
            .into_iter()
            .find(|e| e.id == id)
            .ok_or("Recording missing")?;
        let path = Path::new(&entry.input_path);
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or("Audio extension missing")?
            .to_owned();
        let (sha256, size) = hash_file(path)?;
        if size == 0 || size > MAX_SIZE {
            return Err("Recording must be between 1 byte and 1 GiB".into());
        }
        let transcript = if let Some(path) = entry.output_path.as_ref() {
            let metadata = fs::metadata(path).map_err(error)?;
            if metadata.len() > 4 * 1024 * 1024 {
                return Err("Transcript exceeds 4 MiB".into());
            }
            fs::read_to_string(path).map_err(error)?
        } else {
            String::new()
        };
        let manifest = Manifest {
            version: 1,
            source_id: entry.transfer_source_id.unwrap_or(entry.id),
            name: entry.name,
            extension,
            size,
            sha256,
            transcript,
            created_at: entry.created_at,
            completed_at: entry.completed_at,
            duration_seconds: entry.duration_seconds,
            model: entry.model,
            configuration: entry.configuration,
        };
        let offered = rpc(
            &c,
            "offer",
            json!({"to":to,"manifest":manifest,"metadataOnly":true}),
        )?;
        let session = offered["id"]
            .as_str()
            .ok_or("Transfer ID missing")?
            .to_owned();
        let result = (|| {
            emit(&app, &session, "waiting", 0, size);
            background.progress("waiting", 0, size);
            let started = Instant::now();
            let audio_required = loop {
                background.check()?;
                background.progress("waiting", 0, 0);
                let status = rpc(&c, "status", json!({"id":session}))?;
                match status["status"].as_str() {
                    Some("streaming") => break status["audioRequired"].as_bool().unwrap_or(true),
                    Some("delivered") => break false,
                    Some("cancelled") => return Err("Transfer cancelled".into()),
                    _ => {}
                }
                if started.elapsed() > Duration::from_secs(180) {
                    return Err("Recipient did not accept".into());
                }
                std::thread::sleep(Duration::from_millis(500));
            };
            if audio_required {
                let mut file = fs::File::open(path).map_err(error)?;
                let mut offset = 0;
                let mut buf = [0; BLOCK];
                loop {
                    background.check()?;
                    let n = file.read(&mut buf).map_err(error)?;
                    if n == 0 {
                        break;
                    }
                    response(
                        client()?
                            .put(format!("{}/transfers/{session}/chunk", c.server))
                            .bearer_auth(&c.token)
                            .header("x-offset", offset.to_string())
                            .body(buf[..n].to_vec())
                            .send()
                            .map_err(network_error)?,
                    )?;
                    offset += n as u64;
                    emit(&app, &session, "sending", offset, size);
                    background.progress("sending", offset, size);
                }
            }
            let finish = Instant::now();
            loop {
                background.check()?;
                let s = rpc(&c, "status", json!({"id":session}))?;
                if s["status"] == "delivered" {
                    break;
                }
                if s["status"] == "cancelled" || finish.elapsed() > Duration::from_secs(60) {
                    return Err("Recipient did not confirm saving".into());
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            emit(&app, &session, "delivered", size, size);
            Ok(())
        })();
        if result.is_err() {
            let _ = rpc(&c, "cancel", json!({"id":session}));
        }
        if result.is_ok() {
            background.complete();
        }
        result
    })
    .await
    .map_err(error)?
}
fn validate_manifest(m: &Manifest) -> Result<(), String> {
    if m.version != 1
        || m.size == 0
        || m.size > MAX_SIZE
        || m.name.trim().is_empty()
        || m.name.chars().count() > 200
        || m.name.chars().any(char::is_control)
        || m.source_id.trim().is_empty()
        || m.source_id.chars().any(char::is_control)
        || m.source_id.len() > 200
        || m.transcript.len() > 4 * 1024 * 1024
        || m.sha256.len() != 64
        || !m.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !["m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg"].contains(&m.extension.as_str())
    {
        return Err("Invalid recording manifest".into());
    }
    if let Some(d) = m.duration_seconds {
        if !d.is_finite() || d < 0.0 {
            return Err("Invalid duration".into());
        }
    }
    if let Some(config) = m.configuration.as_ref() {
        config.validate_metadata()?;
    }
    Ok(())
}
fn recording_matches(entry: &HistoryEntry, m: &Manifest, local_id: &str) -> bool {
    entry.id == m.source_id
        || entry.transfer_source_id.as_deref() == Some(m.source_id.as_str())
        || entry.id == local_id
}
fn received_id(m: &Manifest) -> Result<String, String> {
    let digest = Sha256::digest(
        serde_json::to_vec(&json!({"sourceId":m.source_id,"sha256":m.sha256})).map_err(error)?,
    );
    Ok(format!("hearing-{digest:x}"))
}
fn has_verified_audio(dir: &Path, m: &Manifest) -> Result<bool, String> {
    let local_id = received_id(m)?;
    Ok(crate::records::read_history_at(dir)?
        .iter()
        .filter(|entry| recording_matches(entry, m, &local_id))
        .any(|entry| {
            managed_audio_target(dir, entry).is_ok()
                && hash_file(Path::new(&entry.input_path))
                    .is_ok_and(|(hash, size)| hash == m.sha256 && size == m.size)
        }))
}
fn managed_audio_target(dir: &Path, entry: &HistoryEntry) -> Result<PathBuf, String> {
    let path = Path::new(&entry.input_path);
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let expected = dir
        .join("input")
        .join(format!("{}.{}", entry.id, extension));
    if !entry.id.starts_with("hearing-")
        || !entry
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || path != expected
        || !["m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg"].contains(&extension)
    {
        return Err("Invalid archived audio path".into());
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(expected),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(expected),
        _ => Err("Invalid archived audio path".into()),
    }
}
fn update_received_entry(
    dir: &Path,
    entry: &HistoryEntry,
    m: &Manifest,
) -> Result<HistoryEntry, String> {
    let mut updated = entry.clone();
    updated.transfer_source_id = Some(m.source_id.clone());
    updated.size_bytes = Some(m.size);
    if !m.transcript.is_empty() && m.completed_at.unwrap_or(0) >= entry.completed_at.unwrap_or(0) {
        updated.model = m.model.clone();
        updated.configuration = m.configuration.clone();
        updated.completed_at = m.completed_at;
        crate::records::replace_transcript_at(dir, updated.clone(), &m.transcript)?;
        updated.output_path = Some(
            dir.join("output")
                .join(format!("{}.txt", updated.id))
                .to_string_lossy()
                .into(),
        );
    } else {
        let mut entries = crate::records::read_history_at(dir)?;
        *entries
            .iter_mut()
            .find(|e| e.id == entry.id)
            .ok_or("Recording missing")? = updated.clone();
        crate::records::write_history_at(dir, &entries)?;
    }
    Ok(updated)
}
fn repair_received_audio(
    dir: &Path,
    staging: &Path,
    entry: &HistoryEntry,
    m: &Manifest,
) -> Result<HistoryEntry, String> {
    let target = managed_audio_target(dir, entry)?;
    // A damaged/missing local copy is replaceable only by verified incoming bytes.
    if hash_file(staging)? != (m.sha256.clone(), m.size) {
        return Err("Audio checksum mismatch".into());
    }
    let repair = dir
        .join("temp")
        .join(format!("repair-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&repair).map_err(error)?;
    let result = (|| {
        let candidate = repair.join("new");
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .map_err(error)?;
        std::io::copy(&mut fs::File::open(staging).map_err(error)?, &mut output).map_err(error)?;
        output.sync_all().map_err(error)?;
        drop(output);
        let backup = repair.join("previous");
        let existed = target.exists();
        if existed {
            fs::rename(&target, &backup).map_err(error)?;
        }
        let commit = fs::rename(&candidate, &target)
            .map_err(error)
            .and_then(|_| update_received_entry(dir, entry, m));
        if commit.is_err() {
            if existed {
                fs::rename(&backup, &target)
                    .map_err(|e| format!("Audio recovery required at {}: {e}", backup.display()))?;
            } else if target.exists() {
                fs::remove_file(&target).map_err(error)?;
            }
        }
        commit
    })();
    // Preserve any backup if restoring it failed.
    if !repair.join("previous").exists() || result.is_ok() {
        let _ = fs::remove_dir_all(repair);
    }
    result
}
fn commit_received(
    dir: &Path,
    staging: &Path,
    local_id: String,
    m: Manifest,
) -> Result<HistoryEntry, String> {
    validate_manifest(&m)?;
    let mut entries = crate::records::read_history_at(dir)?;
    if let Some(entry) = entries.iter().find(|e| recording_matches(e, &m, &local_id)) {
        managed_audio_target(dir, entry)?;
        if !hash_file(Path::new(&entry.input_path))
            .is_ok_and(|(hash, size)| hash == m.sha256 && size == m.size)
        {
            return repair_received_audio(dir, staging, entry, &m);
        }
        return update_received_entry(dir, entry, &m);
    }
    let target = dir
        .join("input")
        .join(format!("{local_id}.{}", m.extension));
    let transcript = dir.join("output").join(format!("{local_id}.txt"));
    // create_new prevents any existing archive file from being overwritten.
    let mut audio = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(error)?;
    let mut transcript_created = false;
    let commit = (|| {
        let mut input = fs::File::open(staging).map_err(error)?;
        std::io::copy(&mut input, &mut audio).map_err(error)?;
        audio.sync_all().map_err(error)?;
        if !m.transcript.is_empty() {
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&transcript)
                .map_err(error)?;
            transcript_created = true;
            out.write_all(m.transcript.as_bytes()).map_err(error)?;
            out.sync_all().map_err(error)?;
        }
        let entry = HistoryEntry {
            id: local_id,
            transfer_source_id: Some(m.source_id),
            name: m.name,
            input_path: target.to_string_lossy().into(),
            output_path: if m.transcript.is_empty() {
                None
            } else {
                Some(transcript.to_string_lossy().into())
            },
            model: m.model,
            configuration: m.configuration,
            created_at: m.created_at,
            completed_at: m.completed_at,
            size_bytes: Some(m.size),
            duration_seconds: m.duration_seconds,
            results: vec![],
        };
        entries.push(entry.clone());
        crate::records::write_history_at(dir, &entries)?;
        Ok(entry)
    })();
    drop(audio);
    if commit.is_err() {
        let _ = fs::remove_file(&target);
        if transcript_created {
            let _ = fs::remove_file(&transcript);
        }
    }
    commit
}

#[tauri::command]
pub async fn transfer_receive(app: tauri::AppHandle, id: String) -> Result<HistoryEntry, String> {
    let job = crate::JobGuard::acquire()?;
    let background = crate::background::TransferGuard::start(&app, "receive")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _job = job;
        let mut background=background;
        let c = load()?;
        let dir = crate::base()?;
        // Older relays/senders default to a full transfer.
        let mut accept_payload=json!({"id":id});
        if let Ok(inspected)=rpc(&c,"inspect",json!({"id":id})) {
            let manifest:Manifest=serde_json::from_value(inspected["manifest"].clone()).map_err(error)?;
            validate_manifest(&manifest)?;
            let _lock=crate::history_lock()?;
            if inspected["metadataOnlyCapable"]==true && has_verified_audio(&dir,&manifest)? {
                accept_payload=json!({"id":id,"audioRequired":false,"sha256":manifest.sha256,"size":manifest.size});
            }
        }
        let accepted=rpc(&c,"accept",accept_payload)?;
        let staging = dir
            .join("temp")
            .join(format!("transfer-{}.part", uuid::Uuid::new_v4()));
        let result = (|| {
            let m: Manifest =
                serde_json::from_value(accepted["manifest"].clone()).map_err(error)?;
            validate_manifest(&m)?;
            emit(&app, &id, "receiving", 0, m.size);
            let local_id=received_id(&m)?;
            if accepted["audioRequired"]==false {
                background.check()?;
                background.progress("metadata",0,0);
                let _lock=crate::history_lock()?;
                if !has_verified_audio(&dir,&m)? {return Err("Existing audio changed; retry to download it again".into());}
                let entry=commit_received(&dir,&staging,local_id,m.clone())?;
                app.asset_protocol_scope().allow_file(&entry.input_path).map_err(error)?;
                rpc(&c,"finish",json!({"id":id}))?;
                emit(&app,&id,"delivered",m.size,m.size);
                return Ok(entry);
            }
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staging)
                .map_err(error)?;
            let mut offset = 0;
            let mut hash = Sha256::new();
            let mut last = Instant::now();
            while offset < m.size {
                background.check()?;
                let Some((position, buf)) = receive_block(&c, &id)? else {
                    if last.elapsed() > Duration::from_secs(45) {
                        return Err("Sender timed out".into());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                };
                if position != offset {
                    return Err("Unexpected block offset".into());
                }
                if offset + buf.len() as u64 > m.size {
                    return Err("Invalid block length".into());
                }
                output.write_all(&buf).map_err(error)?;
                hash.update(&buf);
                offset += buf.len() as u64;
                rpc(&c, "ack", json!({"id":id,"offset":offset}))?;
                last = Instant::now();
                emit(&app, &id, "receiving", offset, m.size);
                background.progress("receiving",offset,m.size);
            }
            output.sync_all().map_err(error)?;
            drop(output);
            if format!("{:x}", hash.finalize()) != m.sha256 {
                return Err("Audio checksum mismatch".into());
            }
            let _lock = crate::history_lock()?;
            let entry = commit_received(&dir, &staging, local_id, m.clone())?;
            app.asset_protocol_scope()
                .allow_file(&entry.input_path)
                .map_err(error)?;
            rpc(&c, "finish", json!({"id":id}))?;
            emit(&app, &id, "delivered", m.size, m.size);
            Ok(entry)
        })();
        let _ = fs::remove_file(staging);
        if result.is_err() {
            let _ = rpc(&c, "cancel", json!({"id":id}));
        }
        if result.is_ok() {background.complete();}
        result
    })
    .await
    .map_err(error)?
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, PathBuf, Manifest) {
        let dir =
            std::env::temp_dir().join(format!("hearfolio-transfer-test-{}", uuid::Uuid::new_v4()));
        storage::ensure_dirs(&dir).unwrap();
        let staging = dir.join("temp/test.part");
        let bytes = include_bytes!("../tests/fixtures/sine.m4a");
        fs::write(&staging, bytes).unwrap();
        let m = Manifest {
            version: 1,
            source_id: "hearing-test".into(),
            name: "Interview".into(),
            extension: "m4a".into(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            transcript: "Сохранённый текст".into(),
            created_at: 123,
            completed_at: Some(456),
            duration_seconds: Some(1.0),
            model: Some("whisper-base".into()),
            configuration: Some(crate::cloud::RecognitionConfig::default()),
        };
        (dir, staging, m)
    }
    #[test]
    fn importing_preserves_metadata_and_deduplicates_without_overwriting() {
        let (dir, staging, m) = fixture();
        let id = "hearing-transfer-test".to_owned();
        let entry = commit_received(&dir, &staging, id.clone(), m.clone()).unwrap();
        assert_eq!(
            fs::read(&entry.input_path).unwrap(),
            fs::read(&staging).unwrap()
        );
        assert_eq!(
            fs::read_to_string(entry.output_path.as_ref().unwrap()).unwrap(),
            m.transcript
        );
        assert_eq!(entry.created_at, 123);
        assert_eq!(entry.completed_at, Some(456));
        assert_eq!(entry.model, m.model);
        crate::records::rename_entry_at(&dir, &id, "Local rename").unwrap();
        let duplicate = commit_received(&dir, &staging, id, m).unwrap();
        assert_eq!(duplicate.name, "Local rename");
        assert_eq!(crate::records::read_history_at(&dir).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn round_trip_updates_original_and_received_copy_without_duplicates() {
        let (original, audio, mut m) = fixture();
        let (other, _, _) = fixture();
        m.transcript.clear();
        m.completed_at = None;
        let first = commit_received(&original, &audio, m.source_id.clone(), m.clone()).unwrap();
        let mut imported = first.clone();
        imported.transfer_source_id = None; // An original recording predates any transfer.
        crate::records::write_history_at(&original, &[imported]).unwrap();
        let copy = commit_received(&other, &audio, "hearing-copy".into(), m.clone()).unwrap();
        assert_eq!(copy.transfer_source_id.as_deref(), Some(first.id.as_str()));
        m.source_id = copy.transfer_source_id.unwrap();
        m.transcript = "Новая расшифровка на втором устройстве".into();
        m.completed_at = Some(900);
        let returned =
            commit_received(&original, &audio, "hearing-unused".into(), m.clone()).unwrap();
        assert_eq!(returned.id, first.id);
        assert_eq!(returned.input_path, first.input_path);
        assert_eq!(
            fs::read_to_string(returned.output_path.unwrap()).unwrap(),
            m.transcript
        );
        assert_eq!(crate::records::read_history_at(&original).unwrap().len(), 1);
        let refreshed =
            commit_received(&other, &audio, "hearing-unused".into(), m.clone()).unwrap();
        assert_eq!(refreshed.id, "hearing-copy");
        assert_eq!(crate::records::read_history_at(&other).unwrap().len(), 1);
        fs::remove_dir_all(original).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn received_results_preserve_newer_text_and_reject_different_audio() {
        let (dir, audio, mut m) = fixture();
        let entry = commit_received(&dir, &audio, "hearing-copy".into(), m.clone()).unwrap();
        m.completed_at = Some(1);
        m.transcript = "Old".into();
        commit_received(&dir, &audio, "hearing-unused".into(), m.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(entry.output_path.as_ref().unwrap()).unwrap(),
            "Сохранённый текст"
        );
        m.completed_at = Some(999);
        m.transcript.clear();
        commit_received(&dir, &audio, "hearing-unused".into(), m.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(entry.output_path.as_ref().unwrap()).unwrap(),
            "Сохранённый текст"
        );
        m.sha256 = "a".repeat(64);
        assert!(commit_received(&dir, &audio, "hearing-unused".into(), m).is_err());
        assert_eq!(crate::records::read_history_at(&dir).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hash_negotiation_reuses_intact_audio_and_repairs_missing_or_corrupt_copies() {
        let (dir, staging, mut m) = fixture();
        let entry = commit_received(&dir, &staging, "hearing-copy".into(), m.clone()).unwrap();
        assert!(has_verified_audio(&dir, &m).unwrap());
        m.transcript = "Only updated text".into();
        m.completed_at = Some(999);
        // Metadata-only commit needs no downloaded staging file.
        let updated = commit_received(
            &dir,
            &dir.join("absent.part"),
            "hearing-unused".into(),
            m.clone(),
        )
        .unwrap();
        assert_eq!(updated.id, entry.id);
        assert_eq!(
            fs::read_to_string(updated.output_path.as_ref().unwrap()).unwrap(),
            m.transcript
        );
        for missing in [false, true] {
            if missing {
                fs::remove_file(&entry.input_path).unwrap();
            } else {
                fs::write(&entry.input_path, b"corrupt audio").unwrap();
            }
            assert!(!has_verified_audio(&dir, &m).unwrap());
            let repaired =
                commit_received(&dir, &staging, "hearing-unused".into(), m.clone()).unwrap();
            assert_eq!(repaired.id, entry.id);
            assert_eq!(
                fs::read(&entry.input_path).unwrap(),
                fs::read(&staging).unwrap()
            );
            assert!(has_verified_audio(&dir, &m).unwrap());
            assert_eq!(crate::records::read_history_at(&dir).unwrap().len(), 1);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_repair_restores_damaged_audio_and_prior_transcript() {
        let (dir, staging, mut m) = fixture();
        let entry = commit_received(&dir, &staging, "hearing-copy".into(), m.clone()).unwrap();
        fs::write(&entry.input_path, b"old damaged audio").unwrap();
        fs::create_dir(dir.join("history.json.tmp")).unwrap();
        m.transcript = "Updated".into();
        m.completed_at = Some(999);
        assert!(commit_received(&dir, &staging, "hearing-unused".into(), m).is_err());
        assert_eq!(fs::read(&entry.input_path).unwrap(), b"old damaged audio");
        assert_eq!(
            fs::read_to_string(entry.output_path.unwrap()).unwrap(),
            "Сохранённый текст"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn received_update_rolls_back_text_and_metadata_if_history_commit_fails() {
        let (dir, audio, mut m) = fixture();
        let entry = commit_received(&dir, &audio, "hearing-copy".into(), m.clone()).unwrap();
        fs::create_dir(dir.join("history.json.tmp")).unwrap();
        m.transcript = "Updated".into();
        m.completed_at = Some(999);
        assert!(commit_received(&dir, &audio, "hearing-unused".into(), m).is_err());
        assert_eq!(
            fs::read_to_string(entry.output_path.as_ref().unwrap()).unwrap(),
            "Сохранённый текст"
        );
        assert_eq!(
            crate::records::read_history_at(&dir).unwrap()[0].completed_at,
            Some(456)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mobile_receives_local_transcription_without_running_its_model() {
        let (dir, staging, mut m) = fixture();
        m.configuration = Some(crate::cloud::RecognitionConfig {
            provider: "local".into(),
            model: "whisper-base".into(),
            mode: "local".into(),
        });
        assert_eq!(
            crate::platform::validate_configuration_for(m.configuration.as_ref().unwrap(), false)
                .unwrap_err(),
            "errors.localUnavailableMobile"
        );
        validate_manifest(&m).unwrap();
        let entry =
            commit_received(&dir, &staging, "hearing-mobile-transfer".into(), m.clone()).unwrap();
        assert_eq!(
            fs::read(entry.input_path).unwrap(),
            fs::read(staging).unwrap()
        );
        assert_eq!(
            fs::read_to_string(entry.output_path.unwrap()).unwrap(),
            m.transcript
        );
        assert_eq!(entry.configuration, m.configuration);
        let mut invalid = m;
        invalid.configuration.as_mut().unwrap().mode = "arbitrary".into();
        assert!(validate_manifest(&invalid).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_import_preserves_existing_files_and_rolls_back_new_files() {
        let (dir, staging, m) = fixture();
        let id = "hearing-transfer-test".to_owned();
        let foreign = dir.join("output").join(format!("{id}.txt"));
        fs::write(&foreign, "keep").unwrap();
        assert!(commit_received(&dir, &staging, id.clone(), m.clone()).is_err());
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "keep");
        assert_eq!(fs::read_dir(dir.join("input")).unwrap().count(), 0);
        fs::remove_file(foreign).unwrap();
        fs::create_dir(dir.join("history.json.tmp")).unwrap();
        assert!(commit_received(&dir, &staging, id, m).is_err());
        assert_eq!(fs::read_dir(dir.join("input")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(dir.join("output")).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn requests_reuse_one_connection() {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let serving = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for _ in 0..2 {
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                    .unwrap();
            }
        });
        for _ in 0..2 {
            assert_eq!(
                client().unwrap().get(&url).send().unwrap().text().unwrap(),
                "{}"
            );
        }
        serving.join().unwrap();
    }
    #[test]
    fn interrupted_download_retries_before_acknowledging() {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let c = Connection {
            server: format!("http://{}", listener.local_addr().unwrap()),
            id: "fixture".into(),
            token: "fixture".into(),
        };
        let serving = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nx-offset: 0\r\nConnection: close\r\n\r\n").unwrap();
                stream
                    .write_all(if attempt == 0 { b"a" } else { b"abc" })
                    .unwrap();
            }
        });
        assert_eq!(
            receive_block(&c, "fixture").unwrap(),
            Some((0, b"abc".to_vec()))
        );
        serving.join().unwrap();
    }
    #[test]
    fn rust_clients_transfer_fixture_through_real_node_relay() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        struct Server(std::process::Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let (dir, staging, mut m) = fixture();
        // Exercise hundreds of block/ack requests rather than a tiny one-block clip.
        let audio = fs::read(&staging).unwrap().repeat(1024);
        fs::write(&staging, &audio).unwrap();
        m.size = audio.len() as u64;
        m.sha256 = format!("{:x}", Sha256::digest(&audio));
        let relay = Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/relay.mjs");
        let child = Command::new("node")
            .arg(relay)
            .env("PORT", "0")
            .env("HOST", "127.0.0.1")
            .env("RELAY_REGISTRATION_KEY", "fixture-key")
            .env("RELAY_STATE", dir.join("devices.json"))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut server = Server(child);
        let mut line = String::new();
        std::io::BufReader::new(server.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line
            .split_whitespace()
            .find_map(|s| s.parse::<u16>().ok())
            .unwrap();
        let url = format!("http://127.0.0.1:{port}");
        let register = |name: &str| {
            let data = response(
                client()
                    .unwrap()
                    .post(format!("{url}/register"))
                    .header("x-hearfolio-access-key", "fixture-key")
                    .json(&json!({"name":name}))
                    .send()
                    .unwrap(),
            )
            .unwrap();
            Connection {
                server: url.clone(),
                id: data["id"].as_str().unwrap().into(),
                token: data["token"].as_str().unwrap().into(),
            }
        };
        let sender = register("Rust Linux");
        let invitation = rpc(&sender, "code", json!({})).unwrap();
        let join_url = url.clone();
        let code = invitation["code"].as_str().unwrap().to_owned();
        let joining = std::thread::spawn(move || {
            join_relay(&join_url, &json!("Rust Android protocol"), &code).unwrap()
        });
        let waiting = Instant::now();
        let request = loop {
            let inbox = rpc(&sender, "poll", json!({})).unwrap();
            if let Some(id) = inbox["pairRequests"][0]["id"].as_str() {
                break id.to_owned();
            }
            assert!(waiting.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        };
        rpc(&sender, "confirmPair", json!({"id":request,"accept":true})).unwrap();
        let credentials = joining.join().unwrap();
        let receiver = Connection {
            server: url.clone(),
            id: credentials["id"].as_str().unwrap().into(),
            token: credentials["token"].as_str().unwrap().into(),
        };
        let offered = rpc(&sender, "offer", json!({"to":receiver.id,"manifest":m})).unwrap();
        let id = offered["id"].as_str().unwrap().to_owned();
        let received = rpc(&receiver, "accept", json!({"id":id})).unwrap();
        let sid = id.clone();
        let source = staging.clone();
        let sending = std::thread::spawn(move || {
            let mut input = fs::File::open(source).unwrap();
            let mut offset = 0;
            let mut buf = [0; BLOCK];
            loop {
                let n = input.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                response(
                    client()
                        .unwrap()
                        .put(format!("{}/transfers/{sid}/chunk", sender.server))
                        .bearer_auth(&sender.token)
                        .header("x-offset", offset.to_string())
                        .body(buf[..n].to_vec())
                        .send()
                        .unwrap(),
                )
                .unwrap();
                offset += n;
            }
            sender
        });
        let destination = dir.join("temp/received.part");
        let mut file = fs::File::create(&destination).unwrap();
        let mut offset = 0;
        while offset < m.size {
            let Some((position, bytes)) = receive_block(&receiver, &id).unwrap() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            assert_eq!(position, offset);
            file.write_all(&bytes).unwrap();
            offset += bytes.len() as u64;
            rpc(&receiver, "ack", json!({"id":id,"offset":offset})).unwrap();
        }
        file.sync_all().unwrap();
        drop(file);
        let sender = sending.join().unwrap();
        assert_eq!(hash_file(&destination).unwrap().0, m.sha256);
        let manifest: Manifest = serde_json::from_value(received["manifest"].clone()).unwrap();
        let entry =
            commit_received(&dir, &destination, "hearing-network-test".into(), manifest).unwrap();
        assert_eq!(
            fs::read(&entry.input_path).unwrap(),
            fs::read(staging).unwrap()
        );
        assert_eq!(
            fs::read_to_string(entry.output_path.as_ref().unwrap()).unwrap(),
            m.transcript
        );
        rpc(&receiver, "finish", json!({"id":id})).unwrap();
        assert_eq!(
            rpc(&sender, "status", json!({"id":id})).unwrap()["status"],
            "delivered"
        );
        drop(server);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn startup_cleanup_only_removes_owned_partial_files() {
        let (dir, staging, _) = fixture();
        let owned = dir
            .join("temp")
            .join(format!("transfer-{}.part", uuid::Uuid::new_v4()));
        fs::write(&owned, "partial").unwrap();
        let unrelated = dir.join("temp/transfer-personal.part");
        fs::write(&unrelated, "keep").unwrap();
        cleanup_partial_files(&dir);
        assert!(!owned.exists());
        assert!(unrelated.exists());
        assert!(staging.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn http_requires_local_address() {
        assert!(server_url("http://192.168.1.2:8787").is_ok());
        assert!(server_url("http://example.com").is_err());
        assert!(server_url("https://example.com/path").is_err());
    }
    #[test]
    fn manifest_rejects_paths_and_oversize() {
        let mut m = Manifest {
            version: 1,
            source_id: "hearing-1".into(),
            name: "Recording".into(),
            extension: "../wav".into(),
            size: 1,
            sha256: "a".repeat(64),
            transcript: String::new(),
            created_at: 0,
            completed_at: None,
            duration_seconds: None,
            model: None,
            configuration: None,
        };
        assert!(validate_manifest(&m).is_err());
        m.extension = "wav".into();
        assert!(validate_manifest(&m).is_ok());
        m.size = MAX_SIZE + 1;
        assert!(validate_manifest(&m).is_err());
    }
}
