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
            } else {
                _app.state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                    .transfer::<()>("beginTransfer", payload)
                    .map_err(|_| "errors.transferBackgroundUnavailable")?;
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
    .map_err(|e| e.to_string())?
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
    app: AppHandle,
    started: std::time::Instant,
    persisted: std::time::Instant,
    last_activity: std::time::Instant,
    dirty: bool,
    cancelled: bool,
}
static OPERATION: std::sync::Mutex<Option<ActiveOperation>> = std::sync::Mutex::new(None);
// The watchdog is re-armed by every progress report. The absolute cap only
// bounds a job that keeps reporting forever.
const STALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const MAX_OPERATION: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);
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
fn persist_operation(state: &OperationSnapshot) -> Result<(), String> {
    persist_operation_at(&operation_path()?, state)
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
            snapshot.resumable = crate::resume::resumable(&root, record);
        }
    }
    snapshot
}
// The bridge call waits for OperationService readiness. It runs without the
// operation lock, and a lost callback cannot block snapshot or cancel calls.
#[cfg(target_os = "android")]
fn begin_native(app: &AppHandle, payload: serde_json::Value) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let app = app.clone();
    std::thread::spawn(move || {
        let result = app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .transfer::<()>("beginOperation", payload)
            .is_ok();
        let _ = sender.send(result);
    });
    // Kotlin answers within ~10 s of dispatch; the extra time covers a
    // notification permission prompt the user is still reading.
    match receiver.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok(true) => Ok(()),
        _ => Err("errors.operationBackgroundUnavailable".into()),
    }
}
pub(crate) struct OperationGuard {
    id: String,
    completed: bool,
}
impl OperationGuard {
    pub fn start(
        app: &AppHandle,
        kind: &str,
        record_id: Option<&str>,
        _network: bool,
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
        {
            // Reserve and commit ownership under the lock; native startup below
            // happens without it.
            let mut state = OPERATION.lock().map_err(|_| "errors.operationFailed")?;
            if state.is_some() {
                return Err("errors.operationBusy".into());
            }
            persist_operation(&snapshot)?;
            let now = std::time::Instant::now();
            *state = Some(ActiveOperation {
                snapshot,
                app: app.clone(),
                started: now,
                persisted: now,
                last_activity: now,
                dirty: false,
                cancelled: false,
            });
        }
        #[allow(unused_mut)]
        let mut guard = Self {
            id: id.clone(),
            completed: false,
        };
        #[cfg(target_os = "android")]
        {
            let payload = serde_json::json!({"id":id,"kind":kind,"network":_network,"snapshotPath":operation_path()?,"controlPath":operation_control_path(&id)?});
            if let Err(error) = begin_native(app, payload) {
                guard.finish::<()>(&Err(error.clone()));
                return Err(error);
            }
        }
        Ok(guard)
    }
    pub fn finish<T: serde::Serialize>(&mut self, result: &Result<T, String>) {
        use tauri::Emitter;
        let Ok(mut state) = OPERATION.lock() else {
            return;
        };
        let Some(mut active) = state.take() else {
            return;
        };
        if active.snapshot.id != self.id {
            *state = Some(active);
            return;
        }
        active.snapshot.state = match result {
            Ok(_) => "finished",
            Err(error) if error == "errors.operationCancelled" => "cancelled",
            Err(error) if error == "errors.operationInterrupted" => "interrupted",
            _ => "error",
        }
        .into();
        active.snapshot.error = result.as_ref().err().cloned();
        active.snapshot.result = result
            .as_ref()
            .ok()
            .and_then(|value| serde_json::to_value(value).ok());
        if result.is_ok() {
            active.snapshot.partial = None;
        }
        if persist_operation(&active.snapshot).is_err() {
            active.snapshot.error = Some("errors.operationFailed".into());
        }
        drop(state);
        let snapshot = with_resume(active.snapshot);
        let _ = active.app.emit("operation-native-finished", &snapshot);
        self.completed = true;
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.finish::<()>(&Err("errors.operationInterrupted".into()));
        }
    }
}
pub(crate) fn check_current() -> Result<(), String> {
    let mut state = OPERATION.lock().map_err(|_| "errors.operationFailed")?;
    let Some(active) = state.as_mut() else {
        return Ok(());
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
        active.snapshot.error = Some("errors.operationTimeout".into());
        return Err("errors.operationTimeout".into());
    }
    // Trailing flush for throttled progress, so the last state before a quiet
    // period (one long request) is what a recreated UI or the service sees.
    if active.dirty && active.persisted.elapsed() >= std::time::Duration::from_secs(1) {
        let _ = persist_operation(&active.snapshot);
        active.persisted = std::time::Instant::now();
        active.dirty = false;
    }
    #[cfg(target_os = "android")]
    if let Ok(bytes) = std::fs::read(operation_control_path(&active.snapshot.id)?) {
        if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if status["id"] == active.snapshot.id && status["cancelled"] == true {
                active.cancelled = true;
                active.snapshot.error = Some(
                    status["error"]
                        .as_str()
                        .unwrap_or("errors.operationInterrupted")
                        .into(),
                );
                return Err(active.snapshot.error.clone().unwrap());
            }
        }
    }
    Ok(())
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
    let Ok(mut state) = OPERATION.lock() else {
        return;
    };
    let Some(active) = state.as_mut() else { return };
    if active.snapshot.kind != kind {
        return;
    }
    let milestone = progress_milestone(active.snapshot.progress.as_ref(), &progress);
    active.snapshot.progress = Some(progress);
    active.last_activity = std::time::Instant::now();
    active.dirty = true;
    if milestone || active.persisted.elapsed() >= std::time::Duration::from_secs(2) {
        let _ = persist_operation(&active.snapshot);
        active.persisted = std::time::Instant::now();
        active.dirty = false;
    }
}
/// Records live text for snapshot readers and returns `(operationId, recordId)`.
pub(crate) fn operation_partial(kind: &str, text: &str) -> Option<(String, Option<String>)> {
    let mut state = OPERATION.lock().ok()?;
    let active = state.as_mut()?;
    if active.snapshot.kind != kind {
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
        if let Some(active) = OPERATION
            .lock()
            .map_err(|_| "errors.operationFailed")?
            .as_ref()
        {
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
#[tauri::command]
pub async fn cancel_background_operation(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut state = OPERATION.lock().map_err(|_| "errors.operationFailed")?;
        let Some(active) = state.as_mut() else {
            return Ok(());
        };
        if active.snapshot.id != id {
            return Ok(());
        }
        active.cancelled = true;
        active.snapshot.error = Some("errors.operationCancelled".into());
        persist_operation(&active.snapshot)?;
        let _ = app;
        Ok(())
    })
    .await
    .map_err(|_| "errors.operationFailed".to_owned())?
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
        let state = OPERATION.lock().map_err(|_| "errors.operationFailed")?;
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
