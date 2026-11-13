//! Keep a user-started transfer alive while the Android activity is paused.
use tauri::AppHandle;
#[cfg(target_os = "android")]
use tauri::Manager;

#[cfg(target_os = "android")]
fn journal_path(name: &str) -> Result<std::path::PathBuf, String> {
    Ok(crate::base()?.join("temp").join(format!("{name}.json")))
}
#[cfg(target_os = "android")]
fn write_journal(path: &std::path::Path, value: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    let temp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|_| "errors.transferBackgroundUnavailable")?;
    file.write_all(&serde_json::to_vec(value).map_err(|_| "errors.transferBackgroundUnavailable")?)
        .and_then(|_| file.sync_all())
        .map_err(|_| "errors.transferBackgroundUnavailable")?;
    std::fs::rename(temp, path).map_err(|_| "errors.transferBackgroundUnavailable".into())
}
#[cfg(target_os = "android")]
fn read_journal(path: &std::path::Path) -> Option<serde_json::Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}
#[cfg(target_os = "android")]
fn inbox_active() -> bool {
    if !INBOX.lock().is_ok_and(|preferences| preferences.enabled) {
        return false;
    }
    journal_path("inbox-native")
        .ok()
        .and_then(|path| read_journal(&path))
        .is_some_and(|value| value["active"] == true)
}
#[cfg(target_os = "android")]
static ACTIVE_TRANSFER: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
#[cfg(target_os = "android")]
static CANCELLED_TRANSFER: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
pub(crate) fn network_waiting(_waiting: bool) {
    #[cfg(target_os = "android")]
    if let Ok(path) = journal_path("lan-network-state") {
        let _ = write_journal(&path, &serde_json::json!({"waiting":_waiting}));
    }
}
pub(crate) fn check_transfer_cancelled() -> Result<(), String> {
    #[cfg(target_os = "android")]
    if let Some(id) = ACTIVE_TRANSFER
        .lock()
        .map_err(|_| "errors.transferBackgroundUnavailable")?
        .clone()
    {
        if CANCELLED_TRANSFER
            .lock()
            .is_ok_and(|c| c.as_deref() == Some(&id))
            || read_journal(&journal_path(&format!("transfer-control-{id}"))?)
                .is_some_and(|v| v["id"] == id && v["cancelled"] == true)
        {
            return Err("errors.transferCancelled".into());
        }
    }
    Ok(())
}
pub(crate) struct TransferGuard {
    #[cfg(target_os = "android")]
    id: String,
    #[cfg(target_os = "android")]
    state_path: std::path::PathBuf,
    #[cfg(target_os = "android")]
    control_path: std::path::PathBuf,
    #[cfg(target_os = "android")]
    started: std::time::Instant,
    #[cfg(target_os = "android")]
    last_update: std::time::Instant,
    #[cfg(target_os = "android")]
    snapshot: serde_json::Value,
    success: bool,
}
impl TransferGuard {
    pub fn start(_app: &AppHandle, _direction: &str) -> Result<Self, String> {
        crate::transfer::network_restored();
        #[cfg(target_os = "android")]
        {
            let id = uuid::Uuid::new_v4().to_string();
            let state_path = journal_path(&format!("transfer-state-{id}"))?;
            let control_path = journal_path(&format!("transfer-control-{id}"))?;
            let snapshot = serde_json::json!({"id":id,"direction":_direction,"state":"running","stage":"waiting","current":0,"total":0,"error":null,"recordId":null,"result":null});
            write_journal(&state_path, &snapshot)?;
            write_journal(
                &journal_path("transfer-active")?,
                &serde_json::json!({"id":id,"direction":_direction}),
            )?;
            *ACTIVE_TRANSFER
                .lock()
                .map_err(|_| "errors.transferBackgroundUnavailable")? = Some(id.clone());
            // A guard exists before native startup, so denied/failed startup also
            // leaves a terminal snapshot and clears the runtime ownership token.
            let guard = Self {
                id: id.clone(),
                state_path: state_path.clone(),
                control_path: control_path.clone(),
                started: std::time::Instant::now(),
                last_update: std::time::Instant::now(),
                snapshot,
                success: false,
            };
            let payload = serde_json::json!({"id":id,"direction":_direction,"snapshotPath":state_path,"controlPath":control_path});
            if inbox_active() {
                write_journal(&journal_path("inbox-transfer")?, &payload)?;
                let start = std::time::Instant::now();
                while !read_journal(&control_path).is_some_and(|value| value["ready"] == true) {
                    if !inbox_active() || start.elapsed() > std::time::Duration::from_secs(10) {
                        return Err("errors.transferBackgroundUnavailable".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            } else if !bounded_bridge(
                _app,
                "beginTransfer",
                payload,
                std::time::Duration::from_secs(120),
            ) {
                // A lost permission callback must not hold the job lock forever.
                return Err("errors.transferBackgroundUnavailable".into());
            }
            Ok(guard)
        }
        #[cfg(not(target_os = "android"))]
        Ok(Self { success: false })
    }
    pub fn check(&mut self) -> Result<(), String> {
        #[cfg(target_os = "android")]
        if CANCELLED_TRANSFER
            .lock()
            .is_ok_and(|cancelled| cancelled.as_deref() == Some(self.id.as_str()))
            || self.started.elapsed() >= MAX_TRANSFER
            || read_journal(&self.control_path)
                .is_some_and(|value| value["id"] == self.id && value["cancelled"] == true)
        {
            return Err("errors.transferCancelled".into());
        }
        Ok(())
    }
    pub fn progress(&mut self, _stage: &str, _current: u64, _total: u64) {
        #[cfg(target_os = "android")]
        if self.last_update.elapsed() >= std::time::Duration::from_secs(1) {
            self.snapshot["stage"] = serde_json::json!(_stage);
            self.snapshot["current"] = serde_json::json!(_current);
            self.snapshot["total"] = serde_json::json!(_total);
            let _ = write_journal(&self.state_path, &self.snapshot);
            self.last_update = std::time::Instant::now();
        }
    }
    pub fn finish<T: serde::Serialize>(&mut self, _result: &Result<T, String>) {
        crate::transfer::network_restored();
        self.success = _result.is_ok();
        #[cfg(target_os = "android")]
        {
            self.snapshot["state"] =
                serde_json::json!(if self.success { "finished" } else { "error" });
            self.snapshot["error"] = serde_json::json!(_result.as_ref().err());
            self.snapshot["result"] = _result
                .as_ref()
                .ok()
                .and_then(|result| serde_json::to_value(result).ok())
                .unwrap_or(serde_json::Value::Null);
            self.snapshot["recordId"] = self.snapshot["result"]["id"].clone();
        }
    }
    pub fn complete(&mut self) {
        self.success = true;
    }
}
impl Drop for TransferGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "android")]
        {
            self.snapshot["state"] =
                serde_json::json!(if self.success { "finished" } else { "error" });
            self.snapshot["success"] = serde_json::json!(self.success);
            if !self.success && self.snapshot["error"].is_null() {
                self.snapshot["error"] = serde_json::json!("errors.transferBackgroundUnavailable");
            }
            let _ = write_journal(&self.state_path, &self.snapshot);
            if let Ok(mut active) = ACTIVE_TRANSFER.lock() {
                if active.as_deref() == Some(self.id.as_str()) {
                    *active = None;
                }
            }
        }
    }
}

