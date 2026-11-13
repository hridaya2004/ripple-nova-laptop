//! The touchpad's power switch. The LED next to the touchpad means "powered off": measured, it lights as soon
//! as the i2c-hid driver is unbound from the touchpad (the kernel then puts the device in ACPI D3, which is also
//! what suspend does to it, hence the lit LED while suspended) and goes out when the driver is bound again.
//! The firmware attributes `touchpad_toggle_enable` / `breathing_in_suspend` have nothing to do with it.
//!
//! So "touchpad off" = unbind: no pointer, LED on. "On" = bind. Binding is world-readable state, so anyone
//! can ask whether it is on; changing it takes root.

use std::path::Path;

use crate::{HwError, HwResult};

const DEVICES: &str = "/sys/bus/i2c/devices";
const DRIVER: &str = "/sys/bus/i2c/drivers/i2c_hid_acpi";

/// The touchpad's i2c device (`i2c-UNIW0001:00` here; Uniwill's ACPI id is UNIW*).
fn device_in(devices: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(devices)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("i2c-UNIW"))
        .collect();
    names.sort();
    names.into_iter().next()
}

fn is_on_in(devices: &Path) -> Option<bool> {
    let dev = device_in(devices)?;
    Some(devices.join(dev).join("driver").exists())
}

/// Is the touchpad powered (its driver bound)? `None`: this machine has no such touchpad.
pub fn is_on() -> Option<bool> {
    is_on_in(Path::new(DEVICES))
}

/// Power the touchpad on or off (needs root).
pub fn set(on: bool) -> HwResult<()> {
    set_in(Path::new(DEVICES), Path::new(DRIVER), on)
}

fn set_in(devices: &Path, driver: &Path, on: bool) -> HwResult<()> {
    let dev = device_in(devices)
        .ok_or_else(|| HwError::Unavailable("no Uniwill touchpad (i2c-UNIW*) found".into()))?;
    if is_on_in(devices) == Some(on) {
        return Ok(());
    }
    let file = driver.join(if on { "bind" } else { "unbind" });
    std::fs::write(&file, &dev).map_err(|e| HwError::Other(format!("{}: {e}", file.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_unbinds_and_on_binds_and_a_state_that_already_holds_writes_nothing() {
        let root =
            std::env::temp_dir().join(format!("ripple-touchpad-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (devices, driver) = (root.join("devices"), root.join("driver"));
        std::fs::create_dir_all(devices.join("i2c-UNIW0001:00")).unwrap();
        std::fs::create_dir_all(devices.join("i2c-0")).unwrap(); // not the touchpad
        std::fs::create_dir_all(&driver).unwrap();
        assert_eq!(device_in(&devices).as_deref(), Some("i2c-UNIW0001:00"));

        // no `driver` link: unbound = off. Turning it off again writes nothing; on binds.
        assert_eq!(is_on_in(&devices), Some(false));
        set_in(&devices, &driver, false).unwrap();
        assert!(!driver.join("unbind").exists() && !driver.join("bind").exists());
        set_in(&devices, &driver, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(driver.join("bind")).unwrap(),
            "i2c-UNIW0001:00"
        );

        // a `driver` link: bound = on; off unbinds
        std::fs::create_dir_all(devices.join("i2c-UNIW0001:00/driver")).unwrap();
        assert_eq!(is_on_in(&devices), Some(true));
        set_in(&devices, &driver, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(driver.join("unbind")).unwrap(),
            "i2c-UNIW0001:00"
        );

        // no touchpad at all
        assert_eq!(is_on_in(&root.join("nothing")), None);
        assert!(set_in(&root.join("nothing"), &driver, true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
