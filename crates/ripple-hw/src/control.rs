//! Profile (Balanced/Enthusiast/Overboost), fan boost and mode-button handling shared by
//! `rcc` and `rcc-gui`. Everything here was verified on real hardware:
//! PL1/PL2 via EC 1923/1924 (35→55→65 W measured as package power under
//! load), fan boost via the 0x40 bit of EC 1873, mode button = WMI event 176 =
//! key 184 (KEY_F14) on the "Uniwill WMI hotkeys" input device.

use std::io::Read;

use ripple_core::modes::{FanControlByte, OperatingMode};
use ripple_core::regs::{addr, duty_from_ec, duty_to_ec};
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
            return Err(format!(
                "PL1/PL2 must be 5..={PL_CEILING_W} W (got {}/{})",
                self.pl1, self.pl2
            ));
        }
        if self.pl1 > self.pl2 {
            return Err(format!("PL1 {} W is above PL2 {} W", self.pl1, self.pl2));
        }
        if self.ctgp > CTGP_CEILING_W {
            return Err(format!(
                "cTGP offset must be 0..={CTGP_CEILING_W} W (got {})",
                self.ctgp
            ));
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

pub const STOCK_MODES: [OperatingMode; 3] = [
    OperatingMode::Office,
    OperatingMode::Gaming,
    OperatingMode::Turbo,
];

/// The stock presets, in `STOCK_MODES` order. Not editable and not saved: Office/Gaming are the
/// EC's own default registers (1844/1845, 1840/1841); Turbo is the stock app's Smart APC table
/// for this machine (PL 80/80, cTGP +20 W), read from its Windows registry.
pub fn stock(ec: Option<&mut (dyn EcAccess + '_)>) -> [Preset; 3] {
    let mut office = Preset {
        pl1: 35,
        pl2: 35,
        ctgp: 0,
    };
    let mut gaming = Preset {
        pl1: 55,
        pl2: 55,
        ctgp: 0,
    };
    if let Some(ec) = ec {
        let mut rd = |a: u16, d: u8| ec.read(a).ok().filter(|v| *v >= 5).unwrap_or(d);
        office = Preset {
            pl1: rd(addr::OFFICE_PL1_DEFAULT, 35),
            pl2: rd(addr::OFFICE_PL2_DEFAULT, 35),
            ctgp: 0,
        };
        gaming = Preset {
            pl1: rd(addr::GAMING_PL1_DEFAULT, 55),
            pl2: rd(addr::GAMING_PL2_DEFAULT, 55),
            ctgp: 0,
        };
    }
    [
        office,
        gaming,
        Preset {
            pl1: 80,
            pl2: 80,
            ctgp: 20,
        },
    ]
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
        let (r, g, b) = (
            f32::from(r) / max_l,
            f32::from(g) / max_l,
            f32::from(b) / max_l,
        );
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
            s: if max == 0.0 {
                0
            } else {
                (d / max * 100.0).round() as u8
            },
            v: (max * 100.0).round() as u8,
        }
    }
}

/// The custom performance profile preset as shipped (what "reset" goes back to).
pub fn default_custom() -> Preset {
    // between Office and Gaming, so the first Custom is a gentle step, not a copy of a stock one
    Preset {
        pl1: 45,
        pl2: 45,
        ctgp: 0,
    }
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
    /// The custom fan profile (software curves, run by the daemon).
    #[serde(default)]
    pub fan: FanCurves,
    /// Backlight level (1..=4) the keyboard had before "off", to come back to.
    #[serde(default)]
    pub keyboard_level: Option<u8>,
    /// The touchpad is powered (its LED off). Off = the driver is unbound, see `touchpad.rs`.
    #[serde(default = "yes")]
    pub touchpad_on: bool,
}

fn yes() -> bool {
    true
}

impl Config {
    pub const PATH: &'static str = "/etc/ripple-nova/config.json";

    /// Saved config (unknown/old fields ignored, an invalid custom preset reset), else defaults.
    pub fn load() -> Self {
        let mut cfg = std::fs::read_to_string(Self::PATH)
            .ok()
            .and_then(|t| serde_json::from_str::<Self>(&t).ok())
            .unwrap_or(Self {
                custom: default_custom(),
                keyboard: None,
                auto_boost_above: None,
                fan: FanCurves::default(),
                keyboard_level: None,
                touchpad_on: true,
            });
        if cfg.custom.validate().is_err() {
            cfg.custom = default_custom();
        }
        if cfg.fan.validate().is_err() {
            cfg.fan = FanCurves::default(); // also switches it off
        }
        cfg
    }

    pub fn save(&self) -> HwResult<()> {
        let dir = std::path::Path::new(Self::PATH)
            .parent()
            .expect("PATH has a parent");
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
            return Err(HwError::Other(format!(
                "ctgp_offset: wrote {}, read {back}",
                p.ctgp
            )));
        }
    }
    Ok(())
}

