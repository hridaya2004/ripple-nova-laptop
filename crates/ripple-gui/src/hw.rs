//! Blocking hardware layer for the GUI. Everything here may sleep (6 ms EC pacing, sysfs),
//! so the UI only ever calls it from a background task, behind one mutex. The logic itself
//! lives in `ripple_hw::control`, shared with the CLI.

use ripple_core::modes::OperatingMode;
use ripple_core::regs::addr;
use ripple_hw::control::{self, Config, CustomProfile, FanCurves, FanSet, Hsv, Preset, Profile};
use ripple_hw::linux::{self, Battery, SysfsEc};
use ripple_hw::EcAccess;

/// Dropdown order: the three stock profiles, then the user's own (`Snapshot::customs`).
pub const STOCK_PROFILES: [Profile; 3] = [
    Profile::Stock(OperatingMode::Office),
    Profile::Stock(OperatingMode::Gaming),
    Profile::Stock(OperatingMode::Turbo),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Pl1,
    Pl2,
    Pl4,
    Ctgp,
}

#[derive(Clone)]
pub struct Snapshot {
    pub model: String,
    pub backend: String,
    /// Why there is no EC backend (profiles and fan boost are then unavailable).
    pub ec_error: Option<String>,
    pub temps: Vec<(String, f64)>,
    /// `(label, rpm, duty %)`
    pub fans: Vec<(String, u32, Option<u32>)>,
    /// Active profile, `None` when the EC's PL registers match none.
    pub profile: Option<Profile>,
    /// Current PL1/PL2 and cTGP offset as the hardware reports them.
    pub pl: Option<(u8, u8)>,
    pub pl4: Option<u8>,
    pub ctgp: Option<u8>,
    /// Stock presets (read-only, Office/Gaming/Turbo order) and the user's own profiles.
    pub stock: [Preset; 3],
    pub customs: Vec<CustomProfile>,
    /// Fan boost is on (not the custom profile, which shares the EC bit).
    pub boosted: bool,
    /// The curves of the running profile (the user's own, else the default) and whether the daemon
    /// applies them.
    pub fan: FanCurves,
    /// The default curves of the running profile: what Auto shows, what "Default preset" goes back to.
    pub fan_default: FanSet,
    pub auto_boost: Option<u8>,
    pub has_lightbar: bool,
    pub lightbar: Option<(u8, u8, u8)>,
    /// EC-reported RGB keyboard backlight: actual EC levels, and the chosen colour/brightness.
    pub has_keyboard: bool,
    pub keyboard: Option<(u8, u8, u8)>,
    pub rainbow: bool,
    /// Backlight level 0..=4 (0 = off).
    pub kb_level: u8,
    pub hsv: Hsv,
    pub toggles: Vec<(&'static str, bool)>,
    /// The battery's charge mode and readings; `None` without the uniwill driver.
    pub battery: Option<Battery>,
    /// Is the hardware mode button being followed, and by the systemd service rather than by us?
    pub button: bool,
    pub service: bool,
    /// Raw EC values, shown in debug builds only.
    pub debug: String,
    /// Last action / hardware-button result for the status bar.
    pub event: String,
}

impl Snapshot {
    /// A profile's name as shown: the stock modes by name, the user's by their own.
    pub fn label(&self, p: Profile) -> String {
        match p {
            Profile::Custom(i) => self
                .customs
                .get(i)
                .map_or_else(|| "Custom".to_string(), |c| c.name.clone()),
            _ => p.name().to_string(),
        }
    }

    /// The stock mode a profile is, or the one yours started from (what its defaults come from).
    pub fn mode_name(&self, p: Profile) -> &'static str {
        match p {
            Profile::Custom(i) => self
                .customs
                .get(i)
                .map_or("Gaming", |c| Profile::Stock(c.base).name()),
            _ => p.name(),
        }
    }