// UUID validation computes the private journal path; no persisted path is trusted.
#[cfg(any(target_os = "android", test))]
fn transfer_state_name(pointer: &serde_json::Value) -> Result<String, String> {
    let id = pointer["id"]
        .as_str()
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .ok_or("errors.transferBackgroundUnavailable")?;
    Ok(format!("transfer-state-{id}"))
}
#[cfg(any(target_os = "android", test))]
fn restored_transfer(mut snapshot: serde_json::Value, active: Option<&str>) -> serde_json::Value {
    if snapshot["state"] == "running" && snapshot["id"].as_str() != active {
        snapshot["state"] = serde_json::json!("interrupted");
        snapshot["error"] = serde_json::json!("errors.transferInterrupted");
        snapshot["result"] = serde_json::Value::Null;
    }
    snapshot
}
#[tauri::command]
pub async fn transfer_activity_snapshot() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(target_os = "android")]
        {
            let Some(pointer) = read_journal(&journal_path("transfer-active")?) else {
                return Ok(serde_json::json!({"state":"idle"}));
            };
            let snapshot = read_journal(&journal_path(&transfer_state_name(&pointer)?)?)
                .ok_or("errors.transferBackgroundUnavailable")?;
            let active = ACTIVE_TRANSFER
                .lock()
                .map_err(|_| "errors.transferBackgroundUnavailable")?;
            Ok(restored_transfer(snapshot, active.as_deref()))
        }
        #[cfg(not(target_os = "android"))]
        Ok(serde_json::json!({"state":"idle"}))
    })
    .await
    .map_err(|_| "errors.transferBackgroundUnavailable".to_owned())?
}

#[tauri::command]
pub async fn cancel_transfer_activity(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        {
            uuid::Uuid::parse_str(&id).map_err(|_| "errors.transferBackgroundUnavailable")?;
            let active = ACTIVE_TRANSFER
                .lock()
                .map_err(|_| "errors.transferBackgroundUnavailable")?;
            if active.as_deref() != Some(id.as_str()) {
                return Ok(());
            }
            *CANCELLED_TRANSFER
                .lock()
                .map_err(|_| "errors.transferBackgroundUnavailable")? = Some(id.clone());
            write_journal(
                &journal_path(&format!("transfer-control-{id}"))?,
                &serde_json::json!({"id":id,"ready":true,"cancelled":true}),
            )?;
        }
        #[cfg(not(target_os = "android"))]
        let _ = id;
        Ok(())
    })
    .await
    .map_err(|_| "errors.transferBackgroundUnavailable".to_owned())?
}

// A native polling loop continues without WebView timers. All acceptance still
// uses the ordinary authenticated protocol and the same exclusive job guard.
#[derive(Clone, serde::Deserialize, serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InboxPreferences {
    enabled: bool,
    auto_pair: bool,
    auto_receive: bool,
}
#[cfg(target_os = "android")]
static INBOX: std::sync::Mutex<InboxPreferences> = std::sync::Mutex::new(InboxPreferences {
    enabled: false,
    auto_pair: false,
    auto_receive: false,
});
#[cfg(target_os = "android")]
static WATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[tauri::command]
pub async fn transfer_background(
    app: AppHandle,
    preferences: Option<InboxPreferences>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "android")]
        {
            use std::sync::atomic::Ordering;
            let bridge = app.state::<tauri_plugin_recording::Recording<tauri::Wry>>();
            if let Some(p) = preferences {
                if p.enabled {
                    bridge.transfer::<()>("startInbox", serde_json::json!({"id":"inbox","snapshotPath":journal_path("inbox-native")?,"controlPath":journal_path("inbox-offers")?}))?;
                } else {
                    bridge.transfer::<()>("stopInbox", serde_json::json!({}))?;
                }
                *INBOX.lock().map_err(|_| "errors.operationFailed")? = p;
            }
            let status: serde_json::Value =
                bridge.transfer("inboxStatus", serde_json::json!({}))?;
            if status["active"] == true && !WATCHING.swap(true, Ordering::AcqRel) {
                let handle = app.clone();
                std::thread::spawn(move || watch_inbox(handle));
            }
            Ok(serde_json::json!({"enabled":status["active"] == true,"error":status["error"],"result":journal_path("background-transfer-result").ok().and_then(|path|read_journal(&path))}))
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (app, preferences);
            Ok(serde_json::json!({"enabled":false}))
        }
    })
    .await
    .map_err(|_| "errors.operationFailed".to_owned())?
}

