//! Completed cloud chunks are paid work. Each one is committed to a private
//! progress file so a user-started retry of the same record, configuration and
//! source audio can skip it. Nothing here starts a request on its own.
use crate::cloud::RecognitionConfig;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

// Bump when chunk boundaries change so stale parts are never stitched together.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Source {
    path: String,
    size: u64,
    modified: u64,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CloudProgress {
    version: u32,
    record_id: String,
    configuration: RecognitionConfig,
    source: Source,
    total: u64,
    parts: Vec<String>,
}

fn source(input: &Path) -> Option<Source> {
    let metadata = fs::metadata(input).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(Source {
        path: input.to_string_lossy().into_owned(),
        size: metadata.len(),
        modified,
    })
}
pub fn path(root: &Path, record_id: &str) -> PathBuf {
    root.join("temp")
        .join(format!("{record_id}-cloud-progress.json"))
}
fn read(root: &Path, record_id: &str) -> Option<CloudProgress> {
    let bytes = fs::read(path(root, record_id)).ok()?;
    if bytes.len() > 64 * 1024 * 1024 {
        return None;
    }
    let progress: CloudProgress = serde_json::from_slice(&bytes).ok()?;
    (progress.version == VERSION && progress.record_id == record_id).then_some(progress)
}
fn still_valid(progress: &CloudProgress) -> bool {
    source(Path::new(&progress.source.path)).is_some_and(|current| current == progress.source)
        && progress.parts.len() as u64 <= progress.total
}
impl CloudProgress {
    /// Starts a new attempt, reusing committed parts only when every input that
    /// determines them is unchanged.
    pub fn begin(
        root: &Path,
        record_id: &str,
        configuration: &RecognitionConfig,
        input: &Path,
        total: u64,
    ) -> Result<Self, String> {
        let source = source(input).ok_or("errors.audioMissing")?;
        if let Some(previous) = read(root, record_id) {
            if previous.configuration == *configuration
                && previous.source == source
                && previous.total == total
                && still_valid(&previous)
            {
                return Ok(previous);
            }
        }
        let _ = fs::remove_file(path(root, record_id));
        Ok(Self {
            version: VERSION,
            record_id: record_id.into(),
            configuration: configuration.clone(),
            source,
            total,
            parts: Vec::new(),
        })
    }
    pub fn parts(&self) -> &[String] {
        &self.parts
    }
    pub fn commit(&mut self, root: &Path, text: String) -> Result<(), String> {
        self.parts.push(text);
        write_atomic(&path(root, &self.record_id), self)
    }
}
fn write_atomic(target: &Path, value: &CloudProgress) -> Result<(), String> {
    use std::io::Write;
    let temp = target.with_extension("json.tmp");
    let bytes = serde_json::to_vec(value).map_err(|_| "errors.operationFailed")?;
    let mut options = fs::OpenOptions::new();
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
    fs::rename(temp, target).map_err(|_| "errors.operationFailed".into())
}
/// `{done,total}` for an unfinished attempt whose source audio is unchanged.
pub fn resumable(root: &Path, record_id: &str) -> Option<serde_json::Value> {
    let progress = read(root, record_id).filter(still_valid)?;
    (!progress.parts.is_empty()).then(|| {
        serde_json::json!({
            "done": progress.parts.len(),
            "total": progress.total,
            "configuration": progress.configuration,
        })
    })
}
pub fn remove(root: &Path, record_id: &str) {
    let _ = fs::remove_file(path(root, record_id));
}
pub fn remove_all(root: &Path) {
    if let Ok(files) = fs::read_dir(root.join("temp")) {
        for entry in files.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .ends_with("-cloud-progress.json")
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hearfolio-resume-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("temp")).unwrap();
        dir
    }
    fn config(model: &str) -> RecognitionConfig {
        RecognitionConfig {
            provider: "openrouter".into(),
            model: model.into(),
            mode: "streaming".into(),
        }
    }
    #[test]
    fn committed_parts_resume_only_for_identical_configuration_source_and_chunking() {
        let root = root();
        let input = root.join("audio.wav");
        fs::write(&input, b"audio").unwrap();
        let mut progress = CloudProgress::begin(&root, "rec", &config("a"), &input, 3).unwrap();
        assert!(progress.parts().is_empty());
        progress.commit(&root, "one".into()).unwrap();
        progress.commit(&root, "two".into()).unwrap();
        assert_eq!(
            resumable(&root, "rec").unwrap(),
            serde_json::json!({"done":2,"total":3,"configuration":config("a")})
        );
        let again = CloudProgress::begin(&root, "rec", &config("a"), &input, 3).unwrap();
        assert_eq!(again.parts(), ["one", "two"]);
        // A different chunk count means different boundaries.
        assert!(CloudProgress::begin(&root, "rec", &config("a"), &input, 4)
            .unwrap()
            .parts()
            .is_empty());
        assert!(resumable(&root, "rec").is_none());
        let mut progress = CloudProgress::begin(&root, "rec", &config("a"), &input, 3).unwrap();
        progress.commit(&root, "one".into()).unwrap();
        assert!(CloudProgress::begin(&root, "rec", &config("b"), &input, 3)
            .unwrap()
            .parts()
            .is_empty());
        let mut progress = CloudProgress::begin(&root, "rec", &config("a"), &input, 3).unwrap();
        progress.commit(&root, "one".into()).unwrap();
        fs::write(&input, b"changed audio").unwrap();
        assert!(resumable(&root, "rec").is_none());
        assert!(CloudProgress::begin(&root, "rec", &config("a"), &input, 3)
            .unwrap()
            .parts()
            .is_empty());
        let mut progress = CloudProgress::begin(&root, "rec", &config("a"), &input, 3).unwrap();
        progress.commit(&root, "one".into()).unwrap();
        remove_all(&root);
        assert!(!path(&root, "rec").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
