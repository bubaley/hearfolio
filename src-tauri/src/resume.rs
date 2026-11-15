//! Completed cloud chunks are paid work. Each one is committed to a private
//! progress file so a user-started retry of the same record, configuration and
//! source audio can skip it. Nothing here starts a request on its own.
use crate::cloud::RecognitionConfig;
use crate::phrases::{coarse, Phrase};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

// Format of the progress file. 1 and 2 were the slicing (`audio::SLICING`)
// that cut every finished part `chunk_seconds` long from the start of the
// audio. 3 records where every part ends and what it said with times: a retry
// cuts the rest of the audio from the end of the last part, with whatever
// segment length it uses now. Files of 1 and 2 are read as 3, so an update
// never throws away finished, paid parts.
const FORMAT: u32 = 3;
const VERSIONS: std::ops::RangeInclusive<u32> = 1..=FORMAT;

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
    /// Formats 1 and 2: the length of every part. Format 3: the segment length
    /// of the last attempt (informational).
    #[serde(default)]
    chunk_seconds: u32,
    /// Parts finished plus parts left when the last attempt started.
    total: u64,
    parts: Vec<String>,
    /// Indexes of parts the model could not finish; reported with the result.
    #[serde(default)]
    incomplete: Vec<u64>,
    /// Where each part ends, milliseconds from the start of the audio.
    #[serde(default)]
    ends_ms: Vec<u64>,
    /// Timed phrases of each part.
    #[serde(default)]
    phrases: Vec<Vec<Phrase>>,
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
/// A file of an older format becomes format 3: its parts were cut back to
/// back, `chunk_seconds` each, and get one phrase each.
fn upgrade(mut progress: CloudProgress) -> Option<CloudProgress> {
    if progress.version < FORMAT {
        if progress.chunk_seconds == 0 {
            return None;
        }
        let length = u64::from(progress.chunk_seconds) * 1000;
        progress.ends_ms = (1..=progress.parts.len() as u64)
            .map(|count| count * length)
            .collect();
        progress.phrases = Vec::new();
        progress.version = FORMAT;
    }
    if progress.ends_ms.len() != progress.parts.len()
        || progress.ends_ms.windows(2).any(|pair| pair[1] < pair[0])
    {
        return None;
    }
    // Parts without phrases (an older file) get one phrase each.
    let mut start = 0;
    for (index, end) in progress.ends_ms.iter().copied().enumerate() {
        if progress.phrases.len() <= index {
            progress
                .phrases
                .push(coarse(start, Some(end - start), &progress.parts[index]));
        }
        start = end;
    }
    progress.phrases.truncate(progress.parts.len());
    Some(progress)
}
fn read(root: &Path, record_id: &str) -> Option<CloudProgress> {
    let bytes = fs::read(path(root, record_id)).ok()?;
    if bytes.len() > 64 * 1024 * 1024 {
        return None;
    }
    let progress: CloudProgress = serde_json::from_slice(&bytes).ok()?;
    if !VERSIONS.contains(&progress.version) || progress.record_id != record_id {
        return None;
    }
    upgrade(progress)
}
fn still_valid(progress: &CloudProgress) -> bool {
    source(Path::new(&progress.source.path)).is_some_and(|current| current == progress.source)
        && progress.parts.len() as u64 <= progress.total
}
/// Parts that a new attempt would really continue.
fn continuable(progress: &CloudProgress) -> bool {
    still_valid(progress)
        && !progress.parts.is_empty()
        && (progress.parts.len() as u64) < progress.total
}
impl CloudProgress {
    /// Starts a new attempt, reusing committed parts only when the record,
    /// configuration and source audio are unchanged. The audio left to do
    /// starts at `resume_ms`; `plan` then records how many pieces it makes.
    pub fn begin(
        root: &Path,
        record_id: &str,
        configuration: &RecognitionConfig,
        input: &Path,
    ) -> Result<Self, String> {
        let source = source(input).ok_or("errors.audioMissing")?;
        if let Some(previous) = read(root, record_id) {
            if previous.configuration == *configuration
                && previous.source == source
                && still_valid(&previous)
                && !previous.parts.is_empty()
            {
                return Ok(previous);
            }
        }
        let _ = fs::remove_file(path(root, record_id));
        Ok(Self {
            version: FORMAT,
            record_id: record_id.into(),
            configuration: configuration.clone(),
            source,
            chunk_seconds: 0,
            total: 0,
            parts: Vec::new(),
            incomplete: Vec::new(),
            ends_ms: Vec::new(),
            phrases: Vec::new(),
        })
    }
    /// Every part is done: an attempt stopped between the last request and
    /// saving the transcript. Nothing is cut or sent again.
    pub fn complete(&self) -> bool {
        !self.parts.is_empty() && self.parts.len() as u64 >= self.total
    }
    /// Milliseconds of audio the finished parts cover.
    pub fn resume_ms(&self) -> u64 {
        self.ends_ms.last().copied().unwrap_or(0)
    }
    /// The pieces this attempt cut from `resume_ms`, `chunk_seconds` each.
    pub fn plan(&mut self, chunk_seconds: u32, remaining: u64) {
        self.chunk_seconds = chunk_seconds;
        self.total = self.parts.len() as u64 + remaining;
    }
    pub fn parts(&self) -> &[String] {
        &self.parts
    }
    pub fn incomplete(&self) -> &[u64] {
        &self.incomplete
    }
    pub fn ends_ms(&self) -> &[u64] {
        &self.ends_ms
    }
    /// Timed phrases of every finished part, in order.
    pub fn phrases(&self) -> Vec<Phrase> {
        self.phrases.iter().flatten().cloned().collect()
    }
    pub fn commit(
        &mut self,
        root: &Path,
        text: String,
        incomplete: bool,
        end_ms: u64,
        phrases: Vec<Phrase>,
    ) -> Result<(), String> {
        if incomplete {
            self.incomplete.push(self.parts.len() as u64);
        }
        self.parts.push(text);
        self.ends_ms.push(end_ms.max(self.resume_ms()));
        self.phrases.push(phrases);
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
pub fn unfinished(root: &Path) -> Vec<Unfinished> {
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
        if let Some(progress) = read(root, id).filter(continuable) {
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
/// source audio is unchanged and parts remain.
pub fn resumable(root: &Path, record_id: &str) -> Option<serde_json::Value> {
    let progress = read(root, record_id).filter(continuable)?;
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
    fn said(start_ms: u64, end_ms: u64, text: &str) -> Vec<Phrase> {
        coarse(start_ms, Some(end_ms - start_ms), text)
    }
    #[test]
    fn committed_parts_resume_only_for_identical_configuration_and_source() {
        let root = root();
        let input = root.join("audio.wav");
        fs::write(&input, b"audio").unwrap();
        let mut progress = CloudProgress::begin(&root, "rec", &config("a"), &input).unwrap();
        assert!(progress.parts().is_empty());
        assert_eq!(progress.resume_ms(), 0);
        progress.plan(30, 3);
        progress
            .commit(&root, "one".into(), false, 30_000, said(0, 30_000, "one"))
            .unwrap();
        progress
            .commit(
                &root,
                "two".into(),
                true,
                60_000,
                said(30_000, 60_000, "two"),
            )
            .unwrap();
        assert_eq!(
            resumable(&root, "rec").unwrap(),
            serde_json::json!({"done":2,"total":3,"configuration":config("a")})
        );
        assert_eq!(
            unfinished(&root),
            [Unfinished {
                id: "rec".into(),
                done: 2,
                total: 3
            }]
        );
        let again = CloudProgress::begin(&root, "rec", &config("a"), &input).unwrap();
        assert_eq!(again.parts(), ["one", "two"]);
        assert_eq!(
            again.resume_ms(),
            60_000,
            "the rest starts where part two ends"
        );
        assert_eq!(
            again.phrases(),
            [said(0, 30_000, "one"), said(30_000, 60_000, "two")].concat()
        );
        assert_eq!(
            again.incomplete(),
            [1],
            "the unfinished passage survives a restart"
        );
        // Another model starts over.
        assert!(CloudProgress::begin(&root, "rec", &config("b"), &input)
            .unwrap()
            .parts()
            .is_empty());
        assert!(resumable(&root, "rec").is_none());
        // So does changed audio.
        let mut progress = CloudProgress::begin(&root, "rec", &config("a"), &input).unwrap();
        progress.plan(30, 3);
        progress
            .commit(&root, "one".into(), false, 30_000, Vec::new())
            .unwrap();
        fs::write(&input, b"changed audio").unwrap();
        assert!(resumable(&root, "rec").is_none());
        assert!(CloudProgress::begin(&root, "rec", &config("a"), &input)
            .unwrap()
            .parts()
            .is_empty());
        let mut progress = CloudProgress::begin(&root, "rec", &config("a"), &input).unwrap();
        progress.plan(30, 3);
        progress
            .commit(&root, "one".into(), false, 30_000, Vec::new())
            .unwrap();
        remove_all(&root);
        assert!(!path(&root, "rec").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_retry_with_another_segment_length_continues_from_the_end_of_the_last_part() {
        let root = root();
        let input = root.join("audio.wav");
        fs::write(&input, b"audio").unwrap();
        let mut progress = CloudProgress::begin(&root, "rec", &config("a"), &input).unwrap();
        progress.plan(120, 4);
        progress
            .commit(&root, "one".into(), false, 120_000, said(0, 120_000, "one"))
            .unwrap();
        // The user changes the segment length (or a model gets longer pieces).
        let mut resumed = CloudProgress::begin(&root, "rec", &config("a"), &input).unwrap();
        assert_eq!(resumed.resume_ms(), 120_000);
        resumed.plan(300, 2);
        assert_eq!(
            resumable(&root, "rec").unwrap()["total"],
            4,
            "not saved before a part is done"
        );
        resumed
            .commit(
                &root,
                "two".into(),
                false,
                420_000,
                said(120_000, 420_000, "two"),
            )
            .unwrap();
        assert_eq!(resumable(&root, "rec").unwrap()["total"], 3);
        assert_eq!(resumed.ends_ms(), [120_000, 420_000]);
        // An end can never move backwards.
        resumed
            .commit(&root, "three".into(), false, 1, Vec::new())
            .unwrap();
        assert_eq!(resumed.ends_ms(), [120_000, 420_000, 420_000]);
        assert!(resumed.complete());
        assert!(resumable(&root, "rec").is_none(), "every part is done");
        // A finished but unsaved attempt is reused as it is.
        assert!(CloudProgress::begin(&root, "rec", &config("a"), &input)
            .unwrap()
            .complete());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn parts_paid_for_before_an_update_resume_from_where_they_end() {
        let root = root();
        let input = root.join("audio.wav");
        fs::write(&input, b"audio").unwrap();
        let source = source(&input).unwrap();
        // Written by the previous version: fixed 30 s parts, no times, no phrases.
        for version in [1, 2] {
            fs::write(
                path(&root, "rec"),
                serde_json::to_vec(&serde_json::json!({
                    "version": version, "recordId": "rec", "configuration": config("a"),
                    "source": source, "chunkSeconds": 30, "total": 5,
                    "parts": ["one", "two", "", "four"], "incomplete": [2]
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(resumable(&root, "rec").unwrap()["done"], 4);
            assert_eq!(
                unfinished(&root).len(),
                1,
                "offered whatever the segment length is now"
            );
            let mut resumed = CloudProgress::begin(&root, "rec", &config("a"), &input).unwrap();
            assert_eq!(resumed.parts().len(), 4, "nothing paid for is thrown away");
            assert_eq!(resumed.resume_ms(), 120_000);
            assert_eq!(resumed.ends_ms(), [30_000, 60_000, 90_000, 120_000]);
            assert_eq!(resumed.incomplete(), [2]);
            assert_eq!(
                resumed.phrases(),
                [
                    said(0, 30_000, "one"),
                    said(30_000, 60_000, "two"),
                    said(90_000, 120_000, "four")
                ]
                .concat()
            );
            resumed.plan(120, 1);
            resumed
                .commit(
                    &root,
                    "five".into(),
                    false,
                    127_400,
                    said(120_000, 127_400, "five"),
                )
                .unwrap();
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(path(&root, "rec")).unwrap()).unwrap();
            assert_eq!(saved["version"], 3);
            assert_eq!(
                saved["endsMs"],
                serde_json::json!([30_000, 60_000, 90_000, 120_000, 127_400])
            );
        }
        // A very old file without a segment length cannot be placed in time.
        fs::write(
            path(&root, "rec"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "recordId": "rec", "configuration": config("a"),
                "source": source, "total": 3, "parts": ["one"]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(resumable(&root, "rec").is_none());
        assert!(CloudProgress::begin(&root, "rec", &config("a"), &input)
            .unwrap()
            .parts()
            .is_empty());
        // Inconsistent times are not trusted.
        fs::write(
            path(&root, "rec"),
            serde_json::to_vec(&serde_json::json!({
                "version": 3, "recordId": "rec", "configuration": config("a"),
                "source": source, "total": 3, "parts": ["one", "two"], "endsMs": [5000]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(resumable(&root, "rec").is_none());
        // A file from a future format is left alone.
        fs::write(
            path(&root, "rec"),
            serde_json::to_vec(&serde_json::json!({
                "version": 4, "recordId": "rec", "configuration": config("a"),
                "source": source, "total": 3, "parts": ["one"], "endsMs": [5000]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(resumable(&root, "rec").is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
