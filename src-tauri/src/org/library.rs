//! Local mirror of an organization's recordings, kept current with
//! `GET /recordings/sync?cursor=` (items + tombstones).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LibraryState {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub items: BTreeMap<String, Value>,
    #[serde(default)]
    pub synced_at: Option<u64>,
    /// Summaries per recording (`GET /recordings/{id}/results`), fetched when a
    /// recording is opened; a result change bumps the recording in sync.
    #[serde(default)]
    pub results: BTreeMap<String, Vec<Value>>,
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

fn updated_at(item: &Value) -> &str {
    item.get("updated_at").and_then(Value::as_str).unwrap_or("")
}

/// Insert one recording unless the stored copy is newer (an upload response
/// can race with a sync page). ISO-8601 UTC timestamps compare as strings.
pub fn upsert(state: &mut LibraryState, item: Value) -> bool {
    let Some(id) = item.get("id").and_then(Value::as_str).map(str::to_owned) else {
        return false;
    };
    if item.get("deleted_at").is_some_and(|value| !value.is_null()) {
        state.results.remove(&id);
        return state.items.remove(&id).is_some();
    }
    if let Some(existing) = state.items.get(&id) {
        if same_instant_or_newer(existing, &item) {
            return false;
        }
    }
    state.items.insert(id, item);
    true
}

fn same_instant_or_newer(existing: &Value, incoming: &Value) -> bool {
    let (old, new) = (updated_at(existing), updated_at(incoming));
    !old.is_empty() && !new.is_empty() && old > new || existing == incoming
}

/// Apply one sync page. Returns whether anything visible changed. Tombstones of
/// recordings we never had are ignored; the cursor only moves forward.
pub fn merge(state: &mut LibraryState, page: SyncPage, now: u64) -> bool {
    let mut changed = false;
    for item in page.items {
        changed |= upsert(state, item);
    }
    for id in page.deleted_ids {
        state.results.remove(&id);
        changed |= state.items.remove(&id).is_some();
    }
    if let Some(cursor) = page.next_cursor.filter(|cursor| !cursor.is_empty()) {
        state.cursor = Some(cursor);
    }
    state.synced_at = Some(now);
    changed
}

/// Compact list rows: transcripts stay on disk until a recording is opened.
pub fn summaries(state: &LibraryState) -> Vec<Value> {
    let mut items: Vec<Value> = state
        .items
        .values()
        .map(|item| {
            let mut row = item.clone();
            if let Some(object) = row.as_object_mut() {
                let words = object
                    .get("transcript_text")
                    .and_then(Value::as_str)
                    .map(|text| text.split_whitespace().count())
                    .unwrap_or(0);
                object.remove("transcript_text");
                object.remove("transcript_segments");
                object.insert("transcript_words".into(), words.into());
                let id = object.get("id").and_then(Value::as_str).unwrap_or("");
                let results = state.results.get(id).map(Vec::len).unwrap_or(0);
                object.insert("results_count".into(), results.into());
            }
            row
        })
        .collect();
    items.sort_by(|a, b| {
        let key = |item: &Value| {
            item.get("recorded_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        key(b).cmp(&key(a))
    });
    items
}

pub fn read(path: &Path) -> LibraryState {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn write(path: &Path, state: &LibraryState) -> Result<(), String> {
    super::write_private_json(path, state)
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
        let mut state = LibraryState::default();
        assert!(merge(
            &mut state,
            SyncPage {
                items: vec![
                    item("a", "2026-01-01T10:00:00Z", "A"),
                    item("b", "2026-01-01T11:00:00Z", "B")
                ],
                deleted_ids: vec!["never-seen".into()],
                next_cursor: Some("c1".into()),
                has_more: true,
            },
            1
        ));
        assert_eq!(state.items.len(), 2);
        assert_eq!(state.cursor.as_deref(), Some("c1"));
        // Updated title, a tombstone and an unchanged cursor (nothing new).
        assert!(merge(
            &mut state,
            SyncPage {
                items: vec![item("a", "2026-01-02T10:00:00Z", "A2")],
                deleted_ids: vec!["b".into()],
                next_cursor: None,
                has_more: false,
            },
            2
        ));
        assert_eq!(state.items["a"]["title"], "A2");
        assert!(!state.items.contains_key("b"));
        assert_eq!(state.cursor.as_deref(), Some("c1"));
        assert_eq!(state.synced_at, Some(2));
        // An empty page changes nothing visible.
        assert!(!merge(&mut state, SyncPage::default(), 3));
    }

    #[test]
    fn stale_copies_never_overwrite_newer_ones() {
        let mut state = LibraryState::default();
        assert!(upsert(&mut state, item("a", "2026-01-02T10:00:00Z", "new")));
        assert!(!upsert(
            &mut state,
            item("a", "2026-01-01T10:00:00Z", "old")
        ));
        assert_eq!(state.items["a"]["title"], "new");
        assert!(!upsert(
            &mut state,
            item("a", "2026-01-02T10:00:00Z", "new")
        ));
        // A detail fetched after deletion acts as a tombstone.
        let mut deleted = item("a", "2026-01-03T10:00:00Z", "new");
        deleted["deleted_at"] = json!("2026-01-03T10:00:00Z");
        assert!(upsert(&mut state, deleted));
        assert!(state.items.is_empty());
        assert!(!upsert(&mut state, json!({"title":"no id"})));
    }

    #[test]
    fn summaries_drop_transcripts_and_sort_newest_first() {
        let mut state = LibraryState::default();
        upsert(&mut state, item("a", "2026-01-01T10:00:00Z", "A"));
        upsert(&mut state, item("b", "2026-01-03T10:00:00Z", "B"));
        let rows = summaries(&state);
        assert_eq!(rows[0]["id"], "b");
        assert!(rows[0].get("transcript_text").is_none());
        assert_eq!(rows[0]["transcript_words"], 3);
    }
}
