//! Desktop popups for changes the user cares about: the performance profile, fan boost and the
//! platform toggles (Fn lock, ...). Nothing "custom" pops up (the custom fan profile, the custom
//! performance profile preset): those are the user's own settings, made in the window. Whoever
//! causes a change (a click here, the
//! hardware button, the Fn key, the CLI), the GUI sees it in its next snapshot and reports it once.
//!
//! Delivery is `ripple_hw::notify` (the daemon uses it too, for button presses while no window is open).

use ripple_hw::control::Profile;

use crate::hw::{toggle_label, Snapshot};

/// Send one popup to the session this window runs in (best effort). Without a runtime directory
/// nothing is sent.
pub fn send(title: &str, body: &str) {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        ripple_hw::notify::send_to(std::path::Path::new(&dir), title, body);
    }
}

/// Remembers what was last seen and turns differences into popups.
#[derive(Default)]
pub struct Notifier {
    seen: bool,
    /// Last *known* profile: a failed read (`None`) must not hide the change that follows it.
    profile: Option<Profile>,
    boosted: bool,
    toggles: Vec<(&'static str, bool)>,
}

impl Notifier {
    /// `(title, body)` for everything that changed since the previous snapshot. The first snapshot
    /// only records the starting state.
    pub fn observe(&mut self, s: &Snapshot) -> Vec<(String, String)> {
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
    use ripple_core::modes::OperatingMode::*;

    fn snap(profile: Option<Profile>, boosted: bool, toggles: &[(&'static str, bool)]) -> Snapshot {
        Snapshot { profile, pl: Some((55, 55)), boosted, toggles: toggles.to_vec(), ..Snapshot::default() }
    }

    #[test]
    fn reports_each_change_once_and_skips_the_start_and_failed_reads() {
        let mut n = Notifier::default();
        let a = Some(Profile::Stock(Office));
        let b = Some(Profile::Stock(Gaming));
        // the first snapshot only records
        assert!(n.observe(&snap(a, false, &[("fn_lock", false)])).is_empty());
        assert!(n.observe(&snap(a, false, &[("fn_lock", false)])).is_empty());
        // a profile change, with its limits
        assert_eq!(n.observe(&snap(b, false, &[("fn_lock", false)])), [("Performance profile".to_string(), "Enthusiast: PL1 55 W / PL2 55 W".to_string())]);
        // a failed read in between hides nothing: Gaming -> (unknown) -> Office is still Gaming -> Office
        assert!(n.observe(&snap(None, false, &[("fn_lock", false)])).is_empty());
        assert_eq!(n.observe(&snap(a, false, &[("fn_lock", false)])).len(), 1);
        // boost on and off, a toggle on
        assert_eq!(n.observe(&snap(a, true, &[("fn_lock", false)])), [("Fan boost".to_string(), "On".to_string())]);
        assert_eq!(n.observe(&snap(a, false, &[("fn_lock", true)])), [("Fan boost".to_string(), "Off".to_string()), ("Fn lock".to_string(), "On".to_string())]);
        // nothing "custom" pops up: not the custom fan profile ...
        let mut s = snap(a, false, &[("fn_lock", true)]);
        s.fan.enabled = true;
        assert!(n.observe(&s).is_empty());
        // ... and not switching to the Custom performance profile, but leaving it for a stock one does
        let custom = Some(Profile::Custom);
        assert!(n.observe(&snap(custom, false, &[("fn_lock", true)])).is_empty());
        assert_eq!(n.observe(&snap(b, false, &[("fn_lock", true)])).len(), 1);
    }

    #[test]
    fn a_profile_unknown_at_the_start_is_not_a_change_when_it_resolves() {
        let mut n = Notifier::default();
        assert!(n.observe(&snap(None, false, &[])).is_empty());
        assert!(n.observe(&snap(Some(Profile::Stock(Turbo)), false, &[])).is_empty());
    }
}