#[cfg(target_os = "android")]
fn watch_inbox(app: AppHandle) {
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;
    use tauri::Emitter;
    let mut attempted = HashSet::<String>::new();
    let mut known = HashSet::<String>::new();
    loop {
        let p = match INBOX.lock() {
            Ok(p) => p.clone(),
            Err(_) => break,
        };
        let active = inbox_active();
        if !p.enabled || !active {
            break;
        }
        let state = tauri::async_runtime::block_on(crate::transfer::transfer_api(
            "poll".into(),
            serde_json::json!({}),
        ));
        if let Ok(state) = state {
            // Recheck policy after a potentially slow request: disabling automatic
            // acceptance must apply before we accept a newly discovered offer.
            let p = match INBOX.lock() {
                Ok(p) => p.clone(),
                Err(_) => break,
            };
            let active = inbox_active();
            if !p.enabled || !active {
                break;
            }

            let pairs = state["pairRequests"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let offers = state["offers"].as_array().cloned().unwrap_or_default();
            let ids: HashSet<String> = pairs
                .iter()
                .chain(offers.iter())
                .filter_map(|v| v["id"].as_str().map(str::to_owned))
                .collect();
            attempted.retain(|id| ids.contains(id));
            let fresh = ids.iter().any(|id| !known.contains(id));
            if let Ok(path) = journal_path("inbox-offers") {
                let _ = write_journal(
                    &path,
                    &serde_json::json!({"current":ids.len(),"success":fresh,"revision":crate::clock()}),
                );
            }
            known = ids;
            let (pairs, receive) = incoming_actions(
                &state,
                &p,
                crate::JOB_ACTIVE.load(Ordering::Acquire),
                &attempted,
            );
            for id in pairs {
                let _ = tauri::async_runtime::block_on(crate::transfer::transfer_api(
                    "confirmPair".into(),
                    serde_json::json!({"id":id,"accept":true}),
                ));
            }
            if let Some(id) = receive {
                let result = tauri::async_runtime::block_on(crate::transfer::transfer_receive(
                    app.clone(),
                    id.clone(),
                ));
                if !matches!(&result,Err(e) if e=="errors.operationBusy") {
                    attempted.insert(id.clone());
                    let payload =
                        serde_json::json!({"id":id,"ok":result.is_ok(),"error":result.err()});
                    if let Ok(path) = journal_path("background-transfer-result") {
                        let _ = write_journal(&path, &payload);
                    }
                    let _ = app.emit("background-transfer-result", payload);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    WATCHING.store(false, Ordering::Release);
}

#[cfg(any(target_os = "android", test))]
fn incoming_actions(
    state: &serde_json::Value,
    p: &InboxPreferences,
    busy: bool,
    attempted: &std::collections::HashSet<String>,
) -> (Vec<String>, Option<String>) {
    if !p.enabled {
        return (vec![], None);
    }
    let ids = |key: &str| {
        state[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let pairs = if p.auto_pair {
        ids("pairRequests")
    } else {
        vec![]
    };
    let receive = if p.auto_receive && !busy {
        ids("offers").into_iter().find(|id| !attempted.contains(id))
    } else {
        None
    };
    (pairs, receive)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_requires_explicit_consent_and_respects_independent_confirmation_flags() {
        let state = serde_json::json!({"pairRequests":[{"id":"pair"}],"offers":[{"id":"file"}]});
        let attempted = std::collections::HashSet::new();
        let mut p = InboxPreferences::default();
        assert_eq!(
            incoming_actions(&state, &p, false, &attempted),
            (vec![], None)
        );
        p.enabled = true;
        assert_eq!(
            incoming_actions(&state, &p, false, &attempted),
            (vec![], None)
        );
        p.auto_pair = true;
        assert_eq!(
            incoming_actions(&state, &p, false, &attempted),
            (vec!["pair".into()], None)
        );
        p.auto_receive = true;
        assert_eq!(
            incoming_actions(&state, &p, false, &attempted),
            (vec!["pair".into()], Some("file".into()))
        );
        p.enabled = false;
        assert_eq!(
            incoming_actions(&state, &p, false, &attempted),
            (vec![], None)
        );
    }
    #[test]
    fn busy_recording_defers_incoming_and_failed_offer_does_not_loop() {
        let state = serde_json::json!({"offers":[{"id":"failed"},{"id":"next"}]});
        let p = InboxPreferences {
            enabled: true,
            auto_receive: true,
            auto_pair: false,
        };
        let attempted = std::collections::HashSet::from(["failed".into()]);
        assert_eq!(incoming_actions(&state, &p, true, &attempted).1, None);
        assert_eq!(
            incoming_actions(&state, &p, false, &attempted).1,
            Some("next".into())
        );
    }
}

// Operation state is owned by Rust and stored before native work starts. A
// recreated WebView can read the result; a restarted process sees interruption
// and never repeats a cloud request automatically.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSnapshot {
    id: String,
    kind: String,
    record_id: Option<String>,
    state: String,
    progress: Option<serde_json::Value>,
    error: Option<String>,
    result: Option<serde_json::Value>,
    // Live text is held in memory only; committed cloud chunks are durable in
    // the per-record resume file instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    partial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resumable: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    acknowledged: bool,
}
struct ActiveOperation {
    snapshot: OperationSnapshot,
    app: Option<AppHandle>,
    /// Where this operation's snapshot is persisted.
    path: std::path::PathBuf,
    /// The job lock this operation runs under; released if the job is reaped.
    job: Option<u64>,
    started: std::time::Instant,
    persisted: std::time::Instant,
    last_activity: std::time::Instant,
    dirty: bool,
    cancelled: bool,
    /// When a stop was requested (user, watchdog or the native service). A job
    /// that has not ended a grace period later is wedged and is reaped.
    stop_requested: Option<std::time::Instant>,
}
static OPERATION: std::sync::Mutex<Option<ActiveOperation>> = std::sync::Mutex::new(None);
thread_local! {
    /// The operation the current thread works for. A reaped job finds its
    /// operation gone and stops at its next check instead of carrying on.
    static CURRENT_OPERATION: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}
fn operation_state() -> std::sync::MutexGuard<'static, Option<ActiveOperation>> {
    // A panicking job must never leave every later operation "busy".
    OPERATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
// The watchdog is re-armed by every progress report. The absolute cap only
// bounds a job that keeps reporting forever.
const STALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const MAX_OPERATION: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);
/// How long a job may take to notice a stop before it is considered wedged.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(20);
/// Extra time past the watchdog limits before a silent job is reaped: a job
/// that is alive times itself out at its next check well within this.
const STALE_GRACE: std::time::Duration = std::time::Duration::from_secs(2 * 60);
#[cfg(target_os = "android")]
const MAX_TRANSFER: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);
#[cfg(target_os = "android")]
fn operation_control_path(id: &str) -> Result<std::path::PathBuf, String> {
    Ok(crate::base()?
        .join("temp")
        .join(format!("operation-control-{id}.json")))
}
fn operation_path() -> Result<std::path::PathBuf, String> {
    Ok(crate::base()?.join("operation-state.json"))
}
fn persist_operation_at(path: &std::path::Path, state: &OperationSnapshot) -> Result<(), String> {
    use std::io::Write;
    let temp = path.with_extension("json.tmp");
    let durable = OperationSnapshot {
        partial: None,
        ..state.clone()
    };
    let bytes = serde_json::to_vec(&durable).map_err(|_| "errors.operationFailed")?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|_| "errors.operationFailed")?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "errors.operationFailed")?;
    std::fs::rename(temp, path).map_err(|_| "errors.operationFailed".into())
}
fn read_operation_at(path: &std::path::Path) -> Result<Option<OperationSnapshot>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("errors.operationFailed".into()),
    };
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("errors.operationFailed".into());
    }
    let mut state: OperationSnapshot =
        serde_json::from_slice(&bytes).map_err(|_| "errors.operationFailed")?;
    // Only the in-memory registry describes live work. A "running" file is
    // what a process that died mid-job left behind.
    if state.state == "running" {
        state.state = "interrupted".into();
        state.error = Some("errors.operationInterrupted".into());
        state.result = None;
    }
    Ok(Some(state))
}
// Unfinished paid work is reported, never resumed automatically.
fn with_resume(mut snapshot: OperationSnapshot) -> OperationSnapshot {
    if snapshot.kind == "transcribe" && snapshot.state != "running" && snapshot.state != "finished"
    {
        if let (Some(record), Ok(root)) = (snapshot.record_id.as_deref(), crate::base()) {
            let seconds = crate::cloud::load()
                .map_or(crate::cloud::DEFAULT_CHUNK_SECONDS, |settings| {
                    settings.chunk_seconds
                });
            snapshot.resumable = crate::resume::resumable(&root, record, seconds);
        }
    }
    snapshot
}
/// Runs a plugin command that answers through a readiness callback. Android
/// can lose that callback with its Activity (a permission prompt, a recreated
/// window); the caller then gets an error instead of waiting forever, and the
/// detached bridge thread ends whenever the late answer arrives.
#[cfg(target_os = "android")]
fn bounded_bridge(
    app: &AppHandle,
    command: &'static str,
    payload: serde_json::Value,
    timeout: std::time::Duration,
) -> bool {
    let (sender, receiver) = std::sync::mpsc::channel();
    let app = app.clone();
    std::thread::spawn(move || {
        let result = app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .transfer::<()>(command, payload)
            .is_ok();
        let _ = sender.send(result);
    });
    matches!(receiver.recv_timeout(timeout), Ok(true))
}
// The bridge call waits for OperationService readiness. It runs without the
// operation lock, and a lost callback cannot block snapshot or cancel calls.
#[cfg(target_os = "android")]
fn begin_native(app: &AppHandle, payload: serde_json::Value) -> Result<(), String> {
    // Kotlin answers within ~10 s of dispatch; the extra time covers a
    // notification permission prompt the user is still reading.
    if bounded_bridge(
        app,
        "beginOperation",
        payload,
        std::time::Duration::from_secs(120),
    ) {
        Ok(())
    } else {
        Err("errors.operationBackgroundUnavailable".into())
    }
}
pub(crate) struct OperationGuard {
    id: String,
    completed: bool,
}
impl OperationGuard {
    pub fn start(
        app: &AppHandle,
        job: &crate::JobGuard,
        kind: &str,
        record_id: Option<&str>,
        _network: bool,
    ) -> Result<Self, String> {
        #[allow(unused_mut)]
        let mut guard = Self::register(
            Some(app),
            operation_path()?,
            Some(job.token()),
            kind,
            record_id,
        )?;
        #[cfg(target_os = "android")]
        {
            let id = guard.id.clone();
            let payload = serde_json::json!({"id":id,"kind":kind,"network":_network,"snapshotPath":operation_path()?,"controlPath":operation_control_path(&id)?});
            if let Err(error) = begin_native(app, payload) {
                guard.finish::<()>(&Err(error.clone()));
                return Err(error);
            }
        }
        Ok(guard)
    }
    /// Reserves the single operation slot for the calling thread.
    pub(crate) fn register(
        app: Option<&AppHandle>,
        path: std::path::PathBuf,
        job: Option<u64>,
        kind: &str,
        record_id: Option<&str>,
    ) -> Result<Self, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let snapshot = OperationSnapshot {
            id: id.clone(),
            kind: kind.into(),
            record_id: record_id.map(str::to_owned),
            state: "running".into(),
            progress: None,
            error: None,
            result: None,
            partial: None,
            resumable: None,
            acknowledged: false,
        };
        // Reserve and commit ownership under the lock; native startup happens
        // without it.
        let mut state = operation_state();
        if state.is_some() {
            return Err("errors.operationBusy".into());
        }
        persist_operation_at(&path, &snapshot)?;
        let now = std::time::Instant::now();
        *state = Some(ActiveOperation {
            snapshot,
            app: app.cloned(),
            path,
            job,
            started: now,
            persisted: now,
            last_activity: now,
            dirty: false,
            cancelled: false,
            stop_requested: None,
        });
        drop(state);
        CURRENT_OPERATION.with(|current| *current.borrow_mut() = Some(id.clone()));
        Ok(Self {
            id,
            completed: false,
        })
    }
    pub fn finish<T: serde::Serialize>(&mut self, result: &Result<T, String>) {
        use tauri::Emitter;
        self.completed = true;
        CURRENT_OPERATION.with(|current| {
            let mut current = current.borrow_mut();
            if current.as_deref() == Some(self.id.as_str()) {
                *current = None;
            }
        });
        let mut state = operation_state();
        let Some(mut active) = state.take() else {
            return;
        };
        if active.snapshot.id != self.id {
            // Reaped earlier; a newer operation owns the slot now.
            *state = Some(active);
            return;
        }
        let error = result.as_ref().err().cloned();
        active.snapshot.state = match &error {
            None => "finished",
            Some(error) => terminal_state(error),
        }
        .into();
        active.snapshot.error = error;
        active.snapshot.result = result
            .as_ref()
            .ok()
            .and_then(|value| serde_json::to_value(value).ok());
        if result.is_ok() {
            active.snapshot.partial = None;
        }
        if persist_operation_at(&active.path, &active.snapshot).is_err() {
            active.snapshot.error = Some("errors.operationFailed".into());
        }
        drop(state);
        if let Some(app) = active.app.take() {
            let snapshot = with_resume(active.snapshot);
            let _ = app.emit("operation-native-finished", &snapshot);
        }
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.finish::<()>(&Err("errors.operationInterrupted".into()));
        }
    }
}
fn terminal_state(error: &str) -> &'static str {
    match error {
        "errors.operationCancelled" => "cancelled",
        "errors.operationInterrupted" => "interrupted",
        _ => "error",
    }
}
/// Why a still-registered operation must be considered dead, if it must.
fn stale_reason(
    age: std::time::Duration,
    idle: std::time::Duration,
    stop_requested: Option<std::time::Duration>,
) -> Option<&'static str> {
    if stop_requested.is_some_and(|since| since >= STOP_GRACE) {
        return Some("errors.operationInterrupted");
    }
    if idle >= STALL_LIMIT + STALE_GRACE || age >= MAX_OPERATION + STALE_GRACE {
        return Some("errors.operationTimeout");
    }
    None
}
/// Android: the service writes its control file when it is cancelled from the
/// notification, times out or is destroyed by the system.
#[cfg(target_os = "android")]
fn observe_native_stop(active: &mut ActiveOperation) {
    if active.stop_requested.is_some() {
        return;
    }
    let Ok(path) = operation_control_path(&active.snapshot.id) else {
        return;
    };
    if let Some(status) = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        apply_native_control(active, &status);
    }
}
#[cfg(any(target_os = "android", test))]
fn apply_native_control(active: &mut ActiveOperation, status: &serde_json::Value) {
    if active.stop_requested.is_some() {
        return;
    }
    if status["id"] == active.snapshot.id.as_str() && status["cancelled"] == true {
        active.cancelled = true;
        active.snapshot.error = Some(
            status["error"]
                .as_str()
                .unwrap_or("errors.operationInterrupted")
                .into(),
        );
        active.stop_requested = Some(std::time::Instant::now());
    }
}
/// Ends an operation whose job stopped responding: the stop it was asked for
/// went unanswered, or it has been silent past every watchdog. The snapshot
/// becomes terminal (committed cloud parts stay resumable), its job lock is
/// released, and the job's thread stops at its next check if it ever wakes.
pub(crate) fn reap_stale_operation() -> Option<OperationSnapshot> {
    use tauri::Emitter;
    let mut active = {
        let mut state = operation_state();
        let active = state.as_mut()?;
        #[cfg(target_os = "android")]
        observe_native_stop(active);
        let reason = stale_reason(
            active.started.elapsed(),
            active.last_activity.elapsed(),
            active.stop_requested.map(|since| since.elapsed()),
        )?;
        let mut active = state.take()?;
        let error = active
            .snapshot
            .error
            .clone()
            .filter(|_| active.cancelled)
            .unwrap_or_else(|| reason.into());
        active.snapshot.state = terminal_state(&error).into();
        active.snapshot.error = Some(error);
        active.snapshot.result = None;
        let _ = persist_operation_at(&active.path, &active.snapshot);
        active
    };
    if let Some(job) = active.job {
        crate::release_job(job);
    }
    let snapshot = active.snapshot.clone();
    if let Some(app) = active.app.take() {
        let _ = app.emit("operation-native-finished", with_resume(active.snapshot));
    }
    Some(snapshot)
}
pub(crate) fn check_current() -> Result<(), String> {
    let current = CURRENT_OPERATION.with(|current| current.borrow().clone());
    let Some(current) = current else {
        // Not operation work (an archive import, a recording): nothing to check.
        return Ok(());
    };
    let mut state = operation_state();
    let Some(active) = state
        .as_mut()
        .filter(|active| active.snapshot.id == current)
    else {
        // This job was reaped while it was stuck; its result is not wanted.
        return Err("errors.operationInterrupted".into());
    };
    if active.cancelled {
        return Err(active
            .snapshot
            .error
            .clone()
            .unwrap_or_else(|| "errors.operationCancelled".into()));
    }
    if active.started.elapsed() >= MAX_OPERATION || active.last_activity.elapsed() >= STALL_LIMIT {
        active.cancelled = true;
        active
            .stop_requested
            .get_or_insert_with(std::time::Instant::now);
        active.snapshot.error = Some("errors.operationTimeout".into());
        return Err("errors.operationTimeout".into());
    }
    // Trailing flush for throttled progress, so the last state before a quiet
    // period (one long request) is what a recreated UI or the service sees.
    if active.dirty && active.persisted.elapsed() >= std::time::Duration::from_secs(1) {
        let _ = persist_operation_at(&active.path, &active.snapshot);
        active.persisted = std::time::Instant::now();
        active.dirty = false;
    }
    #[cfg(target_os = "android")]
    {
        observe_native_stop(active);
        if active.cancelled {
            return Err(active
                .snapshot
                .error
                .clone()
                .unwrap_or_else(|| "errors.operationInterrupted".into()));
        }
    }
    Ok(())
}
fn thread_operation() -> Option<String> {
    CURRENT_OPERATION.with(|current| current.borrow().clone())
}
/// The current operation's id when the calling thread works for it. Scratch
/// files carry it, so a reaped run and its retry never share a file.
pub(crate) fn current_operation_id() -> Option<String> {
    let current = thread_operation()?;
    operation_state()
        .as_ref()
        .filter(|active| active.snapshot.id == current)
        .map(|_| current)
}
/// True on a thread whose operation was reaped (or replaced): whatever it
/// reports belongs to nobody and must not reach the UI or the new operation.
pub(crate) fn stale_thread() -> bool {
    thread_operation().is_some() && current_operation_id().is_none()
}
/// Runs `work` (an archive write) only while the calling thread's operation
/// is live and not stopped, holding the operation lock so the watchdog cannot
/// reap it half-way. Work outside an operation runs as is. `work` must not
/// report progress or check the operation itself.
pub(crate) fn commit<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let Some(current) = thread_operation() else {
        return work();
    };
    let state = operation_state();
    match state
        .as_ref()
        .filter(|active| active.snapshot.id == current)
    {
        None => Err("errors.operationInterrupted".into()),
        Some(active) if active.cancelled => Err(active
            .snapshot
            .error
            .clone()
            .unwrap_or_else(|| "errors.operationCancelled".into())),
        Some(_) => work(),
    }
}
// Stage and step changes are durable immediately; byte-level deltas are
// throttled and flushed by `check_current`.
fn progress_milestone(previous: Option<&serde_json::Value>, next: &serde_json::Value) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    previous["stage"] != next["stage"]
        || previous["total"] != next["total"]
        || (previous["current"] != next["current"]
            && next["total"].as_u64().is_some_and(|total| total <= 1000))
}
pub(crate) fn operation_progress(kind: &str, progress: serde_json::Value) {
    let Some(current) = thread_operation() else {
        return;
    };
    let mut state = operation_state();
    let Some(active) = state.as_mut() else { return };
    // Only the operation's own thread reports: a reaped job must neither show
    // its progress as the new run's nor hide the new run's stall.
    if active.snapshot.kind != kind || active.snapshot.id != current {
        return;
    }
    let milestone = progress_milestone(active.snapshot.progress.as_ref(), &progress);
    active.snapshot.progress = Some(progress);
    active.last_activity = std::time::Instant::now();
    active.dirty = true;
    if milestone || active.persisted.elapsed() >= std::time::Duration::from_secs(2) {
        let _ = persist_operation_at(&active.path, &active.snapshot);
        active.persisted = std::time::Instant::now();
        active.dirty = false;
    }
}
/// Records live text for snapshot readers and returns `(operationId, recordId)`.
pub(crate) fn operation_partial(kind: &str, text: &str) -> Option<(String, Option<String>)> {
    let current = thread_operation()?;
    let mut state = operation_state();
    let active = state.as_mut()?;
    if active.snapshot.kind != kind || active.snapshot.id != current {
        return None;
    }
    active.snapshot.partial = Some(text.to_owned());
    active.last_activity = std::time::Instant::now();
    Some((
        active.snapshot.id.clone(),
        active.snapshot.record_id.clone(),
    ))
}
#[tauri::command]
pub async fn operation_activity_snapshot() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        reap_stale_operation();
        if let Some(active) = operation_state().as_ref() {
            return serde_json::to_value(&active.snapshot)
                .map_err(|_| "errors.operationFailed".into());
        }
        Ok(match read_operation_at(&operation_path()?)? {
            Some(state) if !state.acknowledged => {
                serde_json::to_value(with_resume(state)).map_err(|_| "errors.operationFailed")?
            }
            _ => serde_json::json!({"state":"idle"}),
        })
    })
    .await
    .map_err(|_| "errors.operationFailed".to_owned())?
}
/// Asks the running job to stop. A job that does not react within
/// `STOP_GRACE` is reaped, so "Stop" always frees the app.
pub(crate) fn request_stop(id: &str) -> Result<bool, String> {
    let mut state = operation_state();
    let Some(active) = state.as_mut().filter(|active| active.snapshot.id == id) else {
        return Ok(false);
    };
    active.cancelled = true;
    active.snapshot.error = Some("errors.operationCancelled".into());
    active
        .stop_requested
        .get_or_insert_with(std::time::Instant::now);
    persist_operation_at(&active.path, &active.snapshot)?;
    Ok(true)
}
#[tauri::command]
pub async fn cancel_background_operation(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if request_stop(&id)? {
            std::thread::spawn(|| {
                std::thread::sleep(STOP_GRACE + std::time::Duration::from_secs(1));
                reap_stale_operation();
            });
        }
        Ok(())
    })
    .await
    .map_err(|_| "errors.operationFailed".to_owned())?
}
/// What holds the exclusive job lock, so a "busy" answer can say what is
/// running and offer to open or stop it.
#[tauri::command]
pub async fn busy_activity() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        reap_stale_operation();
        let operation = operation_state().as_ref().map(|active| {
            (
                OperationSnapshot {
                    partial: None,
                    ..active.snapshot.clone()
                },
                active.stop_requested.is_some(),
            )
        });
        #[cfg(target_os = "android")]
        let transfer = ACTIVE_TRANSFER
            .lock()
            .ok()
            .and_then(|id| id.clone())
            .map(|id| {
                let direction = journal_path("transfer-active")
                    .ok()
                    .and_then(|path| read_journal(&path))
                    .filter(|pointer| pointer["id"] == id.as_str())
                    .and_then(|pointer| pointer["direction"].as_str().map(str::to_owned));
                serde_json::json!({"id":id,"direction":direction})
            });
        #[cfg(not(target_os = "android"))]
        let transfer: Option<serde_json::Value> = None;
        Ok(busy_description(crate::job_holder(), operation, transfer))
    })
    .await
    .map_err(|_| "errors.operationFailed".to_owned())?
}
fn busy_description(
    holder: Option<(&'static str, std::time::Duration)>,
    operation: Option<(OperationSnapshot, bool)>,
    transfer: Option<serde_json::Value>,
) -> serde_json::Value {
    let Some((kind, held)) = holder else {
        return serde_json::json!({"kind":"idle"});
    };
    let (operation, stopping) = match operation {
        Some((snapshot, stopping)) => (Some(snapshot), stopping),
        None => (None, false),
    };
    serde_json::json!({
        "kind": operation.as_ref().map_or(kind, |snapshot| snapshot.kind.as_str()),
        "seconds": held.as_secs(),
        "operation": operation,
        "stopping": stopping,
        "transfer": if kind == "transfer" { transfer } else { None },
    })
}
fn acknowledge_operation_at(path: &std::path::Path, id: &str) -> Result<(), String> {
    let Some(mut snapshot) = read_operation_at(path)? else {
        return Ok(());
    };
    if snapshot.id != id || snapshot.acknowledged {
        return Ok(());
    }
    snapshot.acknowledged = true;
    persist_operation_at(path, &snapshot)
}
/// The UI has shown this terminal result; later snapshots report idle, so a
/// cleared WebView storage cannot replay it.
#[tauri::command]
pub async fn acknowledge_operation(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Holding the lock orders this against a new operation replacing the file.
        let state = operation_state();
        if state.is_some() {
            return Ok(());
        }
        acknowledge_operation_at(&operation_path()?, &id)
    })
    .await
    .map_err(|_| "errors.operationFailed".to_owned())?
}

