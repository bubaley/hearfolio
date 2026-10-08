use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_source_id: Option<String>,
    pub name: String,
    pub input_path: String,
    pub output_path: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub configuration: Option<crate::cloud::RecognitionConfig>,
    pub created_at: u64,
    #[serde(default)]
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
}

pub fn read_history_at(dir: &Path) -> Result<Vec<HistoryEntry>, String> {
    let path = dir.join("history.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(format!("errors.historyRead|{error}")),
    };
    let mut entries: Vec<HistoryEntry> =
        serde_json::from_slice(&bytes).map_err(|_| "errors.historyInvalid".to_owned())?;
    for entry in &mut entries {
        if entry.size_bytes.is_none() {
            entry.size_bytes = fs::metadata(&entry.input_path)
                .ok()
                .map(|metadata| metadata.len());
        }
    }
    Ok(entries)
}

pub fn write_history_at(dir: &Path, entries: &[HistoryEntry]) -> Result<(), String> {
    let path = dir.join("history.json");
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(entries).map_err(|error| error.to_string())?;
    let mut file =
        fs::File::create(&temp).map_err(|error| format!("errors.historySave|{error}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("errors.historySave|{error}"))?;
    fs::rename(temp, path).map_err(|error| format!("errors.historySave|{error}"))
}

pub fn update_configuration_at(
    dir: &Path,
    id: &str,
    mut configuration: crate::cloud::RecognitionConfig,
) -> Result<HistoryEntry, String> {
    configuration.validate()?;
    let mut entries = read_history_at(dir)?;
    let entry = entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or("errors.recordMissing")?;
    entry.configuration = Some(configuration);
    let result = entry.clone();
    write_history_at(dir, &entries)?;
    Ok(result)
}

pub fn rename_entry_at(dir: &Path, id: &str, name: &str) -> Result<HistoryEntry, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("errors.recordNameRequired".into());
    }
    if name.chars().count() > 200 {
        return Err("errors.recordNameLong".into());
    }
    if name.chars().any(char::is_control) {
        return Err("errors.recordNameControl".into());
    }
    let mut entries = read_history_at(dir)?;
    let entry = entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or("errors.recordMissing")?;
    if entry.name == name {
        return Ok(entry.clone());
    }
    entry.name = name.to_owned();
    let renamed = entry.clone();
    write_history_at(dir, &entries)?;
    Ok(renamed)
}

pub fn save_transcript_at(dir: &Path, id: &str, model: String, text: &str) -> Result<(), String> {
    let mut entries = read_history_at(dir)?;
    let entry = entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or("errors.recordMissing")?;
    entry.model = Some(model);
    entry.completed_at = Some(crate::clock());
    replace_transcript_at(dir, entry.clone(), text)
}

// Commit the text and its exact metadata together, restoring the previous text
// if the archive cannot be updated.
pub(crate) fn replace_transcript_at(
    dir: &Path,
    updated: HistoryEntry,
    text: &str,
) -> Result<(), String> {
    let mut entries = read_history_at(dir)?;
    let id = updated.id.as_str();
    let entry = entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or("errors.recordMissing")?;
    if !id.starts_with("hearing-")
        || !id
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || value == b'-')
    {
        return Err("errors.recordIdInvalid".into());
    }
    let output = dir.join("output").join(format!("{id}.txt"));
    let temp = output.with_extension("txt.tmp");
    let prepare = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = prepare {
        let _ = fs::remove_file(&temp);
        return Err(format!("errors.transcriptSave|{error}"));
    }
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let backup = dir.join("temp").join(format!("previous-{id}-{unique}.txt"));
    let stage = match fs::symlink_metadata(&output) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::rename(&output, &backup).map(|_| true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Ok(_) => Err(std::io::Error::other("errors.transcriptPathInvalid")),
        Err(error) => Err(error),
    };
    let has_backup = match stage {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(format!("errors.transcriptBackup|{error}"));
        }
    };
    if let Err(error) = fs::rename(&temp, &output) {
        let _ = fs::remove_file(&temp);
        if has_backup {
            fs::rename(&backup, &output)
                .map_err(|_| format!("errors.transcriptRecovery|{}", backup.display()))?;
        }
        return Err(format!("errors.transcriptSave|{error}"));
    }
    *entry = updated;
    entry.output_path = Some(output.to_string_lossy().into_owned());
    if let Err(error) = write_history_at(dir, &entries) {
        if has_backup {
            fs::rename(&backup, &output)
                .map_err(|_| format!("errors.transcriptRecovery|{}", backup.display()))?;
        } else {
            fs::remove_file(&output)
                .map_err(|_| format!("errors.transcriptRollback|{}", output.display()))?;
        }
        return Err(error);
    }
    if has_backup {
        if let Err(error) = fs::remove_file(&backup) {
            eprintln!(
                "Предыдущая транскрипция ожидает очистки {}: {error}",
                backup.display()
            );
        }
    }
    Ok(())
}

