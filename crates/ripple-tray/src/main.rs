//! The tray icon. It runs in the desktop user's session: the window is a root process and cannot own a
//! StatusNotifierItem on the user's session bus. A click (or "Open") runs the launcher, which opens the
//! window (or brings the open one forward); "Quit" closes the window and the icon.
//!
//! The menu also holds a shortcut for the performance profile (Balanced / Enthusiast / Overboost). The
//! current one is read from the file the daemon publishes (this process cannot read the EC); picking one
//! runs `rcc mode set <name>` as root, through the exact sudoers entries install.sh writes (polkit asks
//! when there are none).
//!
//! GNOME shows tray icons only with the AppIndicator extension. Until a tray host exists the helper
//! simply waits (it does not exit), so enabling the extension later is enough.

use std::process::Command;
use std::time::Duration;

use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, RadioGroup, RadioItem, StandardItem};
use ksni::{Category, Tray};

/// `(name for rcc, label)`. The sudoers rule lists `rcc mode set <name>` for exactly these three.
const PROFILES: [(&str, &str); 3] = [
    ("balanced", "Balanced"),
    ("enthusiast", "Enthusiast"),
    ("overboost", "Overboost"),
];

/// Written by the daemon once a second (`ripple_hw::control::PROFILE_FILE`): the profile it sees, lowercase.
const PROFILE_FILE: &str = "/run/ripple-nova-profile";

struct RippleTray {
    /// What the daemon last published (`custom` and `unknown` are possible); `None` = no fresh file, so no daemon.
    profile: Option<String>,
}

/// The published profile, unless the file is stale (the daemon rewrites it every second).
fn published_profile() -> Option<String> {
    read_profile(std::path::Path::new(PROFILE_FILE))
}

fn read_profile(path: &std::path::Path) -> Option<String> {
    let age = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?;
    if age > Duration::from_secs(5) {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string())
}

/// Run a command detached; the thread reaps it.
fn run(program: &'static str, args: &'static [&'static str]) {
    std::thread::spawn(move || {
        let _ = Command::new(program).args(args).status();
    });
}

fn open_window() {
    run("ripple-nova", &[]);
}

/// Switch the profile. `sudo -n` when the sudoers rule allows exactly this command, else polkit asks (as the
/// launcher does). The daemon notices the change within a second: the menu and the popup follow from there.
fn set_profile(name: &'static str) {
    std::thread::spawn(move || {
        let rcc = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("rcc")))
            .unwrap_or_else(|| "/usr/local/bin/rcc".into());
        let args = ["mode", "set", name];
        let allowed = Command::new("sudo")
            .args(["-n", "-l"])
            .arg(&rcc)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success());
        let done = if allowed {
            Command::new("sudo").arg("-n").arg(&rcc).args(args).status()
        } else {
            Command::new("pkexec").arg(&rcc).args(args).status()
        };
        if !done.is_ok_and(|s| s.success()) {
            eprintln!("rcc-tray: could not switch the profile to {name}");
        }
    });
}

impl Tray for RippleTray {
    fn id(&self) -> String {
        "ripple-nova".into()
    }

    fn title(&self) -> String {
        "Ripple Control Center".into()
    }

    fn icon_name(&self) -> String {
        "ripple-nova".into()
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        open_window();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let current = self.profile.as_deref();
        // the user's own Custom preset (or limits that match nothing) ticks none of the three
        let selected = PROFILES
            .iter()
            .position(|(name, _)| Some(*name) == current)
            .unwrap_or(usize::MAX);
        let header = match current {
            None => "Performance profile (service not running)",
            Some("custom") => "Performance profile: Custom",
            Some(_) => "Performance profile",
        };
        vec![
            StandardItem {
                label: "Open Ripple Control Center".into(),
                icon_name: "ripple-nova".into(),
                activate: Box::new(|_| open_window()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: header.into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            RadioGroup {
                selected,
                select: Box::new(|_, i| set_profile(PROFILES[i].0)),
                options: PROFILES
                    .iter()
                    .map(|(_, label)| RadioItem {
                        label: (*label).into(),
                        ..Default::default()
                    })
                    .collect(),
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| {
                    // closes the window if one is open (fails quietly if not), then the icon goes
                    let _ = Command::new("rcc-gui").arg("--quit").status();
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn main() {
    // `assume_sni_available`: no tray host yet (the GNOME extension loads later, or after a re-login) is
    // not an error; the icon appears when one shows up. If the service ever ends (or cannot start), try again:
    // the icon is meant to be always there; "Quit" is the only way out.
    loop {
        let mut shown = published_profile();
        match (RippleTray {
            profile: shown.clone(),
        })
        .assume_sni_available(true)
        .spawn()
        {
            Ok(handle) => {
                while !handle.is_closed() {
                    std::thread::sleep(Duration::from_secs(1));
                    // only a real change re-sends the menu (an open menu would be redrawn for nothing)
                    let now = published_profile();
                    if now != shown {
                        shown = now.clone();
                        handle.update(|tray| tray.profile = now);
                    }
                }
                eprintln!("rcc-tray: the tray service ended, starting it again");
            }
            Err(e) => eprintln!("rcc-tray: {e}"),
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(ticked index, header label)` of the profile part of the menu.
    fn profile_menu(profile: Option<&str>) -> (usize, String) {
        let tray = RippleTray {
            profile: profile.map(String::from),
        };
        let items = tray.menu();
        let selected = items
            .iter()
            .find_map(|i| match i {
                MenuItem::RadioGroup(g) => Some(g.selected),
                _ => None,
            })
            .expect("a radio group");
        let header = items
            .iter()
            .find_map(|i| match i {
                MenuItem::Standard(s) if s.label.starts_with("Performance profile") => {
                    Some(s.label.clone())
                }
                _ => None,
            })
            .expect("a header");
        (selected, header)
    }

    #[test]
    fn the_menu_ticks_the_published_profile() {
        for (i, (name, _)) in PROFILES.iter().enumerate() {
            assert_eq!(
                profile_menu(Some(name)),
                (i, "Performance profile".to_string())
            );
        }
        // the user's Custom preset ticks none of the three and says so
        assert_eq!(
            profile_menu(Some("custom")),
            (usize::MAX, "Performance profile: Custom".to_string())
        );
        assert_eq!(profile_menu(Some("unknown")).0, usize::MAX);
        // no daemon: nothing ticked, and the header says why
        assert_eq!(
            profile_menu(None),
            (
                usize::MAX,
                "Performance profile (service not running)".to_string()
            )
        );
    }

    #[test]
    fn a_stale_file_means_no_daemon() {
        let path = std::env::temp_dir().join(format!("ripple-tray-test-{}", std::process::id()));
        std::fs::write(&path, "gaming\n").unwrap();
        assert_eq!(read_profile(&path).as_deref(), Some("gaming"));
        let old = std::time::SystemTime::now() - Duration::from_secs(30);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(read_profile(&path), None);
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_profile(&path), None);
    }
}
