//! Browser sign-in: the app opens `{server}/oauth/authorize` in the system
//! browser and receives `code`+`state` either through the `hearfolio://`
//! deep link or, on desktop builds where the scheme is not registered (dev
//! runs), a one-shot loopback listener on 127.0.0.1 (RFC 8252 §7.3).
use super::{api, pkce, server, Connection, Organization, UserBrief};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    time::Duration,
};

const FLOW_TTL_MS: u64 = 15 * 60 * 1000;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Flow {
    state: String,
    verifier: String,
    redirect_uri: String,
    server: String,
    discovery: server::Discovery,
    created_at: u64,
    /// Connection being re-authorized (replaced on success).
    #[serde(default)]
    replaces: Option<String>,
}

fn flows_path() -> Result<std::path::PathBuf, String> {
    Ok(super::root()?.join("flows.json"))
}
fn read_flows() -> Vec<Flow> {
    let now = super::now_ms();
    flows_path()
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<Flow>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|flow| now.saturating_sub(flow.created_at) < FLOW_TTL_MS)
        .collect()
}
fn write_flows(flows: &[Flow]) {
    if let Ok(path) = flows_path() {
        let _ = super::write_private_json(&path, &flows);
    }
}
fn take_flow(state: &str) -> Option<Flow> {
    let _lock = super::lock();
    let mut flows = read_flows();
    let index = flows.iter().position(|flow| flow.state == state)?;
    let flow = flows.remove(index);
    write_flows(&flows);
    Some(flow)
}
pub fn cancel(state: &str) {
    take_flow(state);
}

/// Deep links reach this build only when the OS knows the scheme belongs to it.
fn deep_link_available(_app: &tauri::AppHandle) -> bool {
    #[cfg(mobile)]
    return true;
    #[cfg(target_os = "macos")]
    return std::env::current_exe()
        .map(|path| path.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false);
    #[cfg(any(windows, target_os = "linux"))]
    {
        use tauri_plugin_deep_link::DeepLinkExt;
        _app.deep_link().is_registered("hearfolio").unwrap_or(false)
    }
}

fn device_name() -> String {
    #[cfg(target_os = "macos")]
    let name = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok());
    #[cfg(windows)]
    let name = std::env::var("COMPUTERNAME").ok();
    #[cfg(target_os = "linux")]
    let name = std::fs::read_to_string("/etc/hostname").ok();
    #[cfg(mobile)]
    let name: Option<String> = None;
    let fallback = if cfg!(target_os = "android") {
        "Android"
    } else if cfg!(target_os = "macos") {
        "Mac"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Linux"
    };
    let name = name
        .map(|name| {
            name.trim()
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fallback.to_owned());
    name.chars().take(64).collect()
}

/// Check the server and open the consent page. Returns `{state, redirect}`.
pub fn start(
    app: &tauri::AppHandle,
    input: &str,
    replaces: Option<String>,
) -> Result<Value, String> {
    let typed = server::normalize_server_url(input)?;
    let discovery = api::discovery(&typed)?;
    server::check_compatibility(&discovery, &app.package_info().version.to_string())?;
    let base = server::api_base(&typed, &discovery);
    let verifier = pkce::generate_verifier();
    let state = pkce::generate_state();
    let (redirect_uri, listener) = if deep_link_available(app) {
        (pkce::APP_REDIRECT_URI.to_owned(), None)
    } else {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|_| "errors.orgLoopback")?;
        let port = listener
            .local_addr()
            .map_err(|_| "errors.orgLoopback")?
            .port();
        (pkce::loopback_redirect(port), Some(listener))
    };
    let url = pkce::authorize_url(
        &discovery.oauth.authorize_url,
        &redirect_uri,
        &verifier,
        &state,
        &device_name(),
    )?;
    {
        let _lock = super::lock();
        let mut flows = read_flows();
        flows.push(Flow {
            state: state.clone(),
            verifier,
            redirect_uri: redirect_uri.clone(),
            server: base,
            discovery,
            created_at: super::now_ms(),
            replaces,
        });
        write_flows(&flows);
    }
    if let Some(listener) = listener {
        let app = app.clone();
        let expected = state.clone();
        std::thread::spawn(move || serve_loopback(app, listener, expected));
    }
    {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|_| "errors.orgBrowser")?;
    }
    Ok(
        json!({"state": state, "redirect": if redirect_uri.starts_with("hearfolio:") {"deepLink"} else {"loopback"}}),
    )
}

