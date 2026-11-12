//! Profile (Office/Gaming/Turbo), fan boost and mode-button handling shared by
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
    pub fn name(self) -> &'static str {
        match self {
            Self::Stock(OperatingMode::Office) => "Office",
            Self::Stock(OperatingMode::Gaming) => "Gaming",
            Self::Stock(OperatingMode::Turbo) => "Turbo",
            Self::Custom => "Custom",
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

fn default_custom() -> Preset {
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
            .unwrap_or(Self { custom: default_custom(), keyboard: None, auto_boost_above: None });
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
/// over Custom when both match.
pub fn current(ec: &mut dyn EcAccess, stock: &[Preset; 3], custom: &Preset) -> HwResult<Option<Profile>> {
    let now = (ec.read(addr::PL1_SETTING)?, ec.read(addr::PL2_SETTING)?);
    let hit = |p: &Preset| (p.pl1, p.pl2) == now;
    Ok(STOCK_MODES
        .into_iter()
        .zip(stock)
        .find(|(_, p)| hit(p))
        .map(|(m, _)| Profile::Stock(m))
        .or_else(|| hit(custom).then_some(Profile::Custom)))
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
        assert_eq!(current(&mut Fake(0, 0), &st, &custom).unwrap(), None);
    }
}
