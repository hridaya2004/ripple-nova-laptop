//! Keyboard backlight: colour, brightness level, rainbow.

use super::*;

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
pub(super) const KB_STATUS: u16 = 1932;

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