const PAGE: &str = "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>Hearfolio</title><body style='font:16px -apple-system,system-ui,sans-serif;display:grid;place-items:center;height:90vh;margin:0;text-align:center;color:#222'><div><h2>Hearfolio</h2><p>Готово — вернитесь в приложение.<br>Done — return to the app.</p></div>";

fn serve_loopback(app: tauri::AppHandle, listener: TcpListener, expected: String) {
    let _ = listener.set_nonblocking(true);
    let deadline = std::time::Instant::now() + Duration::from_millis(FLOW_TTL_MS);
    while std::time::Instant::now() < deadline {
        if !read_flows().iter().any(|flow| flow.state == expected) {
            return; // Cancelled or completed through another channel.
        }
        let Ok((mut stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        let _ = BufReader::new(&stream).take_line(&mut line);
        let target = line.split_whitespace().nth(1).unwrap_or("").to_owned();
        match pkce::parse_callback(&target).filter(|callback| loopback_accepts(callback, &expected))
        {
            Some(callback) => {
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}", PAGE.len()).as_bytes(),
                );
                drop(stream);
                handle(&app, callback);
                return;
            }
            // Anything else (a favicon, a stale tab, a foreign state) is refused
            // and the listener keeps waiting for this sign-in.
            None => {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        }
    }
    if let Some(flow) = take_flow(&expected) {
        super::emit(
            "org-connection",
            json!({"state":"failed","flow":flow.state,"error":"errors.orgAuthExpired"}),
        );
    }
}

/// The loopback answers only the callback of its own sign-in.
pub(crate) fn loopback_accepts(callback: &pkce::Callback, expected: &str) -> bool {
    match callback {
        pkce::Callback::Code { state, .. } => !expected.is_empty() && state == expected,
        pkce::Callback::Denied { state } => {
            state.as_deref() == Some(expected) && !expected.is_empty()
        }
    }
}

trait TakeLine {
    fn take_line(&mut self, line: &mut String) -> std::io::Result<usize>;
}
impl<R: std::io::Read> TakeLine for BufReader<R> {
    fn take_line(&mut self, line: &mut String) -> std::io::Result<usize> {
        // The request line only; a hostile client cannot make us buffer more.
        let mut limited = std::io::Read::take(self, 8192);
        BufReader::new(&mut limited).read_line(line)
    }
}

/// Deep link entry point (`hearfolio://oauth/callback?...`). Returns whether it was ours.
pub fn handle_url(app: &tauri::AppHandle, url: &str) -> bool {
    if !url.starts_with("hearfolio://oauth/") {
        return false;
    }
    if let Some(callback) = pkce::parse_callback(url) {
        let app = app.clone();
        std::thread::spawn(move || handle(&app, callback));
    }
    true
}

fn handle(app: &tauri::AppHandle, callback: pkce::Callback) {
    let (state, code) = match callback {
        pkce::Callback::Code { code, state } => (state, Some(code)),
        pkce::Callback::Denied { state } => (state.unwrap_or_default(), None),
    };
    let Some(flow) = take_flow(&state) else {
        // Unknown, expired or replayed state (a launch URL seen again): never
        // exchange a code we did not ask for.
        return;
    };
    let Some(code) = code else {
        super::emit(
            "org-connection",
            json!({"state":"cancelled","flow":flow.state}),
        );
        return;
    };
    super::emit(
        "org-connection",
        json!({"state":"exchanging","flow":flow.state}),
    );
    match finish(&flow, &code) {
        Ok(connection) => {
            super::emit(
                "org-connection",
                json!({"state":"connected","flow":flow.state,"connection":connection}),
            );
            super::worker::kick();
        }
        Err(error) => super::emit(
            "org-connection",
            json!({"state":"failed","flow":flow.state,"error":error}),
        ),
    }
    let _ = app;
}

pub fn organizations_from(value: &Value) -> Vec<Organization> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|org| {
            Some(Organization {
                id: org["id"].as_str()?.to_owned(),
                name: org["name"].as_str().unwrap_or_default().to_owned(),
                key: org["key"].as_str()?.to_owned(),
                role: org["role"].as_str().unwrap_or("member").to_owned(),
            })
        })
        .collect()
}