#[cfg(test)]
mod operation_tests {
    use super::*;
    #[test]
    fn restored_running_work_is_interrupted_without_a_replayable_cloud_request() {
        let path =
            std::env::temp_dir().join(format!("hearfolio-operation-{}.json", uuid::Uuid::new_v4()));
        let mut snapshot = OperationSnapshot {
            id: "job".into(),
            kind: "transcribe".into(),
            record_id: Some("record".into()),
            state: "running".into(),
            progress: Some(serde_json::json!({"stage":"progress.recognizeCloud"})),
            error: None,
            result: Some(serde_json::json!("never replay")),
            partial: Some("live text".into()),
            resumable: None,
            acknowledged: false,
        };
        persist_operation_at(&path, &snapshot).unwrap();
        let recovered = read_operation_at(&path).unwrap().unwrap();
        assert_eq!(recovered.state, "interrupted");
        assert_eq!(
            recovered.error.as_deref(),
            Some("errors.operationInterrupted")
        );
        assert!(recovered.result.is_none());
        assert_eq!(recovered.record_id.as_deref(), Some("record"));
        // Live text is never written to disk.
        assert!(recovered.partial.is_none());
        snapshot.state = "finished".into();
        snapshot.result = Some(serde_json::json!("saved"));
        persist_operation_at(&path, &snapshot).unwrap();
        let recovered = read_operation_at(&path).unwrap().unwrap();
        assert_eq!(recovered.state, "finished");
        assert_eq!(recovered.result, Some(serde_json::json!("saved")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        acknowledge_operation_at(&path, "other").unwrap();
        assert!(!read_operation_at(&path).unwrap().unwrap().acknowledged);
        acknowledge_operation_at(&path, "job").unwrap();
        let recovered = read_operation_at(&path).unwrap().unwrap();
        assert!(recovered.acknowledged);
        assert_eq!(recovered.state, "finished");
        std::fs::remove_file(path).unwrap();
    }
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        crate::JOB_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn temp_state() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("hearfolio-operation-{}.json", uuid::Uuid::new_v4()))
    }
    fn backdate(by: std::time::Duration) -> std::time::Instant {
        std::time::Instant::now()
            .checked_sub(by)
            .expect("monotonic clock older than the test offset")
    }
    /// A job thread that registers like `transcribe` and then blocks in a call
    /// that never checks for cancellation (a lost native callback).
    fn wedged_job(
        path: std::path::PathBuf,
    ) -> (
        String,
        std::sync::mpsc::Sender<()>,
        std::thread::JoinHandle<Result<(), String>>,
    ) {
        let (ready, registered) = std::sync::mpsc::channel();
        let (wake, wait) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let job = crate::JobGuard::acquire("transcribe").unwrap();
            let mut operation = OperationGuard::register(
                None,
                path,
                Some(job.token()),
                "transcribe",
                Some("record"),
            )
            .unwrap();
            ready.send(operation.id.clone()).unwrap();
            let _ = wait.recv();
            // Woken long after it was reaped: it must not carry on.
            let result = check_current();
            operation.finish(&result);
            drop(job);
            result
        });
        (registered.recv().unwrap(), wake, worker)
    }
    #[test]
    fn native_service_death_frees_a_wedged_job_and_keeps_its_result_terminal() {
        let _serial = serial();
        let path = temp_state();
        let (id, wake, worker) = wedged_job(path.clone());
        assert_eq!(
            crate::JobGuard::acquire("transcribe").err().as_deref(),
            Some("errors.operationBusy")
        );
        // OperationService.onDestroy (killed by the system) writes this.
        {
            let mut state = operation_state();
            let active = state.as_mut().unwrap();
            apply_native_control(
                active,
                &serde_json::json!({"id":id,"cancelled":true,"error":"errors.operationInterrupted"}),
            );
            assert!(active.stop_requested.is_some());
        }
        // Within the grace period the job may still notice and end itself.
        assert!(reap_stale_operation().is_none());
        assert!(crate::JobGuard::acquire("transcribe").is_err());
        operation_state().as_mut().unwrap().stop_requested = Some(backdate(STOP_GRACE));
        let reaped = reap_stale_operation().expect("wedged job reaped");
        assert_eq!(reaped.state, "interrupted");
        assert_eq!(reaped.error.as_deref(), Some("errors.operationInterrupted"));
        assert_eq!(reaped.record_id.as_deref(), Some("record"));
        // Persisted for a recreated WebView and for the service's poll.
        assert_eq!(
            read_operation_at(&path).unwrap().unwrap().state,
            "interrupted"
        );
        // The app is free again while the old thread is still blocked.
        let next = crate::JobGuard::acquire("transcribe").expect("job lock released");
        // The old thread wakes, stops at its check and cannot release the new lock.
        wake.send(()).unwrap();
        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            "errors.operationInterrupted"
        );
        assert_eq!(
            crate::job_holder().map(|(kind, _)| kind),
            Some("transcribe")
        );
        assert!(crate::JobGuard::acquire("import").is_err());
        drop(next);
        assert!(crate::job_holder().is_none());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_reaped_thread_cannot_report_into_or_commit_over_its_retry() {
        let _serial = serial();
        let (old_path, new_path) = (temp_state(), temp_state());
        let (ready, registered) = std::sync::mpsc::channel();
        let (wake, wait) = std::sync::mpsc::channel::<()>();
        let committed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let late_commit = committed.clone();
        let thread_path = old_path.clone();
        let worker = std::thread::spawn(move || {
            let job = crate::JobGuard::acquire("transcribe").unwrap();
            let mut operation = OperationGuard::register(
                None,
                thread_path,
                Some(job.token()),
                "transcribe",
                Some("record"),
            )
            .unwrap();
            ready.send(operation.id.clone()).unwrap();
            let _ = wait.recv();
            // Woken after the reap, as a stuck request finally returns.
            let stale = stale_thread();
            operation_progress(
                "transcribe",
                serde_json::json!({"stage":"progress.recognizedChunks","current":7,"total":8}),
            );
            let partial = operation_partial("transcribe", "old run's text");
            let result = commit(|| {
                late_commit.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            });
            operation.finish(&result);
            (stale, partial, result)
        });
        let old_id = registered.recv().unwrap();
        {
            let mut state = operation_state();
            let active = state.as_mut().unwrap();
            active.stop_requested = Some(backdate(STOP_GRACE));
        }
        assert_eq!(reap_stale_operation().unwrap().id, old_id);
        // The retry of the same record starts on this thread.
        let job = crate::JobGuard::acquire("transcribe").unwrap();
        let mut retry = OperationGuard::register(
            None,
            new_path.clone(),
            Some(job.token()),
            "transcribe",
            Some("record"),
        )
        .unwrap();
        let quiet_since = backdate(std::time::Duration::from_secs(60));
        operation_state().as_mut().unwrap().last_activity = quiet_since;
        assert!(!stale_thread());
        assert_eq!(current_operation_id().as_deref(), Some(retry.id.as_str()));
        wake.send(()).unwrap();
        let (stale, partial, result) = worker.join().unwrap();
        assert!(stale);
        assert!(partial.is_none());
        assert_eq!(result.unwrap_err(), "errors.operationInterrupted");
        assert!(!committed.load(std::sync::atomic::Ordering::SeqCst));
        {
            let state = operation_state();
            let active = state.as_ref().unwrap();
            assert_eq!(active.snapshot.id, retry.id);
            assert!(active.snapshot.progress.is_none());
            assert!(active.snapshot.partial.is_none());
            // The old thread's report did not hide the retry's own silence.
            assert_eq!(active.last_activity, quiet_since);
        }
        // The retry's own reports and writes go through.
        operation_progress(
            "transcribe",
            serde_json::json!({"stage":"a","current":1,"total":2}),
        );
        assert!(operation_partial("transcribe", "new text").is_some());
        assert_eq!(commit(|| Ok(5)), Ok(5));
        // A stopped run writes nothing either.
        assert!(request_stop(&retry.id).unwrap());
        assert_eq!(commit(|| Ok(())).unwrap_err(), "errors.operationCancelled");
        retry.finish::<()>(&Err("errors.operationCancelled".into()));
        drop(job);
        // Outside any operation (archive import of a recording) work runs as is.
        assert_eq!(commit(|| Ok("plain")), Ok("plain"));
        assert!(!stale_thread());
        std::fs::remove_file(old_path).unwrap();
        std::fs::remove_file(new_path).unwrap();
    }
    #[test]
    fn unanswered_stop_and_silent_jobs_are_reaped_but_live_ones_are_not() {
        let _serial = serial();
        use std::time::Duration;
        assert_eq!(stale_reason(Duration::ZERO, Duration::ZERO, None), None);
        assert_eq!(
            stale_reason(Duration::from_secs(3600), STALL_LIMIT, None),
            None,
            "a job past the stall limit times itself out at its next check"
        );
        assert_eq!(
            stale_reason(Duration::from_secs(3600), STALL_LIMIT + STALE_GRACE, None),
            Some("errors.operationTimeout")
        );
        assert_eq!(
            stale_reason(MAX_OPERATION + STALE_GRACE, Duration::ZERO, None),
            Some("errors.operationTimeout")
        );
        assert_eq!(
            stale_reason(Duration::ZERO, Duration::ZERO, Some(Duration::from_secs(5))),
            None
        );
        assert_eq!(
            stale_reason(Duration::ZERO, Duration::ZERO, Some(STOP_GRACE)),
            Some("errors.operationInterrupted")
        );
        // "Stop" pressed on a job that never reaches its next check.
        let path = temp_state();
        let (id, wake, worker) = wedged_job(path.clone());
        assert!(request_stop(&id).unwrap());
        assert!(!request_stop("someone-else").unwrap());
        assert!(reap_stale_operation().is_none());
        operation_state().as_mut().unwrap().stop_requested = Some(backdate(STOP_GRACE));
        let reaped = reap_stale_operation().unwrap();
        assert_eq!(reaped.state, "cancelled");
        assert_eq!(reaped.error.as_deref(), Some("errors.operationCancelled"));
        drop(crate::JobGuard::acquire("transcribe").unwrap());
        wake.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
        // Silent past every watchdog: reaped as a timeout.
        let (_, wake, worker) = wedged_job(path.clone());
        operation_state().as_mut().unwrap().last_activity = backdate(STALL_LIMIT + STALE_GRACE);
        let reaped = reap_stale_operation().unwrap();
        assert_eq!(reaped.state, "error");
        assert_eq!(reaped.error.as_deref(), Some("errors.operationTimeout"));
        drop(crate::JobGuard::acquire("transcribe").unwrap());
        wake.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_stale_running_snapshot_on_disk_never_makes_the_app_busy() {
        let _serial = serial();
        let path = temp_state();
        let left_behind = OperationSnapshot {
            id: "previous-process".into(),
            kind: "transcribe".into(),
            record_id: Some("record".into()),
            state: "running".into(),
            progress: Some(
                serde_json::json!({"stage":"progress.recognizedChunks","current":3,"total":8}),
            ),
            error: None,
            result: None,
            partial: None,
            resumable: None,
            acknowledged: false,
        };
        persist_operation_at(&path, &left_behind).unwrap();
        assert!(operation_state().is_none());
        assert_eq!(
            read_operation_at(&path).unwrap().unwrap().state,
            "interrupted"
        );
        let job = crate::JobGuard::acquire("transcribe").expect("not busy");
        let mut operation = OperationGuard::register(
            None,
            path.clone(),
            Some(job.token()),
            "transcribe",
            Some("record"),
        )
        .expect("a new operation replaces the stale file");
        operation.finish(&Ok::<_, String>("text"));
        drop(job);
        assert_eq!(read_operation_at(&path).unwrap().unwrap().state, "finished");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_panicking_job_thread_releases_the_job_and_the_operation() {
        let _serial = serial();
        let path = temp_state();
        let thread_path = path.clone();
        let panicked = std::thread::spawn(move || {
            let job = crate::JobGuard::acquire("transcribe").unwrap();
            let _operation = OperationGuard::register(
                None,
                thread_path,
                Some(job.token()),
                "transcribe",
                Some("record"),
            )
            .unwrap();
            // Poison the registry too: a later operation must still start.
            let _state = operation_state();
            panic!("job failed");
        })
        .join();
        assert!(panicked.is_err());
        assert!(OPERATION.is_poisoned());
        assert!(crate::job_holder().is_none());
        assert!(operation_state().is_none());
        assert_eq!(
            read_operation_at(&path).unwrap().unwrap().state,
            "interrupted"
        );
        let job = crate::JobGuard::acquire("transcribe").unwrap();
        let mut operation =
            OperationGuard::register(None, path.clone(), Some(job.token()), "transcribe", None)
                .unwrap();
        assert!(check_current().is_ok());
        operation.finish(&Ok::<_, String>(()));
        // Work outside an operation is never cancelled by someone else's.
        assert!(check_current().is_ok());
        drop(job);
        OPERATION.clear_poison();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn busy_answer_names_the_running_work_and_its_progress() {
        let _serial = serial();
        assert_eq!(busy_description(None, None, None)["kind"], "idle");
        let snapshot = OperationSnapshot {
            id: "job".into(),
            kind: "transcribe".into(),
            record_id: Some("record".into()),
            state: "running".into(),
            progress: Some(
                serde_json::json!({"stage":"progress.recognizedChunks","current":4,"total":9}),
            ),
            error: None,
            result: None,
            partial: None,
            resumable: None,
            acknowledged: false,
        };
        let busy = busy_description(
            Some(("transcribe", std::time::Duration::from_secs(75))),
            Some((snapshot, false)),
            None,
        );
        assert_eq!(busy["kind"], "transcribe");
        assert_eq!(busy["seconds"], 75);
        assert_eq!(busy["operation"]["recordId"], "record");
        assert_eq!(busy["operation"]["progress"]["current"], 4);
        assert_eq!(busy["stopping"], false);
        let transfer = busy_description(
            Some(("transfer", std::time::Duration::from_secs(3))),
            None,
            Some(serde_json::json!({"id":"t","direction":"receive"})),
        );
        assert_eq!(transfer["kind"], "transfer");
        assert_eq!(transfer["transfer"]["direction"], "receive");
        assert!(transfer["operation"].is_null());
        let recording =
            busy_description(Some(("recording", std::time::Duration::ZERO)), None, None);
        assert_eq!(recording["kind"], "recording");
    }
    #[test]
    fn stage_and_step_changes_persist_immediately_but_byte_deltas_are_throttled() {
        let chunk = |stage: &str, current: u64| serde_json::json!({"stage":stage,"current":current,"total":5,"elapsed":1});
        assert!(progress_milestone(None, &chunk("a", 0)));
        assert!(progress_milestone(Some(&chunk("a", 0)), &chunk("b", 0)));
        assert!(progress_milestone(Some(&chunk("a", 0)), &chunk("a", 1)));
        assert!(!progress_milestone(Some(&chunk("a", 1)), &chunk("a", 1)));
        let bytes =
            |current: u64| serde_json::json!({"stage":"a","current":current,"total":1_000_000});
        assert!(!progress_milestone(Some(&bytes(1)), &bytes(2)));
    }
}

#[cfg(test)]
mod transfer_snapshot_tests {
    use super::*;
    #[test]
    fn only_allocated_uuid_journals_can_be_loaded_and_running_work_requires_runtime_owner() {
        let id = uuid::Uuid::new_v4().to_string();
        let pointer = serde_json::json!({"id":id,"statePath":"/foreign/file.json"});
        assert_eq!(
            transfer_state_name(&pointer).unwrap(),
            format!("transfer-state-{id}")
        );
        assert!(transfer_state_name(&serde_json::json!({"id":"../../foreign"})).is_err());
        let running = serde_json::json!({"id":id,"direction":"receive","state":"running","stage":"receiving","current":5,"total":10});
        assert_eq!(
            restored_transfer(running.clone(), Some(&id))["state"],
            "running"
        );
        assert_eq!(restored_transfer(running, None)["state"], "interrupted");
        let terminal = serde_json::json!({"id":id,"state":"finished","result":{"id":"saved-record"},"recordId":"saved-record"});
        assert_eq!(restored_transfer(terminal.clone(), None), terminal);
    }
}
