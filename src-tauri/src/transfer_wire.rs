//! Authenticated LAN messages. Discovery never exposes the device token.
use super::{client, discovery, error, network_error, Connection};
use hmac::{Hmac, Mac};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
type HmacSha256 = Hmac<Sha256>;
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn sign(key: &str, value: &Value) -> String {
    let mut h =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts arbitrary key length");
    h.update(&serde_json::to_vec(value).expect("JSON value"));
    h.finalize()
        .into_bytes()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
pub fn verify(key: &str, value: &Value, proof: &str) -> bool {
    if proof.len() != 64 || !proof.bytes().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&proof[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    let mut h = HmacSha256::new_from_slice(key.as_bytes()).unwrap();
    h.update(&serde_json::to_vec(value).unwrap());
    h.verify_slice(&bytes).is_ok()
}
pub fn now() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}
pub fn identity(host: &str, device: &str, nonce: &str, epoch: &str, time: u64) -> Value {
    json!(["hearfolio/identity/1", host, device, nonce, epoch, time])
}
pub fn request(
    host: &str,
    device: &str,
    nonce: &str,
    method: &str,
    path: &str,
    sha: &str,
) -> Value {
    json!([
        "hearfolio/request/1",
        host,
        device,
        nonce,
        method,
        path,
        sha
    ])
}
pub fn receipt(
    host: &str,
    device: &str,
    nonce: &str,
    method: &str,
    path: &str,
    status: u16,
    sha: &str,
) -> Value {
    json!([
        "hearfolio/response/1",
        host,
        device,
        nonce,
        method,
        path,
        status,
        sha
    ])
}
pub struct Reply {
    pub status: StatusCode,
    pub body: Vec<u8>,
}
impl Reply {
    pub fn json(self) -> Result<Value, String> {
        let value: Value = serde_json::from_slice(&self.body).map_err(error)?;
        if !self.status.is_success() {
            return Err(value["error"]
                .as_str()
                .unwrap_or("LAN request failed")
                .into());
        }
        Ok(value)
    }
    pub fn detail(&self) -> String {
        serde_json::from_slice::<Value>(&self.body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("HTTP {}", self.status))
    }
}
pub fn send(
    c: &Connection,
    method: Method,
    path: &str,
    body: Vec<u8>,
    part_hash: Option<&str>,
) -> Result<Reply, String> {
    let server = discovery::endpoint(c)?;
    let nonce = if c.host_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        discovery::nonce(c)?
    };
    let mut req = client()?.request(method.clone(), format!("{server}{path}"));
    if c.host_id.is_empty() {
        req = req.bearer_auth(&c.token);
    } else {
        let sha = digest(&body);
        let proof = sign(
            &c.token,
            &request(&c.host_id, &c.id, &nonce, method.as_str(), path, &sha),
        );
        req = req
            .header("x-hearfolio-device", &c.id)
            .header("x-hearfolio-nonce", &nonce)
            .header("x-hearfolio-digest", sha)
            .header("x-hearfolio-proof", proof);
    }
    if let Some(hash) = part_hash {
        req = req.header("x-part-sha256", hash);
    }
    let result = (|| {
        let mut res = req.body(body).send().map_err(network_error)?;
        let status = res.status();
        let proof = res
            .headers()
            .get("x-hearfolio-proof")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let limit = if path.contains("/parts/") && method == Method::GET {
            super::parts::SIZE + 1
        } else {
            5 * 1024 * 1024
        };
        let mut body = vec![];
        res.by_ref()
            .take((limit + 1) as u64)
            .read_to_end(&mut body)
            .map_err(error)?;
        if body.len() > limit {
            return Err("LAN response too large".into());
        }
        if !c.host_id.is_empty()
            && !verify(
                &c.token,
                &receipt(
                    &c.host_id,
                    &c.id,
                    &nonce,
                    method.as_str(),
                    path,
                    status.as_u16(),
                    &digest(&body),
                ),
                &proof,
            )
        {
            return Err("Unverified LAN response; searching for the paired device".into());
        }
        let reply = Reply { status, body };
        if reply.status == StatusCode::CONFLICT && reply.detail() == "Expired request session" {
            return Err("LAN session changed; rediscovering the paired device".into());
        }
        Ok(reply)
    })();
    if result.is_err() {
        discovery::invalidate(c);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authentication_binds_identity_nonce_content_and_direction() {
        let value = request(
            "host",
            "client",
            "nonce",
            "PUT",
            "/parts/1",
            &digest(b"audio"),
        );
        let proof = sign("secret", &value);
        assert!(verify("secret", &value, &proof));
        assert!(!verify("wrong", &value, &proof));
        assert!(!verify(
            "secret",
            &request(
                "other",
                "client",
                "nonce",
                "PUT",
                "/parts/1",
                &digest(b"audio")
            ),
            &proof
        ));
        assert!(!verify(
            "secret",
            &request(
                "host",
                "client",
                "other",
                "PUT",
                "/parts/1",
                &digest(b"audio")
            ),
            &proof
        ));
        assert!(!verify(
            "secret",
            &receipt(
                "host",
                "client",
                "nonce",
                "PUT",
                "/parts/1",
                200,
                &digest(b"audio")
            ),
            &proof
        ));
        assert!(!verify(
            "secret",
            &request(
                "host",
                "client",
                "nonce",
                "PUT",
                "/parts/1",
                &digest(b"changed")
            ),
            &proof
        ));
        assert!(!verify("secret", &value, "zz"));
    }
}