/// Which stored connections a successful sign-in replaces.
#[derive(Debug, PartialEq)]
pub(crate) struct Replacement {
    /// Connection id to store the new token under.
    pub id: String,
    /// Same account (same server and user): replaced in place.
    pub same: Vec<String>,
    /// The connection being re-authorized, but signed in as someone else: it
    /// is removed with everything this device kept for it.
    pub other_account: Vec<String>,
}

pub(crate) fn plan_replacement(
    connections: &[Connection],
    server: &str,
    user_id: &str,
    replaces: Option<&str>,
) -> Replacement {
    let same_account =
        |c: &Connection| !user_id.is_empty() && c.server == server && c.user.id == user_id;
    let same: Vec<String> = connections
        .iter()
        .filter(|c| same_account(c))
        .map(|c| c.id.clone())
        .collect();
    let other_account: Vec<String> = connections
        .iter()
        .filter(|c| Some(c.id.as_str()) == replaces && !same_account(c))
        .map(|c| c.id.clone())
        .collect();
    let id = same
        .first()
        .cloned()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    Replacement {
        id,
        same,
        other_account,
    }
}

fn finish(flow: &Flow, code: &str) -> Result<Connection, String> {
    let body = api::exchange_code(
        &flow.discovery.oauth.token_url,
        code,
        &flow.verifier,
        &flow.redirect_uri,
    )?;
    let token = body["access_token"].as_str().unwrap_or_default().to_owned();
    let user = &body["user"];
    let name = [user["first_name"].as_str(), user["last_name"].as_str()]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let user = UserBrief {
        id: user["id"].as_str().unwrap_or_default().to_owned(),
        email: user["email"].as_str().unwrap_or_default().to_owned(),
        name,
    };
    let mut organizations = organizations_from(&body["organizations"]);
    if organizations.is_empty() {
        // Older token responses: ask explicitly.
        let api = api::Api {
            base: &flow.server,
            token: &token,
            organization_key: None,
        };
        if let Ok(list) = api.get::<Value>("/me/organizations") {
            organizations = organizations_from(&list);
        }
    }
    // Decide under the lock, but never hold it over the keychain or the network.
    let (plan, previous) = {
        let _lock = super::lock();
        let connections = super::read_connections()?;
        let plan = plan_replacement(
            &connections,
            &flow.server,
            &user.id,
            flow.replaces.as_deref(),
        );
        let previous: Vec<Connection> = connections
            .into_iter()
            .filter(|c| plan.same.contains(&c.id) || plan.other_account.contains(&c.id))
            .collect();
        (plan, previous)
    };
    let old_tokens: Vec<(Connection, Option<String>)> = previous
        .into_iter()
        .map(|c| {
            let token = super::secrets::load(&c.id).ok();
            (c, token)
        })
        .collect();
    let secure = match super::secrets::store(&plan.id, &token) {
        Ok(secure) => secure,
        Err(error) => {
            // Nothing on this device can use it: do not leave a live token behind.
            let _ = api::revoke(&flow.discovery.oauth.revoke_url, &token);
            return Err(error);
        }
    };
    for (old, old_token) in &old_tokens {
        if let Some(old_token) = old_token.as_ref().filter(|t| **t != token) {
            let _ = api::revoke(&old.revoke_url, old_token);
        }
    }
    let connection = Connection {
        id: plan.id.clone(),
        server: flow.server.clone(),
        server_name: flow.discovery.name.clone(),
        server_version: flow.discovery.server_version.clone(),
        revoke_url: flow.discovery.oauth.revoke_url.clone(),
        user,
        organizations,
        created_at: super::now_ms(),
        secure,
        needs_reauth: false,
    };
    {
        let _lock = super::lock();
        let mut connections = super::read_connections()?;
        connections.retain(|c| !plan.same.contains(&c.id) && !plan.other_account.contains(&c.id));
        connections.push(connection.clone());
        super::write_connections(&connections)?;
        let mut settings = super::read_settings()?;
        if settings
            .destination
            .as_ref()
            .is_some_and(|d| plan.other_account.contains(&d.connection_id))
        {
            settings.destination = None;
            super::write_settings(&settings)?;
        }
    }
    // Another account signed in: the previous user's mirror, cached audio and
    // queued uploads must never reach the new one.
    for old in &plan.other_account {
        if *old != plan.id {
            super::secrets::delete(old);
        }
        super::commands::forget_organizations(old, &[]);
        super::worker::fail_connection(old);
    }
    super::worker::retry_connection(&plan.id);
    Ok(connection)
}

