//! Profile (Balanced/Enthusiast/Overboost), fan boost and mode-button handling shared by
//! `rcc` and `rcc-gui`. Everything here was verified on real hardware:
//! PL1/PL2 via EC 1923/1924 (35→55→65 W measured as package power under
//! load), fan boost via the 0x40 bit of EC 1873, mode button = WMI event 176 =
//! key 184 (KEY_F14) on the "Uniwill WMI hotkeys" input device.

use std::io::Read;

use ripple_core::modes::{FanControlByte, OperatingMode};
use ripple_core::regs::addr;
use serde::{Deserialize, Serialize};

use crate::linux::SysfsEc;
use crate::{EcAccess, HwError, HwResult};

/// Hard ceiling for any PL1/PL2 we write: the stock app's own Turbo value for this
/// machine (Smart APC table, 80 W). Above this we refuse, whatever the config says.
pub const PL_CEILING_W: u8 = 80;
/// Same idea for the GPU cTGP offset (stock Turbo value is 20 W).
pub const CTGP_CEILING_W: u8 = 25;
/// `KEY_F14`: what the kernel's `uniwill-wmi` emits for the mode button.
pub const MODE_KEY_CODE: u16 = 184;

/// Mode nibble of the fan control byte (EC 1873).
const FAN_MODE_MASK: u8 = 0xF0;
/// Pre-boost mode survives across processes here (tmpfs: cleared on reboot).
const STASH_PATH: &str = "/run/ripple-nova-fan-mode";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub pl1: u8,
    pub pl2: u8,
    /// GPU configurable-TGP offset in watts (kernel `ctgp_offset`).
    pub ctgp: u8,
}

impl Preset {
    pub fn validate(&self) -> Result<(), String> {
        if !(5..=PL_CEILING_W).contains(&self.pl1) || !(5..=PL_CEILING_W).contains(&self.pl2) {
            return Err(format!("PL1/PL2 must be 5..={PL_CEILING_W} W (got {}/{})", self.pl1, self.pl2));
        }
        if self.pl1 > self.pl2 {
            return Err(format!("PL1 {} W is above PL2 {} W", self.pl1, self.pl2));
        }
        if self.ctgp > CTGP_CEILING_W {
            return Err(format!("cTGP offset must be 0..={CTGP_CEILING_W} W (got {})", self.ctgp));
        }
        Ok(())
    }
}

/// A profile the user can pick: one of the three stock ones (read-only, they are the
/// firmware/stock-app values) or their own `Custom` preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Stock(OperatingMode),
    Custom,
}

impl Profile {
    /// The names the Windows app shows (our `OperatingMode` Office/Gaming/Turbo, same three lights).
    pub fn name(self) -> &'static str {
        match self {
            Self::Stock(OperatingMode::Office) => "Balanced",
            Self::Stock(OperatingMode::Gaming) => "Enthusiast",
            Self::Stock(OperatingMode::Turbo) => "Overboost",
            Self::Custom => "Custom",
        }
    }

    /// Inverse of `mode_nibble`: the profile the EC's own mode byte announces. `None` for nibbles
    /// with no known meaning (0x4 is fan boost; 0x2 and 0x8 were seen before an app took control).
    pub fn from_mode_byte(byte: u8) -> Option<Self> {
        match byte >> 4 {
            // 0xE and 0x7 are Balanced and Overboost with the custom fan engine's manual bit on top
            0xA | 0xE => Some(Self::Stock(OperatingMode::Office)),
            0x0 => Some(Self::Stock(OperatingMode::Gaming)),
            0x3 | 0x7 => Some(Self::Stock(OperatingMode::Turbo)),
            _ => None,
        }
    }

    /// What one press of the hardware button does: Office → Gaming → Turbo → Office (0 → 1 → 2
    /// lights). Custom shows as Gaming (1 light), so its next press is Turbo; limits that match
    /// nothing restart the cycle at Office.
    pub fn after_button(current: Option<Self>) -> Self {
        match current {
            Some(Self::Stock(m)) => Self::Stock(m.next()),
            Some(Self::Custom) => Self::Stock(OperatingMode::Gaming.next()),
            None => Self::Stock(OperatingMode::Office),
        }
    }

    /// The mode nibble of the EC fan control byte (EC 1873, high nibble). It is what drives the
    /// three mode lights on the button and, presumably, the EC's fan profile for the mode:
    /// measured on this machine, writing 0xA_ lit 0 lights, 0x3_ lit 2, 0x0_ lit 1. Custom has no
    /// light of its own and shows as Gaming.
    pub fn mode_nibble(self) -> u8 {
        match self {
            Self::Stock(OperatingMode::Office) => 0xA,
            Self::Stock(OperatingMode::Turbo) => 0x3,
            Self::Stock(OperatingMode::Gaming) | Self::Custom => 0x0,
        }
    }
}

pub const STOCK_MODES: [OperatingMode; 3] = [OperatingMode::Office, OperatingMode::Gaming, OperatingMode::Turbo];