// A history file can be edited independently of the app. Only the exact filename
// assigned to this record in an app-owned directory may be removed.
fn managed_file(dir: &Path, entry: &HistoryEntry, path: &str, folder: &str) -> Option<PathBuf> {
    if !entry.id.starts_with("hearing-")
        || !entry
            .id
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || value == b'-')
    {
        return None;
    }
    let path = Path::new(path);
    let expected = if folder == "output" {
        format!("{}.txt", entry.id)
    } else {
        let extension = path.extension()?.to_str()?;
        if !["m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg"].contains(&extension) {
            return None;
        }
        format!("{}.{extension}", entry.id)
    };
    let expected = dir.join(folder).join(expected);
    if path != expected {
        return None;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Some(expected),
        _ => None,
    }
}

fn restore_files(staged: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    for (original, moved) in staged.iter().rev() {
        fs::rename(moved, original)
            .map_err(|_| format!("errors.recordRecovery|{}", moved.display()))?;
    }
    Ok(())
}

pub fn delete_entries_at(dir: &Path, id: Option<&str>) -> Result<(), String> {
    let entries = read_history_at(dir)?;
    if id.is_some_and(|id| !entries.iter().any(|entry| entry.id == id)) {
        return Err("errors.recordMissing".into());
    }
    let mut retained = Vec::new();
    let mut paths = HashSet::new();
    for entry in entries {
        if id.is_some_and(|id| entry.id != id) {
            retained.push(entry);
            continue;
        }
        if let Some(path) = managed_file(dir, &entry, &entry.input_path, "input") {
            paths.insert(path);
        }
        if let Some(output) = &entry.output_path {
            if let Some(path) = managed_file(dir, &entry, output, "output") {
                paths.insert(path);
            }
        }
    }
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let quarantine = dir.join("temp").join(format!("deleting-{unique}"));
    fs::create_dir_all(&quarantine).map_err(|error| format!("errors.recordDelete|{error}"))?;
    let mut staged = Vec::new();
    for (index, path) in paths.into_iter().enumerate() {
        let target = quarantine.join(index.to_string());
        if let Err(error) = fs::rename(&path, &target) {
            restore_files(&staged)?;
            let _ = fs::remove_dir(&quarantine);
            return Err(format!("errors.recordDelete|{error}"));
        }
        staged.push((path, target));
    }
    if let Err(error) = write_history_at(dir, &retained) {
        restore_files(&staged)?;
        let _ = fs::remove_dir(&quarantine);
        return Err(error);
    }
    // The history has committed; cleanup cannot invalidate that commit. If the
    // OS refuses cleanup, keep the staged files for recovery instead of lying
    // to the caller that the record still exists.
    if let Err(error) = fs::remove_dir_all(&quarantine) {
        eprintln!(
            "Удалённая запись ожидает очистки {}: {error}",
            quarantine.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "hearfolio-records-{}-{}-{unique}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        for folder in ["input", "output", "temp"] {
            fs::create_dir_all(dir.join(folder)).unwrap();
        }
        dir
    }
    fn entry(dir: &Path, id: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            transfer_source_id: None,
            name: "audio.wav".into(),
            input_path: dir
                .join("input")
                .join(format!("{id}.wav"))
                .to_string_lossy()
                .into(),
            output_path: Some(
                dir.join("output")
                    .join(format!("{id}.txt"))
                    .to_string_lossy()
                    .into(),
            ),
            model: None,
            configuration: None,
            created_at: 1,
            completed_at: None,
            size_bytes: None,
            duration_seconds: None,
        }
    }
    #[test]
    fn old_history_loads_without_metadata() {
        let parsed: HistoryEntry = serde_json::from_str(r#"{"id":"hearing-1","name":"old.wav","inputPath":"old.wav","outputPath":null,"model":null,"createdAt":1}"#).unwrap();
        assert!(parsed.size_bytes.is_none());
        assert!(parsed.duration_seconds.is_none());
    }
    #[test]
    fn renaming_preserves_audio_transcript_and_previous_name_on_failure() {
        let dir = fixture();
        let record = entry(&dir, "hearing-1");
        fs::write(&record.input_path, b"original audio").unwrap();
        fs::write(record.output_path.as_ref().unwrap(), "saved transcript").unwrap();
        write_history_at(&dir, &[record.clone()]).unwrap();
        let renamed = rename_entry_at(&dir, &record.id, "  Встреча команды 07.10  ").unwrap();
        assert_eq!(renamed.name, "Встреча команды 07.10");
        let persisted = read_history_at(&dir).unwrap().remove(0);
        assert_eq!(persisted.name, renamed.name);
        assert_eq!(persisted.input_path, record.input_path);
        assert_eq!(persisted.output_path, record.output_path);
        assert_eq!(fs::read(&persisted.input_path).unwrap(), b"original audio");
        assert_eq!(
            fs::read_to_string(persisted.output_path.as_ref().unwrap()).unwrap(),
            "saved transcript"
        );
        for name in ["  ", "bad\nname", &"a".repeat(201)] {
            assert!(rename_entry_at(&dir, &record.id, name).is_err());
        }
        assert!(rename_entry_at(&dir, "missing", "Other name").is_err());
        fs::create_dir(dir.join("history.json.tmp")).unwrap();
        assert!(rename_entry_at(&dir, &record.id, "Another title").is_err());
        assert_eq!(read_history_at(&dir).unwrap()[0].name, renamed.name);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn deletion_removes_one_record_and_preserves_other_files() {
        let dir = fixture();
        let first = entry(&dir, "hearing-1");
        let second = entry(&dir, "hearing-2");
        for record in [&first, &second] {
            fs::write(&record.input_path, b"audio").unwrap();
            fs::write(record.output_path.as_ref().unwrap(), "text").unwrap();
        }
        let unrelated = dir.join("input").join("hearing-untracked.wav");
        fs::write(&unrelated, b"keep").unwrap();
        write_history_at(&dir, &[first.clone(), second.clone()]).unwrap();
        delete_entries_at(&dir, Some(&first.id)).unwrap();
        assert!(!Path::new(&first.input_path).exists());
        assert!(!Path::new(first.output_path.as_ref().unwrap()).exists());
        assert!(Path::new(&second.input_path).exists());
        assert!(unrelated.exists());
        assert_eq!(read_history_at(&dir).unwrap()[0].id, second.id);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn deletion_refuses_to_touch_foreign_paths_and_symlinks() {
        let dir = fixture();
        let mut record = entry(&dir, "hearing-1");
        let unrelated = dir.join("input").join("hearing-2.wav");
        fs::write(&unrelated, b"keep").unwrap();
        record.input_path = unrelated.to_string_lossy().into();
        let outside = dir.join("personal.txt");
        fs::write(&outside, "keep").unwrap();
        record.output_path = Some(outside.to_string_lossy().into());
        write_history_at(&dir, &[record]).unwrap();
        delete_entries_at(&dir, None).unwrap();
        assert!(unrelated.exists());
        assert!(outside.exists());
        #[cfg(unix)]
        {
            let record = entry(&dir, "hearing-3");
            std::os::unix::fs::symlink(&unrelated, &record.input_path).unwrap();
            write_history_at(&dir, &[record.clone()]).unwrap();
            delete_entries_at(&dir, None).unwrap();
            assert!(fs::symlink_metadata(&record.input_path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(unrelated.exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_history_commit_restores_record_files() {
        let dir = fixture();
        let record = entry(&dir, "hearing-1");
        fs::write(&record.input_path, b"audio").unwrap();
        fs::write(record.output_path.as_ref().unwrap(), "text").unwrap();
        write_history_at(&dir, &[record.clone()]).unwrap();
        fs::create_dir(dir.join("history.json.tmp")).unwrap();
        assert!(delete_entries_at(&dir, Some(&record.id)).is_err());
        assert!(Path::new(&record.input_path).exists());
        assert!(Path::new(record.output_path.as_ref().unwrap()).exists());
        assert_eq!(read_history_at(&dir).unwrap()[0].id, record.id);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_transcript_commit_preserves_previous_text_and_model() {
        let dir = fixture();
        let mut record = entry(&dir, "hearing-1");
        record.model = Some("whisper-base".into());
        fs::write(
            record.output_path.as_ref().unwrap(),
            "previous saved transcript",
        )
        .unwrap();
        write_history_at(&dir, &[record.clone()]).unwrap();
        fs::create_dir(dir.join("history.json.tmp")).unwrap();
        assert!(save_transcript_at(
            &dir,
            &record.id,
            "openrouter/new-model".into(),
            "new transcript"
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(record.output_path.as_ref().unwrap()).unwrap(),
            "previous saved transcript"
        );
        let retained = read_history_at(&dir).unwrap();
        assert_eq!(retained[0].model, record.model);
        assert_eq!(retained[0].output_path, record.output_path);
        assert_eq!(fs::read_dir(dir.join("temp")).unwrap().count(), 0);
        fs::remove_dir(dir.join("history.json.tmp")).unwrap();
        save_transcript_at(
            &dir,
            &record.id,
            "openrouter/new-model".into(),
            "new transcript",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(record.output_path.as_ref().unwrap()).unwrap(),
            "new transcript"
        );
        assert_eq!(
            read_history_at(&dir).unwrap()[0].model.as_deref(),
            Some("openrouter/new-model")
        );
        assert_eq!(fs::read_dir(dir.join("temp")).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn configuration_selection_persists_without_replacing_previous_transcript_result() {
        let dir = fixture();
        let mut record = entry(&dir, "hearing-1");
        record.model = Some("whisper-base".into());
        fs::write(record.output_path.as_ref().unwrap(), "saved text").unwrap();
        write_history_at(&dir, &[record.clone()]).unwrap();
        let config = crate::cloud::RecognitionConfig {
            provider: "openrouter".into(),
            model: "fish-audio/transcribe-1".into(),
            mode: "transcription".into(),
        };
        let updated = update_configuration_at(&dir, &record.id, config.clone()).unwrap();
        assert_eq!(updated.configuration, Some(config));
        assert_eq!(updated.model, record.model);
        assert_eq!(
            fs::read_to_string(updated.output_path.as_ref().unwrap()).unwrap(),
            "saved text"
        );
        fs::create_dir(dir.join("history.json.tmp")).unwrap();
        assert!(update_configuration_at(
            &dir,
            &record.id,
            crate::cloud::RecognitionConfig::default()
        )
        .is_err());
        assert_eq!(
            read_history_at(&dir).unwrap()[0].configuration,
            updated.configuration
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
