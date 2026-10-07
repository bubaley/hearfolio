use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
static STORAGE_LOCK: Mutex<()> = Mutex::new(());
use crate::records::{read_history_at, write_history_at};

#[cfg(mobile)]
struct MobilePaths {
    config: PathBuf,
    archive: PathBuf,
}
#[cfg(mobile)]
static MOBILE_PATHS: std::sync::OnceLock<MobilePaths> = std::sync::OnceLock::new();

pub fn initialize<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<(), String> {
    #[cfg(mobile)]
    {
        use tauri::Manager;
        let config = app
            .path()
            .app_config_dir()
            .map_err(|error| format!("errors.storageUnavailable|{error}"))?;
        let archive = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("errors.storageUnavailable|{error}"))?
            .join("archive");
        fs::create_dir_all(&config)
            .map_err(|error| format!("errors.storageUnavailable|{error}"))?;
        ensure_dirs(&archive)?;
        MOBILE_PATHS
            .set(MobilePaths { config, archive })
            .map_err(|_| "errors.storageUnavailable")?;
    }
    #[cfg(not(mobile))]
    let _ = app;
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Location {
    root: PathBuf,
}

pub fn config_dir() -> Result<PathBuf, String> {
    #[cfg(mobile)]
    {
        return MOBILE_PATHS
            .get()
            .map(|paths| paths.config.clone())
            .ok_or_else(|| "errors.storageUnavailable".into());
    }
    #[cfg(not(mobile))]
    {
        let home = home_dir()?;
        #[cfg(target_os = "macos")]
        let path = home.join("Library/Application Support/Hearfolio");
        #[cfg(not(target_os = "macos"))]
        let path = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .unwrap_or_else(|| home.join(".config"))
            .join("Hearfolio");
        fs::create_dir_all(&path).map_err(|e| format!("errors.settingsSave|{e}"))?;
        Ok(path)
    }
}

pub fn home_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = home
        .or_else(|| std::env::var_os("USERPROFILE"))
        .or_else(|| {
            let mut drive = std::env::var_os("HOMEDRIVE")?;
            drive.push(std::env::var_os("HOMEPATH")?);
            Some(drive)
        });
    home.filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "errors.homeMissing".into())
}

pub fn ensure_dirs(root: &Path) -> Result<(), String> {
    for name in ["input", "output", "models", "temp"] {
        fs::create_dir_all(root.join(name))
            .map_err(|e| format!("errors.storageUnavailable|{e}"))?;
    }
    Ok(())
}

fn save_location(pointer: &Path, root: &Path) -> Result<(), String> {
    let temp = pointer.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|e| format!("errors.storageCommit|{e}"))?;
    let bytes = serde_json::to_vec_pretty(&Location {
        root: root.to_owned(),
    })
    .map_err(|e| format!("errors.storageCommit|{e}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("errors.storageCommit|{e}"))?;
    fs::rename(temp, pointer).map_err(|e| format!("errors.storageCommit|{e}"))
}

pub fn active_root_at(home: &Path, config: &Path) -> Result<PathBuf, String> {
    let pointer = config.join("storage.json");
    match fs::read(&pointer) {
        Ok(bytes) => {
            let location: Location =
                serde_json::from_slice(&bytes).map_err(|_| "errors.storageConfigInvalid")?;
            if !location.root.is_absolute() || !location.root.is_dir() {
                return Err("errors.storageUnavailable".into());
            }
            ensure_dirs(&location.root)?;
            Ok(location.root)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let root = home.join(".hearfolio");
            let legacy = home.join(".hearing");
            if legacy.is_dir() {
                // Read before copying so an invalid history never replaces the active archive.
                migrate_at(&legacy, home, &pointer, |_, _| {})
            } else {
                ensure_dirs(&root)?;
                save_location(&pointer, &root)?;
                Ok(root)
            }
        }
        Err(error) => Err(format!("errors.storageConfigInvalid|{error}")),
    }
}

pub fn active_root() -> Result<PathBuf, String> {
    #[cfg(mobile)]
    {
        return MOBILE_PATHS
            .get()
            .map(|paths| paths.archive.clone())
            .ok_or_else(|| "errors.storageUnavailable".into());
    }
    #[cfg(not(mobile))]
    {
        let _lock = STORAGE_LOCK
            .lock()
            .map_err(|_| "errors.storageUnavailable")?;
        let home = home_dir()?;
        active_root_at(&home, &config_dir()?)
    }
}

pub fn migrate(
    source: &Path,
    parent: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    if cfg!(mobile) {
        return Err("errors.storageChangeUnavailableMobile".into());
    }
    let _lock = STORAGE_LOCK
        .lock()
        .map_err(|_| "errors.storageUnavailable")?;
    migrate_at(
        source,
        parent,
        &config_dir()?.join("storage.json"),
        progress,
    )
}

fn regular_files(
    root: &Path,
    path: &Path,
    output: &mut Vec<(PathBuf, PathBuf, u64)>,
) -> Result<(), String> {
    for entry in fs::read_dir(path).map_err(|e| format!("errors.storageCopy|{e}"))? {
        let entry = entry.map_err(|e| format!("errors.storageCopy|{e}"))?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|e| format!("errors.storageCopy|{e}"))?;
        if metadata.file_type().is_symlink() {
            return Err("errors.storageSymlink".into());
        }
        if metadata.is_dir() {
            regular_files(root, &entry.path(), output)?;
        } else if metadata.is_file() {
            output.push((
                entry.path(),
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| "errors.storagePathInvalid")?
                    .to_owned(),
                metadata.len(),
            ));
        } else {
            return Err("errors.storagePathInvalid".into());
        }
    }
    Ok(())
}

