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

// The version is the slicing (`audio::SLICING`) the parts were cut with. An
// attempt started by an older build is continued with its own slicing, so an
// update never throws away finished, paid parts.
const VERSIONS: std::ops::RangeInclusive<u32> = 1..=crate::audio::SLICING;

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
    #[serde(default)]
    chunk_seconds: u32,
    total: u64,
    parts: Vec<String>,
    /// Indexes of parts the model could not finish; reported with the result.
    #[serde(default)]
    incomplete: Vec<u64>,
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
    (VERSIONS.contains(&progress.version) && progress.record_id == record_id).then_some(progress)
}
fn still_valid(progress: &CloudProgress) -> bool {
    source(Path::new(&progress.source.path)).is_some_and(|current| current == progress.source)
        && progress.parts.len() as u64 <= progress.total
}
/// Parts that a new attempt with this segment length would really continue.
fn continuable(progress: &CloudProgress, chunk_seconds: u32) -> bool {
    still_valid(progress)
        && progress.chunk_seconds == chunk_seconds
        && !progress.parts.is_empty()
        && (progress.parts.len() as u64) < progress.total
}
/// The slicing a new attempt must use: the one its unfinished parts were cut
/// with, otherwise the current one.
pub fn slicing(
    root: &Path,
    record_id: &str,
    configuration: &RecognitionConfig,
    input: &Path,
    chunk_seconds: u32,
) -> u32 {
    read(root, record_id)
        .filter(|previous| {
            previous.configuration == *configuration
                && source(input).is_some_and(|current| current == previous.source)
                && continuable(previous, chunk_seconds)
        })
        .map_or(crate::audio::SLICING, |previous| previous.version)
}
impl CloudProgress {
    /// Starts a new attempt, reusing committed parts only when every input that
    /// determines them is unchanged.
    pub fn begin(
        root: &Path,
        record_id: &str,
        configuration: &RecognitionConfig,
        input: &Path,
        chunk_seconds: u32,
        total: u64,
        slicing: u32,
    ) -> Result<Self, String> {
        let source = source(input).ok_or("errors.audioMissing")?;
        if let Some(previous) = read(root, record_id) {
            if previous.version == slicing
                && previous.configuration == *configuration
                && previous.source == source
                && previous.chunk_seconds == chunk_seconds
                && previous.total == total
                && still_valid(&previous)
            {
                return Ok(previous);
            }
        }
        let _ = fs::remove_file(path(root, record_id));
        Ok(Self {
            version: slicing,
            record_id: record_id.into(),
            configuration: configuration.clone(),
            source,
            chunk_seconds,
            total,
            parts: Vec::new(),
            incomplete: Vec::new(),
        })
    }
    pub fn parts(&self) -> &[String] {
        &self.parts
    }
    pub fn incomplete(&self) -> &[u64] {
        &self.incomplete
    }
    pub fn commit(&mut self, root: &Path, text: String, incomplete: bool) -> Result<(), String> {
        if incomplete {
            self.incomplete.push(self.parts.len() as u64);
        }
        self.parts.push(text);
        write_atomic(&path(root, &self.record_id), self)
    }
}
/// Every record with an attempt that stopped part-way, for the record list.
#[derive(Serialize, Debug, PartialEq)]
pub struct Unfinished {
    pub id: String,
    pub done: u64,
    pub total: u64,
}
pub fn unfinished(root: &Path, chunk_seconds: u32) -> Vec<Unfinished> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root.join("temp"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_suffix("-cloud-progress.json"))
        else {
            continue;
        };
        if let Some(progress) =
            read(root, id).filter(|progress| continuable(progress, chunk_seconds))
        {
            found.push(Unfinished {
                id: id.to_owned(),
                done: progress.parts.len() as u64,
                total: progress.total,
            });
        }
    }
    found
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
/// `{done,total}` for an unfinished attempt that a retry would continue: the
/// source audio and the segment length are unchanged and parts remain.
pub fn resumable(root: &Path, record_id: &str, chunk_seconds: u32) -> Option<serde_json::Value> {
    let progress = read(root, record_id).filter(|progress| continuable(progress, chunk_seconds))?;
    Some(serde_json::json!({
        "done": progress.parts.len(),
        "total": progress.total,
        "configuration": progress.configuration,
    }))
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
        let mut progress =
            CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 3, 2).unwrap();
        assert!(progress.parts().is_empty());
        progress.commit(&root, "one".into(), false).unwrap();
        progress.commit(&root, "two".into(), true).unwrap();
        assert_eq!(
            resumable(&root, "rec", 30).unwrap(),
            serde_json::json!({"done":2,"total":3,"configuration":config("a")})
        );
        assert_eq!(
            unfinished(&root, 30),
            [Unfinished {
                id: "rec".into(),
                done: 2,
                total: 3
            }]
        );
        let again = CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 3, 2).unwrap();
        assert_eq!(again.parts(), ["one", "two"]);
        assert_eq!(
            again.incomplete(),
            [1],
            "the unfinished passage survives a restart"
        );
        // A progress file written before this field existed still resumes.
        let legacy = fs::read_to_string(path(&root, "rec"))
            .unwrap()
            .replace(",\"incomplete\":[1]", "");
        assert!(!legacy.contains("incomplete"));
        fs::write(path(&root, "rec"), legacy).unwrap();
        let legacy = CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 3, 2).unwrap();
        assert_eq!(legacy.parts(), ["one", "two"]);
        assert!(legacy.incomplete().is_empty());
        // A different chunk count means different boundaries.
        assert!(
            CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 4, 2)
                .unwrap()
                .parts()
                .is_empty()
        );
        assert!(resumable(&root, "rec", 30).is_none());
        let mut progress =
            CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 3, 2).unwrap();
        progress.commit(&root, "one".into(), false).unwrap();
        assert!(
            CloudProgress::begin(&root, "rec", &config("b"), &input, 30, 3, 2)
                .unwrap()
                .parts()
                .is_empty()
        );
        let mut progress =
            CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 3, 2).unwrap();
        progress.commit(&root, "one".into(), false).unwrap();
        fs::write(&input, b"changed audio").unwrap();
        assert!(resumable(&root, "rec", 30).is_none());
        assert!(
            CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 3, 2)
                .unwrap()
                .parts()
                .is_empty()
        );
        let mut progress =
            CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 3, 2).unwrap();
        progress.commit(&root, "one".into(), false).unwrap();
        remove_all(&root);
        assert!(!path(&root, "rec").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn parts_paid_for_before_an_update_resume_with_the_slicing_that_cut_them() {
        let root = root();
        let input = root.join("audio.wav");
        fs::write(&input, b"audio").unwrap();
        // The old build cut five pieces (the last a short remainder) and failed on the fifth.
        let mut old = CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 5, 1).unwrap();
        for part in ["one", "two", "three", "four"] {
            old.commit(&root, part.into(), false).unwrap();
        }
        assert_eq!(slicing(&root, "rec", &config("a"), &input, 30), 1);
        assert_eq!(resumable(&root, "rec", 30).unwrap()["done"], 4);
        let resumed = CloudProgress::begin(&root, "rec", &config("a"), &input, 30, 5, 1).unwrap();
        assert_eq!(resumed.parts().len(), 4, "nothing paid for is thrown away");
        // What is offered is what a retry would do: another segment length or
        // model starts over with the current slicing, and says so.
        assert!(resumable(&root, "rec", 120).is_none());
        assert!(unfinished(&root, 120).is_empty());
        assert_eq!(slicing(&root, "rec", &config("a"), &input, 120), 2);
        assert_eq!(slicing(&root, "rec", &config("b"), &input, 30), 2);
        // A run with every part done is finished or failed for good, never "resumable".
        let mut done = CloudProgress::begin(&root, "full", &config("a"), &input, 30, 1, 2).unwrap();
        done.commit(&root, String::new(), true).unwrap();
        assert!(resumable(&root, "full", 30).is_none());
        assert_eq!(slicing(&root, "full", &config("a"), &input, 30), 2);
        fs::remove_dir_all(root).unwrap();
    }
}