/// The stock presets, in `STOCK_MODES` order. Not editable and not saved: Office/Gaming are the
/// EC's own default registers (1844/1845, 1840/1841); Turbo is the stock app's Smart APC table
/// for this machine (PL 80/80, cTGP +20 W), read from its Windows registry.
pub fn stock(ec: Option<&mut (dyn EcAccess + '_)>) -> [Preset; 3] {
    let mut office = Preset { pl1: 35, pl2: 35, ctgp: 0 };
    let mut gaming = Preset { pl1: 55, pl2: 55, ctgp: 0 };
    if let Some(ec) = ec {
        let mut rd = |a: u16, d: u8| ec.read(a).ok().filter(|v| *v >= 5).unwrap_or(d);
        office = Preset { pl1: rd(addr::OFFICE_PL1_DEFAULT, 35), pl2: rd(addr::OFFICE_PL2_DEFAULT, 35), ctgp: 0 };
        gaming = Preset { pl1: rd(addr::GAMING_PL1_DEFAULT, 55), pl2: rd(addr::GAMING_PL2_DEFAULT, 55), ctgp: 0 };
    }
    [office, gaming, Preset { pl1: 80, pl2: 80, ctgp: 20 }]
}

/// Keyboard colour as hue/saturation/brightness. The EC only stores per-channel levels, so
/// brightness is applied by scaling them; the exact choice is kept in the config to avoid
/// losing hue when the keyboard is dimmed to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hsv {
    /// 0..360
    pub h: u16,
    /// 0..=100
    pub s: u8,
    /// brightness, 0..=100
    pub v: u8,
}

impl Hsv {
    /// EC levels, each `0..=KB_LEVEL_MAX`.
    pub fn to_levels(self) -> (u8, u8, u8) {
        let h = f32::from(self.h % 360) / 60.0;
        let s = f32::from(self.s.min(100)) / 100.0;
        let v = f32::from(self.v.min(100)) / 100.0;
        let c = v * s;
        let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
        let m = v - c;
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let f = |c: f32| ((c + m) * f32::from(KB_LEVEL_MAX)).round() as u8;
        (f(r), f(g), f(b))
    }

    pub fn from_levels((r, g, b): (u8, u8, u8)) -> Self {
        let max_l = f32::from(KB_LEVEL_MAX);
        let (r, g, b) = (f32::from(r) / max_l, f32::from(g) / max_l, f32::from(b) / max_l);
        let max = r.max(g).max(b);
        let d = max - r.min(g).min(b);
        let h = if d == 0.0 {
            0.0
        } else if max == r {
            60.0 * (((g - b) / d) % 6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        Self {
            h: (if h < 0.0 { h + 360.0 } else { h }).round() as u16 % 360,
            s: if max == 0.0 { 0 } else { (d / max * 100.0).round() as u8 },
            v: (max * 100.0).round() as u8,
        }
    }
}

/// The custom performance profile preset as shipped (what "reset" goes back to).
pub fn default_custom() -> Preset {
    // between Office and Gaming, so the first Custom is a gentle step, not a copy of a stock one
    Preset { pl1: 45, pl2: 45, ctgp: 0 }
}

/// Saved settings (`/etc/ripple-nova/config.json`). Stock presets are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// The user's own preset, the only editable one.
    #[serde(default = "default_custom")]
    pub custom: Preset,
    /// Keyboard colour/brightness as last chosen (`None` = never set: read it from the EC).
    #[serde(default)]
    pub keyboard: Option<Hsv>,
    /// Software fan policy: boost the fans above this CPU temperature (°C), `None` = off.
    #[serde(default)]
    pub auto_boost_above: Option<u8>,
}

impl Config {
    pub const PATH: &'static str = "/etc/ripple-nova/config.json";

    /// Saved config (unknown/old fields ignored, an invalid custom preset reset), else defaults.
    pub fn load() -> Self {
        let mut cfg = std::fs::read_to_string(Self::PATH)
            .ok()
            .and_then(|t| serde_json::from_str::<Self>(&t).ok())
            .unwrap_or(Self { custom: default_custom(), keyboard: None, auto_boost_above: None, fan: FanCurves::default(), keyboard_level: None });
        if cfg.custom.validate().is_err() {
            cfg.custom = default_custom();
        }
        cfg
    }

