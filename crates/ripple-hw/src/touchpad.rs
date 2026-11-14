//! The touchpad's power switch. The LED next to the touchpad means "powered off": measured, it lights as soon
//! as the i2c-hid driver is unbound from the touchpad (the kernel then puts the device in ACPI D3, which is also
//! what suspend does to it, hence the lit LED while suspended) and goes out when the driver is bound again.
//! The firmware attributes `touchpad_toggle_enable` / `breathing_in_suspend` have nothing to do with it.
//!
//! So "touchpad off" = unbind: no pointer, LED on. "On" = bind. Binding is world-readable state, so anyone
//! can ask whether it is on; changing it takes root.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

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

/// The touchpad's "surface switch" (feature report 0x07 of the Precision Touchpad spec): 3 = it reports, 0 = it
/// is silent. Measured: 0 stops the pointer and lights the LED with the driver still bound, which is what the
/// firmware's own corner double tap does. Unlike the power switch above the pad stays awake, so the same
/// gesture can switch it on again. (The kernel driver writes 3 whenever it binds or resumes the pad.)
const SURFACE_REPORT: u8 = 7;
const HIDRAW: &str = "/sys/class/hidraw";

/// `HIDIOCGFEATURE` (nr 7) / `HIDIOCSFEATURE` (nr 6) for a buffer of `len` bytes.
fn hidioc(nr: u8, len: usize) -> libc::c_ulong {
    (3 << 30)
        | ((len as libc::c_ulong) << 16)
        | (libc::c_ulong::from(b'H') << 8)
        | libc::c_ulong::from(nr)
}

/// The touchpad's hidraw node: an i2c HID device (bus 0x18) of PixArt (vendor 0x093A).
fn hidraw_in(class: &Path) -> Option<PathBuf> {
    for e in std::fs::read_dir(class).ok()?.flatten() {
        let uevent = std::fs::read_to_string(e.path().join("device/uevent")).unwrap_or_default();
        if uevent
            .lines()
            .any(|l| l.starts_with("HID_ID=0018:0000093A:"))
        {
            return Some(Path::new("/dev").join(e.file_name()));
        }
    }
    None
}

fn open_surface() -> Option<File> {
    let node = hidraw_in(Path::new(HIDRAW))?;
    OpenOptions::new().read(true).write(true).open(node).ok()
}

/// Is the touchpad reporting? `None`: no touchpad node (powered down, not this machine, or not root).
pub fn surface_on() -> Option<bool> {
    let f = open_surface()?;
    let mut buf = [SURFACE_REPORT, 0xFF];
    // SAFETY: HIDIOCGFEATURE writes at most `buf.len()` bytes into the buffer it is given.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), hidioc(7, buf.len()) as _, buf.as_mut_ptr()) };
    (r >= 0).then_some(buf[1] != 0)
}

