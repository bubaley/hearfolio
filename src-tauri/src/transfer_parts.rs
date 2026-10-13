//! Receiver-owned multipart checkpoints; the relay never stores audio on disk.
use super::{client, error, hash_file, network_error, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const SIZE: usize = 1024 * 1024;
pub const WINDOW: usize = 4;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Description {
    pub size: usize,
    pub hashes: Vec<String>,
}
impl Description {
    pub fn validate(&self, size: u64) -> Result<(), String> {
        if self.size != SIZE
            || self.hashes.len() != size.div_ceil(SIZE as u64) as usize
            || self.hashes.iter().any(|h| {
                h.len() != 64
                    || !h
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err("Invalid part manifest".into());
        }
        Ok(())
    }
    fn length(&self, size: u64, index: usize) -> usize {
        (size - index as u64 * SIZE as u64).min(SIZE as u64) as usize
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn describe(path: &Path, expected: &str, size: u64) -> Result<Description, String> {
    let mut file = fs::File::open(path).map_err(error)?;
    let mut buffer = vec![0; SIZE];
    let mut whole = Sha256::new();
    let mut hashes = Vec::new();
    let mut count = 0;
    while count < size {
        let n = (size - count).min(SIZE as u64) as usize;
        file.read_exact(&mut buffer[..n]).map_err(error)?;
        whole.update(&buffer[..n]);
        count += n as u64;
        hashes.push(digest(&buffer[..n]));
    }
    if file.read(&mut buffer[..1]).map_err(error)? != 0 {
        return Err("Source audio changed".into());
    }
    if count != size || format!("{:x}", whole.finalize()) != expected {
        return Err("Source audio changed".into());
    }
    let result = Description { size: SIZE, hashes };
    result.validate(size)?;
    Ok(result)
}
// Old checkpoints are local to the receiver and retained for seven days.
pub fn discard(dir: &Path, identity: &str) {
    let key = digest(identity.as_bytes());
    for extension in ["part", "json"] {
        let _ = fs::remove_file(
            dir.join("temp")
                .join(format!("transfer-parts-{key}.{extension}")),
        );
    }
}
pub fn cleanup(dir: &Path) {
    if let Ok(files) = fs::read_dir(dir.join("temp")) {
        for entry in files.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(key) = name
                .strip_prefix("transfer-parts-")
                .and_then(|s| s.strip_suffix(".json"))
            else {
                continue;
            };
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let expired = entry
                .metadata()
                .ok()
                .filter(|m| m.is_file())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(7 * 24 * 60 * 60));
            if expired {
                for extension in ["part", "json"] {
                    let _ = fs::remove_file(
                        dir.join("temp")
                            .join(format!("transfer-parts-{key}.{extension}")),
                    );
                }
            }
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    key: String,
    size: u64,
    description: Description,
    verified: Vec<bool>,
}
pub struct Checkpoint {
    pub path: PathBuf,
    journal_path: PathBuf,
    journal: Journal,
    file: fs::File,
}
impl Checkpoint {
    pub fn open(
        dir: &Path,
        identity: &str,
        size: u64,
        description: Description,
    ) -> Result<Self, String> {
        description.validate(size)?;
        let key = digest(identity.as_bytes());
        let temp = dir.join("temp");
        fs::create_dir_all(&temp).map_err(error)?;
        let path = temp.join(format!("transfer-parts-{key}.part"));
        let journal_path = temp.join(format!("transfer-parts-{key}.json"));
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.is_file() {
                return Err("Invalid partial audio path".into());
            }
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(error)?;
        let saved = fs::symlink_metadata(&journal_path)
            .ok()
            .filter(|m| m.is_file() && m.len() <= 128 * 1024)
            .and_then(|_| fs::read(&journal_path).ok())
            .and_then(|b| serde_json::from_slice::<Journal>(&b).ok());
        let mut journal = Journal {
            version: 1,
            key: key.clone(),
            size,
            verified: vec![false; description.hashes.len()],
            description,
        };
        if let Some(old) = saved {
            if old.version == 1
                && old.key == key
                && old.size == size
                && old.description.size == SIZE
                && old.description.hashes == journal.description.hashes
                && old.verified.len() == journal.verified.len()
            {
                journal.verified = old.verified;
            }
        }
        // Disk content is authoritative: never advertise a damaged checkpoint.
        for index in 0..journal.verified.len() {
            if file.metadata().map_err(error)?.len()
                >= index as u64 * SIZE as u64 + journal.description.length(size, index) as u64
            {
                let mut bytes = vec![0; journal.description.length(size, index)];
                journal.verified[index] = file
                    .seek(SeekFrom::Start(index as u64 * SIZE as u64))
                    .is_ok()
                    && file.read_exact(&mut bytes).is_ok()
                    && digest(&bytes) == journal.description.hashes[index];
            } else {
                journal.verified[index] = false;
            }
        }
        let mut result = Self {
            path,
            journal_path,
            journal,
            file,
        };
        result.persist()?;
        Ok(result)
    }
    pub fn verified(&self) -> Vec<usize> {
        self.journal
            .verified
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.then_some(i))
            .collect()
    }
    pub fn missing(&self) -> Vec<usize> {
        self.journal
            .verified
            .iter()
            .enumerate()
            .filter_map(|(i, v)| (!v).then_some(i))
            .collect()
    }
    pub fn received(&self) -> u64 {
        self.verified()
            .iter()
            .map(|&i| self.journal.description.length(self.journal.size, i) as u64)
            .sum()
    }
    fn persist(&mut self) -> Result<(), String> {
        let temp = self
            .journal_path
            .with_extension(format!("json.{}.new", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(error)?;
        file.write_all(&serde_json::to_vec(&self.journal).map_err(error)?)
            .map_err(error)?;
        file.sync_all().map_err(error)?;
        fs::rename(&temp, &self.journal_path).map_err(error)?;
        #[cfg(unix)]
        fs::File::open(self.journal_path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(error)?;
        Ok(())
    }
    pub fn write(&mut self, index: usize, bytes: &[u8]) -> Result<(), String> {
        if index >= self.journal.verified.len()
            || bytes.len() != self.journal.description.length(self.journal.size, index)
            || digest(bytes) != self.journal.description.hashes[index]
        {
            return Err("Part checksum mismatch".into());
        }
        self.file
            .seek(SeekFrom::Start(index as u64 * SIZE as u64))
            .map_err(error)?;
        self.file.write_all(bytes).map_err(error)?;
        self.file.sync_data().map_err(error)?;
        self.journal.verified[index] = true;
        self.persist()
    }
    pub fn finish(&mut self, expected: &str) -> Result<(), String> {
        if !self.missing().is_empty() {
            return Err("Missing audio parts".into());
        }
        self.file.set_len(self.journal.size).map_err(error)?;
        self.file.sync_all().map_err(error)?;
        if hash_file(&self.path)? != (expected.to_string(), self.journal.size) {
            return Err("Audio checksum mismatch".into());
        }
        Ok(())
    }
    pub fn remove(self) {
        let path = self.path.clone();
        let journal = self.journal_path.clone();
        drop(self);
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(journal);
    }
}
fn retryable(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}
/// Retry only network/rate-limit/server errors, never authorization or cancellation.
pub(super) fn control(
    c: &Connection,
    action: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let started = Instant::now();
    loop {
        crate::background::check_transfer_cancelled()?;
        let result = client()?
            .post(format!("{}/rpc", c.server))
            .bearer_auth(&c.token)
            .json(&json!({"action":action,"payload":payload}))
            .send();
        let last = match result {
            Ok(res) if res.status().is_success() => match super::response(res) {
                Ok(value) => return Ok(value),
                Err(e) => e,
            },
            Ok(res) => {
                let status = res.status();
                let detail = super::response(res)
                    .err()
                    .unwrap_or_else(|| format!("HTTP {status}"));
                if !retryable(status) {
                    return Err(detail);
                }
                detail
            }
            Err(e) => network_error(e),
        };
        if started.elapsed() >= Duration::from_secs(120) {
            return Err(last);
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
}
fn put(
    c: &Connection,
    id: &str,
    index: usize,
    description: &Description,
    path: &Path,
    size: u64,
) -> Result<(), String> {
    let mut file = fs::File::open(path).map_err(error)?;
    file.seek(SeekFrom::Start(index as u64 * SIZE as u64))
        .map_err(error)?;
    let mut bytes = vec![0; description.length(size, index)];
    file.read_exact(&mut bytes).map_err(error)?;
    if digest(&bytes) != description.hashes[index] {
        return Err("Source audio changed".into());
    }
    let mut last = String::new();
    let started = Instant::now();
    let mut attempt = 0;
    while started.elapsed() < Duration::from_secs(120) {
        crate::background::check_transfer_cancelled()?;
        match client()?
            .put(format!("{}/transfers/{id}/parts/{index}", c.server))
            .bearer_auth(&c.token)
            .header("x-part-sha256", &description.hashes[index])
            .body(bytes.clone())
            .send()
        {
            Ok(res) if res.status() == reqwest::StatusCode::ACCEPTED => {
                // The on-device endpoint retains a bounded buffer until durable ACK.
                if let Err(e) = super::response(res) {
                    last = e;
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
                loop {
                    let status = control(c, "status", json!({"id":id}))?;
                    if status["status"] == "cancelled" {
                        return Err("Transfer cancelled".into());
                    }
                    if !status["parts"]["missing"]
                        .as_array()
                        .ok_or("Invalid resume state")?
                        .iter()
                        .any(|v| v.as_u64() == Some(index as u64))
                    {
                        return Ok(());
                    }
                    if started.elapsed() >= Duration::from_secs(120) {
                        return Err("Recipient timed out; received parts retained".into());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Ok(res) if res.status().is_success() => {
                let receipt = match super::response(res) {
                    Ok(value) => value,
                    Err(e) => {
                        last = e;
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                };
                if receipt["index"].as_u64() != Some(index as u64) {
                    return Err("Invalid part receipt".into());
                }
                return Ok(());
            }
            Ok(res) => {
                let status = res.status();
                let detail = super::response(res).err().unwrap_or_default();
                last = format!("Part upload HTTP {status}: {detail}");
                if !retryable(status) {
                    return Err(last);
                }
            }
            Err(e) => last = network_error(e),
        }
        std::thread::sleep(Duration::from_millis((250 * (attempt + 1)).min(2000)));
        attempt += 1;
    }
    Err(last)
}
fn get(
    c: &Connection,
    id: &str,
    index: usize,
    description: &Description,
    size: u64,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now();

    loop {
        crate::background::check_transfer_cancelled()?;
        if deadline.elapsed() > Duration::from_secs(120) {
            return Err("Sender timed out; received parts retained".into());
        }
        let result = (|| {
            let res = client()?
                .get(format!("{}/transfers/{id}/parts/{index}", c.server))
                .bearer_auth(&c.token)
                .send()
                .map_err(network_error)?;
            if res.status() == reqwest::StatusCode::NO_CONTENT || retryable(res.status()) {
                return Ok(None);
            }
            if !res.status().is_success() {
                let status = res.status();
                let detail = super::response(res).err().unwrap_or_default();
                return Err(format!("Part download HTTP {status}: {detail}"));
            }
            let length = description.length(size, index);
            let mut bytes = Vec::with_capacity(length);
            res.take((length + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(error)?;
            if bytes.len() != length || digest(&bytes) != description.hashes[index] {
                return Err("Part checksum mismatch".into());
            }
            Ok(Some(bytes))
        })();
        match result {
            Ok(Some(bytes)) => return Ok(bytes),
            Ok(None) => {}
            Err(e) => {
                if e.starts_with("Part download HTTP") {
                    return Err(e);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub fn send(
    c: &Connection,
    id: &str,
    path: &Path,
    size: u64,
    description: &Description,
    mut progress: impl FnMut(u64) -> Result<(), String>,
) -> Result<(), String> {
    let status = control(c, "status", json!({"id":id}))?;
    let missing: Vec<usize> =
        serde_json::from_value(status["parts"]["missing"].clone()).map_err(error)?;
    if missing.iter().any(|&i| i >= description.hashes.len()) {
        return Err("Invalid resume state".into());
    }
    progress(status["offset"].as_u64().unwrap_or(0))?;
    for batch in missing.chunks(WINDOW) {
        progress(
            control(c, "status", json!({"id":id}))?["offset"]
                .as_u64()
                .unwrap_or(0),
        )?;
        std::thread::scope(|scope| {
            let jobs: Vec<_> = batch
                .iter()
                .map(|&index| scope.spawn(move || put(c, id, index, description, path, size)))
                .collect();
            let mut result = Ok(());
            for job in jobs {
                if let Err(e) = job
                    .join()
                    .map_err(|_| "Multipart worker failed".to_string())?
                {
                    result = Err(e)
                }
            }
            result
        })?;
    }
    progress(size)
}
pub fn receive(
    c: &Connection,
    id: &str,
    checkpoint: &mut Checkpoint,
    mut progress: impl FnMut(u64) -> Result<(), String>,
) -> Result<(), String> {
    progress(checkpoint.received())?;
    let missing = checkpoint.missing();
    let description = checkpoint.journal.description.clone();
    let size = checkpoint.journal.size;
    for batch in missing.chunks(WINDOW) {
        progress(checkpoint.received())?;
        let result: Result<(), String> = std::thread::scope(|scope| {
            let jobs: Vec<_> = batch
                .iter()
                .map(|&index| {
                    let description = &description;
                    (
                        index,
                        scope.spawn(move || get(c, id, index, description, size)),
                    )
                })
                .collect();
            let mut first_error = None;
            for (index, job) in jobs {
                let result = (|| {
                    let bytes = job
                        .join()
                        .map_err(|_| "Multipart worker failed".to_string())??;
                    checkpoint.write(index, &bytes)?;
                    // Retry is safe: the relay's acknowledgement is idempotent.
                    control(
                        c,
                        "ackPart",
                        json!({"id":id,"index":index,"sha256":description.hashes[index]}),
                    )?;
                    progress(checkpoint.received())?;
                    Ok(())
                })();
                if let Err(e) = result {
                    if first_error.is_none() {
                        first_error = Some(e)
                    }
                }
            }
            first_error.map_or(Ok(()), Err)
        });
        result?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::rpc;
    use super::*;
    #[test]
    fn resume_rechecks_disk_and_keeps_only_good_parts() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let bytes = vec![7; SIZE + 19];
        let description = Description {
            size: SIZE,
            hashes: vec![digest(&bytes[..SIZE]), digest(&bytes[SIZE..])],
        };
        let mut cp = Checkpoint::open(
            &dir,
            "sender/source/hash",
            bytes.len() as u64,
            description.clone(),
        )
        .unwrap();
        cp.write(0, &bytes[..SIZE]).unwrap();
        cp.write(1, &bytes[SIZE..]).unwrap();
        let path = cp.path.clone();
        let _journal_path = cp.journal_path.clone();
        drop(cp);
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(SIZE as u64)).unwrap();
        file.write_all(&[9]).unwrap();
        drop(file);
        let mut cp =
            Checkpoint::open(&dir, "sender/source/hash", bytes.len() as u64, description).unwrap();
        assert_eq!(cp.verified(), vec![0]);
        assert_eq!(cp.missing(), vec![1]);
        assert!(cp.finish(&digest(&bytes)).is_err());
        assert!(cp.write(1, &[0; 19]).is_err());
        cp.write(1, &bytes[SIZE..]).unwrap();
        assert!(cp.finish(&"0".repeat(64)).is_err());
        cp.finish(&digest(&bytes)).unwrap();
        cp.remove();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cleanup_preserves_live_checkpoints_and_removes_expired_pairs() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let description = Description {
            size: SIZE,
            hashes: vec![digest(&[1; 17])],
        };
        let cp = Checkpoint::open(&dir, "expiry-test", 17, description).unwrap();
        let path = cp.path.clone();
        let journal = cp.journal_path.clone();
        drop(cp);
        cleanup(&dir);
        assert!(path.exists() && journal.exists());
        fs::File::options()
            .write(true)
            .open(&journal)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(
                    std::time::SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60),
                ),
            )
            .unwrap();
        cleanup(&dir);
        assert!(!path.exists() && !journal.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rust_multipart_resumes_after_relay_restart_without_redownloading_verified_parts() {
        use std::io::BufRead;
        use std::process::{Child, Command, Stdio};
        struct Relay(Child);
        impl Drop for Relay {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        fn start(dir: &Path, port: u16) -> Relay {
            let mut child = Command::new("node")
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/relay.mjs"))
                .env("PORT", port.to_string())
                .env("HOST", "127.0.0.1")
                .env("RELAY_STATE", dir.join("devices.json"))
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut line = String::new();
            std::io::BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert!(line.contains("Hearfolio relay"));
            Relay(child)
        }
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let relay = start(&dir, port);
        let url = format!("http://127.0.0.1:{port}");
        let register = |name: &str| {
            let value = super::super::response(
                client()
                    .unwrap()
                    .post(format!("{url}/register"))
                    .json(&json!({"name":name}))
                    .send()
                    .unwrap(),
            )
            .unwrap();
            Connection {
                server: url.clone(),
                id: value["id"].as_str().unwrap().into(),
                token: value["token"].as_str().unwrap().into(),
                local_host: false,
            }
        };
        let sender = register("Sender");
        let receiver = register("Receiver");
        let code = rpc(&receiver, "code", json!({})).unwrap()["code"].clone();
        rpc(&sender, "pair", json!({"code":code})).unwrap();
        let request = rpc(&receiver, "poll", json!({})).unwrap()["pairRequests"][0]["id"].clone();
        rpc(
            &receiver,
            "confirmPair",
            json!({"id":request,"accept":true}),
        )
        .unwrap();
        let bytes: Vec<u8> = (0..(SIZE * 6 + 13)).map(|i| (i % 251) as u8).collect();
        let source = dir.join("source.wav");
        fs::write(&source, &bytes).unwrap();
        let hash = digest(&bytes);
        let size = bytes.len() as u64;
        let description = describe(&source, &hash, size).unwrap();
        let manifest = json!({"version":1,"name":"fixture.wav","extension":"wav","sourceId":"same-recording","size":size,"sha256":hash,"transcript":"new text","createdAt":1});
        let offer = || {
            rpc(
                &sender,
                "offer",
                json!({"to":receiver.id,"manifest":manifest,"parts":description}),
            )
            .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let id = offer();
        let mut cp = Checkpoint::open(
            &dir,
            "stable sender/audio identity",
            size,
            description.clone(),
        )
        .unwrap();
        rpc(
            &receiver,
            "accept",
            json!({"id":id,"parts":{"verified":cp.verified()}}),
        )
        .unwrap();
        let c = sender.clone();
        let part = description.clone();
        let path = source.clone();
        let sid = id.clone();
        let sending = std::thread::spawn(move || send(&c, &sid, &path, size, &part, |_| Ok(())));
        assert!(receive(&receiver, &id, &mut cp, |n| if n >= SIZE as u64 {
            Err("simulated receiver interruption".into())
        } else {
            Ok(())
        })
        .is_err());
        rpc(&sender, "cancel", json!({"id":id})).unwrap();
        assert!(sending.join().unwrap().is_err());
        assert_eq!(cp.verified(), vec![0, 1, 2, 3]);
        let retained = cp.received();
        drop(cp);
        drop(relay);
        let relay = start(&dir, port);
        rpc(&receiver, "poll", json!({})).unwrap();
        rpc(&sender, "poll", json!({})).unwrap();
        let id = offer();
        let mut cp = Checkpoint::open(
            &dir,
            "stable sender/audio identity",
            size,
            description.clone(),
        )
        .unwrap();
        assert_eq!(cp.received(), retained);
        rpc(
            &receiver,
            "accept",
            json!({"id":id,"parts":{"verified":cp.verified()}}),
        )
        .unwrap();
        let missing: Vec<usize> = serde_json::from_value(
            rpc(&sender, "status", json!({"id":id})).unwrap()["parts"]["missing"].clone(),
        )
        .unwrap();
        assert_eq!(missing, vec![4, 5, 6]);
        let c = sender.clone();
        let part = description.clone();
        let path = source.clone();
        let sid = id.clone();
        let sending = std::thread::spawn(move || send(&c, &sid, &path, size, &part, |_| Ok(())));
        receive(&receiver, &id, &mut cp, |_| Ok(())).unwrap();
        sending.join().unwrap().unwrap();
        cp.finish(&hash).unwrap();
        assert_eq!(fs::read(&cp.path).unwrap(), bytes);
        rpc(&receiver, "finish", json!({"id":id})).unwrap();
        cp.remove();
        drop(relay);
        fs::remove_dir_all(dir).unwrap();
    }
}