    pub fn save(&self) -> HwResult<()> {
        let dir = std::path::Path::new(Self::PATH).parent().expect("PATH has a parent");
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(|e| HwError::Other(e.to_string()))?;
        std::fs::write(Self::PATH, text)?;
        Ok(())
    }
}

/// Apply a preset: PL2/PL1 via EC (read back to verify), cTGP via the kernel attr.
pub fn apply(ec: &mut dyn EcAccess, sysfs: Option<&SysfsEc>, p: Preset) -> HwResult<()> {
    p.validate().map_err(HwError::Other)?;
    // Keep PL1 <= PL2 at every step: raise PL2 first, lower PL1 first.
    let cur_pl2 = ec.read(addr::PL2_SETTING)?;
    if p.pl2 >= cur_pl2 {
        ec.write_verified(addr::PL2_SETTING, p.pl2)?;
        ec.write_verified(addr::PL1_SETTING, p.pl1)?;
    } else {
        ec.write_verified(addr::PL1_SETTING, p.pl1)?;
        ec.write_verified(addr::PL2_SETTING, p.pl2)?;
    }
    if let Some(u) = sysfs.filter(|u| u.get_attr("ctgp_offset").is_ok()) {
        u.set_attr("ctgp_offset", &p.ctgp.to_string())?;
        let back = u.get_attr("ctgp_offset")?;
        if back.trim() != p.ctgp.to_string() {
            return Err(HwError::Other(format!("ctgp_offset: wrote {}, read {back}", p.ctgp)));
        }
    }
    Ok(())
}

/// Which profile the EC is running, judged by its PL1/PL2 setting registers. Stock profiles win
/// over Custom when both match. Both registers read 0 until something applies limits (fresh boot,
/// or the EC still in the state Windows left it): then the EC runs its own profile and its mode
/// byte (the button's lights) says which, so the profile is read from there.
pub fn current(ec: &mut dyn EcAccess, stock: &[Preset; 3], custom: &Preset) -> HwResult<Option<Profile>> {
    let now = (ec.read(addr::PL1_SETTING)?, ec.read(addr::PL2_SETTING)?);
    let hit = |p: &Preset| (p.pl1, p.pl2) == now;
    let by_limits = STOCK_MODES
        .into_iter()
        .zip(stock)
        .find(|(_, p)| hit(p))
        .map(|(m, _)| Profile::Stock(m))
        .or_else(|| hit(custom).then_some(Profile::Custom));
    if by_limits.is_some() || now != (0, 0) {
        return Ok(by_limits);
    }
    Ok(Profile::from_mode_byte(ec.read(addr::MAFAN_CONTROL_BYTE)?))
}

pub fn preset_for(stock: &[Preset; 3], custom: &Preset, p: Profile) -> Preset {
    match p {
        Profile::Stock(m) => stock[m as usize],
        Profile::Custom => *custom,
    }
}

/// Start-up sync (daemon and GUI): find the profile the EC is running, write its limits when none
/// are set yet (so the EC enforces what the UI shows, as the stock Windows service does on boot),
/// and put the button's lights on it.
pub fn adopt(ec: &mut dyn EcAccess, sysfs: Option<&SysfsEc>, stock: &[Preset; 3], custom: &Preset) -> HwResult<Option<Profile>> {
    let Some(p) = current(ec, stock, custom)? else { return Ok(None) };
    if (ec.read(addr::PL1_SETTING)?, ec.read(addr::PL2_SETTING)?) == (0, 0) {
        apply(ec, sysfs, preset_for(stock, custom, p))?;
    }
    set_mode_byte(ec, p)?;
    Ok(Some(p))
}

/// Does the EC report a lightbar? (`SupportByteOneFlag` bit 6, the stock app's
/// `LightbarSupport`.) This machine: no, even though the force-loaded kernel driver
/// still exposes a lightbar LED and the rainbow/breathing attributes.
pub fn has_lightbar(ec: &mut dyn EcAccess) -> bool {
    ec.read(addr::SUPPORT_BYTE1).is_ok_and(|b| b & 0x40 != 0)
}

// ------------------------------------------------------------- keyboard colour

/// Highest per-channel level the EC's keyboard backlight registers take. The firmware's
/// own default colour is (0, 50, 50); brightness is a separate Fn-key setting.
pub const KB_LEVEL_MAX: u8 = 50;
/// `RGBKEYBOARD_TRIGGER_BIT` (bit 5 of `ADDR_TRIGGER_BYTE`): "apply the colour registers".
const KB_APPLY_BIT: u8 = 0x20;

/// Does the EC report an RGB keyboard? (`SupportByteTwoFlag` bit 2, the stock app's
/// `EC_RGBKEYBOARD_SUPPORT_BIT`.)
pub fn has_keyboard_rgb(ec: &mut dyn EcAccess) -> bool {
    ec.read(addr::SUPPORT_BYTE2).is_ok_and(|b| b & 0x04 != 0)
}

/// Current keyboard colour, each channel `0..=KB_LEVEL_MAX`.
pub fn keyboard_rgb(ec: &mut dyn EcAccess) -> HwResult<(u8, u8, u8)> {
    Ok((ec.read(addr::RGBKB_LEVEL_R)?, ec.read(addr::RGBKB_LEVEL_G)?, ec.read(addr::RGBKB_LEVEL_B)?))
}

/// Set the keyboard colour (each channel `0..=KB_LEVEL_MAX`). Writing the registers alone
/// does nothing visible: the EC only re-lights the keys once the apply bit is set (verified
/// on this machine: without it the colour stays put, with it the keyboard changes at once).
pub fn set_keyboard_rgb(ec: &mut dyn EcAccess, (r, g, b): (u8, u8, u8)) -> HwResult<()> {
    if r.max(g).max(b) > KB_LEVEL_MAX {
        return Err(HwError::Other(format!("keyboard colour levels must be 0..={KB_LEVEL_MAX} (got {r},{g},{b})")));
    }
    ec.write_verified(addr::RGBKB_LEVEL_R, r)?;
    ec.write_verified(addr::RGBKB_LEVEL_G, g)?;
    ec.write_verified(addr::RGBKB_LEVEL_B, b)?;
    ec.update(addr::TRIGGER_BYTE, KB_APPLY_BIT, KB_APPLY_BIT)?; // other trigger bits untouched
    Ok(())
}

/// Apply an HSV colour (brightness included).
pub fn set_keyboard_hsv(ec: &mut dyn EcAccess, hsv: Hsv) -> HwResult<()> {
    set_keyboard_rgb(ec, hsv.to_levels())
}

/// `0..=255` per channel (hex colours) → EC levels.
pub fn kb_from_8bit((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    let f = |v: u8| (u16::from(v) * u16::from(KB_LEVEL_MAX) / 255) as u8;
    (f(r), f(g), f(b))
}

// ------------------------------------------------------------------ mode lights / fan mode

/// Write the profile's mode nibble into EC 1873 (lights + fan profile). While fan boost is on the
/// nibble belongs to the boost, so the new mode is stashed and takes effect when boost ends.
pub fn set_mode_byte(ec: &mut dyn EcAccess, p: Profile) -> HwResult<()> {
    let cur = ec.read(addr::MAFAN_CONTROL_BYTE)?;
    let nib = p.mode_nibble();
    if is_boosted(cur) {
        std::fs::write(STASH_PATH, (nib << 4).to_string())?;
        return Ok(());
    }
    let new = (cur & 0x0F) | (nib << 4);
    if new != cur {
        ec.write_verified(addr::MAFAN_CONTROL_BYTE, new)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ fan boost

/// New fan control byte for boost on/off. `stash` remembers the pre-boost mode so
/// "off" restores it. "Off" only touches a byte that is actually boosted: the EC may
/// already have changed it itself (Fn key), and clobbering that is the one thing we
/// must not do.
fn boost_byte(cur: u8, on: bool, stash: &mut Option<u8>) -> u8 {
    let boost = FanControlByte::FanBoost.to_byte();
    let boosted = cur & FAN_MODE_MASK == boost;
    if on {
        if !boosted {
            *stash = Some(cur & FAN_MODE_MASK);
        }
        (cur & !FAN_MODE_MASK) | boost
    } else if boosted {
        (cur & !FAN_MODE_MASK) | stash.take().unwrap_or(0)
    } else {
        *stash = None;
        cur
    }
}

/// Switch fan boost on/off, returns `(old, new)` control byte (equal = nothing written).
pub fn set_boost(ec: &mut dyn EcAccess, on: bool) -> HwResult<(u8, u8)> {
    let mut stash = std::fs::read_to_string(STASH_PATH).ok().and_then(|s| s.trim().parse().ok());
    let cur = ec.read(addr::MAFAN_CONTROL_BYTE)?;
    let new = boost_byte(cur, on, &mut stash);
    if new != cur {
        ec.write_verified(addr::MAFAN_CONTROL_BYTE, new)?;
    }
    match stash {
        Some(s) => std::fs::write(STASH_PATH, s.to_string())?,
        None => {
            let _ = std::fs::remove_file(STASH_PATH);
        }
    }
    Ok((cur, new))
}

pub fn is_boosted(byte: u8) -> bool {
    byte & FAN_MODE_MASK == FanControlByte::FanBoost.to_byte()
}

/// Software fan policy with 8 °C hysteresis. Only turns off a boost it turned on.
pub fn auto_boost_action(threshold: Option<u8>, temp_c: f64, boosted: bool, ours: bool) -> Option<bool> {
    let t = f64::from(threshold?);
    if !boosted && temp_c >= t {
        Some(true)
    } else if boosted && ours && temp_c <= t - 8.0 {
        Some(false)
    } else {
        None
    }
}

// ---------------------------------------------------------------- mode button

/// Only one process should follow the mode button (two would each cycle the profile). Followers
/// hold this lock for as long as they follow: the daemon (`rcc watch`) waits for it, the GUI only
/// tries, and stays out of the way when the daemon already holds it.
const FOLLOW_LOCK: &str = "/run/ripple-nova-button.lock";

pub fn follow_lock(blocking: bool) -> Option<std::fs::File> {
    follow_lock_at(std::path::Path::new(FOLLOW_LOCK), blocking)
}

fn follow_lock_at(path: &std::path::Path, blocking: bool) -> Option<std::fs::File> {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).ok()?;
    if blocking {
        f.lock().ok()?;
    } else {
        f.try_lock().ok()?;
    }
    Some(f)
}

/// Open the "Uniwill WMI hotkeys" input device (root only).
pub fn open_mode_key() -> HwResult<std::fs::File> {
    for e in std::fs::read_dir("/sys/class/input")?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("event") {
            continue;
        }
        let dev_name = std::fs::read_to_string(e.path().join("device/name")).unwrap_or_default();
        if dev_name.trim() == "Uniwill WMI hotkeys" {
            return Ok(std::fs::File::open(format!("/dev/input/{name}"))?);
        }
    }
    Err(HwError::Unavailable(
        "no 'Uniwill WMI hotkeys' input device (uniwill-laptop not loaded?)".into(),
    ))
}

/// Block until the mode button is pressed once (key 184 down).
pub fn wait_mode_key(dev: &mut std::fs::File) -> HwResult<()> {
    // struct input_event { struct timeval (2 words); u16 type; u16 code; i32 value }
    let tv = 2 * std::mem::size_of::<usize>();
    let mut buf = vec![0u8; tv + 8];
    loop {
        dev.read_exact(&mut buf)?;
        let typ = u16::from_le_bytes([buf[tv], buf[tv + 1]]);
        let code = u16::from_le_bytes([buf[tv + 2], buf[tv + 3]]);
        let value = i32::from_le_bytes([buf[tv + 4], buf[tv + 5], buf[tv + 6], buf[tv + 7]]);
        if typ == 1 && code == MODE_KEY_CODE && value == 1 {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boost_off_restores_the_pre_boost_mode() {
        let mut stash = None;
        assert_eq!(boost_byte(0x20, true, &mut stash), 0x40);
        // a second "on" while already boosted must not overwrite the stash
        assert_eq!(boost_byte(0x40, true, &mut stash), 0x40);
        assert_eq!(boost_byte(0x40, false, &mut stash), 0x20);
        assert_eq!(stash, None);
        // no stash (restarted while boosted) → Normal
        assert_eq!(boost_byte(0x40, false, &mut stash), 0x00);
        // low nibble is preserved
        assert_eq!(boost_byte(0x23, true, &mut stash), 0x43);
        // "off" on a byte that is not boosted is a no-op
        let mut stash = Some(0x20);
        assert_eq!(boost_byte(0x20, false, &mut stash), 0x20);
        assert_eq!(stash, None);
    }

    #[test]
    fn presets_are_validated() {
        assert!(Preset { pl1: 35, pl2: 35, ctgp: 0 }.validate().is_ok());
        assert!(Preset { pl1: 80, pl2: 80, ctgp: 20 }.validate().is_ok());
        assert!(Preset { pl1: 81, pl2: 81, ctgp: 0 }.validate().is_err());
        assert!(Preset { pl1: 60, pl2: 50, ctgp: 0 }.validate().is_err());
        assert!(Preset { pl1: 4, pl2: 40, ctgp: 0 }.validate().is_err());
        assert!(Preset { pl1: 35, pl2: 35, ctgp: 26 }.validate().is_err());
    }

    #[test]
    fn auto_boost_has_hysteresis_and_only_undoes_its_own_boost() {
        assert_eq!(auto_boost_action(None, 99.0, false, false), None);
        assert_eq!(auto_boost_action(Some(85), 85.0, false, false), Some(true));
        assert_eq!(auto_boost_action(Some(85), 80.0, true, true), None);
        assert_eq!(auto_boost_action(Some(85), 77.0, true, true), Some(false));
        // user boosted by hand: leave it alone
        assert_eq!(auto_boost_action(Some(85), 50.0, true, false), None);
    }

    #[test]
    fn keyboard_colours_scale_to_the_ec_range() {
        assert_eq!(kb_from_8bit((255, 255, 255)), (50, 50, 50));
        assert_eq!(kb_from_8bit((0, 0, 0)), (0, 0, 0));
        assert_eq!(kb_from_8bit((255, 0, 128)), (50, 0, 25));
    }

    #[test]
    fn only_one_process_can_follow_the_button() {
        let path = std::env::temp_dir().join(format!("ripple-follow-test-{}", std::process::id()));
        let first = follow_lock_at(&path, false).expect("free lock");
        assert!(follow_lock_at(&path, false).is_none(), "second follower must be refused");
        drop(first);
        assert!(follow_lock_at(&path, false).is_some(), "released lock can be taken again");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn button_cycles_stock_profiles_and_restarts_from_anything_else() {
        use OperatingMode::*;
        // the button's LEDs go 0 → 1 → 2: Office → Gaming → Turbo → Office
        assert_eq!(Profile::after_button(Some(Profile::Stock(Office))), Profile::Stock(Gaming));
        assert_eq!(Profile::after_button(Some(Profile::Stock(Gaming))), Profile::Stock(Turbo));
        assert_eq!(Profile::after_button(Some(Profile::Stock(Turbo))), Profile::Stock(Office));
        assert_eq!(Profile::after_button(Some(Profile::Custom)), Profile::Stock(Turbo)); // shows as 1 light
        assert_eq!(Profile::after_button(None), Profile::Stock(Office));
        // lights: measured 0xA_ = 0 lights, 0x0_ = 1 light, 0x3_ = 2 lights
        assert_eq!(Profile::Stock(Office).mode_nibble(), 0xA);
        assert_eq!(Profile::Stock(Gaming).mode_nibble(), 0x0);
        assert_eq!(Profile::Stock(Turbo).mode_nibble(), 0x3);
    }

    #[test]
    fn hsv_matches_the_ec_levels() {
        assert_eq!(Hsv { h: 0, s: 100, v: 100 }.to_levels(), (50, 0, 0));
        assert_eq!(Hsv { h: 120, s: 100, v: 100 }.to_levels(), (0, 50, 0));
        assert_eq!(Hsv { h: 0, s: 0, v: 100 }.to_levels(), (50, 50, 50));
        assert_eq!(Hsv { h: 0, s: 100, v: 50 }.to_levels(), (25, 0, 0));
        assert_eq!(Hsv { h: 200, s: 40, v: 0 }.to_levels(), (0, 0, 0));
        // the firmware's default colour is cyan at full brightness
        assert_eq!(Hsv::from_levels((0, 50, 50)), Hsv { h: 180, s: 100, v: 100 });
        for c in [(50, 0, 0), (0, 50, 25), (10, 20, 30), (50, 50, 50)] {
            let back = Hsv::from_levels(c).to_levels();
            assert!(back.0.abs_diff(c.0) <= 1 && back.1.abs_diff(c.1) <= 1 && back.2.abs_diff(c.2) <= 1, "{c:?} -> {back:?}");
        }
    }

    #[test]
    fn current_prefers_stock_over_custom() {
        struct Fake(u8, u8);
        impl EcAccess for Fake {
            fn name(&self) -> &str { "fake" }
            fn read(&mut self, a: u16) -> HwResult<u8> { Ok(if a == addr::PL1_SETTING { self.0 } else { self.1 }) }
            fn write(&mut self, _: u16, _: u8) -> HwResult<()> { Ok(()) }
        }
        let st = stock(None);
        let custom = Preset { pl1: 35, pl2: 35, ctgp: 5 }; // same limits as Office
        assert_eq!(current(&mut Fake(35, 35), &st, &custom).unwrap(), Some(Profile::Stock(OperatingMode::Office)));
        assert_eq!(current(&mut Fake(45, 45), &st, &Preset { pl1: 45, pl2: 45, ctgp: 0 }).unwrap(), Some(Profile::Custom));
        assert_eq!(current(&mut Fake(50, 50), &st, &custom).unwrap(), None);
    }

    fn ec_with(pl: (u8, u8), mode_byte: u8) -> crate::mock::MockEc {
        let mut ec = crate::mock::MockEc::new();
        ec.write(addr::PL1_SETTING, pl.0).unwrap();
        ec.write(addr::PL2_SETTING, pl.1).unwrap();
        ec.write(addr::MAFAN_CONTROL_BYTE, mode_byte).unwrap();
        ec
    }

    #[test]
    fn curves_interpolate_and_clamp() {
        let c = [20, 30, 45, 70, 100];
        assert_eq!(curve_duty(&c, 30.0), 20); // below the first anchor: flat
        assert_eq!(curve_duty(&c, 45.0), 20);
        assert_eq!(curve_duty(&c, 50.0), 25); // halfway 20 -> 30
        assert_eq!(curve_duty(&c, 75.0), 70);
        assert_eq!(curve_duty(&c, 80.0), 85);
        assert_eq!(curve_duty(&c, 120.0), 100); // above the last: flat
        assert_eq!(fan_smooth(50, 80), 80); // up at once
        assert_eq!(fan_smooth(80, 20), 78); // down by 2 per tick
        assert_eq!(fan_smooth(21, 20), 20);
    }

    #[test]
    fn setting_a_point_keeps_the_curve_rising() {
        let mut c = [20, 30, 45, 70, 100];
        set_curve_point(&mut c, 2, 80); // up: later points follow
        assert_eq!(c, [20, 30, 80, 80, 100]);
        set_curve_point(&mut c, 2, 10); // down: earlier points follow
        assert_eq!(c, [10, 10, 10, 80, 100]);
        set_curve_point(&mut c, 4, 0);
        assert_eq!(c, [0, 0, 0, 0, 0]);
        // a curve that lets the fans stop at 85 °C is refused
        assert!(FanCurves { cpu: c, ..FanCurves::default() }.validate().is_err());
        assert!(FanCurves { gpu: [0, 0, 0, 0, 59], ..FanCurves::default() }.validate().is_err());
        assert!(FanCurves { gpu: [0, 0, 0, 0, 60], ..FanCurves::default() }.validate().is_ok());
    }

    #[test]
    fn fan_targets_fail_safe() {
        let cfg = FanCurves::default();
        assert_eq!(fan_targets(&cfg, Some(45.0), Some(45.0)), [20, 0]);
        assert_eq!(fan_targets(&cfg, Some(89.0), Some(40.0))[0], curve_duty(&cfg.cpu, 89.0));
        assert_eq!(fan_targets(&cfg, Some(90.0), Some(40.0)), [100, 100]); // either sensor hot
        assert_eq!(fan_targets(&cfg, Some(40.0), Some(95.0)), [100, 100]);
        assert_eq!(fan_targets(&cfg, None, Some(65.0)), [curve_duty(&cfg.cpu, 65.0), curve_duty(&cfg.gpu, 65.0)]);
        assert_eq!(fan_targets(&cfg, None, None), [100, 100]); // blind = flat out
        assert!(FanCurves::default().validate().is_ok());
        assert!(FanCurves { cpu: [20, 30, 25, 70, 100], ..FanCurves::default() }.validate().is_err());
        assert!(FanCurves { gpu: [0, 25, 40, 65, 101], ..FanCurves::default() }.validate().is_err());
    }

    /// A mock EC with the two fan duty registers.
    struct FanEc(crate::mock::MockEc, [u8; 2]);
    impl EcAccess for FanEc {
        fn name(&self) -> &str { "fan-mock" }
        fn read(&mut self, a: u16) -> HwResult<u8> { self.0.read(a) }
        fn write(&mut self, a: u16, v: u8) -> HwResult<()> { self.0.write(a, v) }
        fn fan_duty(&mut self, f: u8) -> HwResult<u8> { Ok(self.1[usize::from(f) - 1]) }
        fn set_fan_duty(&mut self, f: u8, d: u8) -> HwResult<()> { self.1[usize::from(f) - 1] = d; Ok(()) }
    }

    #[test]
    fn engine_takes_over_on_top_of_the_mode_and_hands_back() {
        let mut ec = FanEc(crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0xA7), [60, 0]); // Balanced
        let cfg = FanCurves { enabled: true, ..FanCurves::default() };
        let mut eng = FanEngine { settle: std::time::Duration::ZERO, ..FanEngine::default() };
        eng.tick(&mut ec, &cfg, Some(65.0), Some(65.0)).unwrap();
        assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0xE7); // mode nibble kept, manual bit on
        // at 65 °C the curves say 45 % (CPU) and 40 % (GPU): both above what the firmware ran, so at once
        assert_eq!(ec.1, [duty_to_ec(45), duty_to_ec(40)]);
        // a hot GPU alone pushes both fans to the top at once
        eng.tick(&mut ec, &cfg, Some(60.0), Some(91.0)).unwrap();
        assert_eq!(ec.1, [200, 200]);
        // the mode button dropping bit 6 is repaired on the next tick
        ec.write(addr::MAFAN_CONTROL_BYTE, 0x37).unwrap();
        eng.tick(&mut ec, &cfg, Some(60.0), Some(60.0)).unwrap();
        assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x77);
        // switched off: the firmware gets the fans back, mode intact
        eng.tick(&mut ec, &FanCurves::default(), Some(60.0), Some(60.0)).unwrap();
        assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x37);
    }

    #[test]
    fn mode_byte_only_keeps_the_manual_bit_the_engine_set() {
        use OperatingMode::*;
        let run = |cur: u8, p: Profile, on: bool| {
            let mut ec = crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, cur);
            set_mode_byte_with(&mut ec, p, on).unwrap();
            ec.read(addr::MAFAN_CONTROL_BYTE).unwrap()
        };
        // the profile being on must not create the bit: the engine never sees its own mark otherwise
        // and would take the byte for a user's boost (boot: byte 0x07, adopt writes the mode byte)
        assert_eq!(run(0x07, Profile::Stock(Gaming), true), 0x07);
        assert_eq!(run(0xA7, Profile::Stock(Turbo), true), 0x37);
        // a bit the engine set stays through a mode change
        assert_eq!(run(0xE7, Profile::Stock(Gaming), true), 0x47);
        assert_eq!(run(0x47, Profile::Stock(Turbo), true), 0x77);
        assert_eq!(run(0x77, Profile::Stock(Office), true), 0xE7);
        // profile off: plain mode change
        assert_eq!(run(0xA7, Profile::Stock(Gaming), false), 0x07);
    }

