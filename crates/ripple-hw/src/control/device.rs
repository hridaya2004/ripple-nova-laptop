//! Platform toggles and the touchpad's power.

use super::*;

/// Does the EC report a lightbar? (`SupportByteOneFlag` bit 6, the stock app's
/// `LightbarSupport`.) This machine: no, even though the force-loaded kernel driver
/// still exposes a lightbar LED and the rainbow/breathing attributes.
pub fn has_lightbar(ec: &mut dyn EcAccess) -> bool {
    ec.read(addr::SUPPORT_BYTE1).is_ok_and(|b| b & 0x40 != 0)
}

/// Does a platform attribute get a toggle? `ctgp_offset` belongs to the profiles; `rainbow_animation` (older
/// drivers; the newer one made it an LED trigger) and `breathing_in_suspend` are lightbar animations and mean
/// nothing without a lightbar, so they follow `has_lightbar` (the same test that shows the lightbar controls).
/// The rest are shown on every machine. `touchpad_toggle_enable` (the firmware's double tap) did nothing visible
/// on this laptop; the real touchpad switch is `touchpad.rs`.
pub(super) fn toggle_shown(name: &str, has_lightbar: bool) -> bool {
    match name {
        "ctgp_offset" => false,
        "rainbow_animation" | "breathing_in_suspend" => has_lightbar,
        _ => true,
    }
}

/// The platform toggles that are plain 0/1 (Fn lock, ...), as `(name, on)`, plus the touchpad's power switch
/// as `("touchpad", on)` when there is a touchpad to switch.
pub fn platform_toggles(u: &SysfsEc, has_lightbar: bool) -> Vec<(&'static str, bool)> {
    let mut toggles: Vec<(&'static str, bool)> = u
        .platform_attrs()
        .into_iter()
        .filter(|(n, _)| toggle_shown(n, has_lightbar))
        .filter_map(|(n, v)| match v.as_deref() {
            Some("1") => Some((n, true)),
            Some("0") => Some((n, false)),
            _ => None,
        })
        .collect();
    if let Some(on) = crate::touchpad::is_on() {
        toggles.push(("touchpad", on));
    }
    toggles
}

/// Switch the touchpad on or off, now, and remember it: the daemon keeps the touchpad the way the user chose
/// (after a reboot the driver binds by itself, so an "off" has to be re-applied).
pub fn set_touchpad(on: bool) -> HwResult<()> {
    Config::update(|c| {
        c.touchpad_on = on;
        Ok(())
    })?;
    crate::touchpad::set(on)
}

/// The daemon's touchpad loop: every two seconds, make the touchpad match the saved choice.
pub fn touchpad_loop(stop: &std::sync::atomic::AtomicBool) {
    use std::sync::atomic::Ordering::Relaxed;
    while !stop.load(Relaxed) {
        let want = Config::load().touchpad_on;
        if crate::touchpad::is_on().is_some_and(|now| now != want) {
            if let Err(e) = crate::touchpad::set(want) {
                eprintln!("touchpad: {e}");
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}
