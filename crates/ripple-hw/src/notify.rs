//! Desktop popups, sent by the daemon (so they work with no window open). Root cannot join a user's session
//! bus, so the popup is sent by `notify-send` dropped to the owner of the user's runtime directory
//! (`/run/user/<uid>`) with only that session bus in its environment. Never as root.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::control::Profile;

/// Held (exclusively) by the running window: one window per machine.
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

/// How a platform toggle is named in the window and in popups.
pub fn toggle_label(name: &str) -> &str {
    match name {
        "fn_lock" => "Fn lock",
        "super_key_enable" => "Super key",
        "rainbow_animation" => "Lightbar rainbow",
        "breathing_in_suspend" => "Breathing in suspend",
        "touchpad" => "Touchpad power",
        "touchpad_toggle_enable" => "Touchpad toggle (firmware)",
        "ac_auto_boot" => "AC Auto-Boot",
        "usb_powershare_high" => "USB powershare",
        other => other,
    }
}

/// What the popups are about, as read from the hardware.
#[derive(Clone, Default)]
pub struct Observed {
    pub profile: Option<Profile>,
    /// Current PL1/PL2, for the popup text.
    pub pl: Option<(u8, u8)>,
    pub boosted: bool,
    pub toggles: Vec<(&'static str, bool)>,
}

/// Remembers what was last seen and turns differences into popups (title, body). Nothing "custom" pops
/// up (the custom fan profile, switching to the Custom performance preset): those are the user's own
/// settings, made in the window. Whoever causes a change (a click, the hardware button, the Fn key, the
/// CLI), it is reported once.
#[derive(Default)]
pub struct Notifier {
    seen: bool,
    /// Last *known* profile: a failed read (`None`) must not hide the change that follows it.
    profile: Option<Profile>,
    boosted: bool,
    toggles: Vec<(&'static str, bool)>,
}

impl Notifier {
    /// Everything that changed since the previous call. The first call only records the starting state.
    pub fn observe(&mut self, s: &Observed) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let first = !std::mem::replace(&mut self.seen, true);
        let onoff = |on: bool| if on { "On" } else { "Off" }.to_string();

        if let Some(now) = s.profile {
            // switching *to* Custom is the user's own preset at work: no popup (leaving it for a stock one is)
            if !first && now != Profile::Custom && self.profile.is_some_and(|before| before != now) {
                let limits = s.pl.map(|(a, b)| format!(": PL1 {a} W / PL2 {b} W")).unwrap_or_default();
                out.push(("Performance profile".into(), format!("{}{limits}", now.name())));
            }
            self.profile = Some(now);
        }
        if !first && s.boosted != self.boosted {
            out.push(("Fan boost".into(), onoff(s.boosted)));
        }
        for (name, on) in &s.toggles {
            let before = self.toggles.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
            if !first && before.is_some_and(|b| b != *on) {
                out.push((toggle_label(name).to_string(), onoff(*on)));
            }
        }
        self.boosted = s.boosted;
        self.toggles = s.toggles.clone();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_labels_use_the_real_sysfs_names() {
        // the keys are the kernel attribute names (see PLATFORM_ATTRS); a typo here leaves the raw name on screen
        for name in crate::linux::PLATFORM_ATTRS {
            if matches!(*name, "fn_lock" | "super_key_enable" | "ac_auto_boot" | "usb_powershare_high" | "breathing_in_suspend" | "touchpad_toggle_enable") {
                assert_ne!(toggle_label(name), *name, "{name} has no label");
            }
        }
        assert_eq!(toggle_label("ac_auto_boot"), "AC Auto-Boot");
    }

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

    fn seen(profile: Option<Profile>, boosted: bool, toggles: &[(&'static str, bool)]) -> Observed {
        Observed { profile, pl: Some((55, 55)), boosted, toggles: toggles.to_vec() }
    }

    #[test]
    fn reports_each_change_once_and_skips_the_start_and_failed_reads() {
        use ripple_core::modes::OperatingMode::*;
        let mut n = Notifier::default();
        let a = Some(Profile::Stock(Office));
        let b = Some(Profile::Stock(Gaming));
        // the first observation only records
        assert!(n.observe(&seen(a, false, &[("fn_lock", false)])).is_empty());
        assert!(n.observe(&seen(a, false, &[("fn_lock", false)])).is_empty());
        // a profile change, with its limits
        assert_eq!(n.observe(&seen(b, false, &[("fn_lock", false)])), [("Performance profile".to_string(), "Enthusiast: PL1 55 W / PL2 55 W".to_string())]);
        // a failed read in between hides nothing: Gaming -> (unknown) -> Office is still Gaming -> Office
        assert!(n.observe(&seen(None, false, &[("fn_lock", false)])).is_empty());
        assert_eq!(n.observe(&seen(a, false, &[("fn_lock", false)])).len(), 1);
        // boost on and off, a toggle on
        assert_eq!(n.observe(&seen(a, true, &[("fn_lock", false)])), [("Fan boost".to_string(), "On".to_string())]);
        assert_eq!(
            n.observe(&seen(a, false, &[("fn_lock", true)])),
            [("Fan boost".to_string(), "Off".to_string()), ("Fn lock".to_string(), "On".to_string())]
        );
        // nothing "custom" pops up: switching to the Custom performance profile does not, leaving it for a stock one does
        let custom = Some(Profile::Custom);
        assert!(n.observe(&seen(custom, false, &[("fn_lock", true)])).is_empty());
        assert_eq!(n.observe(&seen(b, false, &[("fn_lock", true)])).len(), 1);
    }

    #[test]
    fn a_profile_unknown_at_the_start_is_not_a_change_when_it_resolves() {
        let mut n = Notifier::default();
        assert!(n.observe(&seen(None, false, &[])).is_empty());
        assert!(n.observe(&seen(Some(Profile::Stock(ripple_core::modes::OperatingMode::Turbo)), false, &[])).is_empty());
    }
}
