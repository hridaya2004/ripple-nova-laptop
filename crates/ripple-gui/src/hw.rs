//! Blocking hardware layer for the GUI. Everything here may sleep (6 ms EC pacing, sysfs),
//! so the UI only ever calls it from a background task, behind one mutex. The logic itself
//! lives in `ripple_hw::control`, shared with the CLI.

use ripple_core::modes::OperatingMode;
use ripple_core::regs::addr;
use ripple_hw::control::{self, Config, FanCurves, Hsv, Preset, Profile};
use ripple_hw::linux::{self, SysfsEc};
use ripple_hw::EcAccess;

/// Dropdown order: the three stock profiles, then the user's own.
pub const PROFILES: [Profile; 4] = [
    Profile::Stock(OperatingMode::Office),
    Profile::Stock(OperatingMode::Gaming),
    Profile::Stock(OperatingMode::Turbo),
    Profile::Custom,
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Pl1,
    Pl2,
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
    pub ctgp: Option<u8>,
    /// Stock presets (read-only, Office/Gaming/Turbo order) and the user's Custom one.
    pub stock: [Preset; 3],
    pub custom: Preset,
    /// Fan boost is on (not the custom profile, which shares the EC bit).
    pub boosted: bool,
    /// The custom fan profile: curves and whether the daemon applies them.
    pub fan: FanCurves,
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
    /// Is the hardware mode button being followed, and by the systemd service rather than by us?
    pub button: bool,
    pub service: bool,
    /// Raw EC values, shown in debug builds only.
    pub debug: String,
    /// Last action / hardware-button result for the status bar.
    pub event: String,
}