pub fn migrate_at(
    source: &Path,
    parent: &Path,
    pointer: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    let source_alias = source.to_owned();
    let source = fs::canonicalize(source).map_err(|e| format!("errors.storageUnavailable|{e}"))?;
    let parent =
        fs::canonicalize(parent).map_err(|e| format!("errors.storageParentInvalid|{e}"))?;
    if !parent.is_dir() {
        return Err("errors.storageParentInvalid".into());
    }
    let target = parent.join(".hearfolio");
    if target == source {
        return Ok(source);
    }
    if parent.starts_with(&source) {
        return Err("errors.storagePathInvalid".into());
    }
    if fs::symlink_metadata(&target).is_ok() {
        return Err("errors.storageConflict".into());
    }
    let mut entries = read_history_at(&source)?;
    for entry in &mut entries {
        let input = Path::new(&entry.input_path)
            .strip_prefix(&source_alias)
            .or_else(|_| Path::new(&entry.input_path).strip_prefix(&source))
            .map_err(|_| "errors.storagePathInvalid")?;
        if input
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("errors.storagePathInvalid".into());
        }
        entry.input_path = target.join(input).to_string_lossy().into_owned();
        if let Some(path) = &entry.output_path {
            let output = Path::new(path)
                .strip_prefix(&source_alias)
                .or_else(|_| Path::new(path).strip_prefix(&source))
                .map_err(|_| "errors.storagePathInvalid")?;
            if output
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err("errors.storagePathInvalid".into());
            }
            // Legacy archives used the original transcript mtime as completion
            // order. Persist it before copying or retargeting the output path.
            if entry.completed_at.is_none() {
                entry.completed_at = fs::metadata(source.join(output))
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as u64);
            }
            entry.output_path = Some(target.join(output).to_string_lossy().into_owned());
        }
    }
    let mut files = vec![];
    regular_files(&source, &source, &mut files)?;
    let total = files.iter().map(|(_, _, size)| size).sum();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = parent.join(format!(".hearfolio-migration-{unique}"));
    fs::create_dir(&staging).map_err(|e| format!("errors.storageCopy|{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)) {
            let _ = fs::remove_dir(&staging);
            return Err(format!("errors.storageCopy|{error}"));
        }
    }
    let result = (|| {
        let mut copied = 0;
        progress(0, total);
        for (original, relative, size) in files {
            let destination = staging.join(relative);
            fs::create_dir_all(destination.parent().ok_or("errors.storagePathInvalid")?)
                .map_err(|e| format!("errors.storageCopy|{e}"))?;
            let mut input =
                fs::File::open(&original).map_err(|e| format!("errors.storageCopy|{e}"))?;
            let metadata = input
                .metadata()
                .map_err(|e| format!("errors.storageCopy|{e}"))?;
            let modified = metadata
                .modified()
                .map_err(|e| format!("errors.storageCopy|{e}"))?;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut output = options
                .open(&destination)
                .map_err(|e| format!("errors.storageCopy|{e}"))?;
            let mut bytes = 0;
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                let read = input
                    .read(&mut buffer)
                    .map_err(|e| format!("errors.storageCopy|{e}"))?;
                if read == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|e| format!("errors.storageCopy|{e}"))?;
                bytes += read as u64;
                progress(copied + bytes, total);
            }
            output
                .set_modified(modified)
                .and_then(|_| output.sync_all())
                .map_err(|e| format!("errors.storageCopy|{e}"))?;
            fs::set_permissions(&destination, metadata.permissions())
                .map_err(|e| format!("errors.storageCopy|{e}"))?;
            if bytes != size {
                return Err("errors.storageCopy".into());
            }
            copied += size;
            progress(copied, total);
        }
        ensure_dirs(&staging)?;
        write_history_at(&staging, &entries)?;
        fs::rename(&staging, &target).map_err(|e| format!("errors.storageCopy|{e}"))?;
        if let Err(error) = save_location(pointer, &target) {
            // Only our just-created copy is rolled back; source stays intact.
            if let Err(cleanup) = fs::remove_dir_all(&target) {
                return Err(format!(
                    "{error}; errors.storageRollback|{}: {cleanup}",
                    target.display()
                ));
            }
            return Err(error);
        }
        Ok(target.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "hearfolio-storage-{}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("config")).unwrap();
        fs::create_dir_all(root.join("parent")).unwrap();
        ensure_dirs(&root.join(".hearing")).unwrap();
        root
    }
    #[test]
    fn migration_copies_archive_and_token_and_retargets_history() {
        let root = fixture();
        let source = root.join(".hearing");
        fs::write(source.join("input/hearing-1.wav"), b"audio").unwrap();
        fs::write(source.join("output/hearing-1.txt"), "text").unwrap();
        fs::write(source.join("settings.json"), r#"{"token":"secret"}"#).unwrap();
        fs::write(source.join("history.json"), serde_json::to_vec(&serde_json::json!([{"id":"hearing-1","name":"audio","inputPath":source.join("input/hearing-1.wav"),"outputPath":source.join("output/hearing-1.txt"),"model":null,"createdAt":1}])).unwrap()).unwrap();
        let target = migrate_at(
            &source,
            &root.join("parent"),
            &root.join("config/storage.json"),
            |_, _| {},
        )
        .unwrap();
        let entry = read_history_at(&target).unwrap().remove(0);
        assert_eq!(
            entry.input_path,
            target.join("input/hearing-1.wav").to_string_lossy()
        );
        assert_eq!(fs::read(entry.input_path).unwrap(), b"audio");
        assert!(source.join("input/hearing-1.wav").exists());
        assert!(fs::read_to_string(target.join("settings.json"))
            .unwrap()
            .contains("secret"));
        assert_eq!(active_root_at(&root, &root.join("config")).unwrap(), target);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn failed_pointer_commit_rolls_back_copy_and_keeps_previous_root() {
        let root = fixture();
        let source = root.join(".hearing");
        let pointer = root.join("config/storage.json");
        save_location(&pointer, &source).unwrap();
        fs::write(source.join("input/hearing-1.wav"), b"keep").unwrap();
        fs::create_dir(pointer.with_extension("json.tmp")).unwrap();
        assert!(migrate_at(&source, &root.join("parent"), &pointer, |_, _| {}).is_err());
        assert!(!root.join("parent/.hearfolio").exists());
        assert!(source.join("input/hearing-1.wav").exists());
        assert_eq!(active_root_at(&root, &root.join("config")).unwrap(), source);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn conflicting_archive_and_nested_parent_are_rejected() {
        let root = fixture();
        let source = root.join(".hearing");
        fs::create_dir(root.join("parent/.hearfolio")).unwrap();
        fs::write(root.join("parent/.hearfolio/keep"), "other archive").unwrap();
        assert!(migrate_at(
            &source,
            &root.join("parent"),
            &root.join("config/storage.json"),
            |_, _| {}
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(root.join("parent/.hearfolio/keep")).unwrap(),
            "other archive"
        );
        assert!(migrate_at(
            &source,
            &source,
            &root.join("config/storage.json"),
            |_, _| {}
        )
        .is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn external_history_path_and_symlink_are_rejected() {
        let root = fixture();
        let source = root.join(".hearing");
        fs::write(source.join("history.json"),r#"[{"id":"hearing-1","name":"audio","inputPath":"/tmp/private.wav","outputPath":null,"model":null,"createdAt":1}]"#).unwrap();
        assert!(migrate_at(
            &source,
            &root.join("parent"),
            &root.join("config/storage.json"),
            |_, _| {}
        )
        .is_err());
        fs::remove_file(source.join("history.json")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("config"), source.join("input/link")).unwrap();
            assert!(migrate_at(
                &source,
                &root.join("parent"),
                &root.join("config/storage.json"),
                |_, _| {}
            )
            .is_err());
        }
        assert!(!root.join("parent/.hearfolio").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn startup_copies_legacy_once_and_same_current_root_is_allowed() {
        let root = fixture();
        let source = root.join(".hearing");
        fs::write(source.join("input/hearing-1.wav"), b"legacy").unwrap();
        let first = active_root_at(&root, &root.join("config")).unwrap();
        assert_eq!(first, fs::canonicalize(root.join(".hearfolio")).unwrap());
        assert_eq!(
            fs::read(first.join("input/hearing-1.wav")).unwrap(),
            b"legacy"
        );
        assert!(source.join("input/hearing-1.wav").exists());
        let again = active_root_at(&root, &root.join("config")).unwrap();
        assert_eq!(again, first);
        assert_eq!(
            migrate_at(&first, &root, &root.join("config/storage.json"), |_, _| {}).unwrap(),
            fs::canonicalize(first).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
