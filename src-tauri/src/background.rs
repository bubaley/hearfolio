//! Keep a user-started operation alive while the Android activity is paused.
use tauri::AppHandle;
#[cfg(target_os = "android")]
use tauri::Manager;

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
            .invoke::<()>(command, payload)
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
        Ok(busy_description(crate::job_holder(), operation))
    })
    .await
    .map_err(|_| "errors.operationFailed".to_owned())?
}
fn busy_description(
    holder: Option<(&'static str, std::time::Duration)>,
    operation: Option<(OperationSnapshot, bool)>,
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
        assert_eq!(busy_description(None, None)["kind"], "idle");
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
        );
        assert_eq!(busy["kind"], "transcribe");
        assert_eq!(busy["seconds"], 75);
        assert_eq!(busy["operation"]["recordId"], "record");
        assert_eq!(busy["operation"]["progress"]["current"], 4);
        assert_eq!(busy["stopping"], false);
        let recording = busy_description(Some(("recording", std::time::Duration::ZERO)), None);
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