pub fn install(app: &tauri::AppHandle) {
    use tauri_plugin_deep_link::DeepLinkExt;
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            handle_url(&handle, url.as_str());
        }
    });
    // Android may have recreated the app while the browser was in front.
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for url in urls {
            handle_url(app, url.as_str());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::Organization;

    fn connection(id: &str, user: &str) -> Connection {
        Connection {
            id: id.into(),
            server: "https://cloud.example.com".into(),
            server_name: "X".into(),
            server_version: String::new(),
            revoke_url: String::new(),
            user: UserBrief {
                id: user.into(),
                email: String::new(),
                name: String::new(),
            },
            organizations: vec![Organization {
                id: "o".into(),
                name: "O".into(),
                key: "k".into(),
                role: "member".into(),
            }],
            created_at: 0,
            secure: true,
            needs_reauth: true,
        }
    }

    #[test]
    fn signing_in_again_as_someone_else_never_reuses_the_old_connection() {
        let stored = [connection("c-anna", "anna"), connection("c-boris", "boris")];
        let server = "https://cloud.example.com";
        // Same account re-authorizes: the connection keeps its id.
        let same = plan_replacement(&stored, server, "anna", Some("c-anna"));
        assert_eq!(same.id, "c-anna");
        assert_eq!(same.same, vec!["c-anna".to_string()]);
        assert!(same.other_account.is_empty());
        // Anna's expired connection is re-authorized as a new user: new id, Anna's removed.
        let other = plan_replacement(&stored, server, "vera", Some("c-anna"));
        assert_ne!(other.id, "c-anna");
        assert!(other.same.is_empty());
        assert_eq!(other.other_account, vec!["c-anna".to_string()]);
        // …as Boris, who is already connected: Boris keeps his connection.
        let boris = plan_replacement(&stored, server, "boris", Some("c-anna"));
        assert_eq!(boris.id, "c-boris");
        assert_eq!(boris.other_account, vec!["c-anna".to_string()]);
        // A missing user id never matches anyone.
        assert!(plan_replacement(&stored, server, "", None).same.is_empty());
        // Another server is another account.
        assert!(
            plan_replacement(&stored, "https://other.example.com", "anna", None)
                .same
                .is_empty()
        );
    }

    #[test]
    fn the_loopback_listener_ignores_foreign_callbacks() {
        let code = |state: &str| pkce::Callback::Code {
            code: "c".into(),
            state: state.into(),
        };
        assert!(loopback_accepts(&code("s1"), "s1"));
        assert!(!loopback_accepts(&code("other"), "s1"));
        assert!(!loopback_accepts(&code(""), ""));
        assert!(loopback_accepts(
            &pkce::Callback::Denied {
                state: Some("s1".into())
            },
            "s1"
        ));
        assert!(!loopback_accepts(
            &pkce::Callback::Denied { state: None },
            "s1"
        ));
    }
}