/// Make the touchpad report again (`true`) or go silent with its LED on (`false`). Needs root.
pub fn set_surface(on: bool) -> HwResult<()> {
    let f = open_surface().ok_or_else(|| {
        HwError::Unavailable("no touchpad hidraw node (powered down? not root?)".into())
    })?;
    let buf = [SURFACE_REPORT, if on { 3 } else { 0 }];
    // SAFETY: HIDIOCSFEATURE reads `buf.len()` bytes from the buffer it is given.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), hidioc(6, buf.len()) as _, buf.as_ptr()) };
    if r < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Switch it the other way and say what it is now (`true` = reporting).
pub fn toggle_surface() -> HwResult<bool> {
    let on = surface_on()
        .ok_or_else(|| HwError::Unavailable("cannot read the touchpad's surface switch".into()))?;
    set_surface(!on)?;
    Ok(!on)
}

const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTMETA: u16 = 125;
const KEY_F24: u16 = 194;
const KEY_TOUCHPAD_TOGGLE: u16 = 530;

/// Spots the touchpad toggle in the key events of the internal keyboard. For the corner double tap the embedded
/// controller types Left Ctrl + Left Meta + F24 (measured: Meta down, Ctrl down 2 ms later, F24 for 6 ms, then
/// both up); where F24 was turned into the touchpad-toggle key by a hwdb entry, that key alone counts too.
#[derive(Default)]
pub struct ToggleKey {
    ctrl: bool,
    meta: bool,
}

impl ToggleKey {
    /// One key event (`value`: 0 up, 1 down, 2 repeat). `true` = the touchpad is to be toggled now.
    pub fn feed(&mut self, code: u16, value: i32) -> bool {
        match code {
            KEY_LEFTCTRL => self.ctrl = value != 0,
            KEY_LEFTMETA => self.meta = value != 0,
            KEY_TOUCHPAD_TOGGLE if value == 1 => return true,
            KEY_F24 if value == 1 => return self.ctrl && self.meta,
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_double_tap_chord_toggles_once_and_nothing_else_does() {
        let mut k = ToggleKey::default();
        // the real sequence from the laptop: Meta, Ctrl, F24 down and up, Ctrl up, Meta up
        let fired: Vec<bool> = [(125, 1), (29, 1), (194, 1), (194, 0), (29, 0), (125, 0)]
            .into_iter()
            .map(|(c, v)| k.feed(c, v))
            .collect();
        assert_eq!(fired, [false, false, true, false, false, false]);
        // F24 on its own, or with one modifier, is just a key
        assert!(!k.feed(194, 1));
        k.feed(29, 1);
        assert!(!k.feed(194, 1));
        k.feed(29, 0);
        // key repeat does not toggle again
        k.feed(125, 1);
        k.feed(29, 1);
        assert!(k.feed(194, 1));
        assert!(!k.feed(194, 2));
        // with F24 mapped to the touchpad-toggle key it counts alone, once per press
        let mut k = ToggleKey::default();
        assert!(k.feed(530, 1));
        assert!(!k.feed(530, 2) && !k.feed(530, 0));
        // modifiers released: back to normal
        let mut k = ToggleKey::default();
        k.feed(125, 1);
        k.feed(29, 1);
        k.feed(125, 0);
        assert!(!k.feed(194, 1));
    }

    #[test]
    fn the_hidraw_node_is_the_pixart_i2c_one_and_the_ioctls_are_the_hid_ones() {
        let root = std::env::temp_dir().join(format!("ripple-hidraw-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (node, id) in [
            ("hidraw0", "HID_ID=0003:00003434:0000D030"), // a USB keyboard
            ("hidraw10", "HID_ID=0018:0000093A:00000255"), // the touchpad
        ] {
            std::fs::create_dir_all(root.join(node).join("device")).unwrap();
            std::fs::write(
                root.join(node).join("device/uevent"),
                format!("{id}\nHID_NAME=x\n"),
            )
            .unwrap();
        }
        assert_eq!(hidraw_in(&root), Some(PathBuf::from("/dev/hidraw10")));
        assert_eq!(hidraw_in(&root.join("none")), None);
        let _ = std::fs::remove_dir_all(&root);
        // what the kernel headers give: _IOC(READ|WRITE, 'H', 7 / 6, 2)
        assert_eq!(hidioc(7, 2), 0xC002_4807);
        assert_eq!(hidioc(6, 2), 0xC002_4806);
    }

    /// Run by hand as root on the laptop: `sudo target/debug/deps/ripple_hw-* --ignored surface_roundtrip`.
    /// The pointer stops for about a second and the LED lights; it always ends with the pad reporting.
    #[test]
    #[ignore = "switches the real touchpad off for a second: needs root and the touchpad"]
    fn surface_roundtrip() {
        let before = surface_on().expect("no touchpad");
        let res = std::panic::catch_unwind(|| {
            set_surface(false).unwrap();
            assert_eq!(surface_on(), Some(false));
            std::thread::sleep(std::time::Duration::from_secs(1));
            assert!(toggle_surface().unwrap());
            assert_eq!(surface_on(), Some(true));
        });
        set_surface(true).unwrap(); // always leave the pad reporting
        res.unwrap();
        assert!(before, "the pad was off before the test");
    }

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
