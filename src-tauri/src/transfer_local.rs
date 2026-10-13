//! On-device LAN endpoint. No external service and no audio stored by the endpoint.
use super::{error, parts, Manifest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    net::Ipv4Addr,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tiny_http::{Header, Method, Request, Response, Server};
const PORT: u16 = 47831;
fn default_port() -> u16 {
    PORT
}
const TTL: Duration = Duration::from_secs(600);
#[derive(Clone, Serialize, Deserialize)]
struct Device {
    id: String,
    token: String,
    name: String,
    peers: HashSet<String>,
}
#[derive(Serialize, Deserialize)]
struct Saved {
    #[serde(default = "default_port")]
    port: u16,
    host: String,
    devices: Vec<Device>,
}
struct Transfer {
    request_id: String,
    request_digest: String,
    from: String,
    to: String,
    manifest: Value,
    hashes: Vec<String>,
    acked: HashSet<usize>,
    buffers: HashMap<usize, Vec<u8>>,
    status: &'static str,
    audio: bool,
    updated: Instant,
}
struct Join {
    to: String,
    name: String,
    secret: String,
    device: Option<Device>,
    rejected: bool,
    existing: Option<String>,
    updated: Instant,
}
struct State {
    epoch: String,
    saved: Saved,
    path: PathBuf,
    online: HashMap<String, Instant>,
    codes: HashMap<String, (String, Instant)>,
    joins: HashMap<String, Join>,
    transfers: HashMap<String, Transfer>,
    attempts: HashMap<String, (u32, Instant)>,
    requests: HashMap<String, usize>,
    nonces: HashMap<(String, String), Instant>,
}
type Reply = Result<(u16, Vec<u8>, &'static str), (u16, String)>;
fn fail(message: &str, status: u16) -> (u16, String) {
    (status, message.into())
}
fn reply(value: Value) -> Reply {
    Ok((200, serde_json::to_vec(&value).unwrap(), "application/json"))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn name(value: &Value) -> Result<String, (u16, String)> {
    let n = value.as_str().unwrap_or("").trim();
    if n.is_empty() || n.len() > 200 || n.chars().any(char::is_control) {
        return Err(fail("Invalid device name", 400));
    }
    Ok(n.into())
}
fn private(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local()
}
pub fn address() -> Result<String, String> {
    let mut addresses: Vec<_> = if_addrs::get_if_addrs()
        .map_err(error)?
        .into_iter()
        .filter_map(|a| match a.ip() {
            std::net::IpAddr::V4(ip)
                if ip.is_private() && super::discovery::lan_interface(&a.name) =>
            {
                Some((!a.name.starts_with('w'), ip))
            }
            _ => None,
        })
        .collect();
    addresses.sort_by_key(|(wired, ip)| (*wired, *ip));
    Ok(format!(
        "http://{}:{}",
        addresses
            .first()
            .map(|x| x.1)
            .ok_or("Connect to Wi-Fi or a local network first")?,
        start()?.2
    ))
}
impl State {
    fn persist(&self) -> Result<(), (u16, String)> {
        let temp = self.path.with_extension("tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&temp).map_err(|e| fail(&error(e), 500))?;
        use std::io::Write;
        f.write_all(&serde_json::to_vec(&self.saved).map_err(|e| fail(&error(e), 500))?)
            .map_err(|e| fail(&error(e), 500))?;
        f.sync_all().map_err(|e| fail(&error(e), 500))?;
        fs::rename(temp, &self.path).map_err(|e| fail(&error(e), 500))
    }
    fn prune(&mut self) {
        self.transfers.retain(|_, t| t.updated.elapsed() < TTL);
        self.nonces
            .retain(|_, at| at.elapsed() < Duration::from_secs(120));
        self.codes
            .retain(|_, (_, at)| at.elapsed() < Duration::from_secs(180));
        self.joins
            .retain(|_, j| j.updated.elapsed() < Duration::from_secs(180));
        self.attempts
            .retain(|_, (_, at)| at.elapsed() < Duration::from_secs(60));
    }
    fn handle(
        &mut self,
        method: &Method,
        url: &str,
        bearer: &str,
        hash: &str,
        body: Vec<u8>,
    ) -> Reply {
        self.prune();
        if method == &Method::Get && url == "/health" {
            return reply(json!({"ok":true,"protocol":1,"multipart":1,"local":true}));
        }
        let v: Value = if method == &Method::Post {
            serde_json::from_slice(&body).map_err(|_| fail("Invalid request", 400))?
        } else {
            Value::Null
        };
        if url == "/identity" && method == &Method::Post {
            let nonce = v["nonce"]
                .as_str()
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .ok_or(fail("Invalid identity challenge", 400))?;
            let d = self
                .saved
                .devices
                .iter()
                .find(|d| Some(d.id.as_str()) == v["id"].as_str())
                .ok_or(fail("Unknown device", 404))?;
            let host = &self.saved.host;
            let time = super::wire::now();
            return reply(
                json!({"hostId":host,"epoch":self.epoch,"time":time,"proof":super::wire::sign(&d.token,&super::wire::identity(host,&d.id,nonce,&self.epoch,time))}),
            );
        }
        if url == "/join" && method == &Method::Post {
            if self.joins.len() >= 20 || self.saved.devices.len() >= 100 {
                return Err(fail("Device limit reached", 429));
            }
            let attempt = self
                .attempts
                .entry("join".into())
                .or_insert((0, Instant::now()));
            attempt.0 += 1;
            if attempt.0 > 30 {
                return Err(fail("Too many invitations; wait one minute", 429));
            }
            let n = name(&v["name"])?;
            let (to, _) = self
                .codes
                .remove(v["code"].as_str().unwrap_or(""))
                .ok_or(fail("Invalid or expired invitation", 400))?;
            let key = id();
            let secret = token();
            self.joins.insert(
                key.clone(),
                Join {
                    to,
                    name: n,
                    secret: secret.clone(),
                    device: None,
                    rejected: false,
                    existing: None,
                    updated: Instant::now(),
                },
            );
            return reply(json!({"id":key,"secret":secret}));
        }
        if url == "/join/status" && method == &Method::Post {
            let j = self
                .joins
                .get(v["id"].as_str().unwrap_or(""))
                .filter(|j| Some(j.secret.as_str()) == v["secret"].as_str())
                .ok_or(fail("Invitation expired or invalid", 404))?;
            return reply(
                json!({"status":if j.device.is_some(){"approved"}else if j.rejected{"rejected"}else{"waiting"},"device":j.device.as_ref().map(|d|json!({"id":d.id,"token":d.token,"hostId":self.saved.host}))}),
            );
        }
        let d = self
            .saved
            .devices
            .iter()
            .find(|d| d.token == bearer)
            .cloned()
            .ok_or(fail("Unauthorized", 401))?;
        self.online.insert(d.id.clone(), Instant::now());
        if method == &Method::Post && url == "/rpc" {
            let p = &v["payload"];
            let action = v["action"].as_str().unwrap_or("");
            match action {
                "poll" => {
                    return reply(
                        json!({"device":{"id":d.id,"name":d.name},"peers":self.saved.devices.iter().filter(|p|d.peers.contains(&p.id)).map(|p|json!({"id":p.id,"name":p.name,"online":self.online.get(&p.id).is_some_and(|at|at.elapsed()<Duration::from_secs(15))})).collect::<Vec<_>>(),"pairRequests":self.joins.iter().filter(|(_,j)|j.to==d.id&&j.device.is_none()&&!j.rejected).map(|(id,j)|json!({"id":id,"name":j.name})).collect::<Vec<_>>(),"offers":self.transfers.iter().filter(|(_,t)|t.to==d.id&&t.status=="offered").map(|(id,t)|json!({"id":id,"name":t.manifest["name"],"size":t.manifest["size"],"from":t.from})).collect::<Vec<_>>(),"transfers":self.transfers.iter().filter(|(_,t)|t.from==d.id||t.to==d.id).map(|(id,t)|json!({"id":id,"status":t.status,"offset":t.offset(),"size":t.manifest["size"]})).collect::<Vec<_>>()}),
                    )
                }
                "rename" => {
                    let n = name(&p["name"])?;
                    self.saved
                        .devices
                        .iter_mut()
                        .find(|x| x.id == d.id)
                        .unwrap()
                        .name = n;
                    self.persist()?;
                    return reply(json!({"ok":true}));
                }
                "code" => {
                    self.codes.retain(|_, (to, _)| to != &d.id);
                    let raw = uuid::Uuid::new_v4().as_u128();
                    let code = format!("{:08}", 10000000 + raw % 90000000);
                    self.codes.insert(code.clone(), (d.id, Instant::now()));
                    return reply(json!({"code":code,"expiresAt":now()+180000}));
                }
                "pair" => {
                    let a = self
                        .attempts
                        .entry(d.id.clone())
                        .or_insert((0, Instant::now()));
                    a.0 += 1;
                    if a.0 > 10 {
                        return Err(fail("Too many attempts; wait one minute", 429));
                    }
                    let (to, _) = self
                        .codes
                        .get(p["code"].as_str().unwrap_or(""))
                        .filter(|(to, _)| to != &d.id)
                        .cloned()
                        .ok_or(fail("Invalid or expired code", 400))?;
                    self.codes.remove(p["code"].as_str().unwrap());
                    self.joins.insert(
                        id(),
                        Join {
                            to,
                            name: d.name,
                            secret: token(),
                            device: None,
                            rejected: false,
                            existing: Some(d.id),
                            updated: Instant::now(),
                        },
                    );
                    return reply(json!({"ok":true}));
                }
                "confirmPair" => {
                    let j = self
                        .joins
                        .get_mut(p["id"].as_str().unwrap_or(""))
                        .filter(|j| j.to == d.id && j.device.is_none() && !j.rejected)
                        .ok_or(fail("Pair request expired", 400))?;
                    if p["accept"] == true {
                        let mut other = if let Some(existing) = &j.existing {
                            self.saved
                                .devices
                                .iter()
                                .find(|x| &x.id == existing)
                                .cloned()
                                .ok_or(fail("Unknown device", 404))?
                        } else {
                            Device {
                                id: id(),
                                token: token(),
                                name: j.name.clone(),
                                peers: HashSet::new(),
                            }
                        };
                        other.peers.insert(d.id.clone());
                        j.device = Some(other.clone());
                        self.saved
                            .devices
                            .iter_mut()
                            .find(|x| x.id == d.id)
                            .unwrap()
                            .peers
                            .insert(other.id.clone());
                        self.online.insert(other.id.clone(), Instant::now());
                        if let Some(existing) =
                            self.saved.devices.iter_mut().find(|x| x.id == other.id)
                        {
                            *existing = other;
                        } else {
                            self.saved.devices.push(other);
                        }
                        self.persist()?;
                    } else {
                        j.rejected = true;
                    }
                    return reply(json!({"ok":true}));
                }
                "unpair" => {
                    let other = p["id"].as_str().unwrap_or("");
                    for x in &mut self.saved.devices {
                        if x.id == d.id {
                            x.peers.remove(other);
                        }
                        if x.id == other {
                            x.peers.remove(&d.id);
                        }
                    }
                    self.transfers.retain(|_, t| {
                        !([t.from.as_str(), t.to.as_str()].contains(&d.id.as_str())
                            && [t.from.as_str(), t.to.as_str()].contains(&other))
                    });
                    self.persist()?;
                    return reply(json!({"ok":true}));
                }
                "offer" => {
                    let to = p["to"].as_str().unwrap_or("");
                    if !d.peers.contains(to) {
                        return Err(fail("Device is not paired", 403));
                    }
                    // Endpoint is on one of the two devices; never relay through a third device.
                    if d.id != self.saved.host && to != self.saved.host {
                        return Err(fail("Pair directly with the receiving device", 400));
                    }
                    let request_id = p["requestId"].as_str().unwrap_or("");
                    if !request_id.is_empty() && uuid::Uuid::parse_str(request_id).is_err() {
                        return Err(fail("Invalid offer request ID", 400));
                    }
                    let request_digest=super::wire::digest(&serde_json::to_vec(&json!({"to":to,"manifest":p["manifest"],"parts":p["parts"],"metadataOnly":p["metadataOnly"]})).unwrap());
                    if !request_id.is_empty() {
                        if let Some((id, t)) = self.transfers.iter().find(|(_, t)| {
                            t.from == d.id && t.to == to && t.request_id == request_id
                        }) {
                            if t.request_digest != request_digest {
                                return Err(fail(
                                    "Request ID already used for another transfer",
                                    409,
                                ));
                            }
                            return reply(json!({"id":id}));
                        }
                    }
                    if !self
                        .online
                        .get(to)
                        .is_some_and(|at| at.elapsed() < Duration::from_secs(15))
                    {
                        return Err(fail("Recipient is offline", 409));
                    }
                    if self
                        .transfers
                        .values()
                        .filter(|t| !["delivered", "cancelled"].contains(&t.status))
                        .count()
                        >= 2
                    {
                        return Err(fail("Device transfer limit reached", 429));
                    }
                    if self
                        .transfers
                        .values()
                        .any(|t| t.to == to && !["delivered", "cancelled"].contains(&t.status))
                    {
                        return Err(fail("Recipient has another transfer", 409));
                    }
                    if self.transfers.len() >= 128 {
                        self.transfers
                            .retain(|_, t| !["delivered", "cancelled"].contains(&t.status));
                    }
                    let m: Manifest = serde_json::from_value(p["manifest"].clone())
                        .map_err(|_| fail("Invalid recording manifest", 400))?;
                    super::validate_manifest(&m).map_err(|e| fail(&e, 400))?;
                    let description: parts::Description =
                        serde_json::from_value(p["parts"].clone())
                            .map_err(|_| fail("Multipart client required", 400))?;
                    description.validate(m.size).map_err(|e| fail(&e, 400))?;
                    let key = id();
                    self.transfers.insert(
                        key.clone(),
                        Transfer {
                            request_id: request_id.into(),
                            request_digest,
                            from: d.id,
                            to: to.into(),
                            manifest: p["manifest"].clone(),
                            hashes: description.hashes,
                            acked: HashSet::new(),
                            buffers: HashMap::new(),
                            status: "offered",
                            audio: true,
                            updated: Instant::now(),
                        },
                    );
                    return reply(json!({"id":key}));
                }
                _ => {}
            }
            let t = self
                .transfers
                .get_mut(p["id"].as_str().unwrap_or(""))
                .filter(|t| t.from == d.id || t.to == d.id)
                .ok_or(fail("Unknown transfer", 404))?;
            t.updated = Instant::now();
            match action {
                "status" => reply(
                    json!({"status":t.status,"offset":t.offset(),"size":t.manifest["size"],"audioRequired":t.audio,"parts":t.description(true),"peerOnline":self.online.get(if t.from==d.id{&t.to}else{&t.from}).is_some_and(|at|at.elapsed()<Duration::from_secs(15))}),
                ),
                "inspect" if t.to == d.id && t.status == "offered" => reply(
                    json!({"manifest":t.manifest,"from":t.from,"metadataOnlyCapable":true,"parts":t.description(false)}),
                ),
                "accept" if t.to == d.id && ["offered", "streaming"].contains(&t.status) => {
                    if t.status == "streaming" {
                        return reply(
                            json!({"manifest":t.manifest,"from":t.from,"audioRequired":t.audio,"parts":t.description(false)}),
                        );
                    }
                    if p["audioRequired"] == false {
                        if p["sha256"] != t.manifest["sha256"] || p["size"] != t.manifest["size"] {
                            return Err(fail("Invalid existing audio confirmation", 400));
                        }
                        t.audio = false;
                    } else {
                        let verified = p["parts"]["verified"]
                            .as_array()
                            .ok_or(fail("Invalid resume state", 400))?;
                        let mut acked = HashSet::new();
                        for i in verified {
                            let i = i
                                .as_u64()
                                .filter(|i| (*i as usize) < t.hashes.len())
                                .ok_or(fail("Invalid part index", 400))?
                                as usize;
                            if !acked.insert(i) {
                                return Err(fail("Duplicate verified part", 400));
                            }
                        }
                        t.acked = acked;
                    }
                    t.status = "streaming";
                    reply(
                        json!({"manifest":t.manifest,"from":t.from,"audioRequired":t.audio,"parts":t.description(false)}),
                    )
                }
                "ackPart" if t.to == d.id && ["streaming", "delivered"].contains(&t.status) => {
                    let i = p["index"].as_u64().unwrap_or(u64::MAX) as usize;
                    if t.hashes.get(i).map(String::as_str) != p["sha256"].as_str() {
                        return Err(fail("Part checksum mismatch", 422));
                    }
                    if !t.acked.contains(&i) && !t.buffers.contains_key(&i) {
                        return Err(fail("Part not available", 409));
                    }
                    t.acked.insert(i);
                    t.buffers.remove(&i);
                    reply(json!({"ok":true,"offset":t.offset()}))
                }
                "finish"
                    if t.to == d.id
                        && ["streaming", "delivered"].contains(&t.status)
                        && t.offset() == t.size() =>
                {
                    t.status = "delivered";
                    t.manifest["transcript"] = json!("");
                    t.buffers.clear();
                    reply(json!({"ok":true}))
                }
                "cancel" => {
                    if t.status != "delivered" {
                        t.status = "cancelled";
                        t.buffers.clear();
                    }
                    reply(json!({"ok":true}))
                }
                _ => Err(fail("Invalid action", 400)),
            }
        } else {
            let path: Vec<_> = url.split('/').collect();
            if path.len() != 5 || path[1] != "transfers" || path[3] != "parts" {
                return Err(fail("Not found", 404));
            }
            let slots: usize = self.transfers.values().map(|t| t.buffers.len()).sum();
            let t = self
                .transfers
                .get_mut(path[2])
                .filter(|t| t.from == d.id || t.to == d.id)
                .ok_or(fail("Unknown transfer", 404))?;
            t.updated = Instant::now();
            let i = path[4]
                .parse::<usize>()
                .ok()
                .filter(|i| *i < t.hashes.len())
                .ok_or(fail("Invalid part index", 400))?;
            if method == &Method::Put
                && d.id == t.from
                && ["streaming", "delivered"].contains(&t.status)
                && hash == t.hashes[i]
                && t.acked.contains(&i)
            {
                return reply(json!({"index":i,"offset":t.offset()}));
            }
            if t.status != "streaming" || !t.audio {
                return Err(fail("Multipart transfer is not active", 409));
            }
            if method == &Method::Put && d.id == t.from {
                if hash != t.hashes[i]
                    || body.len() != t.length(i)
                    || format!("{:x}", Sha256::digest(&body)) != hash
                {
                    return Err(fail("Part checksum mismatch", 422));
                }
                if !t.buffers.contains_key(&i) {
                    if slots >= parts::WINDOW {
                        return Err(fail("Part window full", 429));
                    }
                    t.buffers.insert(i, body);
                }
                Ok((
                    202,
                    serde_json::to_vec(&json!({"index":i})).unwrap(),
                    "application/json",
                ))
            } else if method == &Method::Get && d.id == t.to {
                match t.buffers.get(&i) {
                    Some(b) => Ok((200, b.clone(), "application/octet-stream")),
                    None => Ok((204, vec![], "application/octet-stream")),
                }
            } else {
                Err(fail("Not found", 404))
            }
        }
    }
}
impl Transfer {
    fn size(&self) -> u64 {
        self.manifest["size"].as_u64().unwrap()
    }
    fn length(&self, i: usize) -> usize {
        ((self.size() - i as u64 * parts::SIZE as u64).min(parts::SIZE as u64)) as usize
    }
    fn offset(&self) -> u64 {
        if !self.audio {
            self.size()
        } else {
            self.acked.iter().map(|i| self.length(*i) as u64).sum()
        }
    }
    fn description(&self, missing: bool) -> Value {
        let mut v = json!({"size":parts::SIZE,"window":parts::WINDOW,"hashes":self.hashes});
        if missing {
            v.as_object_mut().unwrap().remove("hashes");
            v["missing"] = json!((0..self.hashes.len())
                .filter(|i| !self.acked.contains(i))
                .collect::<Vec<_>>());
        }
        v
    }
}
struct RequestSlot<'a> {
    state: &'a Mutex<State>,
    key: String,
}
impl Drop for RequestSlot<'_> {
    fn drop(&mut self) {
        let mut s = self.state.lock().unwrap();
        if let Some(n) = s.requests.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                s.requests.remove(&self.key);
            }
        }
    }
}
#[derive(Clone)]
struct SignedRequest {
    host: String,
    device: String,
    token: String,
    nonce: String,
    digest: String,
}
fn header(req: &Request, name: &'static str) -> String {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.to_string())
        .unwrap_or_default()
}
fn respond(req: Request, status: u16, body: Vec<u8>, kind: &str, signed: Option<&SignedRequest>) {
    let mut response = Response::from_data(body.clone())
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", kind).unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    if let Some(auth) = signed {
        let proof = super::wire::sign(
            &auth.token,
            &super::wire::receipt(
                &auth.host,
                &auth.device,
                &auth.nonce,
                req.method().as_str(),
                req.url(),
                status,
                &super::wire::digest(&body),
            ),
        );
        response = response.with_header(Header::from_bytes("x-hearfolio-proof", proof).unwrap());
    }
    let _ = req.respond(response);
}
fn reject(req: Request, status: u16, message: &str, signed: Option<&SignedRequest>) {
    respond(
        req,
        status,
        serde_json::to_vec(&json!({"error":message})).unwrap(),
        "application/json",
        signed,
    );
}
fn serve(mut req: Request, state: &Mutex<State>) {
    let peer = req.remote_addr().map(|a| a.ip());
    if !peer.is_some_and(|ip| matches!(ip,std::net::IpAddr::V4(ip) if private(ip))) {
        reject(req, 403, "Local network required", None);
        return;
    }
    let mut bearer = header(&req, "Authorization")
        .strip_prefix("Bearer ")
        .unwrap_or("")
        .to_owned();
    let device = header(&req, "x-hearfolio-device");
    let nonce = header(&req, "x-hearfolio-nonce");
    let sha = header(&req, "x-hearfolio-digest");
    let proof = header(&req, "x-hearfolio-proof");
    let mut signed = None;
    if !device.is_empty() {
        let mut s = state.lock().unwrap();
        s.prune();
        if let Some(d) = s.saved.devices.iter().find(|d| d.id == device) {
            signed = Some(SignedRequest {
                host: s.saved.host.clone(),
                device: device.clone(),
                token: d.token.clone(),
                nonce: nonce.clone(),
                digest: sha.clone(),
            });
        }
        let valid = signed.as_ref().is_some_and(|a| {
            nonce.split(':').count() == 3
                && sha.len() == 64
                && sha.bytes().all(|c| c.is_ascii_hexdigit())
                && super::wire::verify(
                    &a.token,
                    &super::wire::request(
                        &a.host,
                        &device,
                        &nonce,
                        req.method().as_str(),
                        req.url(),
                        &sha,
                    ),
                    &proof,
                )
        });
        if !valid {
            drop(s);
            reject(req, 401, "Invalid device authentication", signed.as_ref());
            return;
        }
        let fields: Vec<_> = nonce.split(':').collect();
        let time = fields.get(1).and_then(|v| v.parse::<u64>().ok());
        let fresh = fields.first() == Some(&s.epoch.as_str())
            && fields
                .get(2)
                .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
            && time.is_some_and(|stamp| {
                stamp <= super::wire::now().saturating_add(30000)
                    && super::wire::now().saturating_sub(stamp) <= 90000
            });
        if !fresh {
            drop(s);
            reject(req, 409, "Expired request session", signed.as_ref());
            return;
        }
        if s.nonces.contains_key(&(device.clone(), nonce.clone())) {
            drop(s);
            reject(req, 409, "Repeated request nonce", signed.as_ref());
            return;
        }
        if s.nonces.len() >= 16000 {
            drop(s);
            reject(req, 429, "Authentication window full", signed.as_ref());
            return;
        }
        s.nonces.insert((device, nonce), Instant::now());
        bearer = signed.as_ref().unwrap().token.clone();
    }
    let key = {
        let mut s = state.lock().unwrap();
        let public = matches!(
            req.url(),
            "/join" | "/join/status" | "/health" | "/identity"
        );
        let authenticated = s.saved.devices.iter().any(|d| d.token == bearer);
        if !public && !authenticated {
            drop(s);
            reject(req, 401, "Unauthorized", signed.as_ref());
            return;
        }
        let key = if authenticated {
            bearer.clone()
        } else {
            format!("anonymous:{peer:?}")
        };
        let n = s.requests.entry(key.clone()).or_default();
        if *n >= if authenticated { 4 } else { 2 } {
            drop(s);
            reject(req, 429, "Device connection limit reached", signed.as_ref());
            return;
        }
        *n += 1;
        key
    };
    let _slot = RequestSlot { state, key };
    let hash = header(&req, "x-part-sha256");
    let mut body = vec![];
    let max = if matches!(
        req.url(),
        "/join" | "/join/status" | "/identity" | "/health"
    ) {
        1024
    } else if req.method() == &Method::Put {
        parts::SIZE
    } else {
        5 * 1024 * 1024
    };
    let result = if req
        .as_reader()
        .take((max + 1) as u64)
        .read_to_end(&mut body)
        .is_err()
        || body.len() > max
    {
        Err(fail("Invalid request body", 413))
    } else if signed
        .as_ref()
        .is_some_and(|a| super::wire::digest(&body) != a.digest)
    {
        Err(fail("Request checksum mismatch", 422))
    } else {
        state
            .lock()
            .unwrap()
            .handle(req.method(), req.url(), &bearer, &hash, body)
    };
    let (status, body, kind) = match result {
        Ok(v) => v,
        Err((status, message)) => (
            status,
            serde_json::to_vec(&json!({"error":message})).unwrap(),
            "application/json",
        ),
    };
    respond(req, status, body, kind, signed.as_ref());
}
pub(super) fn session_epoch() -> String {
    static EPOCH: OnceLock<String> = OnceLock::new();
    EPOCH.get_or_init(id).clone()
}
pub fn start() -> Result<(String, String, u16), String> {
    static LOCAL: OnceLock<Result<(String, String, u16), String>> = OnceLock::new();
    LOCAL
        .get_or_init(|| {
            let path = crate::storage::config_dir()?.join("transfer-local.json");
            let mut saved: Saved = if path.exists() {
                serde_json::from_slice(&fs::read(&path).map_err(error)?).map_err(error)?
            } else {
                let host = Device {
                    id: id(),
                    token: token(),
                    name: "Hearfolio".into(),
                    peers: HashSet::new(),
                };
                Saved {
                    port: PORT,
                    host: host.id.clone(),
                    devices: vec![host],
                }
            };
            let host = saved
                .devices
                .iter()
                .find(|d| d.id == saved.host)
                .ok_or("Invalid local device state")?;
            let server = Arc::new(
                Server::http(format!("0.0.0.0:{}", saved.port))
                    .or_else(|_| Server::http("0.0.0.0:0"))
                    .map_err(error)?,
            );
            let port = server
                .server_addr()
                .to_ip()
                .ok_or("Invalid local listener")?
                .port();
            let credentials = (host.id.clone(), host.token.clone(), port);
            saved.port = port;
            let state = Arc::new(Mutex::new(State {
                epoch: session_epoch(),
                saved,
                path,
                online: HashMap::new(),
                codes: HashMap::new(),
                joins: HashMap::new(),
                transfers: HashMap::new(),
                attempts: HashMap::new(),
                requests: HashMap::new(),
                nonces: HashMap::new(),
            }));
            state.lock().unwrap().persist().map_err(|(_, e)| e)?;
            let _ = super::discovery::advertise(&credentials.0, port);

            for _ in 0..8 {
                let server = server.clone();
                let state = state.clone();
                std::thread::spawn(move || {
                    for req in server.incoming_requests() {
                        serve(req, &state);
                    }
                });
            }
            let cleanup = state.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(10));
                cleanup.lock().unwrap().prune();
            });
            Ok(credentials)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::super::{client, response, rpc, Connection};
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    fn fixture() -> (State, Device, Device, PathBuf) {
        let dir = std::env::temp_dir().join(id());
        fs::create_dir_all(&dir).unwrap();
        let host = Device {
            id: id(),
            token: token(),
            name: "Laptop".into(),
            peers: HashSet::new(),
        };
        let mut phone = Device {
            id: id(),
            token: token(),
            name: "Android".into(),
            peers: HashSet::from([host.id.clone()]),
        };
        let mut host = host;
        host.peers.insert(phone.id.clone());
        phone.peers.insert(host.id.clone());
        let state = State {
            epoch: id(),
            saved: Saved {
                port: PORT,
                host: host.id.clone(),
                devices: vec![host.clone(), phone.clone()],
            },
            path: dir.join("devices.json"),
            online: HashMap::from([
                (host.id.clone(), Instant::now()),
                (phone.id.clone(), Instant::now()),
            ]),
            codes: HashMap::new(),
            joins: HashMap::new(),
            transfers: HashMap::new(),
            attempts: HashMap::new(),
            requests: HashMap::new(),
            nonces: HashMap::new(),
        };
        (state, host, phone, dir)
    }
    fn value(result: Reply) -> Value {
        let (_, body, _) = result.unwrap();
        serde_json::from_slice(&body).unwrap()
    }
    fn call(state: &mut State, d: &Device, action: &str, payload: Value) -> Reply {
        state.handle(
            &Method::Post,
            "/rpc",
            &d.token,
            "",
            serde_json::to_vec(&json!({"action":action,"payload":payload})).unwrap(),
        )
    }
    #[test]
    fn invitations_require_confirmation_and_expired_codes_cannot_register() {
        let (mut state, host, phone, dir) = fixture();
        assert_eq!(
            state
                .handle(&Method::Post, "/register", "", "", b"{}".to_vec())
                .unwrap_err()
                .0,
            401
        );
        let code = value(call(&mut state, &host, "code", json!({})))["code"].clone();
        let ticket = value(state.handle(
            &Method::Post,
            "/join",
            "",
            "",
            serde_json::to_vec(&json!({"name":"New phone","code":code})).unwrap(),
        ));
        let payload = serde_json::to_vec(&ticket).unwrap();
        assert_eq!(
            value(state.handle(&Method::Post, "/join/status", "", "", payload.clone()))["status"],
            "waiting"
        );
        assert_eq!(
            state
                .handle(
                    &Method::Post,
                    "/join/status",
                    "",
                    "",
                    serde_json::to_vec(&json!({"id":ticket["id"],"secret":"wrong"})).unwrap()
                )
                .unwrap_err()
                .0,
            404
        );
        assert!(call(
            &mut state,
            &phone,
            "confirmPair",
            json!({"id":ticket["id"],"accept":true})
        )
        .is_err());
        value(call(
            &mut state,
            &host,
            "confirmPair",
            json!({"id":ticket["id"],"accept":true}),
        ));
        let accepted = value(state.handle(&Method::Post, "/join/status", "", "", payload));
        assert_eq!(accepted["status"], "approved");
        assert!(state
            .saved
            .devices
            .iter()
            .any(|d| d.id == accepted["device"]["id"] && d.peers.contains(&host.id)));
        let old = value(call(&mut state, &host, "code", json!({})))["code"]
            .as_str()
            .unwrap()
            .to_string();
        state.codes.get_mut(&old).unwrap().1 = Instant::now() - Duration::from_secs(181);
        assert!(state
            .handle(
                &Method::Post,
                "/join",
                "",
                "",
                serde_json::to_vec(&json!({"name":"Expired","code":old})).unwrap()
            )
            .is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn local_clients_transfer_large_file_despite_lost_responses_and_network_errors() {
        let (state, host, phone, dir) = fixture();
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let url = format!("http://{}", server.server_addr());
        let state = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let lost_put = Arc::new(AtomicBool::new(false));
        let lost_ack = Arc::new(AtomicBool::new(false));
        let failed_get = Arc::new(AtomicBool::new(false));
        let mut workers = vec![];
        for _ in 0..8 {
            let server = server.clone();
            let state = state.clone();
            let stop = stop.clone();
            let lost_put = lost_put.clone();
            let lost_ack = lost_ack.clone();
            let failed_get = failed_get.clone();
            workers.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Some(mut req) = server.recv_timeout(Duration::from_millis(100)).unwrap()
                    else {
                        continue;
                    };
                    let bearer = req
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("Authorization"))
                        .map(|h| h.value.as_str().strip_prefix("Bearer ").unwrap())
                        .unwrap_or("")
                        .to_string();
                    let hash = req
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("x-part-sha256"))
                        .map(|h| h.value.to_string())
                        .unwrap_or_default();
                    let mut body = vec![];
                    req.as_reader().read_to_end(&mut body).unwrap();
                    let ack = serde_json::from_slice::<Value>(&body)
                        .is_ok_and(|v| v["action"] == "ackPart");
                    let put = req.method() == &Method::Put;
                    let get = req.method() == &Method::Get && req.url().contains("/parts/");
                    let result =
                        state
                            .lock()
                            .unwrap()
                            .handle(req.method(), req.url(), &bearer, &hash, body);
                    // Server has stored/ACKed the part, but the response is lost to the client.
                    if (put && !lost_put.swap(true, Ordering::Relaxed))
                        || (ack && !lost_ack.swap(true, Ordering::Relaxed))
                        || (get && !failed_get.swap(true, Ordering::Relaxed))
                    {
                        let _ = req
                            .respond(Response::from_string("network switch").with_status_code(503));
                        continue;
                    }
                    let (status, body, _) = result.unwrap_or_else(|(status, e)| {
                        (
                            status,
                            serde_json::to_vec(&json!({"error":e})).unwrap(),
                            "application/json",
                        )
                    });
                    let _ = req.respond(Response::from_data(body).with_status_code(status));
                }
            }));
        }
        let source = dir.join("source.wav");
        // 96 MiB exercises 97 verified blocks, several dozen independent windows.
        {
            use std::io::Write;
            let mut file = fs::File::create(&source).unwrap();
            let chunk = vec![37; parts::SIZE];
            for _ in 0..96 {
                file.write_all(&chunk).unwrap();
            }
            file.write_all(b"last short part").unwrap();
        }
        let (sha, size) = super::super::hash_file(&source).unwrap();
        let description = parts::describe(&source, &sha, size).unwrap();
        let sender = Connection {
            server: url.clone(),
            id: host.id,
            token: host.token,
            local_host: false,
            host_id: String::new(),
        };
        let receiver = Connection {
            server: url,
            id: phone.id,
            token: phone.token,
            local_host: false,
            host_id: String::new(),
        };
        let manifest = json!({"version":1,"sourceId":"original","name":"Long recording","extension":"wav","size":size,"sha256":sha,"transcript":"Updated transcript","createdAt":now()});
        let offered = rpc(
            &sender,
            "offer",
            json!({"to":receiver.id,"manifest":manifest,"parts":description}),
        )
        .unwrap();
        let session = offered["id"].as_str().unwrap().to_string();
        let mut cp =
            parts::Checkpoint::open(&dir, "local/original/hash", size, description.clone())
                .unwrap();
        cp.write(0, &vec![37; parts::SIZE]).unwrap();
        cp.write(1, &vec![37; parts::SIZE]).unwrap();
        drop(cp);
        let mut cp =
            parts::Checkpoint::open(&dir, "local/original/hash", size, description.clone())
                .unwrap();
        assert_eq!(cp.verified(), vec![0, 1]);
        rpc(
            &receiver,
            "accept",
            json!({"id":session,"parts":{"verified":cp.verified()}}),
        )
        .unwrap();
        std::thread::scope(|scope| {
            let sending = scope
                .spawn(|| parts::send(&sender, &session, &source, size, &description, |_| Ok(())));
            parts::receive(&receiver, &session, &mut cp, |_| Ok(())).unwrap();
            cp.finish(&sha).unwrap();
            parts::control(&receiver, "finish", json!({"id":session})).unwrap();
            sending.join().unwrap().unwrap();
        });
        assert!(
            lost_put.load(Ordering::Relaxed)
                && lost_ack.load(Ordering::Relaxed)
                && failed_get.load(Ordering::Relaxed)
        );
        // Last receipt can be retried after finish; it must not become HTTP 409.
        let bytes = fs::read(&source).unwrap();
        let final_index = description.hashes.len() - 1;
        let receipt = response(
            client()
                .unwrap()
                .put(format!(
                    "{}/transfers/{session}/parts/{final_index}",
                    sender.server
                ))
                .bearer_auth(&sender.token)
                .header("x-part-sha256", &description.hashes[final_index])
                .body(bytes[final_index * parts::SIZE..].to_vec())
                .send()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["index"], final_index);
        assert_eq!(
            rpc(&sender, "status", json!({"id":session})).unwrap()["status"],
            "delivered"
        );
        assert!(state
            .lock()
            .unwrap()
            .transfers
            .values()
            .all(|t| t.buffers.is_empty()));
        // Existing audio is confirmed by size+hash; this offer sends transcript only.
        let next = rpc(
            &sender,
            "offer",
            json!({"to":receiver.id,"manifest":manifest,"parts":description}),
        )
        .unwrap()["id"]
            .clone();
        rpc(
            &receiver,
            "accept",
            json!({"id":next,"audioRequired":false,"sha256":sha,"size":size}),
        )
        .unwrap();
        rpc(&receiver, "finish", json!({"id":next})).unwrap();
        assert_eq!(
            rpc(&sender, "status", json!({"id":next})).unwrap()["audioRequired"],
            false
        );
        stop.store(true, Ordering::Relaxed);
        for w in workers {
            w.join().unwrap();
        }
        cp.remove();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn signed_transfer_survives_endpoint_change_without_pairing_again() {
        let (state, host, phone, dir) = fixture();
        let state = Arc::new(Mutex::new(state));
        fn listener(
            addr: &str,
            state: Arc<Mutex<State>>,
        ) -> (String, Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>) {
            let server = Arc::new(Server::http(addr).unwrap());
            let url = format!("http://{}", server.server_addr());
            let stop = Arc::new(AtomicBool::new(false));
            let mut threads = vec![];
            for _ in 0..8 {
                let server = server.clone();
                let state = state.clone();
                let stop = stop.clone();
                threads.push(std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if let Some(req) = server.recv_timeout(Duration::from_millis(100)).unwrap()
                        {
                            serve(req, &state);
                        }
                    }
                }));
            }
            (url, stop, threads)
        }
        let (old_url, old_stop, old_threads) = listener("127.0.0.1:0", state.clone());
        let sender = Connection {
            server: old_url.clone(),
            id: host.id.clone(),
            token: host.token.clone(),
            local_host: false,
            host_id: host.id.clone(),
        };
        let receiver = Connection {
            server: old_url.clone(),
            id: phone.id.clone(),
            token: phone.token.clone(),
            local_host: false,
            host_id: host.id.clone(),
        };
        assert_eq!(
            rpc(&sender, "poll", json!({})).unwrap()["device"]["id"],
            host.id
        );
        assert_eq!(
            rpc(&receiver, "poll", json!({})).unwrap()["device"]["id"],
            phone.id
        );
        let bytes = vec![41; parts::SIZE * 8 + 13];
        let source = dir.join("source.wav");
        fs::write(&source, &bytes).unwrap();
        let (sha, size) = super::super::hash_file(&source).unwrap();
        let description = parts::describe(&source, &sha, size).unwrap();
        let manifest = json!({"version":1,"sourceId":"stable-recording","name":"Network change","extension":"wav","size":size,"sha256":sha,"transcript":"Text","createdAt":now()});
        let offer =
            json!({"requestId":id(),"to":receiver.id,"manifest":manifest,"parts":description});
        let session = rpc(&sender, "offer", offer.clone()).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(rpc(&sender, "offer", offer).unwrap()["id"], session);
        assert_eq!(state.lock().unwrap().transfers.len(), 1);
        let mut cp =
            parts::Checkpoint::open(&dir, "lan/stable-recording", size, description.clone())
                .unwrap();
        rpc(
            &receiver,
            "accept",
            json!({"id":session,"parts":{"verified":[]}}),
        )
        .unwrap();
        let moved = Arc::new(AtomicBool::new(false));
        // Different IP and port, same host/device keys and active transfer session.
        let (new_url, new_stop, new_threads) = listener("127.0.0.2:0", state.clone());
        std::thread::scope(|scope| {
            let sending = scope
                .spawn(|| parts::send(&sender, &session, &source, size, &description, |_| Ok(())));
            parts::receive(&receiver, &session, &mut cp, |offset| {
                if offset >= parts::SIZE as u64 * 4 && !moved.swap(true, Ordering::Relaxed) {
                    old_stop.store(true, Ordering::Relaxed);
                    // Deterministic stand-in for the already separately tested DNS-SD update.
                    super::super::discovery::remember(&host.id, &new_url);
                    super::super::discovery::invalidate(&sender);
                    super::super::discovery::invalidate(&receiver);
                }
                Ok(())
            })
            .unwrap();
            cp.finish(&sha).unwrap();
            parts::control(&receiver, "finish", json!({"id":session})).unwrap();
            sending.join().unwrap().unwrap();
        });
        assert!(moved.load(Ordering::Relaxed));
        assert_eq!(
            super::super::discovery::endpoint(&receiver).unwrap(),
            new_url
        );
        assert_eq!(
            rpc(&sender, "status", json!({"id":session})).unwrap()["status"],
            "delivered"
        );
        assert_eq!(
            state.lock().unwrap().saved.devices.len(),
            2,
            "A network change must not register a duplicate device"
        );
        assert_eq!(
            state.lock().unwrap().transfers.len(),
            1,
            "A network change keeps the logical transfer session"
        );
        // An exact replay and a modified signed body are rejected, with verifiable responses.
        let nonce = super::super::discovery::nonce(&receiver).unwrap();
        let body = serde_json::to_vec(&json!({"action":"poll","payload":{}})).unwrap();
        let sha = super::super::wire::digest(&body);
        let proof = super::super::wire::sign(
            &receiver.token,
            &super::super::wire::request(&host.id, &receiver.id, &nonce, "POST", "/rpc", &sha),
        );
        let signed = |payload: Vec<u8>| {
            client()
                .unwrap()
                .post(format!("{new_url}/rpc"))
                .header("x-hearfolio-device", &receiver.id)
                .header("x-hearfolio-nonce", &nonce)
                .header("x-hearfolio-digest", &sha)
                .header("x-hearfolio-proof", &proof)
                .body(payload)
                .send()
                .unwrap()
        };
        assert!(signed(body.clone()).status().is_success());
        assert_eq!(signed(body.clone()).status(), reqwest::StatusCode::CONFLICT);
        // Old request sessions must remain invalid even after replay-cache pruning.
        state.lock().unwrap().nonces.clear();
        let old_epoch = state.lock().unwrap().epoch.clone();
        state.lock().unwrap().epoch = id();
        assert_eq!(signed(body).status(), reqwest::StatusCode::CONFLICT);
        state.lock().unwrap().epoch = old_epoch;
        old_stop.store(true, Ordering::Relaxed);
        new_stop.store(true, Ordering::Relaxed);
        for t in old_threads.into_iter().chain(new_threads) {
            t.join().unwrap();
        }
        cp.remove();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn spoofed_mdns_identity_never_receives_credentials() {
        let (state, host, phone, dir) = fixture();
        let state = Arc::new(Mutex::new(state));
        let real = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let real_url = format!("http://{}", real.server_addr());
        let fake = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let fake_url = format!("http://{}", fake.server_addr());
        let stop = Arc::new(AtomicBool::new(false));
        let leaked = Arc::new(AtomicBool::new(false));
        let fake_stop = stop.clone();
        let fake_leaked = leaked.clone();
        let fake_host = host.id.clone();
        let f = std::thread::spawn(move || {
            while !fake_stop.load(Ordering::Relaxed) {
                if let Some(req) = fake.recv_timeout(Duration::from_millis(100)).unwrap() {
                    if !header(&req, "Authorization").is_empty()
                        || !header(&req, "x-hearfolio-proof").is_empty()
                    {
                        fake_leaked.store(true, Ordering::Relaxed);
                    }
                    let body = json!({"hostId":fake_host,"epoch":id(),"time":now(),"proof":"00".repeat(32)});
                    respond(
                        req,
                        200,
                        serde_json::to_vec(&body).unwrap(),
                        "application/json",
                        None,
                    );
                }
            }
        });
        let real_stop = stop.clone();
        let r = std::thread::spawn(move || {
            while !real_stop.load(Ordering::Relaxed) {
                if let Some(req) = real.recv_timeout(Duration::from_millis(100)).unwrap() {
                    serve(req, &state);
                }
            }
        });
        let c = Connection {
            server: fake_url,
            id: phone.id,
            token: phone.token,
            local_host: false,
            host_id: host.id.clone(),
        };
        super::super::discovery::remember(&host.id, &real_url);
        assert_eq!(rpc(&c, "poll", json!({})).unwrap()["device"]["id"], c.id);
        assert!(!leaked.load(Ordering::Relaxed));
        stop.store(true, Ordering::Relaxed);
        f.join().unwrap();
        r.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
