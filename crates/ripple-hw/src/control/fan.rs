//! Fans: the mode byte and boost, the user's curves per profile, and the engine that drives the fans from them.

use super::*;

/// Mode nibble of the fan control byte (EC 1873).
const FAN_MODE_MASK: u8 = 0xF0;

/// Pre-boost mode survives across processes here (tmpfs: cleared on reboot).
const STASH_PATH: &str = "/run/ripple-nova-fan-mode";

/// Write the profile's mode nibble into EC 1873 (lights + fan profile). While fan boost is on the
/// nibble belongs to the boost, so the new mode is stashed and takes effect when boost ends.
pub fn set_mode_byte(ec: &mut dyn EcAccess, p: Profile) -> HwResult<()> {
    set_mode_byte_with(ec, p, Config::load().fan.enabled)
}

pub(super) fn set_mode_byte_with(
    ec: &mut dyn EcAccess,
    p: Profile,
    custom_fan_on: bool,
) -> HwResult<()> {
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

/// New fan control byte for boost on/off. `stash` remembers the pre-boost mode so
/// "off" restores it. "Off" only touches a byte that is actually boosted: the EC may
/// already have changed it itself (Fn key), and clobbering that is the one thing we
/// must not do.
pub(super) fn boost_byte(cur: u8, on: bool, stash: &mut Option<u8>) -> u8 {
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
pub const FAN_MIN_AT_TOP: u8 = 60;

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

/// Only the tests start from a ready-made curve; the real defaults are per mode (`default_fan_set`).
#[cfg(test)]
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

/// Whether the custom fan profile is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FanSwitch {
    #[serde(default)]
    pub enabled: bool,
}

/// One profile's two curves: duty in percent at each of `FAN_ANCHORS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanSet {
    /// Fan 1, driven by the CPU temperature.
    pub cpu: [u8; 5],
    /// Fan 2, driven by the GPU temperature.
    pub gpu: [u8; 5],
}

impl FanSet {
    pub fn validate(&self) -> Result<(), String> {
        FanCurves {
            enabled: false,
            cpu: self.cpu,
            gpu: self.gpu,
        }
        .validate()
    }
}

/// The fan curves the Windows app starts each mode with, read off its Fan Settings page (Balanced = Office,
/// Enthusiast = Gaming, Overboost = Turbo; Gaming and Turbo are the same). The app draws its points evenly
/// spaced, not at temperatures, so they are taken here at 5 °C steps from 40 to 85 °C (its own tables for
/// other models end at about 85 °C); at the five anchors that is 30 / 35 / 48 / 55 / 85 % for the CPU fan,
/// the GPU fan staying off to 45 °C in Gaming and Turbo. Office tops out at 55 % there; that is 60 % here,
/// the floor every curve keeps at 85 °C.
pub fn default_fan_set(mode: OperatingMode) -> FanSet {
    match mode {
        OperatingMode::Office => FanSet {
            cpu: [30, 35, 48, 55, FAN_MIN_AT_TOP],
            gpu: [30, 35, 48, 55, FAN_MIN_AT_TOP],
        },
        OperatingMode::Gaming | OperatingMode::Turbo => FanSet {
            cpu: [30, 35, 48, 55, 85],
            gpu: [0, 35, 48, 55, 85],
        },
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
        set_boost(ec, false)?; // restores the pre-boost mode, or Gaming (nibble 0) without a stash
    } else {
        ec.write(addr::MAFAN_CONTROL_BYTE, byte & !MANUAL_BIT)?;
    }
    Ok(())
}

/// Drives the two fans from the custom curves; call `tick` every `FAN_TICK`.
pub struct FanEngine {
    pub(super) duty: [u8; 2],
    /// Is the manual bit ours? Gaming + manual is byte 0x40, exactly the Boost byte: without
    /// this marker a boosted byte is the user's and left alone.
    pub(super) ours: bool,
    /// The EC sets both duties to full when the mode byte changes; wait before writing ours.
    pub(super) settle: std::time::Duration,
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
pub(super) fn cpu_gpu_temps(s: &crate::linux::Sensors) -> (Option<f64>, Option<f64>) {
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

/// Is Boost on? Boost is the manual bit with both duties at full (200). In Gaming the custom fan
/// profile's manual byte is the very same `0x40`, and while the engine hands the fans back after the
/// profile is switched off the byte is still set: the duties (the profile's curve, not full) tell them apart.
pub(super) fn boost_is_on(ec: &mut dyn EcAccess) -> bool {
    ec.read(addr::MAFAN_CONTROL_BYTE).is_ok_and(is_boosted)
        && ec.fan_duty(1).is_ok_and(|d| d >= 200)
        && ec.fan_duty(2).is_ok_and(|d| d >= 200)
}