    #[test]
    fn engine_leaves_a_users_boost_alone() {
        let mut ec = FanEc(crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0x40), [200, 200]);
        let cfg = FanCurves { enabled: true, ..FanCurves::default() };
        let mut eng = FanEngine { settle: std::time::Duration::ZERO, ..FanEngine::default() };
        eng.tick(&mut ec, &cfg, Some(40.0), Some(40.0)).unwrap();
        assert_eq!((ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), ec.1), (0x40, [200, 200]));
        eng.stop(&mut ec).unwrap(); // not ours: nothing is released
        assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x40);
    }

    #[test]
    fn rainbow_is_a_sticky_bit_that_a_colour_clears() {
        let mut ec = crate::mock::MockEc::with(addr::TRIGGER_BYTE, 0x00);
        set_keyboard_rainbow(&mut ec, true).unwrap();
        assert_eq!(ec.read(addr::TRIGGER_BYTE).unwrap(), 0xA0); // rainbow + apply
        assert!(keyboard_rainbow(&mut ec).unwrap());
        set_keyboard_rgb(&mut ec, (1, 2, 3)).unwrap();
        assert_eq!(ec.read(addr::TRIGGER_BYTE).unwrap(), 0x20); // rainbow gone, apply set
        assert!(!keyboard_rainbow(&mut ec).unwrap());
        // other trigger bits survive both writes
        let mut ec = crate::mock::MockEc::with(addr::TRIGGER_BYTE, 0x01);
        set_keyboard_rainbow(&mut ec, true).unwrap();
        set_keyboard_rainbow(&mut ec, false).unwrap();
        assert_eq!(ec.read(addr::TRIGGER_BYTE).unwrap(), 0x21);
    }

    #[test]
    fn backlight_off_and_back_on() {
        let mut ec = crate::mock::MockEc::with(KB_STATUS, 0x88); // level 4, turbo bits set
        assert_eq!(keyboard_backlight(&mut ec).unwrap(), 4);
        assert_eq!(keyboard_off(&mut ec).unwrap(), 4);
        assert_eq!(keyboard_backlight(&mut ec).unwrap(), 0);
        assert_eq!(ec.read(KB_STATUS).unwrap(), 0x18); // level 0, apply, other bits kept
        keyboard_on(&mut ec, Some(2)).unwrap();
        assert_eq!(keyboard_backlight(&mut ec).unwrap(), 2);
        keyboard_on(&mut ec, Some(4)).unwrap(); // already on: untouched
        assert_eq!(keyboard_backlight(&mut ec).unwrap(), 2);
        keyboard_off(&mut ec).unwrap();
        keyboard_on(&mut ec, None).unwrap(); // unknown level: full
        assert_eq!(keyboard_backlight(&mut ec).unwrap(), 4);
        // a power-off bit left by the EC is cleared by a write
        let mut ec = crate::mock::MockEc::with(KB_STATUS, 0x02);
        set_keyboard_backlight(&mut ec, 3).unwrap();
        assert_eq!(ec.read(KB_STATUS).unwrap(), 0x70);
    }

    #[test]
    fn keyboard_choice_follows_the_ec_when_it_changed() {
        let saved = Hsv { h: 0, s: 100, v: 50 }; // dim red: levels (25, 0, 0)
        assert_eq!(keyboard_choice(Some(saved), Some((25, 0, 0))), Some(saved));
        assert_eq!(keyboard_choice(Some(saved), Some((0, 0, 50))), Some(Hsv::from_levels((0, 0, 50))));
        assert_eq!(keyboard_choice(Some(saved), None), Some(saved));
        assert_eq!(keyboard_choice(None, None), None);
    }

    #[test]
    fn boot_state_is_read_from_the_mode_byte() {
        use OperatingMode::*;
        let st = stock(None);
        let custom = Preset { pl1: 45, pl2: 45, ctgp: 0 };
        let cur = |pl, byte| current(&mut ec_with(pl, byte), &st, &custom).unwrap();
        // measured right after a boot on the real machine: PL regs 0/0, mode byte 0x00 (1 light)
        assert_eq!(cur((0, 0), 0x00), Some(Profile::Stock(Gaming)));
        assert_eq!(cur((0, 0), 0xA7), Some(Profile::Stock(Office)));
        assert_eq!(cur((0, 0), 0x37), Some(Profile::Stock(Turbo)));
        // fan boost (0x4_) and nibbles with no known meaning stay unknown
        assert_eq!(cur((0, 0), 0x47), None);
        assert_eq!(cur((0, 0), 0x20), None);
        // set limits that match nothing are not second-guessed by the lights
        assert_eq!(cur((50, 50), 0xA7), None);
    }

    #[test]
    fn adopt_writes_the_limits_of_the_profile_the_lights_show() {
        let st = stock(None);
        let custom = Preset { pl1: 45, pl2: 45, ctgp: 0 };
        let mut ec = ec_with((0, 0), 0x07);
        let p = adopt(&mut ec, None, &st, &custom).unwrap();
        assert_eq!(p, Some(Profile::Stock(OperatingMode::Gaming)));
        assert_eq!((ec.read(addr::PL1_SETTING).unwrap(), ec.read(addr::PL2_SETTING).unwrap()), (55, 55));
        assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x07); // lights untouched
        // limits that are already set are left alone
        let mut ec = ec_with((45, 45), 0x07);
        assert_eq!(adopt(&mut ec, None, &st, &custom).unwrap(), Some(Profile::Custom));
        assert_eq!(ec.read(addr::PL1_SETTING).unwrap(), 45);
    }
}
