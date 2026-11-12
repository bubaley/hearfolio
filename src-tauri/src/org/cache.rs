//! Cloud audio kept on the device after upload or playback. A file's
//! modification time records its last use; files unused for longer than the
//! retention setting are removed (Telegram-style). Transcripts are never cached
//! here, so eviction only costs a re-download.
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

pub const RETENTION_CHOICES: [Option<u32>; 4] = [Some(7), Some(30), Some(90), None];
pub const DEFAULT_RETENTION_DAYS: u32 = 30;

pub fn valid_retention(days: Option<u32>) -> bool {
    RETENTION_CHOICES.contains(&days)
}

/// File for a cloud recording: `<remote id>.<ext>`; ids are server UUIDs.
pub fn file_name(remote_id: &str, extension: &str) -> Option<String> {
    let id_ok = !remote_id.is_empty()
        && remote_id.len() <= 64
        && remote_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let ext_ok = !extension.is_empty()
        && extension.len() <= 5
        && extension.bytes().all(|b| b.is_ascii_alphanumeric());
    (id_ok && ext_ok).then(|| format!("{remote_id}.{}", extension.to_ascii_lowercase()))
}

pub fn find(dir: &Path, remote_id: &str) -> Option<PathBuf> {
    let prefix = format!("{remote_id}.");
    fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().into_string().ok()?;
        (name.starts_with(&prefix) && !name.ends_with(".part") && entry.path().is_file())
            .then(|| entry.path())
    })
}

/// Mark a cached file as just used, postponing its eviction.
pub fn touch(path: &Path) {
    if let Ok(file) = fs::OpenOptions::new().append(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

#[derive(serde::Serialize, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub files: u64,
    pub bytes: u64,
}

pub fn usage(dir: &Path) -> Usage {
    let mut usage = Usage::default();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if let Ok(metadata) = entry.metadata() {
            if metadata.is_file() {
                usage.files += 1;
                usage.bytes += metadata.len();
            }
        }
    }
    usage
}

/// Remove cached audio unused for `days` (None keeps everything). Interrupted
/// downloads (`.part`) older than a day are removed too. Returns removed count.
pub fn evict(dir: &Path, days: Option<u32>, now: SystemTime) -> usize {
    let mut removed = 0;
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let partial = path.extension().is_some_and(|ext| ext == "part");
        let limit = if partial {
            Some(Duration::from_secs(86_400))
        } else {
            days.map(|days| Duration::from_secs(u64::from(days) * 86_400))
        };
        let Some(limit) = limit else { continue };
        let age = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .unwrap_or_default();
        if age > limit && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub fn clear(dir: &Path) -> usize {
    evict(dir, Some(0), SystemTime::now() + Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hearfolio-cache-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn aged(dir: &Path, name: &str, days: u64) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, b"audio").unwrap();
        let file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(days * 86_400))
            .unwrap();
        path
    }

    #[test]
    fn eviction_follows_last_use_and_retention() {
        let dir = dir();
        let old = aged(&dir, "0a1b-old.m4a", 40);
        let recent = aged(&dir, "0a1b-recent.m4a", 3);
        let used = aged(&dir, "0a1b-used.m4a", 40);
        let partial = aged(&dir, "0a1b-x.m4a.part", 2);
        touch(&used);
        assert_eq!(
            usage(&dir),
            Usage {
                files: 4,
                bytes: 20
            }
        );
        assert_eq!(
            evict(&dir, None, SystemTime::now()),
            1,
            "forever keeps audio"
        );
        assert!(!partial.exists());
        assert!(old.exists());
        assert_eq!(evict(&dir, Some(30), SystemTime::now()), 1);
        assert!(!old.exists() && recent.exists() && used.exists());
        assert_eq!(
            evict(
                &dir,
                Some(7),
                SystemTime::now() + Duration::from_secs(10 * 86_400)
            ),
            2
        );
        assert_eq!(usage(&dir), Usage::default());
        aged(&dir, "a.m4a", 0);
        fs::write(dir.join("b.m4a.part"), b"downloading").unwrap();
        assert_eq!(clear(&dir), 1, "an in-progress download is left alone");
        assert!(dir.join("b.m4a.part").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cache_names_are_confined_to_server_ids() {
        assert_eq!(file_name("3f2a-11", "M4A").as_deref(), Some("3f2a-11.m4a"));
        assert_eq!(file_name("../etc", "m4a"), None);
        assert_eq!(file_name("abc", "m4a/../x"), None);
        assert!(valid_retention(Some(30)) && valid_retention(None) && !valid_retention(Some(5)));
        let dir = dir();
        fs::write(dir.join("ab-1.m4a.part"), b"x").unwrap();
        assert_eq!(find(&dir, "ab-1"), None);
        fs::write(dir.join("ab-1.m4a"), b"x").unwrap();
        assert_eq!(find(&dir, "ab-1"), Some(dir.join("ab-1.m4a")));
        fs::remove_dir_all(dir).unwrap();
    }
}
