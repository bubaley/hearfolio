//! Panic log. Release builds unwind (not abort), so a panic in a worker is
//! caught by tokio and the hook below leaves a trace for diagnosis.
use std::{
    backtrace::Backtrace,
    fs,
    io::Write,
    panic::{self, PanicHookInfo},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 256 * 1024;

pub fn log_path() -> Option<PathBuf> {
    Some(
        crate::storage::config_dir()
            .ok()?
            .join("logs")
            .join("panic.log"),
    )
}

fn describe(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned());
    let location = info
        .location()
        .map(|place| format!("{}:{}:{}", place.file(), place.line(), place.column()))
        .unwrap_or_else(|| "unknown".to_owned());
    let thread = std::thread::current();
    format!(
        "[{}] hearfolio {} thread={:?}\nlocation: {location}\nmessage: {message}\n{}\n\n",
        crate::clock(),
        env!("CARGO_PKG_VERSION"),
        thread.name().unwrap_or("unnamed"),
        Backtrace::force_capture()
    )
}

/// Appends `entry`; when the file passes the cap the old one is kept as `.1`.
fn append(path: &Path, entry: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_BYTES) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(entry.as_bytes())
}

/// Appends a free-form diagnostic line (e.g. recovery failures) to the log.
pub fn note(message: &str) {
    let _ = panic::catch_unwind(|| {
        if let Some(path) = log_path() {
            let _ = append(
                &path,
                &format!(
                    "[{}] hearfolio {} note: {message}\n\n",
                    crate::clock(),
                    env!("CARGO_PKG_VERSION")
                ),
            );
        }
    });
}

pub fn install() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Nothing here may panic: a panic inside the hook aborts the process.
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            if let Some(path) = log_path() {
                let _ = append(&path, &describe(info));
            }
        }));
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn append_rotates_past_the_cap() {
        let dir = std::env::temp_dir().join(format!("hearfolio-crashlog-{}", uuid::Uuid::new_v4()));
        let path = dir.join("logs").join("panic.log");
        append(&path, &"x".repeat(MAX_BYTES as usize + 1)).unwrap();
        append(&path, "second\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        assert!(path.with_extension("log.1").is_file());
        fs::remove_dir_all(dir).unwrap();
    }
}