    /// The limits a profile sets.
    pub fn preset(&self, p: Profile) -> Preset {
        control::preset_for(&self.stock, &self.customs, p)
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        let p = Preset {
            pl1: 0,
            pl2: 0,
            pl4: 0,
            ctgp: 0,
        };
        let gaming = control::default_fan_set(OperatingMode::Gaming);
        Self {
            model: String::new(),
            backend: String::new(),
            ec_error: None,
            temps: vec![],
            fans: vec![],
            profile: None,
            pl: None,
            pl4: None,
            ctgp: None,
            stock: [p; 3],
            customs: vec![],
            boosted: false,
            fan: FanCurves {
                enabled: false,
                cpu: gaming.cpu,
                gpu: gaming.gpu,
            },
            fan_default: gaming,
            auto_boost: None,
            has_lightbar: false,
            lightbar: None,
            has_keyboard: false,
            keyboard: None,
            rainbow: false,
            kb_level: control::KB_BACKLIGHT_MAX,
            hsv: Hsv {
                h: 180,
                s: 100,
                v: 100,
            },
            toggles: vec![],
            battery: None,
            button: false,
            service: false,
            debug: String::new(),
            event: String::new(),
        }
    }
}

pub struct Hw {
    ec: Option<Box<dyn EcAccess>>,
    ec_error: Option<String>,
    sysfs: Option<SysfsEc>,
    stock: [Preset; 3],
    model: String,
    /// Did the auto-boost policy (not the user) switch boost on?
    ours: bool,
    pub button: bool,
    pub service: bool,
    /// Last hardware-button result, reported in the status bar.
    pub event: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    Auto,
    Custom,
    Boost,
}