impl Default for Snapshot {
    fn default() -> Self {
        let p = Preset { pl1: 0, pl2: 0, ctgp: 0 };
        Self {
            model: String::new(),
            backend: String::new(),
            ec_error: None,
            temps: vec![],
            fans: vec![],
            profile: None,
            pl: None,
            ctgp: None,
            stock: [p; 3],
            custom: p,
            boosted: false,
            fan: FanCurves::default(),
            auto_boost: None,
            has_lightbar: false,
            lightbar: None,
            has_keyboard: false,
            keyboard: None,
            rainbow: false,
            kb_level: control::KB_BACKLIGHT_MAX,
            hsv: Hsv { h: 180, s: 100, v: 100 },
            toggles: vec![],
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
    cfg: Config,
    model: String,
    /// Did the auto-boost policy (not the user) switch boost on?
    ours: bool,
    pub button: bool,
    pub service: bool,
    /// Last hardware-button result, reported in the status bar.
    pub event: String,
}

/// How a platform toggle is named in the window and in popups.
pub fn toggle_label(name: &str) -> &str {
    match name {
        "fn_lock" => "Fn lock",
        "super_key_enable" => "Super key",
        "rainbow_animation" => "Lightbar rainbow",
        "breathing_in_suspend" => "Lightbar breathing",
        "ac_auto_boost" => "AC Auto-Boot",
        "usb_powershare_high" => "USB powershare",
        other => other,
    }
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
            let _ = control::adopt(ec, sysfs.as_ref(), &stock, &cfg.custom);
        }
        Self {
            ec,
            ec_error,
            sysfs,
            stock,
            cfg,
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
        let mut s = Snapshot {
            model: self.model.clone(),
            backend: self.ec.as_ref().map_or("none".into(), |e| e.name().to_string()),
            ec_error: self.ec_error.clone(),
            stock: self.stock,
            custom: self.cfg.custom,
            auto_boost: self.cfg.auto_boost_above,
            button: self.button,
            service: self.service,
            event: self.event.clone(),
            ..Default::default()
        };
        let (stock, custom) = (self.stock, self.cfg.custom);
        // the CLI edits the fan profile too: always start from what is saved
        self.cfg.fan = Config::load().fan;
        s.fan = self.cfg.fan.clone();
        if let Some(ec) = self.ec.as_deref_mut() {
            s.profile = control::current(ec, &stock, &custom).ok().flatten();
            s.pl = ec.read(addr::PL1_SETTING).ok().zip(ec.read(addr::PL2_SETTING).ok());
            let fan_byte = ec.read(addr::MAFAN_CONTROL_BYTE).ok();
            s.boosted = fan_byte.is_some_and(control::is_boosted) && !self.cfg.fan.enabled;
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
        // The chosen colour/brightness while the EC still shows it; else whatever the EC shows now.
        s.hsv = control::keyboard_choice(self.cfg.keyboard, s.keyboard).unwrap_or(s.hsv);
        if let Some(u) = &self.sysfs {
            s.ctgp = u.get_attr("ctgp_offset").ok().and_then(|v| v.trim().parse().ok());
            if let Ok(x) = u.sensors() {
                s.temps = x.temps.iter().map(|(l, mv)| (l.clone(), f64::from(*mv) / 1000.0)).collect();
                s.fans = x
                    .fans
                    .iter()
                    .enumerate()
                    .map(|(i, (l, rpm))| (l.clone(), *rpm, x.pwms.get(i).map(|p| u32::from(*p) * 100 / 255)))
                    .collect();
            }
            if s.has_lightbar {
                s.lightbar = u.lightbar().ok().map(|(r, g, b, _)| (r, g, b));
            }
            // Only plain 0/1 attributes become toggles; `ctgp_offset` belongs to the profiles, and
            // the lightbar animations are meaningless without a lightbar. `touchpad_toggle_enable`
            // had no visible effect here (removed on request until it is understood).
            s.toggles = u
                .platform_attrs()
                .into_iter()
                .filter(|(n, _)| !matches!(*n, "ctgp_offset" | "touchpad_toggle_enable"))
                .filter(|(n, _)| s.has_lightbar || !matches!(*n, "rainbow_animation" | "breathing_in_suspend"))
                .filter_map(|(n, v)| match v.as_deref() {
                    Some("1") => Some((n, true)),
                    Some("0") => Some((n, false)),
                    _ => None,
                })
                .collect();
        }
        s
    }

    /// Run the auto-boost policy, then snapshot.
    pub fn poll(&mut self) -> Snapshot {
        let snap = self.snapshot();
        let hottest = snap.temps.iter().map(|(_, c)| *c).fold(f64::MIN, f64::max);
        // the custom fan profile owns the fans while it is on
        let policy = if snap.fan.enabled { None } else { snap.auto_boost };
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
        match p {
            Profile::Stock(m) => self.stock[m as usize],
            Profile::Custom => self.cfg.custom,
        }
    }

    pub fn set_profile(&mut self, p: Profile) -> Result<String, String> {
        let pr = self.preset(p);
        let sysfs = self.sysfs.take();
        let r = self.ec().and_then(|ec| {
            control::apply(ec, sysfs.as_ref(), pr).map_err(|e| e.to_string())?;
            control::set_mode_byte(ec, p).map_err(|e| e.to_string()) // mode lights + fan profile
        });
        self.sysfs = sysfs;
        r?;
        Ok(format!("{}: PL1 {} W / PL2 {} W / GPU cTGP +{} W", p.name().to_lowercase(), pr.pl1, pr.pl2, pr.ctgp))
    }

    /// One press of the hardware mode button.
    pub fn cycle_profile(&mut self) -> String {
        let (stock, custom) = (self.stock, self.cfg.custom);
        let cur = match self.ec() {
            Ok(ec) => control::current(ec, &stock, &custom).ok().flatten(),
            Err(e) => return format!("mode button: {e}"),
        };
        match self.set_profile(Profile::after_button(cur)) {
            Ok(m) => format!("button → {m}"),
            Err(e) => format!("mode button: {e}"),
        }
    }

    /// Set one value of the Custom preset (the stock ones are read-only). PL1 <= PL2 is kept by
    /// moving the other limit along. Validated, saved, and re-applied if Custom is active.
    pub fn set_custom(&mut self, field: Field, value: u8) -> Result<String, String> {
        let mut p = self.cfg.custom;
        match field {
            Field::Pl1 => {
                p.pl1 = value;
                p.pl2 = p.pl2.max(value);
            }
            Field::Pl2 => {
                p.pl2 = value;
                p.pl1 = p.pl1.min(value);
            }
            Field::Ctgp => p.ctgp = value,
        }
        self.store_custom(p)
    }

    /// Back to the default custom performance profile preset.
    pub fn reset_custom(&mut self) -> Result<String, String> {
        self.store_custom(control::default_custom())
    }

    fn store_custom(&mut self, p: Preset) -> Result<String, String> {
        p.validate()?;
        if p == self.cfg.custom {
            return Ok("custom preset unchanged".into());
        }
        let (stock, old) = (self.stock, self.cfg.custom);
        let was_active = self.ec().ok().and_then(|ec| control::current(ec, &stock, &old).ok().flatten()) == Some(Profile::Custom);
        self.cfg.custom = p;
        self.cfg.save().map_err(|e| e.to_string())?;
        if was_active {
            return self.set_profile(Profile::Custom);
        }
        Ok(format!("custom preset: PL1 {} W / PL2 {} W / cTGP +{} W", p.pl1, p.pl2, p.ctgp))
    }

    /// Auto (firmware), the custom fan profile (the daemon applies it) or boost.
    pub fn set_fan_mode(&mut self, mode: FanMode) -> Result<String, String> {
        self.cfg.fan = Config::load().fan;
        let was_custom = self.cfg.fan.enabled;
        self.cfg.fan.enabled = mode == FanMode::Custom;
        self.cfg.save().map_err(|e| e.to_string())?;
        self.ours = false; // a manual choice always wins over the auto-boost policy
        let ec = self.ec()?;
        if was_custom && mode != FanMode::Custom {
            // the daemon's engine hands the fans back within one tick; let it, or it could undo
            // (or clear) what is set next
            for _ in 0..15 {
                if ec.read(addr::MAFAN_CONTROL_BYTE).map_err(|e| e.to_string())? & 0x40 == 0 {
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

    /// Set one point of a custom curve (fan 0 = CPU, 1 = GPU); the curve never falls, so the points
    /// around it move with it.
    pub fn set_fan_point(&mut self, fan: usize, idx: usize, duty: u8) -> Result<String, String> {
        self.cfg.fan = Config::load().fan;
        let c = if fan == 0 { &mut self.cfg.fan.cpu } else { &mut self.cfg.fan.gpu };
        control::set_curve_point(c, idx, duty);
        self.cfg.fan.validate()?;
        self.cfg.save().map_err(|e| e.to_string())?;
        Ok(format!("{} fan: {duty} % at {} °C", if fan == 0 { "CPU" } else { "GPU" }, control::FAN_ANCHORS[idx]))
    }

    /// The default preset (on/off is kept).
    pub fn reset_fan_curves(&mut self) -> Result<String, String> {
        let enabled = Config::load().fan.enabled;
        self.cfg.fan = FanCurves { enabled, ..FanCurves::default() };
        self.cfg.save().map_err(|e| e.to_string())?;
        Ok("fan curves: default preset".into())
    }

    pub fn set_auto_boost(&mut self, t: Option<u8>) -> Result<String, String> {
        self.cfg.auto_boost_above = t;
        self.cfg.save().map_err(|e| e.to_string())?;
        Ok(match t {
            Some(t) => format!("auto-boost fans above {t} °C"),
            None => "auto-boost off".into(),
        })
    }

    /// The colour/brightness in effect: the last choice made here while the EC still shows it, else
    /// what the EC shows.
    pub fn hsv(&mut self) -> Hsv {
        let saved = self.cfg.keyboard;
        let levels = self.ec().ok().and_then(|ec| control::keyboard_rgb(ec).ok());
        control::keyboard_choice(saved, levels).unwrap_or(Hsv { h: 180, s: 100, v: 100 })
    }

    pub fn set_rainbow(&mut self, on: bool) -> Result<String, String> {
        let was = self.cfg.keyboard_level;
        let ec = self.ec()?;
        control::keyboard_on(ec, was).map_err(|e| e.to_string())?;
        control::set_keyboard_rainbow(ec, on).map_err(|e| e.to_string())?;
        Ok(if on { "keyboard: rainbow" } else { "keyboard: colour" }.into())
    }

    /// Backlight off; the level it had is kept to come back to.
    pub fn set_keyboard_off(&mut self) -> Result<String, String> {
        let ec = self.ec()?;
        let level = control::keyboard_off(ec).map_err(|e| e.to_string())?;
        if level > 0 {
            self.cfg.keyboard_level = Some(level);
            self.cfg.save().map_err(|e| e.to_string())?;
        }
        Ok("keyboard: off".into())
    }

    /// Set the keyboard colour and brightness, and remember the exact choice.
    pub fn set_keyboard(&mut self, hsv: Hsv) -> Result<String, String> {
        let was = self.cfg.keyboard_level;
        let ec = self.ec()?;
        control::keyboard_on(ec, was).map_err(|e| e.to_string())?; // picking a colour lights it again
        control::set_keyboard_hsv(ec, hsv).map_err(|e| e.to_string())?;
        self.cfg.keyboard = Some(hsv);
        self.cfg.save().map_err(|e| e.to_string())?;
        let l = hsv.to_levels();
        Ok(format!("keyboard: hue {} sat {} % brightness {} % (levels {},{},{})", hsv.h, hsv.s, hsv.v, l.0, l.1, l.2))
    }

    pub fn set_lightbar(&self, rgb: Option<(u8, u8, u8)>) -> Result<String, String> {
        let u = self.sysfs.as_ref().ok_or("uniwill kernel driver not loaded")?;
        u.set_lightbar(rgb).map_err(|e| e.to_string())?;
        Ok("lightbar updated".into())
    }

    pub fn set_toggle(&self, name: &'static str, on: bool) -> Result<String, String> {
        let u = self.sysfs.as_ref().ok_or("uniwill kernel driver not loaded")?;
        u.set_attr(name, if on { "on" } else { "off" }).map_err(|e| e.to_string())?;
        Ok(format!("{name} → {}", if on { "on" } else { "off" }))
    }
}
