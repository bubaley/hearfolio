#[cfg(any(target_os = "macos", test))]
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};
#[cfg(target_os = "macos")]
use tauri::Manager;

// The parent keeps the pipe open until the OS closes its descriptors on exit.
// Arguments are separate argv values, including paths with shell metacharacters.
#[cfg(any(target_os = "macos", test))]
const WAIT_FOR_EXIT: &str = "IFS= read -r _; exec \"$@\"";

#[cfg(any(target_os = "macos", test))]
fn application_bundle(executable: &Path) -> io::Result<PathBuf> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "not an application bundle");
    let macos = executable.parent().ok_or_else(invalid)?;
    let contents = macos.parent().ok_or_else(invalid)?;
    let bundle = contents.parent().ok_or_else(invalid)?;
    if macos.file_name() != Some("MacOS".as_ref())
        || contents.file_name() != Some("Contents".as_ref())
        || bundle.extension() != Some("app".as_ref())
        || !executable.is_file()
        || !contents.join("Info.plist").is_file()
    {
        return Err(invalid());
    }
    bundle.canonicalize()
}

#[cfg(any(target_os = "macos", test))]
fn launch_after_exit(launcher: &Path, arguments: &[OsString]) -> io::Result<(Child, ChildStdin)> {
    let mut child = Command::new("/bin/sh")
        .args(["-c", WAIT_FOR_EXIT, "hearfolio-relaunch"])
        .arg(launcher)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pipe = child.stdin.take().expect("piped helper stdin");
    Ok((child, pipe))
}

#[cfg(target_os = "macos")]
struct RelaunchTicket {
    _pipe: ChildStdin,
    _job: crate::JobGuard,
}

#[cfg(target_os = "macos")]
static PENDING_RELAUNCH: std::sync::Mutex<Option<RelaunchTicket>> = std::sync::Mutex::new(None);

#[tauri::command]
pub fn restart_application(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let job = crate::JobGuard::acquire()?;
        let binary = tauri::process::current_binary(&app.env())
            .map_err(|_| "errors.restartUnavailable".to_owned())?;
        let bundle =
            application_bundle(&binary).map_err(|_| "errors.restartUnavailable".to_owned())?;
        let mut pending = PENDING_RELAUNCH
            .lock()
            .map_err(|_| "errors.restartFailed".to_owned())?;
        let (_, pipe) = launch_after_exit(
            Path::new("/usr/bin/open"),
            &[OsString::from("-n"), bundle.into_os_string()],
        )
        .map_err(|error| format!("errors.restartFailed|{error}"))?;
        *pending = Some(RelaunchTicket {
            _pipe: pipe,
            _job: job,
        });
        drop(pending);
        // Use normal Tauri cleanup. The helper starts via LaunchServices only
        // after this process exits, rather than spawning the app executable.
        app.exit(0);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("errors.restartUnavailable".into())
    }
}

#[cfg(target_os = "macos")]
pub fn install_menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::{
        menu::{Menu, MenuItem, MenuItemKind},
        Emitter,
    };
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(application)) = menu.items()?.into_iter().next() {
        let restart = MenuItem::with_id(
            app,
            "hearfolio-restart",
            "Restart Hearfolio",
            true,
            None::<&str>,
        )?;
        // Tauri's default application submenu ends with the native Quit item.
        application.insert(&restart, application.items()?.len().saturating_sub(1))?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id().as_ref() == "hearfolio-restart" {
            let _ = app.emit("hearfolio-request-restart", ());
        }
    });
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "hearfolio-restart-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn executable(&self) -> PathBuf {
            let path = self
                .0
                .join("Hearfolio QA ' $; spaces.app/Contents/MacOS/speechdesk");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"fixture executable").unwrap();
            fs::write(
                path.parent().unwrap().parent().unwrap().join("Info.plist"),
                b"fixture plist",
            )
            .unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn validates_the_current_bundle_and_rejects_unbundled_executables() {
        let fixture = Fixture::new();
        let executable = fixture.executable();
        assert_eq!(
            application_bundle(&executable).unwrap(),
            executable
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .canonicalize()
                .unwrap()
        );
        let unbundled = fixture.0.join("speechdesk");
        fs::write(&unbundled, b"fixture").unwrap();
        assert!(application_bundle(&unbundled).is_err());
        fs::remove_file(
            executable
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("Info.plist"),
        )
        .unwrap();
        assert!(application_bundle(&executable).is_err());
    }

    #[test]
    fn helper_waits_for_exit_and_preserves_exact_path_arguments() {
        let fixture = Fixture::new();
        let bundle = application_bundle(&fixture.executable()).unwrap();
        let launcher = fixture.0.join("fake launcher.sh");
        let receipt = fixture.0.join("launcher receipt.txt");
        fs::write(
            &launcher,
            b"#!/bin/sh\nreceipt=\"$1\"; shift\nprintf '%s\\n' \"$@\" > \"$receipt\"\n",
        )
        .unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        let (mut child, pipe) = launch_after_exit(
            &launcher,
            &[
                receipt.clone().into_os_string(),
                "-n".into(),
                bundle.clone().into_os_string(),
            ],
        )
        .unwrap();
        thread::sleep(Duration::from_millis(75));
        assert!(
            !receipt.exists(),
            "must not launch while the parent pipe is open"
        );
        assert!(child.try_wait().unwrap().is_none());
        drop(pipe);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("helper did not launch after EOF");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(receipt).unwrap(),
            format!("-n\n{}\n", bundle.display())
        );
    }
}
