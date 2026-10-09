//! Keep a user-started transfer alive while the Android activity is paused.
use tauri::AppHandle;
#[cfg(target_os = "android")]
use tauri::Manager;

pub(crate) struct TransferGuard {
    #[cfg(target_os = "android")]
    app: AppHandle,
    #[cfg(target_os = "android")]
    id: String,
    #[cfg(target_os = "android")]
    last_check: std::time::Instant,
    #[cfg(target_os = "android")]
    last_update: std::time::Instant,
    success: bool,
}
impl TransferGuard {
    pub fn start(_app: &AppHandle, _direction: &str) -> Result<Self, String> {
        #[cfg(target_os = "android")]
        {
            let id = uuid::Uuid::new_v4().to_string();
            _app.state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .transfer::<()>(
                    "beginTransfer",
                    serde_json::json!({"id":id,"direction":_direction}),
                )
                .map_err(|_| "errors.transferBackgroundUnavailable".to_owned())?;
            Ok(Self {
                app: _app.clone(),
                id,
                last_check: std::time::Instant::now(),
                last_update: std::time::Instant::now(),
                success: false,
            })
        }
        #[cfg(not(target_os = "android"))]
        Ok(Self { success: false })
    }
    pub fn check(&mut self) -> Result<(), String> {
        #[cfg(target_os = "android")]
        if self.last_check.elapsed() >= std::time::Duration::from_millis(250) {
            let status: serde_json::Value = self
                .app
                .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .transfer("checkTransfer", serde_json::json!({"id":self.id}))?;
            self.last_check = std::time::Instant::now();
            if status["cancelled"] == true {
                return Err("errors.transferCancelled".into());
            }
        }
        Ok(())
    }
    pub fn progress(&mut self, _stage: &str, _current: u64, _total: u64) {
        #[cfg(target_os = "android")]
        if self.last_update.elapsed() >= std::time::Duration::from_secs(1) {
            let _ = self.app.state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .transfer::<()>("updateTransfer", serde_json::json!({"id":self.id,"stage":_stage,"current":_current,"total":_total}));
            self.last_update = std::time::Instant::now();
        }
    }
    pub fn complete(&mut self) {
        self.success = true;
    }
}
impl Drop for TransferGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "android")]
        let _ = self
            .app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .transfer::<()>(
                "finishTransfer",
                serde_json::json!({"id":self.id,"success":self.success}),
            );
    }
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
                    bridge.transfer::<()>("startInbox", serde_json::json!({}))?;
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
            Ok(serde_json::json!({"enabled":status["active"] == true}))
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
        let bridge = app.state::<tauri_plugin_recording::Recording<tauri::Wry>>();
        let p = match INBOX.lock() {
            Ok(p) => p.clone(),
            Err(_) => break,
        };
        let active = bridge
            .transfer::<serde_json::Value>("inboxStatus", serde_json::json!({}))
            .ok()
            .is_some_and(|v| v["active"] == true);
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
            let active = bridge
                .transfer::<serde_json::Value>("inboxStatus", serde_json::json!({}))
                .ok()
                .is_some_and(|v| v["active"] == true);
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
            let _ = bridge.transfer::<()>(
                "inboxOffers",
                serde_json::json!({"id":"inbox","current":ids.len(),"success":fresh}),
            );
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
                    attempted.insert(id);
                    let _ = app.emit(
                        "background-transfer-result",
                        serde_json::json!({"ok":result.is_ok(),"error":result.err()}),
                    );
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
