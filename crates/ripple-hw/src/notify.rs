//! Desktop popups from a root process (the GUI, the daemon). Root cannot join a user's session
//! bus, so the popup is sent by `notify-send` dropped to the owner of the user's runtime directory
//! (`/run/user/<uid>`) with only that session bus in its environment. Never as root.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Held (exclusively) by the running window; the launcher and the daemon look at it too.
pub const GUI_LOCK: &str = "/run/ripple-nova-gui.lock";
const GUI_SETTINGS: &str = "/etc/ripple-nova/gui.json";

/// Send one popup to the session whose runtime directory is `dir`, best effort and off the calling
/// thread. Nothing is sent for a directory owned by root or without a session bus.
pub fn send_to(dir: &Path, title: &str, body: &str) {
    let Ok(md) = std::fs::metadata(dir) else { return };
    let (uid, gid) = (md.uid(), md.gid());
    if uid == 0 || !dir.join("bus").exists() {
        return;
    }
    let (dir, title, body) = (dir.to_path_buf(), title.to_string(), body.to_string());
    std::thread::spawn(move || {
        // `status()` waits for the child, so it does not linger as a zombie
        let _ = Command::new("setpriv")
            .args(["--reuid", &uid.to_string(), "--regid", &gid.to_string(), "--clear-groups"])
            .args(["notify-send", "--app-name=Ripple Control Center", "--icon=ripple-nova", "--transient", "--"])
            .args([&title, &body])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}/bus", dir.display()))
            .status();
    });
}

/// Send to everyone logged in (the daemon has no session of its own).
pub fn send_to_logged_in(title: &str, body: &str) {
    for dir in session_dirs(Path::new("/run/user")) {
        send_to(&dir, title, body);
    }
}

/// `<base>/<uid>` directories of real users that have a session bus.
fn session_dirs(base: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(base) else { return Vec::new() };
    rd.flatten()
        .filter(|e| e.file_name().to_string_lossy().parse::<u32>().is_ok_and(|uid| uid != 0))
        .map(|e| e.path())
        .filter(|p| p.join("bus").exists())
        .collect()
}

/// Is the window running? (It holds `GUI_LOCK` exclusively; a shared try-lock fails only then.)
pub fn gui_running() -> bool {
    std::fs::File::open(GUI_LOCK).is_ok_and(|f| matches!(f.try_lock_shared(), Err(std::fs::TryLockError::WouldBlock)))
}

/// The "Show popup" setting (the window saves it next to its other preferences): on unless switched off.
pub fn popups_enabled() -> bool {
    popups_from(&std::fs::read_to_string(GUI_SETTINGS).unwrap_or_default())
}

fn popups_from(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("popups").and_then(|p| p.as_bool()))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_are_on_unless_the_file_says_off() {
        assert!(popups_from(""));
        assert!(popups_from("not json"));
        assert!(popups_from(r#"{"light":true}"#));
        assert!(popups_from(r#"{"popups":true}"#));
        assert!(!popups_from(r#"{"popups":false}"#));
    }

    #[test]
    fn only_numeric_non_root_dirs_with_a_bus_count_as_sessions() {
        let base = std::env::temp_dir().join(format!("ripple-notify-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["1000", "0", "notanid"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("1000/bus"), b"").unwrap();
        std::fs::write(base.join("0/bus"), b"").unwrap();
        std::fs::write(base.join("notanid/bus"), b"").unwrap();
        std::fs::create_dir_all(base.join("1001")).unwrap(); // a user without a bus
        assert_eq!(session_dirs(&base), [base.join("1000")]);
        assert!(session_dirs(&base.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
