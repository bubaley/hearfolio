//! Local mirror of an organization's recordings, kept current with
//! `GET /recordings/sync?cursor=` (items + tombstones).
//!
//! Storage, per organization (`library/<connection>/<organization>/`):
//! - `index.json` — cursor and list fields only (no transcripts), written once
//!   per sync run; this is all the list screen reads;
//! - `items/<id>.json` — one recording's full detail (transcript, segments) and
//!   its summaries, written only when that recording changed or was opened.
//!
//! JSON files instead of SQLite: the data is a per-organization cache that the
//! server can always rebuild, access is "read the list / read one recording",
//! and no new native dependency is needed on four platforms.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

/// Serializes library files; separate from the queue/connection lock and
/// never held across network requests.
static LIBRARY_LOCK: Mutex<()> = Mutex::new(());
pub fn lock() -> MutexGuard<'static, ()> {
    LIBRARY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LibraryIndex {
    #[serde(default)]
    pub cursor: Option<String>,
    /// List fields only, keyed by recording id.
    #[serde(default)]
    pub items: BTreeMap<String, Value>,
    #[serde(default)]
    pub synced_at: Option<u64>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Detail {
    #[serde(default)]
    pub recording: Value,
    /// `GET /recordings/{id}/results`; `None` until fetched once.
    #[serde(default)]
    pub results: Option<Vec<Value>>,
}

#[derive(Deserialize, Debug, Default)]
pub struct SyncPage {
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub deleted_ids: Vec<String>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub has_more: bool,
}

/// What a merge changed: full recordings to store as details, ids to drop.
#[derive(Default, Debug)]
pub struct Change {
    pub upserts: Vec<Value>,
    pub removed: Vec<String>,
}
impl Change {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.removed.is_empty()
    }
}

fn updated_at(item: &Value) -> &str {
    item.get("updated_at").and_then(Value::as_str).unwrap_or("")
}

/// List row of a recording: everything but the transcript.
pub fn summarize(item: &Value, results_count: Option<usize>) -> Value {
    let mut row = item.clone();
    if let Some(object) = row.as_object_mut() {
        let words = object
            .get("transcript_text")
            .and_then(Value::as_str)
            .map(|text| text.split_whitespace().count());
        object.remove("transcript_text");
        object.remove("transcript_segments");
        if let Some(words) = words {
            object.insert("transcript_words".into(), words.into());
        }
        if let Some(count) = results_count {
            object.insert("results_count".into(), count.into());
        }
    }
    row
}

/// Insert one recording unless the stored copy is newer (an upload response
/// can race with a sync page). ISO-8601 UTC timestamps compare as strings.
pub fn upsert(index: &mut LibraryIndex, item: &Value, change: &mut Change) -> bool {
    let Some(id) = item.get("id").and_then(Value::as_str).map(str::to_owned) else {
        return false;
    };
    if item.get("deleted_at").is_some_and(|value| !value.is_null()) {
        change.removed.push(id.clone());
        return index.items.remove(&id).is_some();
    }
    let previous = index.items.get(&id);
    if let Some(existing) = previous {
        let (old, new) = (updated_at(existing), updated_at(item));
        if !old.is_empty() && !new.is_empty() && old > new {
            return false;
        }
    }
    let results = previous
        .and_then(|row| row["results_count"].as_u64())
        .map(|n| n as usize);
    let mut row = summarize(item, results);
    // A detail without transcript fields keeps the stored word count.
    if item.get("transcript_text").is_none() {
        if let (Some(object), Some(words)) = (
            row.as_object_mut(),
            previous.and_then(|row| row.get("transcript_words")),
        ) {
            object.insert("transcript_words".into(), words.clone());
        }
    }
    if previous == Some(&row) {
        return false;
    }
    index.items.insert(id, row);
    change.upserts.push(item.clone());
    true
}

