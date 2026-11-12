//! "Start at login": an XDG autostart entry (no systemd unit) that launches the app minimized.
//!
//! The app needs root (EC access), but an autostart entry runs as the desktop user, so the entry
//! starts a small launcher that asks polkit for root (`pkexec`, one password prompt per login) and
//! passes the display variables through (pkexec starts with a clean environment).
//!
//! Security: what pkexec runs must not be replaceable by an unprivileged process, so enabling
//! installs a root-owned *copy* of the binary under `/usr/local/lib/ripple-nova` instead of
//! pointing at the build directory. After rebuilding, switch the option off and on to refresh it.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct Layout {
    pub dir: PathBuf,
    pub entry: PathBuf,
}

impl Layout {
    pub fn system() -> Self {
        Self {
            dir: "/usr/local/lib/ripple-nova".into(),
            entry: "/etc/xdg/autostart/ripple-nova.desktop".into(),
        }
    }

    fn bin(&self) -> PathBuf {
        self.dir.join("rcc-gui")
    }

    fn launcher(&self) -> PathBuf {
        self.dir.join("autostart.sh")
    }

    pub fn is_enabled(&self) -> bool {
        self.entry.exists()
    }

    fn launcher_script(&self) -> String {
        format!(
            "#!/bin/sh\n\
             # Installed by Ripple Control Center (Settings > Start at login).\n\
             # Runs at login as the desktop user and asks polkit for root, because the app needs the\n\
             # EC. The display variables are passed through: pkexec starts with a clean environment.\n\
             set --\n\
             for v in WAYLAND_DISPLAY XDG_RUNTIME_DIR DISPLAY XAUTHORITY; do\n\
             \teval \"val=\\${{$v}}\"\n\
             \t[ -n \"$val\" ] && set -- \"$@\" \"$v=$val\"\n\
             done\n\
             exec pkexec env \"$@\" {} --minimized\n",
            self.bin().display()
        )
    }

    fn desktop_entry(&self) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Ripple Control Center\n\
             Comment=Follows the mode button and keeps the mode lights in sync (asks for your password: it needs root)\n\
             Exec={}\n\
             Terminal=false\n\
             NoDisplay=true\n\
             X-GNOME-Autostart-enabled=true\n\
             X-GNOME-Autostart-Delay=5\n",
            self.launcher().display()
        )
    }

    /// Install the root-owned copy of `exe`, the launcher and the autostart entry.
    pub fn enable(&self, exe: &Path) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        if exe != self.bin() {
            // copy + rename: replacing a binary that is currently running fails with ETXTBSY
            let tmp = self.dir.join(".rcc-gui.new");
            std::fs::copy(exe, &tmp)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            std::fs::rename(&tmp, self.bin())?;
        }
        std::fs::write(self.launcher(), self.launcher_script())?;
        std::fs::set_permissions(self.launcher(), std::fs::Permissions::from_mode(0o755))?;
        if let Some(parent) = self.entry.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.entry, self.desktop_entry())?;
        std::fs::set_permissions(&self.entry, std::fs::Permissions::from_mode(0o644))
    }

    /// Remove the entry, the launcher and the installed copy (missing files are fine).
    pub fn disable(&self) -> io::Result<()> {
        for p in [self.entry.clone(), self.launcher(), self.bin()] {
            match std::fs::remove_file(&p) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        let _ = std::fs::remove_dir(&self.dir); // only if empty
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_installs_a_copy_launcher_and_entry_and_disable_removes_them() {
        let root = std::env::temp_dir().join(format!("ripple-autostart-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("built-binary");
        std::fs::write(&exe, b"binary").unwrap();
        let l = Layout { dir: root.join("lib"), entry: root.join("autostart/ripple-nova.desktop") };

        assert!(!l.is_enabled());
        l.enable(&exe).unwrap();
        assert!(l.is_enabled());
        assert_eq!(std::fs::read(l.bin()).unwrap(), b"binary");
        assert_eq!(std::fs::metadata(l.bin()).unwrap().permissions().mode() & 0o777, 0o755);
        let script = std::fs::read_to_string(l.launcher()).unwrap();
        assert!(script.contains("exec pkexec env"));
        assert!(script.contains(&format!("{} --minimized", l.bin().display())));
        let entry = std::fs::read_to_string(&l.entry).unwrap();
        assert!(entry.contains(&format!("Exec={}", l.launcher().display())));

        // enabling twice (e.g. after a rebuild) refreshes the copy
        std::fs::write(&exe, b"binary v2").unwrap();
        l.enable(&exe).unwrap();
        assert_eq!(std::fs::read(l.bin()).unwrap(), b"binary v2");

        l.disable().unwrap();
        assert!(!l.is_enabled() && !l.bin().exists() && !l.launcher().exists());
        l.disable().unwrap(); // idempotent
        let _ = std::fs::remove_dir_all(&root);
    }
}
