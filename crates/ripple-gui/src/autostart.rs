//! "Open at login": an XDG autostart entry (no systemd unit) that starts the app at login.
//!
//! Two modes, chosen by "Start minimized". Without it the launcher opens the window, and the window
//! starts the tray icon too. With it the launcher starts only the tray icon (the window opens from
//! it), which runs as the desktop user and needs no root. The systemd service is independent: it
//! always starts at boot.
//!
//! The window needs root (EC access), but an autostart entry runs as the desktop user, so the
//! launcher gets root without a password through sudo when install.sh's sudoers rule allows it,
//! else from polkit (`pkexec`, one password prompt per login, with the display variables passed
//! through because pkexec starts with a clean environment).
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

    fn launcher_script(&self, tray: &Path, minimized: bool) -> String {
        if minimized {
            // tray only: it runs as the desktop user, so no root is involved
            return format!(
                "#!/bin/sh\n\
                 # Installed by Ripple Control Center (Settings > Open at login, Start minimized).\n\
                 # Runs at login as the desktop user. Start minimized: only the tray icon, which opens the\n\
                 # window on a click. Nothing is started when the tray already runs.\n\
                 pgrep -x rcc-tray >/dev/null 2>&1 && exit 0\n\
                 exec {tray}\n",
                tray = sh_quote(tray)
            );
        }
        // the sudo probe must ask about exactly what it then runs, or the sudoers rule would not match
        format!(
            "#!/bin/sh\n\
             # Installed by Ripple Control Center (Settings > Open at login).\n\
             # Runs at login as the desktop user and opens the window, which starts the tray icon too. The\n\
             # window needs root for the EC: sudo does it without a password when install.sh's sudoers rule\n\
             # is there, otherwise polkit asks. Nothing is started when the window is already running.\n\
             lock=/run/ripple-nova-gui.lock\n\
             if [ -e \"$lock\" ] && ! flock -n \"$lock\" true 2>/dev/null; then exit 0; fi\n\
             if sudo -n -l {bin} >/dev/null 2>&1; then exec sudo -n {bin}; fi\n\
             # pkexec starts with a clean environment: pass the display variables through\n\
             set --\n\
             for v in WAYLAND_DISPLAY XDG_RUNTIME_DIR DISPLAY XAUTHORITY; do\n\
             \teval \"val=\\${{$v}}\"\n\
             \t[ -n \"$val\" ] && set -- \"$@\" \"$v=$val\"\n\
             done\n\
             exec pkexec env \"$@\" {bin}\n",
            bin = self.bin().display()
        )
    }

    fn desktop_entry(&self) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Ripple Control Center\n\
             Comment=Starts the tray icon and, unless Start minimized is on, the window\n\
             Icon=ripple-nova\n\
             Exec={}\n\
             Terminal=false\n\
             NoDisplay=true\n\
             X-GNOME-Autostart-enabled=true\n\
             X-GNOME-Autostart-Delay=5\n",
            self.launcher().display()
        )
    }

    /// Install the root-owned copy of `exe`, the launcher and the autostart entry. `minimized`: start
    /// only the tray icon (`tray`), not the window. Calling it again refreshes everything (after a
    /// rebuild, or when the minimized option changes).
    pub fn enable(&self, exe: &Path, tray: &Path, minimized: bool) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        if exe != self.bin() {
            // copy + rename: replacing a binary that is currently running fails with ETXTBSY
            let tmp = self.dir.join(".rcc-gui.new");
            std::fs::copy(exe, &tmp)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            std::fs::rename(&tmp, self.bin())?;
        }
        std::fs::write(self.launcher(), self.launcher_script(tray, minimized))?;
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

/// A path as one shell word.
fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_installs_a_copy_launcher_and_entry_and_disable_removes_them() {
        let root =
            std::env::temp_dir().join(format!("ripple-autostart-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("built-binary");
        std::fs::write(&exe, b"binary").unwrap();
        let l = Layout {
            dir: root.join("lib"),
            entry: root.join("autostart/ripple-nova.desktop"),
        };
        let tray = root.join("it's a tray/rcc-tray");

        assert!(!l.is_enabled());
        l.enable(&exe, &tray, false).unwrap();
        assert!(l.is_enabled());
        assert_eq!(std::fs::read(l.bin()).unwrap(), b"binary");
        assert_eq!(
            std::fs::metadata(l.bin()).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // the window mode: sudo is asked about exactly what it then runs, and polkit stays the fallback
        let script = std::fs::read_to_string(l.launcher()).unwrap();
        let bin = l.bin().display().to_string();
        assert!(
            script.contains("exec pkexec env"),
            "polkit stays the fallback"
        );
        assert!(
            script.contains(&format!(
                "sudo -n -l {bin} >/dev/null 2>&1; then exec sudo -n {bin}; fi"
            )),
            "{script}"
        );
        assert!(script.contains("flock -n"), "no second instance");
        assert!(!script.contains("--minimized") && !script.contains("rcc-tray"));
        let parsed = std::process::Command::new("sh")
            .arg("-n")
            .arg(l.launcher())
            .status()
            .unwrap();
        assert!(
            parsed.success(),
            "the generated launcher must be valid shell:\n{script}"
        );
        let entry = std::fs::read_to_string(&l.entry).unwrap();
        assert!(entry.contains(&format!("Exec={}", l.launcher().display())));

        // enabling twice (e.g. after a rebuild) refreshes the copy
        std::fs::write(&exe, b"binary v2").unwrap();
        l.enable(&exe, &tray, false).unwrap();
        assert_eq!(std::fs::read(l.bin()).unwrap(), b"binary v2");

        // "start minimized": only the tray icon, as the user, no root, and not twice
        l.enable(&exe, &tray, true).unwrap();
        let minimized = std::fs::read_to_string(l.launcher()).unwrap();
        assert!(
            minimized.contains(&format!("exec {}\n", sh_quote(&tray))),
            "{minimized}"
        );
        // a shell reads the quoted word back as the very same path, quote and space included
        let echoed = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", sh_quote(&tray)))
            .output()
            .unwrap();
        assert_eq!(echoed.stdout, tray.display().to_string().into_bytes());
        assert!(minimized.contains("pgrep -x rcc-tray"), "no second tray");
        assert!(
            !minimized.contains("sudo") && !minimized.contains("pkexec"),
            "the tray needs no root: {minimized}"
        );
        assert!(std::process::Command::new("sh")
            .arg("-n")
            .arg(l.launcher())
            .status()
            .unwrap()
            .success());

        l.disable().unwrap();
        assert!(!l.is_enabled() && !l.bin().exists() && !l.launcher().exists());
        l.disable().unwrap(); // idempotent
        let _ = std::fs::remove_dir_all(&root);
    }
}
