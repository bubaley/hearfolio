//! API tokens of organization connections. Never logged, never sent to the WebView.
//!
//! Desktop: the OS credential store through the `keyring` crate (macOS Keychain,
//! Windows Credential Manager, Secret Service on Linux). It needs no master
//! password or extra UI, unlike Stronghold, and the OS protects the secret with
//! the user's login. Linux without a Secret Service daemon falls back to an
//! owner-only file and the connection is marked as not secure.
//!
//! Android: an AES-GCM key generated inside Android Keystore (non-exportable)
//! encrypts the token; only the ciphertext is stored in app-private preferences.
use std::path::PathBuf;

#[cfg(all(desktop, not(test)))]
const SERVICE: &str = "com.bubaley.speechdesk.organizations";

#[cfg(target_os = "android")]
static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

pub fn install(_app: &tauri::AppHandle) {
    #[cfg(target_os = "android")]
    let _ = APP.set(_app.clone());
}

#[cfg_attr(test, allow(dead_code))]
fn fallback_path(id: &str) -> Result<PathBuf, String> {
    Ok(super::root()?.join("secrets").join(id))
}

#[cfg(test)]
static MEMORY: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
    std::sync::Mutex::new(None);

/// Store a token. Returns whether OS-backed secure storage holds it.
#[allow(clippy::needless_return)]
pub fn store(id: &str, token: &str) -> Result<bool, String> {
    super::valid_id(id)?;
    #[cfg(test)]
    {
        MEMORY
            .lock()
            .unwrap()
            .get_or_insert_with(Default::default)
            .insert(id.into(), token.into());
        return Ok(true);
    }
    #[cfg(all(not(test), target_os = "android"))]
    {
        use tauri::Manager;
        let app = APP.get().ok_or("errors.orgSecretStore")?;
        app.state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .transfer::<serde_json::Value>(
                "secretPut",
                serde_json::json!({"name": format!("org-{id}"), "value": token}),
            )
            .map_err(|_| "errors.orgSecretStore")?;
        return Ok(true);
    }
    #[cfg(all(not(test), desktop))]
    {
        match keyring::Entry::new(SERVICE, id).and_then(|entry| entry.set_password(token)) {
            Ok(()) => {
                let _ = std::fs::remove_file(fallback_path(id)?);
                Ok(true)
            }
            Err(_) => {
                super::write_private_bytes(&fallback_path(id)?, token.as_bytes())?;
                Ok(false)
            }
        }
    }
    #[cfg(all(not(test), not(desktop), not(target_os = "android")))]
    {
        let _ = token;
        Err("errors.orgSecretStore".into())
    }
}

#[allow(clippy::needless_return)]
pub fn load(id: &str) -> Result<String, String> {
    super::valid_id(id)?;
    #[cfg(test)]
    {
        return MEMORY
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|map| map.get(id).cloned())
            .ok_or_else(|| "errors.orgUnauthorized".into());
    }
    #[cfg(all(not(test), target_os = "android"))]
    {
        use tauri::Manager;
        let app = APP.get().ok_or("errors.orgSecretStore")?;
        let value = app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .transfer::<serde_json::Value>(
                "secretGet",
                serde_json::json!({"name": format!("org-{id}")}),
            )
            .map_err(|_| "errors.orgSecretStore")?;
        return value["value"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "errors.orgUnauthorized".into());
    }
    #[cfg(all(not(test), desktop))]
    {
        if let Ok(token) = keyring::Entry::new(SERVICE, id).and_then(|entry| entry.get_password()) {
            return Ok(token);
        }
        std::fs::read_to_string(fallback_path(id)?)
            .ok()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "errors.orgUnauthorized".into())
    }
    #[cfg(all(not(test), not(desktop), not(target_os = "android")))]
    Err("errors.orgSecretStore".into())
}

#[allow(clippy::needless_return)]
pub fn delete(id: &str) {
    if super::valid_id(id).is_err() {
        return;
    }
    #[cfg(test)]
    {
        if let Some(map) = MEMORY.lock().unwrap().as_mut() {
            map.remove(id);
        }
        return;
    }
    #[cfg(all(not(test), target_os = "android"))]
    {
        use tauri::Manager;
        if let Some(app) = APP.get() {
            let _ = app
                .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .transfer::<serde_json::Value>(
                    "secretDelete",
                    serde_json::json!({"name": format!("org-{id}")}),
                );
        }
    }
    #[cfg(all(not(test), desktop))]
    {
        if let Ok(entry) = keyring::Entry::new(SERVICE, id) {
            let _ = entry.delete_credential();
        }
    }
    #[cfg(not(test))]
    if let Ok(path) = fallback_path(id) {
        let _ = std::fs::remove_file(path);
    }
}