/// Apply one sync page. Tombstones of recordings we never had are ignored;
/// the cursor only moves forward.
pub fn merge(index: &mut LibraryIndex, page: SyncPage, now: u64, change: &mut Change) {
    for item in &page.items {
        upsert(index, item, change);
    }
    for id in page.deleted_ids {
        if index.items.remove(&id).is_some() {
            change.removed.push(id);
        }
    }
    if let Some(cursor) = page.next_cursor.filter(|cursor| !cursor.is_empty()) {
        index.cursor = Some(cursor);
    }
    index.synced_at = Some(now);
}

/// List rows, newest first.
pub fn summaries(index: &LibraryIndex) -> Vec<Value> {
    let mut items: Vec<Value> = index.items.values().cloned().collect();
    let key = |item: &Value| {
        item.get("recorded_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    items.sort_by_key(|item| std::cmp::Reverse(key(item)));
    items
}

// ---------------------------------------------------------------- files

pub fn dir(connection: &str, organization: &str) -> Result<PathBuf, String> {
    super::valid_id(connection)?;
    super::valid_id(organization)?;
    Ok(super::root()?
        .join("library")
        .join(connection)
        .join(organization))
}
fn detail_path(dir: &Path, id: &str) -> Result<PathBuf, String> {
    super::valid_id(id)?;
    Ok(dir.join("items").join(format!("{id}.json")))
}

pub fn read_index(dir: &Path) -> LibraryIndex {
    fs::read(dir.join("index.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
pub fn write_index(dir: &Path, index: &LibraryIndex) -> Result<(), String> {
    super::write_private_json(&dir.join("index.json"), index)
}
pub fn read_detail(dir: &Path, id: &str) -> Option<Detail> {
    fs::read(detail_path(dir, id).ok()?)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}
pub fn write_detail(dir: &Path, id: &str, detail: &Detail) -> Result<(), String> {
    super::write_private_json(&detail_path(dir, id)?, detail)
}

/// Persist a change: details of changed recordings (keeping fetched results),
/// removal of tombstoned ones.
pub fn apply_change(dir: &Path, change: &Change) -> Result<(), String> {
    for item in &change.upserts {
        let Some(id) = item["id"].as_str() else {
            continue;
        };
        let mut detail = read_detail(dir, id).unwrap_or_default();
        // A list-only copy (no transcript fields) keeps the stored transcript.
        if item.get("transcript_text").is_none() {
            if let (Some(target), Some(object)) = (detail.recording.as_object(), item.as_object()) {
                let mut merged = target.clone();
                merged.extend(object.clone());
                detail.recording = Value::Object(merged);
            } else {
                detail.recording = item.clone();
            }
        } else {
            detail.recording = item.clone();
        }
        write_detail(dir, id, &detail)?;
    }
    for id in &change.removed {
        if let Ok(path) = detail_path(dir, id) {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

/// Remember summaries of a recording (and their count in its list row).
pub fn store_results(
    dir: &Path,
    index: &mut LibraryIndex,
    id: &str,
    results: Vec<Value>,
) -> Result<(), String> {
    if let Some(row) = index.items.get_mut(id).and_then(Value::as_object_mut) {
        row.insert("results_count".into(), results.len().into());
    }
    let mut detail = read_detail(dir, id).unwrap_or_default();
    detail.results = Some(results);
    write_detail(dir, id, &detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str, updated: &str, title: &str) -> Value {
        json!({"id":id,"updated_at":updated,"recorded_at":updated,"title":title,"deleted_at":null,"transcript_text":"one two three","transcript_segments":[]})
    }

    #[test]
    fn sync_pages_merge_with_tombstones_and_cursor() {
        let mut index = LibraryIndex::default();
        let mut change = Change::default();
        merge(
            &mut index,
            SyncPage {
                items: vec![
                    item("a", "2026-01-01T10:00:00Z", "A"),
                    item("b", "2026-01-01T11:00:00Z", "B"),
                ],
                deleted_ids: vec!["never-seen".into()],
                next_cursor: Some("c1".into()),
                has_more: true,
            },
            1,
            &mut change,
        );
        assert_eq!(index.items.len(), 2);
        assert_eq!(change.upserts.len(), 2);
        assert!(change.removed.is_empty(), "unknown tombstones are ignored");
        assert_eq!(index.cursor.as_deref(), Some("c1"));
        assert!(
            index.items["a"].get("transcript_text").is_none(),
            "the index has no transcripts"
        );
        assert_eq!(index.items["a"]["transcript_words"], 3);
        let mut change = Change::default();
        merge(
            &mut index,
            SyncPage {
                items: vec![item("a", "2026-01-02T10:00:00Z", "A2")],
                deleted_ids: vec!["b".into()],
                next_cursor: None,
                has_more: false,
            },
            2,
            &mut change,
        );
        assert_eq!(index.items["a"]["title"], "A2");
        assert!(!index.items.contains_key("b"));
        assert_eq!(change.removed, vec!["b".to_string()]);
        assert_eq!(index.cursor.as_deref(), Some("c1"));
        assert_eq!(index.synced_at, Some(2));
        let mut change = Change::default();
        merge(&mut index, SyncPage::default(), 3, &mut change);
        assert!(change.is_empty());
    }

    #[test]
    fn stale_copies_never_overwrite_newer_ones() {
        let mut index = LibraryIndex::default();
        let mut change = Change::default();
        assert!(upsert(
            &mut index,
            &item("a", "2026-01-02T10:00:00Z", "new"),
            &mut change
        ));
        assert!(!upsert(
            &mut index,
            &item("a", "2026-01-01T10:00:00Z", "old"),
            &mut change
        ));
        assert_eq!(index.items["a"]["title"], "new");
        assert!(!upsert(
            &mut index,
            &item("a", "2026-01-02T10:00:00Z", "new"),
            &mut change
        ));
        let mut deleted = item("a", "2026-01-03T10:00:00Z", "new");
        deleted["deleted_at"] = json!("2026-01-03T10:00:00Z");
        assert!(upsert(&mut index, &deleted, &mut change));
        assert!(index.items.is_empty());
        assert!(!upsert(&mut index, &json!({"title":"no id"}), &mut change));
    }

    #[test]
    fn details_live_beside_the_index_and_keep_results() {
        let dir = std::env::temp_dir().join(format!("hearfolio-library-{}", uuid::Uuid::new_v4()));
        let mut index = LibraryIndex::default();
        let mut change = Change::default();
        upsert(
            &mut index,
            &item("a", "2026-01-01T10:00:00Z", "A"),
            &mut change,
        );
        upsert(
            &mut index,
            &item("b", "2026-01-03T10:00:00Z", "B"),
            &mut change,
        );
        apply_change(&dir, &change).unwrap();
        store_results(&dir, &mut index, "a", vec![json!({"id":"r"})]).unwrap();
        write_index(&dir, &index).unwrap();
        let rows = summaries(&read_index(&dir));
        assert_eq!(rows[0]["id"], "b");
        assert_eq!(rows[1]["results_count"], 1);
        // A list-only update (e.g. the upload response) keeps transcript and results.
        let mut change = Change::default();
        let mut slim = item("a", "2026-01-04T10:00:00Z", "A2");
        slim.as_object_mut().unwrap().remove("transcript_text");
        upsert(&mut index, &slim, &mut change);
        apply_change(&dir, &change).unwrap();
        let detail = read_detail(&dir, "a").unwrap();
        assert_eq!(detail.recording["title"], "A2");
        assert_eq!(detail.recording["transcript_text"], "one two three");
        assert_eq!(detail.results.unwrap().len(), 1);
        assert_eq!(index.items["a"]["results_count"], 1);
        assert_eq!(index.items["a"]["transcript_words"], 3);
        let mut change = Change::default();
        merge(
            &mut index,
            SyncPage {
                deleted_ids: vec!["a".into()],
                ..Default::default()
            },
            9,
            &mut change,
        );
        apply_change(&dir, &change).unwrap();
        assert!(read_detail(&dir, "a").is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