/// Which profile the EC is running, judged by its PL1/PL2 setting registers. Stock profiles win
/// over Custom when both match. Both registers read 0 until something applies limits (fresh boot,
/// or the EC still in the state Windows left it): then the EC runs its own profile and its mode
/// byte (the button's lights) says which, so the profile is read from there.
pub fn current(
    ec: &mut dyn EcAccess,
    stock: &[Preset; 3],
    custom: &Preset,
) -> HwResult<Option<Profile>> {
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
pub fn adopt(
    ec: &mut dyn EcAccess,
    sysfs: Option<&SysfsEc>,
    stock: &[Preset; 3],
    custom: &Preset,
) -> HwResult<Option<Profile>> {
    let Some(p) = current(ec, stock, custom)? else {
        return Ok(None);
    };
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
/// Bit 7 of the same byte (the stock app's `RGBKeybaordWelcome_Trigger`, the kernel's
/// `RGB_RAINBOW_EFFECT`). Measured here: unlike the apply bit the EC never clears it, and it only
/// acts on the next apply: set + apply = static rainbow, clear + apply = the colour registers again.
/// (Bit 6, the logo effect, showed nothing: no logo LED.)
const KB_RAINBOW_BIT: u8 = 0x80;

/// Does the EC report an RGB keyboard? (`SupportByteTwoFlag` bit 2, the stock app's
/// `EC_RGBKEYBOARD_SUPPORT_BIT`.)
pub fn has_keyboard_rgb(ec: &mut dyn EcAccess) -> bool {
    ec.read(addr::SUPPORT_BYTE2).is_ok_and(|b| b & 0x04 != 0)
}

/// Current keyboard colour, each channel `0..=KB_LEVEL_MAX`.
pub fn keyboard_rgb(ec: &mut dyn EcAccess) -> HwResult<(u8, u8, u8)> {
    Ok((
        ec.read(addr::RGBKB_LEVEL_R)?,
        ec.read(addr::RGBKB_LEVEL_G)?,
        ec.read(addr::RGBKB_LEVEL_B)?,
    ))
}

/// Set the keyboard colour (each channel `0..=KB_LEVEL_MAX`). Writing the registers alone
/// does nothing visible: the EC only re-lights the keys once the apply bit is set (verified
/// on this machine: without it the colour stays put, with it the keyboard changes at once).
pub fn set_keyboard_rgb(ec: &mut dyn EcAccess, (r, g, b): (u8, u8, u8)) -> HwResult<()> {
    if r.max(g).max(b) > KB_LEVEL_MAX {
        return Err(HwError::Other(format!(
            "keyboard colour levels must be 0..={KB_LEVEL_MAX} (got {r},{g},{b})"
        )));
    }
    ec.write_verified(addr::RGBKB_LEVEL_R, r)?;
    ec.write_verified(addr::RGBKB_LEVEL_G, g)?;
    ec.write_verified(addr::RGBKB_LEVEL_B, b)?;
    // picking a colour ends the rainbow: the EC would otherwise apply it with bit 7 still set
    ec.update(
        addr::TRIGGER_BYTE,
        KB_APPLY_BIT | KB_RAINBOW_BIT,
        KB_APPLY_BIT,
    )?;
    Ok(())
}

/// EC 1932 (the kernel's `KBD_STATUS`): bits 7..5 = backlight level 0..=4 (the Fn keys and the
/// kernel LED use the same), bit 4 = apply (always written), bit 1 = power off (cleared on a write).
const KB_STATUS: u16 = 1932;
const KB_STATUS_LEVEL: u8 = 0xE0;
const KB_STATUS_APPLY: u8 = 0x10;
const KB_STATUS_POWER_OFF: u8 = 0x02;
/// Highest backlight level on this keyboard.
pub const KB_BACKLIGHT_MAX: u8 = 4;

/// Backlight level, 0 = off.
pub fn keyboard_backlight(ec: &mut dyn EcAccess) -> HwResult<u8> {
    Ok(((ec.read(KB_STATUS)? & KB_STATUS_LEVEL) >> 5).min(KB_BACKLIGHT_MAX))
}

pub fn set_keyboard_backlight(ec: &mut dyn EcAccess, level: u8) -> HwResult<()> {
    let cur = ec.read(KB_STATUS)?;
    let keep = cur & !(KB_STATUS_LEVEL | KB_STATUS_POWER_OFF);
    ec.write(
        KB_STATUS,
        keep | (level.min(KB_BACKLIGHT_MAX) << 5) | KB_STATUS_APPLY,
    )
}

/// Backlight off. Black is not a colour here (the colour registers can't do it), so this is the
/// level, like the kernel LED's brightness 0. Returns the level it had.
pub fn keyboard_off(ec: &mut dyn EcAccess) -> HwResult<u8> {
    let level = keyboard_backlight(ec)?;
    set_keyboard_backlight(ec, 0)?;
    Ok(level)
}

/// Back on at `was` (the level before it went off; full when unknown), unless it is already on.
pub fn keyboard_on(ec: &mut dyn EcAccess, was: Option<u8>) -> HwResult<()> {
    if keyboard_backlight(ec)? == 0 {
        set_keyboard_backlight(ec, was.filter(|l| *l > 0).unwrap_or(KB_BACKLIGHT_MAX))?;
    }
    Ok(())
}

/// Is the keyboard in rainbow mode?
pub fn keyboard_rainbow(ec: &mut dyn EcAccess) -> HwResult<bool> {
    Ok(ec.read(addr::TRIGGER_BYTE)? & KB_RAINBOW_BIT != 0)
}

/// Rainbow on, or back to the colour registers.
pub fn set_keyboard_rainbow(ec: &mut dyn EcAccess, on: bool) -> HwResult<()> {
    let bit = if on { KB_RAINBOW_BIT } else { 0 };
    ec.update(
        addr::TRIGGER_BYTE,
        KB_APPLY_BIT | KB_RAINBOW_BIT,
        KB_APPLY_BIT | bit,
    )?;
    Ok(())
}

/// The colour the keyboard shows: the saved choice while it still matches the EC's levels (it keeps
/// the hue of a dimmed colour), else what the EC reports (changed behind our back by `rcc` or the
/// firmware).
pub fn keyboard_choice(saved: Option<Hsv>, levels: Option<(u8, u8, u8)>) -> Option<Hsv> {
    match (saved, levels) {
        (Some(h), Some(l)) => {
            let c = h.to_levels();
            let close = c.0.abs_diff(l.0) <= 1 && c.1.abs_diff(l.1) <= 1 && c.2.abs_diff(l.2) <= 1;
            Some(if close { h } else { Hsv::from_levels(l) })
        }
        (Some(h), None) => Some(h),
        (None, l) => l.map(Hsv::from_levels),
    }
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
    set_mode_byte_with(ec, p, Config::load().fan.enabled)
}

fn set_mode_byte_with(ec: &mut dyn EcAccess, p: Profile, custom_fan_on: bool) -> HwResult<()> {
    let cur = ec.read(addr::MAFAN_CONTROL_BYTE)?;
    let nib = p.mode_nibble();
    // With the custom fan engine on, bit 6 is its manual bit (not a boost) and has to stay. The
    // config only *keeps* a bit that is already there: only the engine creates it, otherwise it
    // would never see its own mark and take the byte for a user's boost (fans stuck at 100 %).
    let manual = custom_fan_on && cur & MANUAL_BIT != 0;
    if is_boosted(cur) && !manual {
        std::fs::write(STASH_PATH, (nib << 4).to_string())?;
        return Ok(());
    }
    let new = (cur & 0x0F) | (nib << 4) | if manual { MANUAL_BIT } else { 0 };
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
    let mut stash = std::fs::read_to_string(STASH_PATH)
        .ok()
        .and_then(|s| s.trim().parse().ok());
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

/// What each fan (CPU, GPU) is *told* to run at, in %, while the EC follows the duty registers (the manual bit:
/// the custom profile, boost); `None` when it does not. hwmon's `pwm` is the EC's reported duty (EC 0x75B/0x75C),
/// not those registers: in the window a stopped fan showed 100 % in custom mode. A duty that cannot be read is `None`.
pub fn manual_duties(ec: &mut dyn EcAccess, byte: u8) -> Option<[Option<u8>; 2]> {
    (byte & MANUAL_BIT != 0).then(|| [1, 2].map(|fan| ec.fan_duty(fan).ok().map(duty_from_ec)))
}

/// Software fan policy with 8 °C hysteresis. Only turns off a boost it turned on.
pub fn auto_boost_action(
    threshold: Option<u8>,
    temp_c: f64,
    boosted: bool,
    ours: bool,
) -> Option<bool> {
    let t = f64::from(threshold?);
    if !boosted && temp_c >= t {
        Some(true)
    } else if boosted && ours && temp_c <= t - 8.0 {
        Some(false)
    } else {
        None
    }
}

// ------------------------------------------------------------ custom fan curves

/// °C at which the custom curve is defined, like the stock app's five user levels.
pub const FAN_ANCHORS: [u8; 5] = [45, 55, 65, 75, 85];
/// At or above this temperature, or with no readable sensor, the engine ignores the curves and runs
/// both fans flat out.
pub const FAN_PANIC_C: f64 = 90.0;
/// How often the engine looks at the temperatures.
pub const FAN_TICK: std::time::Duration = std::time::Duration::from_secs(1);
/// The EC follows the per-fan duty registers while this bit of EC 1873 is set (it is also the Boost
/// bit). Measured here: it keeps the mode nibble, so the mode lights stay as they were.
const MANUAL_BIT: u8 = 0x40;
/// Floor for the duty at the last anchor, so a curve can never let a hot machine's fans stop (the
/// 90 °C rule is the last resort, not the plan).
const FAN_MIN_AT_TOP: u8 = 60;
/// Fans slow down by at most this many points per tick, i.e. 2 per second (they speed up at once).
/// The EC sets both duties to full whenever the mode byte is written (a mode change, say), so a short
/// tick also keeps that blast short.
const FAN_STEP_DOWN: u8 = 2;

/// The user's fan profile: duty in percent at each of `FAN_ANCHORS`, per fan. `enabled` = the
/// daemon drives the fans from these curves instead of the firmware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanCurves {
    pub enabled: bool,
    /// Fan 1, driven by the CPU temperature.
    pub cpu: [u8; 5],
    /// Fan 2, driven by the GPU temperature.
    pub gpu: [u8; 5],
}

impl Default for FanCurves {
    fn default() -> Self {
        Self {
            enabled: false,
            cpu: [20, 30, 45, 70, 100],
            gpu: [0, 25, 40, 65, 100],
        }
    }
}

impl FanCurves {
    pub fn validate(&self) -> Result<(), String> {
        for (name, c) in [("CPU fan", &self.cpu), ("GPU fan", &self.gpu)] {
            if c.iter().any(|d| *d > 100) {
                return Err(format!("{name}: duty is 0..=100 %"));
            }
            if c.windows(2).any(|w| w[0] > w[1]) {
                return Err(format!(
                    "{name}: duty must not fall as the temperature rises"
                ));
            }
            if c[4] < FAN_MIN_AT_TOP {
                return Err(format!(
                    "{name}: at least {FAN_MIN_AT_TOP} % at {} °C",
                    FAN_ANCHORS[4]
                ));
            }
        }
        Ok(())
    }
}

/// Duty (percent) of `curve` at `temp_c`: linear between the anchors, flat outside them.
pub fn curve_duty(curve: &[u8; 5], temp_c: f64) -> u8 {
    let a = FAN_ANCHORS.map(f64::from);
    if temp_c <= a[0] {
        return curve[0];
    }
    for i in 1..5 {
        if temp_c <= a[i] {
            let (lo, hi) = (f64::from(curve[i - 1]), f64::from(curve[i]));
            return (lo + (temp_c - a[i - 1]) / (a[i] - a[i - 1]) * (hi - lo)).round() as u8;
        }
    }
    curve[4]
}

/// Set one point of a curve; the curve never falls, so the points around it move along.
pub fn set_curve_point(curve: &mut [u8; 5], idx: usize, duty: u8) {
    curve[idx] = duty;
    curve[idx + 1..]
        .iter_mut()
        .for_each(|d| *d = (*d).max(duty));
    curve[..idx].iter_mut().for_each(|d| *d = (*d).min(duty));
}

/// Next duty: speeding up is immediate, slowing down is `FAN_STEP_DOWN` points per tick.
pub fn fan_smooth(prev: u8, target: u8) -> u8 {
    if target >= prev {
        target
    } else {
        prev.saturating_sub(FAN_STEP_DOWN).max(target)
    }
}

/// Target duties (percent) `[fan 1, fan 2]` for the sensor readings. A fan whose own sensor is
/// missing follows the other one; no sensor at all, or one at `FAN_PANIC_C`, means both flat out.
pub fn fan_targets(cfg: &FanCurves, cpu: Option<f64>, gpu: Option<f64>) -> [u8; 2] {
    match (cpu.or(gpu), gpu.or(cpu)) {
        (Some(c), Some(g)) if c < FAN_PANIC_C && g < FAN_PANIC_C => {
            [curve_duty(&cfg.cpu, c), curve_duty(&cfg.gpu, g)]
        }
        _ => [100, 100],
    }
}

/// Hand the fans back to the firmware: drop the manual bit, keep the mode.
pub fn fan_release(ec: &mut dyn EcAccess) -> HwResult<()> {
    let byte = ec.read(addr::MAFAN_CONTROL_BYTE)?;
    if byte & MANUAL_BIT == 0 {
        return Ok(());
    }
    if is_boosted(byte) {
        set_boost(ec, false)?; // restores the pre-boost mode, or Enthusiast (nibble 0) without a stash
    } else {
        ec.write(addr::MAFAN_CONTROL_BYTE, byte & !MANUAL_BIT)?;
    }
    Ok(())
}

/// Drives the two fans from the custom curves; call `tick` every `FAN_TICK`.
pub struct FanEngine {
    duty: [u8; 2],
    /// Is the manual bit ours? Enthusiast + manual is byte 0x40, exactly the Boost byte: without
    /// this marker a boosted byte is the user's and left alone.
    ours: bool,
    /// The EC sets both duties to full when the mode byte changes; wait before writing ours.
    settle: std::time::Duration,
}

impl Default for FanEngine {
    fn default() -> Self {
        Self {
            duty: [100; 2],
            ours: false,
            settle: std::time::Duration::from_millis(500),
        }
    }
}

impl FanEngine {
    pub fn tick(
        &mut self,
        ec: &mut dyn EcAccess,
        cfg: &FanCurves,
        cpu: Option<f64>,
        gpu: Option<f64>,
    ) -> HwResult<()> {
        if !cfg.enabled {
            return self.stop(ec);
        }
        let byte = ec.read(addr::MAFAN_CONTROL_BYTE)?;
        if is_boosted(byte) && !self.ours {
            return Ok(());
        }
        if !self.ours {
            // start from what the firmware is doing now, so taking over does not jerk the fans
            self.duty = [duty_from_ec(ec.fan_duty(1)?), duty_from_ec(ec.fan_duty(2)?)];
        }
        let target = fan_targets(cfg, cpu, gpu);
        for (d, t) in self.duty.iter_mut().zip(target) {
            *d = fan_smooth(*d, t);
        }
        if byte & MANUAL_BIT == 0 {
            ec.write(addr::MAFAN_CONTROL_BYTE, byte | MANUAL_BIT)?;
            std::thread::sleep(self.settle);
        }
        self.ours = true;
        for (fan, d) in [1u8, 2].into_iter().zip(self.duty) {
            let raw = duty_to_ec(d);
            if ec.fan_duty(fan)? != raw {
                ec.set_fan_duty(fan, raw)?;
            }
        }
        Ok(())
    }

    /// Give the fans back to the firmware, if they are ours.
    pub fn stop(&mut self, ec: &mut dyn EcAccess) -> HwResult<()> {
        if self.ours {
            fan_release(ec)?;
            self.ours = false;
        }
        Ok(())
    }
}

/// CPU and GPU temperature (°C) out of the hwmon readings, by label, else by position.
fn cpu_gpu_temps(s: &crate::linux::Sensors) -> (Option<f64>, Option<f64>) {
    let by = |name: &str, pos: usize| {
        let hit = s
            .temps
            .iter()
            .find(|(l, _)| l.to_ascii_uppercase().contains(name))
            .or_else(|| s.temps.get(pos));
        hit.map(|(_, mv)| f64::from(*mv) / 1000.0)
    };
    (by("CPU", 0), by("GPU", 1))
}

/// Does a platform attribute get a toggle? `ctgp_offset` belongs to the profiles; `rainbow_animation` (older
/// drivers; the newer one made it an LED trigger) and `breathing_in_suspend` are lightbar animations and mean
/// nothing without a lightbar, so they follow `has_lightbar` (the same test that shows the lightbar controls).
/// The rest are shown on every machine, the doubtful one marked experimental in the window
/// (`notify::is_experimental`): `touchpad_toggle_enable` (the firmware's double tap) did nothing visible on this
/// laptop, and the real touchpad switch is `touchpad.rs`.
fn toggle_shown(name: &str, has_lightbar: bool) -> bool {
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
    let mut cfg = Config::load();
    cfg.touchpad_on = on;
    cfg.save()?;
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

/// Is Boost on? Boost is the manual bit with both duties at full (200). In Enthusiast the custom fan
/// profile's manual byte is the very same `0x40`, and while the engine hands the fans back after the
/// profile is switched off the byte is still set: the duties (the profile's curve, not full) tell them apart.
fn boost_is_on(ec: &mut dyn EcAccess) -> bool {
    ec.read(addr::MAFAN_CONTROL_BYTE).is_ok_and(is_boosted)
        && ec.fan_duty(1).is_ok_and(|d| d >= 200)
        && ec.fan_duty(2).is_ok_and(|d| d >= 200)
}

/// What the popups look at right now.
pub fn observed(
    ec: &mut dyn EcAccess,
    sysfs: Option<&SysfsEc>,
    stock: &[Preset; 3],
    cfg: &Config,
) -> crate::notify::Observed {
    crate::notify::Observed {
        profile: current(ec, stock, &cfg.custom).ok().flatten(),
        pl: ec
            .read(addr::PL1_SETTING)
            .ok()
            .zip(ec.read(addr::PL2_SETTING).ok()),
        boosted: !cfg.fan.enabled && boost_is_on(ec),
        toggles: sysfs
            .map(|u| platform_toggles(u, has_lightbar(ec)))
            .unwrap_or_default(),
    }
}

/// Where the daemon publishes the profile it sees, as a lowercase name (`balanced`, `enthusiast`, `overboost`,
/// `custom`, `unknown`), rewritten every second. World-readable: the tray icon runs as the desktop user, cannot
/// read the EC, and treats a file older than a few seconds as "the daemon is not running".
pub const PROFILE_FILE: &str = "/run/ripple-nova-profile";

fn publish_profile(path: &std::path::Path, p: Option<Profile>) {
    use std::os::unix::fs::PermissionsExt;
    let tmp = path.with_extension("tmp");
    // written aside and renamed, so a reader never sees half a file
    let _ = std::fs::write(
        &tmp,
        p.map_or("unknown".into(), |p| p.name().to_lowercase()),
    )
    .and_then(|()| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)))
    .and_then(|()| std::fs::rename(&tmp, path));
}

/// The popup loop (the daemon runs it in a thread, so popups work with no window open): once a second,
/// look at the profile, fan boost and toggles, and pop up what changed when "Show popup" is on. The profile
/// is also published for the tray icon (`PROFILE_FILE`).
pub fn popup_loop(stop: &std::sync::atomic::AtomicBool) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut ec = match crate::linux::open_auto() {
        Ok(ec) => ec,
        Err(e) => return eprintln!("popups: {e}"),
    };
    let sysfs = SysfsEc::probe().ok();
    let stock = stock(Some(ec.as_mut()));
    let mut notifier = crate::notify::Notifier::default();
    while !stop.load(Relaxed) {
        let cfg = Config::load();
        let seen = observed(ec.as_mut(), sysfs.as_ref(), &stock, &cfg);
        publish_profile(std::path::Path::new(PROFILE_FILE), seen.profile);
        let changes = notifier.observe(&seen);
        if !changes.is_empty() && crate::notify::popups_enabled() {
            for (title, body) in changes {
                crate::notify::send_to_logged_in(&title, &body);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// The engine's main loop (the daemon runs it in a thread): every tick, read the saved profile and
/// the sensors and drive the fans. Without the uniwill hwmon driver there are no sensors to go by,
/// so the fans stay with the firmware. Hands the fans back when `stop` is set.
pub fn fan_loop(stop: &std::sync::atomic::AtomicBool) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut ec = match crate::linux::open_auto() {
        Ok(ec) => ec,
        Err(e) => return eprintln!("fan engine: {e}"),
    };
    let Ok(sysfs) = SysfsEc::probe() else {
        return eprintln!(
            "fan engine: no uniwill hwmon (kernel driver not loaded), fans stay with the firmware"
        );
    };
    let mut engine = FanEngine::default();
    let mut last_err = String::new(); // a persistent error is reported once, not every tick
    while !stop.load(Relaxed) {
        let cfg = Config::load().fan;
        let (cpu, gpu) = sysfs.sensors().map_or((None, None), |s| cpu_gpu_temps(&s));
        match engine.tick(ec.as_mut(), &cfg, cpu, gpu) {
            Ok(()) => last_err.clear(),
            Err(e) if e.to_string() != last_err => {
                last_err = e.to_string();
                eprintln!("fan engine: {last_err}");
            }
            Err(_) => {}
        }
        std::thread::sleep(FAN_TICK);
    }
    let _ = engine.stop(ec.as_mut());
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
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
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
        assert!(Preset {
            pl1: 35,
            pl2: 35,
            ctgp: 0
        }
        .validate()
        .is_ok());
        assert!(Preset {
            pl1: 80,
            pl2: 80,
            ctgp: 20
        }
        .validate()
        .is_ok());
        assert!(Preset {
            pl1: 81,
            pl2: 81,
            ctgp: 0
        }
        .validate()
        .is_err());
        assert!(Preset {
            pl1: 60,
            pl2: 50,
            ctgp: 0
        }
        .validate()
        .is_err());
        assert!(Preset {
            pl1: 4,
            pl2: 40,
            ctgp: 0
        }
        .validate()
        .is_err());
        assert!(Preset {
            pl1: 35,
            pl2: 35,
            ctgp: 26
        }
        .validate()
        .is_err());
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
        assert!(
            follow_lock_at(&path, false).is_none(),
            "second follower must be refused"
        );
        drop(first);
        assert!(
            follow_lock_at(&path, false).is_some(),
            "released lock can be taken again"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn button_cycles_stock_profiles_and_restarts_from_anything_else() {
        use OperatingMode::*;
        // the button's LEDs go 0 → 1 → 2: Office → Gaming → Turbo → Office
        assert_eq!(
            Profile::after_button(Some(Profile::Stock(Office))),
            Profile::Stock(Gaming)
        );
        assert_eq!(
            Profile::after_button(Some(Profile::Stock(Gaming))),
            Profile::Stock(Turbo)
        );
        assert_eq!(
            Profile::after_button(Some(Profile::Stock(Turbo))),
            Profile::Stock(Office)
        );
        assert_eq!(
            Profile::after_button(Some(Profile::Custom)),
            Profile::Stock(Turbo)
        ); // shows as 1 light
        assert_eq!(Profile::after_button(None), Profile::Stock(Office));
        // lights: measured 0xA_ = 0 lights, 0x0_ = 1 light, 0x3_ = 2 lights
        assert_eq!(Profile::Stock(Office).mode_nibble(), 0xA);
        assert_eq!(Profile::Stock(Gaming).mode_nibble(), 0x0);
        assert_eq!(Profile::Stock(Turbo).mode_nibble(), 0x3);
    }

    #[test]
    fn hsv_matches_the_ec_levels() {
        assert_eq!(
            Hsv {
                h: 0,
                s: 100,
                v: 100
            }
            .to_levels(),
            (50, 0, 0)
        );
        assert_eq!(
            Hsv {
                h: 120,
                s: 100,
                v: 100
            }
            .to_levels(),
            (0, 50, 0)
        );
        assert_eq!(Hsv { h: 0, s: 0, v: 100 }.to_levels(), (50, 50, 50));
        assert_eq!(
            Hsv {
                h: 0,
                s: 100,
                v: 50
            }
            .to_levels(),
            (25, 0, 0)
        );
        assert_eq!(
            Hsv {
                h: 200,
                s: 40,
                v: 0
            }
            .to_levels(),
            (0, 0, 0)
        );
        // the firmware's default colour is cyan at full brightness
        assert_eq!(
            Hsv::from_levels((0, 50, 50)),
            Hsv {
                h: 180,
                s: 100,
                v: 100
            }
        );
        for c in [(50, 0, 0), (0, 50, 25), (10, 20, 30), (50, 50, 50)] {
            let back = Hsv::from_levels(c).to_levels();
            assert!(
                back.0.abs_diff(c.0) <= 1 && back.1.abs_diff(c.1) <= 1 && back.2.abs_diff(c.2) <= 1,
                "{c:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn current_prefers_stock_over_custom() {
        struct Fake(u8, u8);
        impl EcAccess for Fake {
            fn name(&self) -> &str {
                "fake"
            }
            fn read(&mut self, a: u16) -> HwResult<u8> {
                Ok(if a == addr::PL1_SETTING {
                    self.0
                } else {
                    self.1
                })
            }
            fn write(&mut self, _: u16, _: u8) -> HwResult<()> {
                Ok(())
            }
        }
        let st = stock(None);
        let custom = Preset {
            pl1: 35,
            pl2: 35,
            ctgp: 5,
        }; // same limits as Office
        assert_eq!(
            current(&mut Fake(35, 35), &st, &custom).unwrap(),
            Some(Profile::Stock(OperatingMode::Office))
        );
        assert_eq!(
            current(
                &mut Fake(45, 45),
                &st,
                &Preset {
                    pl1: 45,
                    pl2: 45,
                    ctgp: 0
                }
            )
            .unwrap(),
            Some(Profile::Custom)
        );
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
        assert!(FanCurves {
            cpu: c,
            ..FanCurves::default()
        }
        .validate()
        .is_err());
        assert!(FanCurves {
            gpu: [0, 0, 0, 0, 59],
            ..FanCurves::default()
        }
        .validate()
        .is_err());
        assert!(FanCurves {
            gpu: [0, 0, 0, 0, 60],
            ..FanCurves::default()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn fan_targets_fail_safe() {
        let cfg = FanCurves::default();
        assert_eq!(fan_targets(&cfg, Some(45.0), Some(45.0)), [20, 0]);
        assert_eq!(
            fan_targets(&cfg, Some(89.0), Some(40.0))[0],
            curve_duty(&cfg.cpu, 89.0)
        );
        assert_eq!(fan_targets(&cfg, Some(90.0), Some(40.0)), [100, 100]); // either sensor hot
        assert_eq!(fan_targets(&cfg, Some(40.0), Some(95.0)), [100, 100]);
        assert_eq!(
            fan_targets(&cfg, None, Some(65.0)),
            [curve_duty(&cfg.cpu, 65.0), curve_duty(&cfg.gpu, 65.0)]
        );
        assert_eq!(fan_targets(&cfg, None, None), [100, 100]); // blind = flat out
        assert!(FanCurves::default().validate().is_ok());
        assert!(FanCurves {
            cpu: [20, 30, 25, 70, 100],
            ..FanCurves::default()
        }
        .validate()
        .is_err());
        assert!(FanCurves {
            gpu: [0, 25, 40, 65, 101],
            ..FanCurves::default()
        }
        .validate()
        .is_err());
    }

    /// A mock EC with the two fan duty registers.
    struct FanEc(crate::mock::MockEc, [u8; 2]);
    impl EcAccess for FanEc {
        fn name(&self) -> &str {
            "fan-mock"
        }
        fn read(&mut self, a: u16) -> HwResult<u8> {
            self.0.read(a)
        }
        fn write(&mut self, a: u16, v: u8) -> HwResult<()> {
            self.0.write(a, v)
        }
        fn fan_duty(&mut self, f: u8) -> HwResult<u8> {
            Ok(self.1[usize::from(f) - 1])
        }
        fn set_fan_duty(&mut self, f: u8, d: u8) -> HwResult<()> {
            self.1[usize::from(f) - 1] = d;
            Ok(())
        }
    }

    #[test]
    fn engine_takes_over_on_top_of_the_mode_and_hands_back() {
        let mut ec = FanEc(
            crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0xA7),
            [60, 0],
        ); // Balanced
        let cfg = FanCurves {
            enabled: true,
            ..FanCurves::default()
        };
        let mut eng = FanEngine {
            settle: std::time::Duration::ZERO,
            ..FanEngine::default()
        };
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
        eng.tick(&mut ec, &FanCurves::default(), Some(60.0), Some(60.0))
            .unwrap();
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
    fn which_attributes_become_toggles() {
        for lightbar in [false, true] {
            assert!(!toggle_shown("ctgp_offset", lightbar)); // the profiles' business
                                                             // shown everywhere (the doubtful one is marked experimental, not hidden)
            for name in [
                "fn_lock",
                "super_key_enable",
                "ac_auto_boot",
                "usb_powershare_high",
                "touchpad_toggle_enable",
            ] {
                assert!(
                    toggle_shown(name, lightbar),
                    "{name} (lightbar: {lightbar})"
                );
            }
        }
        // the lightbar animations only with a lightbar
        for name in ["rainbow_animation", "breathing_in_suspend"] {
            assert!(
                !toggle_shown(name, false) && toggle_shown(name, true),
                "{name}"
            );
        }
    }

    #[test]
    fn manual_duties_are_what_the_fans_are_told() {
        let mut ec = FanEc(
            crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0x47),
            [0, 200],
        );
        // a stopped fan is 0 %, not what hwmon's reported duty says in manual mode
        assert_eq!(manual_duties(&mut ec, 0x47), Some([Some(0), Some(100)]));
        // the firmware's own curve: hwmon's number is the right one
        assert_eq!(manual_duties(&mut ec, 0xA7), None);
        // no WMI duty access: no number rather than a wrong one
        let mut plain = crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0x47);
        assert_eq!(manual_duties(&mut plain, 0x47), Some([None, None]));
    }

    #[test]
    fn the_published_profile_is_a_plain_readable_name() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("ripple-profile-test-{}", std::process::id()));
        publish_profile(&path, Some(Profile::Stock(OperatingMode::Office)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "balanced");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        publish_profile(&path, Some(Profile::Custom));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "custom");
        publish_profile(&path, None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "unknown");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_touchpad_choice_defaults_to_on_and_survives_an_old_config() {
        assert!(serde_json::from_str::<Config>("{}").unwrap().touchpad_on);
        assert!(
            !serde_json::from_str::<Config>(r#"{"touchpad_on":false}"#)
                .unwrap()
                .touchpad_on
        );
    }

    #[test]
    fn boost_is_the_manual_bit_with_both_fans_at_full() {
        let boosted = |byte: u8, duties: [u8; 2]| {
            boost_is_on(&mut FanEc(
                crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, byte),
                duties,
            ))
        };
        assert!(boosted(0x47, [200, 200])); // Boost
        assert!(!boosted(0x47, [60, 40])); // the custom profile's byte in Enthusiast, curve duties: not a boost
        assert!(!boosted(0x47, [200, 40])); // one fan at full only
        assert!(!boosted(0x07, [200, 200])); // no manual bit
        assert!(!boosted(0xE7, [200, 200])); // Balanced + manual is not the boost nibble
    }

    #[test]
    fn engine_leaves_a_users_boost_alone() {
        let mut ec = FanEc(
            crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0x40),
            [200, 200],
        );
        let cfg = FanCurves {
            enabled: true,
            ..FanCurves::default()
        };
        let mut eng = FanEngine {
            settle: std::time::Duration::ZERO,
            ..FanEngine::default()
        };
        eng.tick(&mut ec, &cfg, Some(40.0), Some(40.0)).unwrap();
        assert_eq!(
            (ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), ec.1),
            (0x40, [200, 200])
        );
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
        let saved = Hsv {
            h: 0,
            s: 100,
            v: 50,
        }; // dim red: levels (25, 0, 0)
        assert_eq!(keyboard_choice(Some(saved), Some((25, 0, 0))), Some(saved));
        assert_eq!(
            keyboard_choice(Some(saved), Some((0, 0, 50))),
            Some(Hsv::from_levels((0, 0, 50)))
        );
        assert_eq!(keyboard_choice(Some(saved), None), Some(saved));
        assert_eq!(keyboard_choice(None, None), None);
    }

    #[test]
    fn boot_state_is_read_from_the_mode_byte() {
        use OperatingMode::*;
        let st = stock(None);
        let custom = Preset {
            pl1: 45,
            pl2: 45,
            ctgp: 0,
        };
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
        let custom = Preset {
            pl1: 45,
            pl2: 45,
            ctgp: 0,
        };
        let mut ec = ec_with((0, 0), 0x07);
        let p = adopt(&mut ec, None, &st, &custom).unwrap();
        assert_eq!(p, Some(Profile::Stock(OperatingMode::Gaming)));
        assert_eq!(
            (
                ec.read(addr::PL1_SETTING).unwrap(),
                ec.read(addr::PL2_SETTING).unwrap()
            ),
            (55, 55)
        );
        assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x07); // lights untouched
                                                                      // limits that are already set are left alone
        let mut ec = ec_with((45, 45), 0x07);
        assert_eq!(
            adopt(&mut ec, None, &st, &custom).unwrap(),
            Some(Profile::Custom)
        );
        assert_eq!(ec.read(addr::PL1_SETTING).unwrap(), 45);
    }
}
