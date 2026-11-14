//! The saved configuration, `/etc/ripple-nova/config.json`.

use super::*;

/// Saved settings (`/etc/ripple-nova/config.json`). Stock presets are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// The user's own profiles, in the order they were made.
    #[serde(default)]
    pub profiles: Vec<CustomProfile>,
    /// The single custom preset of older versions: it becomes "Profile 1" on load and is not written back.
    #[serde(default, skip_serializing)]
    custom: Option<Preset>,
    /// Keyboard colour/brightness as last chosen (`None` = never set: read it from the EC).
    #[serde(default)]
    pub keyboard: Option<Hsv>,
    /// Software fan policy: boost the fans above this CPU temperature (°C), `None` = off.
    #[serde(default)]
    pub auto_boost_above: Option<u8>,
    /// Is the custom fan profile on (the daemon drives the fans from the curves)? Older versions kept one
    /// pair of curves here; they are per profile now, in `fan_sets`.
    #[serde(default)]
    pub fan: FanSwitch,
    /// The user's fan curves per profile (`profile_key`). A profile with no entry has the default of its mode.
    #[serde(default)]
    pub fan_sets: BTreeMap<String, FanSet>,
    /// Backlight level (1..=4) the keyboard had before "off", to come back to.
    #[serde(default)]
    pub keyboard_level: Option<u8>,
    /// The touchpad is powered (its LED off). Off = the driver is unbound, see `touchpad.rs`.
    #[serde(default = "yes")]
    pub touchpad_on: bool,
    /// NVIDIA GPU clock offsets (only used while the NVIDIA driver is there).
    #[serde(default)]
    pub gpu: crate::gpu::GpuTune,
}

fn yes() -> bool {
    true
}

impl Config {
    pub const PATH: &'static str = "/etc/ripple-nova/config.json";

    /// Saved config (unknown/old fields ignored, an invalid profile dropped), else defaults. For
    /// reading: a damaged file reads as the defaults here, but `update` refuses to overwrite it.
    pub fn load() -> Self {
        Self::load_from(Path::new(Self::PATH))
    }

    pub fn load_from(path: &Path) -> Self {
        Self::read(path).unwrap_or_else(|_| Self::empty())
    }

    fn empty() -> Self {
        Self {
            profiles: Vec::new(),
            custom: None,
            keyboard: None,
            auto_boost_above: None,
            fan: FanSwitch::default(),
            fan_sets: BTreeMap::new(),
            keyboard_level: None,
            touchpad_on: true,
            gpu: crate::gpu::GpuTune::default(),
        }
    }

    /// The file's config; a missing file is the defaults, an unreadable or damaged one is an error.
    fn read(path: &Path) -> HwResult<Self> {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(t) => serde_json::from_str::<Self>(&t)
                .map_err(|e| HwError::Other(format!("{} is damaged: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::empty(),
            Err(e) => return Err(e.into()),
        };
        cfg.settle();
        Ok(cfg)
    }

    /// Bring an old or damaged config in line: the single custom preset of older versions becomes the
    /// first profile (only when it was changed from the default), invalid profiles and curves are dropped.
    pub(super) fn settle(&mut self) {
        if let Some(old) = self.custom.take() {
            if self.profiles.is_empty() && old != default_custom() {
                self.profiles.push(CustomProfile {
                    name: next_profile_name(&[]),
                    base: OperatingMode::Gaming,
                    preset: old,
                });
            }
        }
        self.profiles.retain(|c| c.preset.validate().is_ok());
        self.fan_sets.retain(|_, set| set.validate().is_ok());
        if self.gpu.validate().is_err() {
            self.gpu = crate::gpu::GpuTune::default();
        }
    }

    /// The key a profile's own settings are saved under: the stock mode's name, or the profile's name.
    pub fn profile_key(&self, p: Profile) -> String {
        match p {
            Profile::Stock(m) => m.as_str().to_string(),
            Profile::Custom(i) => self
                .profiles
                .get(i)
                .map_or_else(|| "gaming".to_string(), |c| c.name.clone()),
        }
    }

    /// The default fan curves of a profile: those of its mode (of the mode it started from, for yours).
    pub fn fan_default(&self, p: Profile) -> FanSet {
        let mode = match p {
            Profile::Stock(m) => m,
            Profile::Custom(i) => self
                .profiles
                .get(i)
                .map_or(OperatingMode::Gaming, |c| c.base),
        };
        default_fan_set(mode)
    }

    /// The fan curves of a profile: the user's own for it, else the default.
    pub fn fan_set(&self, p: Profile) -> FanSet {
        self.fan_sets
            .get(&self.profile_key(p))
            .copied()
            .unwrap_or_else(|| self.fan_default(p))
    }

    /// What the engine runs for a profile: its curves, on or off as the switch says.
    pub fn fan_curves(&self, p: Profile) -> FanCurves {
        let set = self.fan_set(p);
        FanCurves {
            enabled: self.fan.enabled,
            cpu: set.cpu,
            gpu: set.gpu,
        }
    }

    /// Keep a profile's curves; the default is not stored, so it stays the default if that ever changes.
    pub fn set_fan_set(&mut self, p: Profile, set: FanSet) {
        let key = self.profile_key(p);
        if set == self.fan_default(p) {
            self.fan_sets.remove(&key);
        } else {
            self.fan_sets.insert(key, set);
        }
    }

    /// Change the saved config: load it fresh, let `f` change what it owns, write it back, all under
    /// one lock. Nothing is written when `f` fails, nor when the file is damaged (it is left for the
    /// user to fix: saving defaults over it would lose every profile and curve).
    pub fn update<R>(f: impl FnOnce(&mut Self) -> HwResult<R>) -> HwResult<R> {
        Self::update_at(Path::new(Self::PATH), f)
    }

    pub fn update_at<R>(path: &Path, f: impl FnOnce(&mut Self) -> HwResult<R>) -> HwResult<R> {
        store::locked(path, || {
            Ok(Self::read(path).and_then(|mut cfg| {
                let out = f(&mut cfg)?;
                let text = serde_json::to_string_pretty(&cfg)
                    .map_err(|e| HwError::Other(e.to_string()))?;
                store::write_atomic(path, text.as_bytes())?;
                Ok(out)
            }))
        })?
    }
}
