//! The tray icon. It runs in the desktop user's session: the window is a root process and cannot own a
//! StatusNotifierItem on the user's session bus. A click (or "Open") runs the launcher, which opens the
//! window (or brings the open one forward); "Quit" closes the window and the icon.
//!
//! GNOME shows tray icons only with the AppIndicator extension. Until a tray host exists the helper
//! simply waits (it does not exit), so enabling the extension later is enough.

use std::process::Command;

use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, Tray};

struct RippleTray;

/// Run a command detached; the thread reaps it.
fn run(program: &'static str, args: &'static [&'static str]) {
    std::thread::spawn(move || {
        let _ = Command::new(program).args(args).status();
    });
}

fn open_window() {
    run("ripple-nova", &[]);
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
        match RippleTray.assume_sni_available(true).spawn() {
            Ok(handle) => {
                while !handle.is_closed() {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
                eprintln!("rcc-tray: the tray service ended, starting it again");
            }
            Err(e) => eprintln!("rcc-tray: {e}"),
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}
