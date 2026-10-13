//! LAN location is disposable; paired identity and credentials remain stable.
use super::{client, error, wire, Connection};
use mdns_sd::{IfKind, IfPredicate, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde_json::json;
use std::{
    collections::HashMap,
    net::Ipv4Addr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use tauri::Emitter;
const SERVICE: &str = "_hearfolio._tcp.local.";
struct Candidate {
    host: String,
    urls: Vec<String>,
    seen: Instant,
}
#[derive(Clone)]
struct Route {
    host: String,
    url: String,
    epoch: String,
    offset: i64,
}
static CANDIDATES: OnceLock<Mutex<HashMap<String, Candidate>>> = OnceLock::new();
static ROUTES: OnceLock<Mutex<HashMap<String, Route>>> = OnceLock::new();
static PROBE: Mutex<()> = Mutex::new(());
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static WAITING: AtomicBool = AtomicBool::new(false);
fn candidates() -> &'static Mutex<HashMap<String, Candidate>> {
    CANDIDATES.get_or_init(Default::default)
}
fn routes() -> &'static Mutex<HashMap<String, Route>> {
    ROUTES.get_or_init(Default::default)
}
fn key(c: &Connection) -> String {
    format!("{}:{}", c.id, wire::digest(c.token.as_bytes()))
}
pub fn lan_interface(name: &str) -> bool {
    ![
        "docker",
        "veth",
        "tun",
        "wg",
        "br-",
        "virbr",
        "vmnet",
        "tailscale",
        "utun",
        "rmnet",
        "ccmni",
        "pdp",
        "clat",
        "v4-",
        "wwan",
        "ppp",
    ]
    .iter()
    .any(|p| name.starts_with(p))
}
fn lan_ip(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local()
}
pub fn waiting(value: bool) {
    if WAITING.swap(value, Ordering::Relaxed) != value {
        crate::background::network_waiting(value);
        if let Some(app) = APP.get() {
            let _ = app.emit("transfer-network-state", json!({"waiting":value}));
        }
    }
}
pub fn is_waiting() -> bool {
    WAITING.load(Ordering::Relaxed)
}
pub fn attach(app: tauri::AppHandle) {
    let _ = APP.set(app);
}
fn daemon() -> Result<ServiceDaemon, String> {
    static DAEMON: OnceLock<Result<ServiceDaemon, String>> = OnceLock::new();
    DAEMON
        .get_or_init(|| {
            let daemon = ServiceDaemon::new().map_err(error)?;
            daemon.set_ip_check_interval(1).map_err(error)?;
            daemon
                .disable_interface(IfKind::Predicate(IfPredicate::new(|i| {
                    !lan_interface(&i.name)
                        || !matches!(i.ip(),std::net::IpAddr::V4(ip) if lan_ip(ip))
                })))
                .map_err(error)?;
            let rx = daemon.browse(SERVICE).map_err(error)?;
            std::thread::spawn(move || {
                while let Ok(event) = rx.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let Some(host) = info
                                .get_property_val_str("id")
                                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                            else {
                                continue;
                            };
                            let port = info.get_port();
                            if port == 0 {
                                continue;
                            }
                            let urls = info
                                .get_addresses_v4()
                                .into_iter()
                                .filter(|ip| lan_ip(*ip))
                                .take(16)
                                .map(|ip| format!("http://{ip}:{port}"))
                                .collect();
                            let mut entries = candidates().lock().unwrap();
                            entries.retain(|_, v| v.seen.elapsed() < Duration::from_secs(300));
                            if entries.len() < 512 || entries.contains_key(info.get_fullname()) {
                                entries.insert(
                                    info.get_fullname().into(),
                                    Candidate {
                                        host: host.into(),
                                        urls,
                                        seen: Instant::now(),
                                    },
                                );
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, name) => {
                            candidates().lock().unwrap().remove(&name);
                        }
                        _ => {}
                    }
                }
            });
            Ok(daemon)
        })
        .clone()
}
pub fn start() -> Result<(), String> {
    daemon().map(|_| ())
}
pub fn advertise(host: &str, port: u16) -> Result<(), String> {
    let daemon = daemon()?;
    let mut info = ServiceInfo::new(
        SERVICE,
        &format!("Hearfolio-{host}"),
        &format!("hearfolio-{host}.local."),
        "",
        port,
        [("id", host), ("protocol", "2")].as_slice(),
    )
    .map_err(error)?
    .enable_addr_auto();
    info.set_interfaces(vec![IfKind::Predicate(IfPredicate::new(|i| {
        lan_interface(&i.name) && matches!(i.ip(),std::net::IpAddr::V4(ip) if lan_ip(ip))
    }))]);
    daemon.register(info).map_err(error)
}
fn probe(c: &Connection, url: &str) -> Result<Route, String> {
    super::server_url(url)?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let res = client()?
        .post(format!("{url}/identity"))
        .timeout(Duration::from_secs(2))
        .json(&json!({"id":c.id,"nonce":nonce}))
        .send()
        .map_err(error)?;
    let value = super::response(res)?;
    let host = value["hostId"]
        .as_str()
        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
        .ok_or("Invalid LAN identity")?;
    if !c.host_id.is_empty() && c.host_id != host {
        return Err("Unexpected LAN device identity".into());
    }
    let epoch = value["epoch"]
        .as_str()
        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
        .ok_or("Invalid LAN session identity")?;
    let time = value["time"].as_u64().ok_or("Invalid LAN clock")?;
    if !wire::verify(
        &c.token,
        &wire::identity(host, &c.id, &nonce, epoch, time),
        value["proof"].as_str().unwrap_or(""),
    ) {
        return Err("Unverified LAN device identity".into());
    }
    Ok(Route {
        host: host.into(),
        url: url.into(),
        epoch: epoch.into(),
        offset: time as i64 - wire::now() as i64,
    })
}
/// Used once to upgrade an existing 0.8 pairing without asking for a new QR.
pub fn recover(c: &Connection) -> Result<(String, String), String> {
    let _ = start();
    let _probe = PROBE.lock().map_err(error)?;
    let cached = { routes().lock().map_err(error)?.get(&key(c)).cloned() };
    if let Some(route) = cached {
        if c.host_id.is_empty() || c.host_id == route.host {
            return Ok((route.host, route.url));
        }
        routes().lock().map_err(error)?.remove(&key(c));
    }
    let mut urls: Vec<String> = candidates()
        .lock()
        .map_err(error)?
        .values()
        .filter(|v| {
            v.seen.elapsed() < Duration::from_secs(300)
                && (c.host_id.is_empty() || v.host == c.host_id)
        })
        .flat_map(|v| v.urls.clone())
        .take(16)
        .collect();
    urls.push(c.server.clone());
    urls.sort();
    urls.dedup();
    // Try the existing hint first on a fresh pairing; every candidate needs proof.
    if let Some(i) = urls.iter().position(|s| s == &c.server) {
        let old = urls.remove(i);
        urls.insert(0, old);
    }
    for url in urls {
        crate::background::check_transfer_cancelled()?;
        if let Ok(route) = probe(c, &url) {
            let result = (route.host.clone(), route.url.clone());
            routes().lock().map_err(error)?.insert(key(c), route);
            waiting(false);
            return Ok(result);
        }
    }
    waiting(true);
    Err("Waiting for the paired device on the local network".into())
}
pub fn endpoint(c: &Connection) -> Result<String, String> {
    if c.local_host || c.host_id.is_empty() {
        return Ok(c.server.clone());
    }
    if let Some(route) = routes().lock().map_err(error)?.get(&key(c)).cloned() {
        if route.host == c.host_id {
            return Ok(route.url);
        }
    }
    recover(c).map(|(_, url)| url)
}
pub fn nonce(c: &Connection) -> Result<String, String> {
    if c.local_host {
        return Ok(format!(
            "{}:{}:{}",
            super::local::session_epoch(),
            wire::now(),
            uuid::Uuid::new_v4()
        ));
    }
    let route = routes()
        .lock()
        .map_err(error)?
        .get(&key(c))
        .cloned()
        .ok_or("LAN device not discovered")?;
    Ok(format!(
        "{}:{}:{}",
        route.epoch,
        (wire::now() as i64 + route.offset).max(0),
        uuid::Uuid::new_v4()
    ))
}
pub fn invalidate(c: &Connection) {
    routes().lock().unwrap().remove(&key(c));
    if !c.local_host && !c.host_id.is_empty() {
        waiting(true);
    }
}
#[cfg(test)]
pub(super) fn remember(host: &str, url: &str) {
    candidates().lock().unwrap().insert(
        format!("test:{host}"),
        Candidate {
            host: host.into(),
            urls: vec![url.into()],
            seen: Instant::now(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mdns_finds_same_identity_at_changed_address_and_port() {
        let reader = daemon().unwrap();
        reader.enable_interface(IfKind::LoopbackV4).unwrap();
        let publisher = ServiceDaemon::new().unwrap();
        publisher.enable_interface(IfKind::LoopbackV4).unwrap();
        let host = uuid::Uuid::new_v4().to_string();
        let service = |addr: &str, port: u16| {
            let mut info = ServiceInfo::new(
                SERVICE,
                &format!("test-{host}"),
                &format!("test-{host}.local."),
                addr,
                port,
                [("id", host.as_str())].as_slice(),
            )
            .unwrap();
            info.set_interfaces(vec![IfKind::LoopbackV4]);
            info
        };
        let first = service("127.0.0.1", 48001);
        let fullname = first.get_fullname().to_string();
        publisher.register(first).unwrap();
        for expected in ["http://127.0.0.1:48001", "http://127.0.0.2:48002"] {
            if expected.ends_with("48002") {
                publisher.register(service("127.0.0.2", 48002)).unwrap();
            }
            let started = Instant::now();
            loop {
                if candidates()
                    .lock()
                    .unwrap()
                    .values()
                    .any(|v| v.host == host && v.urls.iter().any(|s| s == expected))
                {
                    break;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(12),
                    "DNS-SD address was not resolved: {expected}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        publisher.unregister(&fullname).unwrap();
        publisher.shutdown().unwrap();
    }
}