impl Hw {
    pub fn open() -> Self {
        let (mut ec, ec_error) = match linux::open_auto() {
            Ok(ec) => (Some(ec), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let model = linux::detect_model(ec.as_deref_mut()).to_string();
        let stock = control::stock(ec.as_deref_mut());
        let cfg = Config::load();
        let sysfs = SysfsEc::probe().ok();
        // find out which profile the EC is running (right after boot only the lights know), write its
        // limits and put the button's lights on it
        if let Some(ec) = ec.as_deref_mut() {
            let _ = control::adopt(ec, sysfs.as_ref(), &stock, &cfg.profiles);
        }
        Self {
            ec,
            ec_error,
            sysfs,
            stock,
            model,
            ours: false,
            button: false,
            service: false,
            event: String::new(),
        }
    }

    fn ec(&mut self) -> Result<&mut dyn EcAccess, String> {
        match self.ec.as_deref_mut() {
            Some(ec) => Ok(ec),
            None => Err("no EC backend (see `rcc doctor`)".into()),
        }
    }

    pub fn snapshot(&mut self) -> Snapshot {
        // the CLI and the tray edit the config too: always start from what is saved
        let cfg = Config::load();
        let mut s = Snapshot {
            model: self.model.clone(),
            backend: self
                .ec
                .as_ref()
                .map_or("none".into(), |e| e.name().to_string()),
            ec_error: self.ec_error.clone(),
            stock: self.stock,
            customs: cfg.profiles.clone(),
            auto_boost: cfg.auto_boost_above,
            button: self.button,
            service: self.service,
            event: self.event.clone(),
            ..Default::default()
        };
        let (stock, customs) = (self.stock, cfg.profiles.clone());
        let mut manual = None; // what the fans are told to run at, while the EC follows the duty registers
        if let Some(ec) = self.ec.as_deref_mut() {
            s.profile = control::running(ec, &stock, &customs).ok().flatten();
            s.pl = ec
                .read(addr::PL1_SETTING)
                .ok()
                .zip(ec.read(addr::PL2_SETTING).ok());
            s.pl4 = ec.read(addr::PL4_SETTING).ok();
            let fan_byte = ec.read(addr::MAFAN_CONTROL_BYTE).ok();
            s.boosted = fan_byte.is_some_and(control::is_boosted) && !cfg.fan.enabled;
            manual = fan_byte.and_then(|b| control::manual_duties(ec, b));
            s.has_lightbar = control::has_lightbar(ec);
            s.has_keyboard = control::has_keyboard_rgb(ec);
            if s.has_keyboard {
                s.keyboard = control::keyboard_rgb(ec).ok();
                s.rainbow = control::keyboard_rainbow(ec).unwrap_or(false);
                s.kb_level = control::keyboard_backlight(ec).unwrap_or(control::KB_BACKLIGHT_MAX);
            }
            s.debug = format!(
                "fan control byte {} · PL1/PL2 regs {}/{} · cTGP reg {}",
                fan_byte.map_or("n/a".into(), |b| format!("{b:#04x}")),
                s.pl.map_or("n/a".into(), |p| p.0.to_string()),
                s.pl.map_or("n/a".into(), |p| p.1.to_string()),
                ec.read(1860).map_or("n/a".into(), |v| v.to_string()),
            );
        }
        // each profile has its own fan curves: those of the running one (Gaming's when it is not known)
        let shown = s.profile.unwrap_or(Profile::Stock(OperatingMode::Gaming));
        s.fan = cfg.fan_curves(shown);
        s.fan_default = cfg.fan_default(shown);
        // The chosen colour/brightness while the EC still shows it; else whatever the EC shows now.
        s.hsv = control::keyboard_choice(cfg.keyboard, s.keyboard).unwrap_or(s.hsv);
        if let Some(u) = &self.sysfs {
            s.ctgp = u
                .get_attr("ctgp_offset")
                .ok()
                .and_then(|v| v.trim().parse().ok());
            if let Ok(x) = u.sensors() {
                s.temps = x
                    .temps
                    .iter()
                    .map(|(l, mv)| (l.clone(), f64::from(*mv) / 1000.0))
                    .collect();
                s.fans = x
                    .fans
                    .iter()
                    .enumerate()
                    .map(|(i, (l, rpm))| {
                        // in manual mode (custom profile, boost) hwmon's duty is not the one the fans follow
                        let duty = match manual {
                            Some(told) => told.get(i).copied().flatten().map(u32::from),
                            None => x.pwms.get(i).map(|p| u32::from(*p) * 100 / 255),
                        };
                        (l.clone(), *rpm, duty)
                    })
                    .collect();
            }
            if s.has_lightbar {
                s.lightbar = u.lightbar().ok().map(|(r, g, b, _)| (r, g, b));
            }
            s.battery = u.battery().ok();
            // Only plain 0/1 attributes become toggles (which ones: `control::toggle_shown`).
            s.toggles = control::platform_toggles(u, s.has_lightbar);
        }
        s
    }

    /// Run the auto-boost policy, then snapshot.
    pub fn poll(&mut self) -> Snapshot {
        let snap = self.snapshot();
        let hottest = snap.temps.iter().map(|(_, c)| *c).fold(f64::MIN, f64::max);
        // the custom fan profile owns the fans while it is on
        let policy = if snap.fan.enabled {
            None
        } else {
            snap.auto_boost
        };
        if let Some(on) = control::auto_boost_action(policy, hottest, snap.boosted, self.ours) {
            if let Ok(ec) = self.ec() {
                if control::set_boost(ec, on).is_ok() {
                    self.ours = on;
                }
            }
            return self.snapshot();
        }
        snap
    }

    fn preset(&self, p: Profile) -> Preset {
        control::preset_for(&self.stock, &Config::load().profiles, p)
    }

    /// A profile's name as shown in messages.
    fn label(&self, p: Profile) -> String {
        match p {
            Profile::Custom(i) => Config::load()
                .profiles
                .get(i)
                .map_or_else(|| "custom".to_string(), |c| c.name.clone()),
            _ => p.name().to_lowercase(),
        }
    }

    pub fn set_profile(&mut self, p: Profile) -> Result<String, String> {
        let pr = self.preset(p);
        let sysfs = self.sysfs.take();
        let r = self.ec().and_then(|ec| {
            let pl4 = control::apply(ec, sysfs.as_ref(), pr).map_err(|e| e.to_string())?;
            control::set_mode_byte(ec, p).map_err(|e| e.to_string())?; // mode lights + fan profile
            Ok(pl4)
        });
        self.sysfs = sysfs;
        let pl4 = match r? {
            None => format!("PL4 {} W", pr.pl4),
            Some(_) => "PL4 not applied".to_string(),
        };
        // limits alone cannot tell a profile from the stock one it was copied from
        control::remember_profile(&Config::load().profiles, p);
        Ok(format!(
            "{}: PL1 {} W / PL2 {} W / {pl4} / GPU cTGP +{} W",
            self.label(p),
            pr.pl1,
            pr.pl2,
            pr.ctgp
        ))
    }

    /// One press of the hardware mode button.
    pub fn cycle_profile(&mut self) -> String {
        let (stock, customs) = (self.stock, Config::load().profiles);
        let cur = match self.ec() {
            Ok(ec) => control::running(ec, &stock, &customs).ok().flatten(),
            Err(e) => return format!("mode button: {e}"),
        };
        match self.set_profile(Profile::after_button(cur)) {
            Ok(m) => format!("button → {m}"),
            Err(e) => format!("mode button: {e}"),
        }
    }

    /// The profiles as last saved (the CLI edits them too), and the one at `idx` in them.
    fn profile_at(&mut self, idx: usize) -> Result<CustomProfile, String> {
        Config::load()
            .profiles
            .get(idx)
            .cloned()
            .ok_or_else(|| "that profile is gone".to_string())
    }

    /// Set one value of one of your own profiles (the stock ones are read-only). PL1 <= PL2 <= PL4 is
    /// kept by moving the other limits along. Validated, saved, and re-applied if it is running.
    pub fn set_custom(&mut self, idx: usize, field: Field, value: u8) -> Result<String, String> {
        let mut p = self.profile_at(idx)?.preset;
        match field {
            Field::Pl1 => {
                p.pl1 = value;
                p.pl2 = p.pl2.max(value);
                p.pl4 = p.pl4.max(p.pl2);
            }
            Field::Pl2 => {
                p.pl2 = value;
                p.pl1 = p.pl1.min(value);
                p.pl4 = p.pl4.max(value);
            }
            Field::Pl4 => {
                p.pl4 = value;
                p.pl2 = p.pl2.min(value);
                p.pl1 = p.pl1.min(p.pl2);
            }
            Field::Ctgp => p.ctgp = value,
        }
        self.store_custom(idx, p)
    }

    /// Back to the stock mode the profile started from.
    pub fn reset_custom(&mut self, idx: usize) -> Result<String, String> {
        let base = self.profile_at(idx)?.base;
        self.store_custom(idx, self.stock[base as usize])
    }

    fn store_custom(&mut self, idx: usize, p: Preset) -> Result<String, String> {
        p.validate()?;
        let old = self.profile_at(idx)?.preset;
        if p == old {
            return Ok("profile unchanged".into());
        }
        let was_running = self.is_running(Profile::Custom(idx));
        let name = Config::update(|c| {
            let slot = c
                .profiles
                .get_mut(idx)
                .ok_or_else(|| "that profile is gone".to_string())?;
            slot.preset = p;
            Ok(slot.name.clone())
        })?;
        if was_running {
            return self.set_profile(Profile::Custom(idx));
        }
        Ok(format!(
            "{name}: PL1 {} W / PL2 {} W / PL4 {} W / cTGP +{} W",
            p.pl1, p.pl2, p.pl4, p.ctgp
        ))
    }

    fn is_running(&mut self, p: Profile) -> bool {
        let (stock, customs) = (self.stock, Config::load().profiles);
        self.ec()
            .ok()
            .and_then(|ec| control::running(ec, &stock, &customs).ok().flatten())
            == Some(p)
    }

    /// Make "Profile N" as a copy of what is running now (the stock mode it started from is its base)
    /// and switch to it, so it is the one you edit.
    pub fn new_profile(&mut self) -> Result<String, String> {
        let (stock, customs) = (self.stock, Config::load().profiles);
        let gaming = (OperatingMode::Gaming, stock[OperatingMode::Gaming as usize]);
        let running = self
            .ec()
            .ok()
            .and_then(|ec| control::running(ec, &stock, &customs).ok().flatten());
        let (base, preset) = match running {
            Some(Profile::Stock(m)) => (m, stock[m as usize]),
            Some(Profile::Custom(i)) => customs.get(i).map_or(gaming, |c| (c.base, c.preset)),
            None => gaming,
        };
        let idx = Config::update(|c| {
            let name = control::next_profile_name(&c.profiles);
            c.profiles.push(CustomProfile { name, base, preset });
            Ok(c.profiles.len() - 1)
        })?;
        self.set_profile(Profile::Custom(idx))
    }

    /// Delete a profile; if it was running, go back to the stock mode it started from.
    pub fn delete_profile(&mut self, idx: usize) -> Result<String, String> {
        let was_running = self.is_running(Profile::Custom(idx));
        let gone = Config::update(|c| {
            if idx >= c.profiles.len() {
                return Err("that profile is gone".to_string().into());
            }
            let gone = c.profiles.remove(idx);
            c.fan_sets.remove(&gone.name);
            Ok(gone)
        })?;
        if was_running {
            self.set_profile(Profile::Stock(gone.base))?;
        }
        Ok(format!("deleted {}", gone.name))
    }

    /// Auto (firmware), the custom fan profile (the daemon applies it) or boost.
    pub fn set_fan_mode(&mut self, mode: FanMode) -> Result<String, String> {
        let was_custom = Config::update(|c| {
            let was = c.fan.enabled;
            c.fan.enabled = mode == FanMode::Custom;
            Ok(was)
        })?;
        self.ours = false; // a manual choice always wins over the auto-boost policy
        let ec = self.ec()?;
        if was_custom && mode != FanMode::Custom {
            // the daemon's engine hands the fans back within one tick; let it, or it could undo
            // (or clear) what is set next
            for _ in 0..15 {
                if ec
                    .read(addr::MAFAN_CONTROL_BYTE)
                    .map_err(|e| e.to_string())?
                    & 0x40
                    == 0
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
        // anything still manual (no daemon running) or boosted goes back to the firmware first
        control::fan_release(ec).map_err(|e| e.to_string())?;
        Ok(match mode {
            FanMode::Auto => "fans: firmware auto".into(),
            FanMode::Custom => "fans: custom profile".into(),
            FanMode::Boost => {
                control::set_boost(ec, true).map_err(|e| e.to_string())?;
                "fans: boost".into()
            }
        })
    }

    /// Set one point of a profile's curve (fan 0 = CPU, 1 = GPU); the curve never falls, so the points
    /// around it move with it.
    pub fn set_fan_point(
        &mut self,
        profile: Profile,
        fan: usize,
        idx: usize,
        duty: u8,
    ) -> Result<String, String> {
        Config::update(|cfg| {
            let mut set = cfg.fan_set(profile);
            let c = if fan == 0 { &mut set.cpu } else { &mut set.gpu };
            control::set_curve_point(c, idx, duty);
            set.validate()?;
            cfg.set_fan_set(profile, set);
            Ok(())
        })?;
        Ok(format!(
            "{} fan ({}): {duty} % at {} °C",
            if fan == 0 { "CPU" } else { "GPU" },
            self.label(profile),
            control::FAN_ANCHORS[idx]
        ))
    }

    /// Back to the default curves of the profile (on/off is kept).
    pub fn reset_fan_curves(&mut self, profile: Profile) -> Result<String, String> {
        Config::update(|cfg| {
            let default = cfg.fan_default(profile);
            cfg.set_fan_set(profile, default);
            Ok(())
        })?;
        Ok(format!(
            "fan curves ({}): default preset",
            self.label(profile)
        ))
    }

    pub fn set_auto_boost(&mut self, t: Option<u8>) -> Result<String, String> {
        Config::update(|c| {
            c.auto_boost_above = t;
            Ok(())
        })?;
        Ok(match t {
            Some(t) => format!("auto-boost fans above {t} °C"),
            None => "auto-boost off".into(),
        })
    }

    /// The colour/brightness in effect: the last choice made here while the EC still shows it, else
    /// what the EC shows.
    pub fn hsv(&mut self) -> Hsv {
        let saved = Config::load().keyboard;
        let levels = self.ec().ok().and_then(|ec| control::keyboard_rgb(ec).ok());
        control::keyboard_choice(saved, levels).unwrap_or(Hsv {
            h: 180,
            s: 100,
            v: 100,
        })
    }

    pub fn set_rainbow(&mut self, on: bool) -> Result<String, String> {
        let was = Config::load().keyboard_level;
        let ec = self.ec()?;
        control::keyboard_on(ec, was).map_err(|e| e.to_string())?;
        control::set_keyboard_rainbow(ec, on).map_err(|e| e.to_string())?;
        Ok(if on {
            "keyboard: rainbow"
        } else {
            "keyboard: colour"
        }
        .into())
    }

    /// Backlight off; the level it had is kept to come back to.
    pub fn set_keyboard_off(&mut self) -> Result<String, String> {
        let ec = self.ec()?;
        let level = control::keyboard_off(ec).map_err(|e| e.to_string())?;
        if level > 0 {
            Config::update(|c| {
                c.keyboard_level = Some(level);
                Ok(())
            })?;
        }
        Ok("keyboard: off".into())
    }

    /// Set the keyboard colour and brightness, and remember the exact choice.
    pub fn set_keyboard(&mut self, hsv: Hsv) -> Result<String, String> {
        let was = Config::load().keyboard_level;
        let ec = self.ec()?;
        control::keyboard_on(ec, was).map_err(|e| e.to_string())?; // picking a colour lights it again
        control::set_keyboard_hsv(ec, hsv).map_err(|e| e.to_string())?;
        Config::update(|c| {
            c.keyboard = Some(hsv);
            Ok(())
        })?;
        let l = hsv.to_levels();
        Ok(format!(
            "keyboard: hue {} sat {} % brightness {} % (levels {},{},{})",
            hsv.h, hsv.s, hsv.v, l.0, l.1, l.2
        ))
    }

    /// Choose the battery charge mode (`Standard`, `Long_Life` or `Trickle`, the kernel's names).
    pub fn set_charge_type(&self, kernel: &'static str) -> Result<String, String> {
        let u = self
            .sysfs
            .as_ref()
            .ok_or("uniwill kernel driver not loaded")?;
        u.set_charge_types(kernel).map_err(|e| e.to_string())?;
        let label = linux::CHARGE_PROFILES
            .iter()
            .find(|p| p.kernel == kernel)
            .map_or(kernel, |p| p.label);
        Ok(format!("battery charge mode → {label}"))
    }

    pub fn set_lightbar(&self, rgb: Option<(u8, u8, u8)>) -> Result<String, String> {
        let u = self
            .sysfs
            .as_ref()
            .ok_or("uniwill kernel driver not loaded")?;
        u.set_lightbar(rgb).map_err(|e| e.to_string())?;
        Ok("lightbar updated".into())
    }

    pub fn set_toggle(&self, name: &'static str, on: bool) -> Result<String, String> {
        if name == "touchpad" {
            // not a sysfs attribute: powering the touchpad down (LED on) or up
            control::set_touchpad(on).map_err(|e| e.to_string())?;
            return Ok(format!("touchpad → {}", if on { "on" } else { "off" }));
        }
        let u = self
            .sysfs
            .as_ref()
            .ok_or("uniwill kernel driver not loaded")?;
        u.set_attr(name, if on { "on" } else { "off" })
            .map_err(|e| e.to_string())?;
        Ok(format!("{name} → {}", if on { "on" } else { "off" }))
    }
}
