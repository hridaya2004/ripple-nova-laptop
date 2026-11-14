//! The performance profiles (Office / Gaming / Turbo and the user's own), their power limits, applying
//! them to the EC and telling which one is running.

use super::*;

/// Hard ceiling for any PL1/PL2 we write: the stock app's own Turbo value for this
/// machine (Smart APC table, 80 W). Above this we refuse, whatever the config says.
pub const PL_CEILING_W: u8 = 80;

/// Same idea for the GPU cTGP offset.
pub const CTGP_CEILING_W: u8 = 25;

/// Ceiling for the peak power limit (PL4): the stock value of all three profiles on this machine.
pub const PL4_CEILING_W: u8 = 145;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub pl1: u8,
    pub pl2: u8,
    /// Peak power (PL4/fPPT) via EC 1925. A preset saved before it existed gets the stock value.
    #[serde(default = "default_pl4")]
    pub pl4: u8,
    /// GPU configurable-TGP offset in watts (kernel `ctgp_offset`).
    pub ctgp: u8,
}

fn default_pl4() -> u8 {
    PL4_CEILING_W
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
        if !(self.pl2..=PL4_CEILING_W).contains(&self.pl4) {
            return Err(format!(
                "PL4 must be PL2..={PL4_CEILING_W} W (got {} with PL2 {})",
                self.pl4, self.pl2
            ));
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

/// A profile the user can pick: one of the three stock ones (read-only, they are the EC's own
/// defaults) or one of their own, by its index in `Config::profiles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Stock(OperatingMode),
    Custom(usize),
}

impl Profile {
    /// The OEM's own mode names (its profile data says Office / Gaming / Turbo; the Windows app only
    /// labels them Balanced / Enthusiast / Overboost, translated per language, in its UI strings).
    pub fn name(self) -> &'static str {
        match self {
            Self::Stock(OperatingMode::Office) => "Office",
            Self::Stock(OperatingMode::Gaming) => "Gaming",
            Self::Stock(OperatingMode::Turbo) => "Turbo",
            Self::Custom(_) => "Custom",
        }
    }

    /// Inverse of `mode_nibble`: the profile the EC's own mode byte announces. `None` for nibbles
    /// with no known meaning (0x4 is fan boost; 0x2 and 0x8 were seen before an app took control).
    pub fn from_mode_byte(byte: u8) -> Option<Self> {
        match byte >> 4 {
            // 0xE, 0x7 and 0x5 are Office and Turbo with the custom fan engine's manual bit on top
            0xA | 0xE => Some(Self::Stock(OperatingMode::Office)),
            0x0 => Some(Self::Stock(OperatingMode::Gaming)),
            // 0x1 is how the Windows app leaves Turbo (EC 0x10, the OEM's "turbo"); we write 0x3
            0x1 | 0x3 | 0x5 | 0x7 => Some(Self::Stock(OperatingMode::Turbo)),
            _ => None,
        }
    }

    /// What one press of the hardware button does: Office → Gaming → Turbo → Office (0 → 1 → 2
    /// lights). Custom shows as Gaming (1 light), so its next press is Turbo; limits that match
    /// nothing restart the cycle at Office.
    pub fn after_button(current: Option<Self>) -> Self {
        match current {
            Some(Self::Stock(m)) => Self::Stock(m.next()),
            Some(Self::Custom(_)) => Self::Stock(OperatingMode::Gaming.next()),
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
            Self::Stock(OperatingMode::Gaming) | Self::Custom(_) => 0x0,
        }
    }
}

pub const STOCK_MODES: [OperatingMode; 3] = [
    OperatingMode::Office,
    OperatingMode::Gaming,
    OperatingMode::Turbo,
];

/// The stock presets, in `STOCK_MODES` order. Not editable and not saved: the EC holds a default
/// PL1/PL2/PL4 slot for each mode, and the Windows app shows exactly these (Office 35/35/145, Gaming
/// 55/55/145, Turbo 65/65/145; none unlocks the GPU cTGP). The OEM names the third slot "BatterySaver",
/// but its values are Windows' Turbo. A slot that cannot be read falls back to the value in the table.
pub fn stock(ec: Option<&mut (dyn EcAccess + '_)>) -> [Preset; 3] {
    // ((PL1, PL2, PL4) default registers, the values they are expected to hold)
    const SLOTS: [([u16; 3], [u8; 3]); 3] = [
        (
            [
                addr::OFFICE_PL1_DEFAULT,
                addr::OFFICE_PL2_DEFAULT,
                addr::OFFICE_PL4_DEFAULT,
            ],
            [35, 35, 145],
        ),
        (
            [
                addr::GAMING_PL1_DEFAULT,
                addr::GAMING_PL2_DEFAULT,
                addr::GAMING_PL4_DEFAULT,
            ],
            [55, 55, 145],
        ),
        (
            [
                addr::BATTERYSAVER_PL1_DEFAULT,
                addr::BATTERYSAVER_PL2_DEFAULT,
                addr::BATTERYSAVER_PL4_DEFAULT,
            ],
            [65, 65, 145],
        ),
    ];
    let mut ec = ec;
    SLOTS.map(|(regs, dflt)| {
        let mut v = dflt;
        if let Some(ec) = ec.as_deref_mut() {
            for (v, reg) in v.iter_mut().zip(regs) {
                if let Some(read) = ec.read(reg).ok().filter(|r| *r >= 5) {
                    *v = read;
                }
            }
        }
        Preset {
            pl1: v[0],
            pl2: v[1],
            pl4: v[2],
            ctgp: 0,
        }
    })
}

/// The custom performance profile preset as shipped (what "reset" goes back to).
pub fn default_custom() -> Preset {
    // between Office and Gaming, so the first Custom is a gentle step, not a copy of a stock one
    Preset {
        pl1: 45,
        pl2: 45,
        pl4: PL4_CEILING_W,
        ctgp: 0,
    }
}

/// A profile the user made: its name, the stock profile it started from (what "reset" goes back to)
/// and its own limits. The stock profiles themselves are never editable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomProfile {
    pub name: String,
    pub base: OperatingMode,
    pub preset: Preset,
}

/// "Profile 1", "Profile 2", …: the lowest number no profile has taken yet.
pub fn next_profile_name(profiles: &[CustomProfile]) -> String {
    (1..)
        .map(|n| format!("Profile {n}"))
        .find(|name| profiles.iter().all(|p| p.name != *name))
        .unwrap_or_default()
}

/// Apply a preset: PL1/PL2/PL4 via EC (read back to verify), cTGP via the kernel attr. PL4 is best
/// effort: `Ok(Some(why))` means the EC refused it and everything else was applied.
pub fn apply(
    ec: &mut dyn EcAccess,
    sysfs: Option<&SysfsEc>,
    p: Preset,
) -> HwResult<Option<String>> {
    p.validate().map_err(HwError::Other)?;
    // Keep PL1 <= PL2 <= PL4 at every step: what goes up is written from the top down (PL4, PL2,
    // PL1), what goes down from the bottom up.
    let (mut up, mut down) = (Vec::new(), Vec::new());
    for (reg, want) in [
        (addr::PL1_SETTING, p.pl1),
        (addr::PL2_SETTING, p.pl2),
        (addr::PL4_SETTING, p.pl4),
    ] {
        // an EC that refuses PL4 must not stop the limits that matter, nor the daemon at boot
        let now = if reg == addr::PL4_SETTING {
            ec.read(reg).unwrap_or(0)
        } else {
            ec.read(reg)?
        };
        if want >= now {
            up.push((reg, want));
        } else {
            down.push((reg, want));
        }
    }
    let mut pl4_refused = None;
    for (reg, want) in up.into_iter().rev().chain(down) {
        match ec.write_verified(reg, want) {
            Err(e) if reg == addr::PL4_SETTING => pl4_refused = Some(e.to_string()),
            r => r?,
        }
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
    Ok(pl4_refused)
}

/// The custom profile applied last, by name. Limits alone cannot tell a profile from the stock one it
/// was copied from, so this settles which of the two is running. In tmpfs: after a reboot the EC's
/// limits are unset anyway and the lights decide.
const HINT_PATH: &str = "/run/ripple-nova-profile-hint";

/// Note what was just applied: a custom profile's name, or nothing for a stock one.
pub fn remember_profile(customs: &[CustomProfile], p: Profile) {
    match p {
        Profile::Custom(i) if i < customs.len() => {
            let _ = std::fs::write(HINT_PATH, &customs[i].name);
        }
        _ => {
            let _ = std::fs::remove_file(HINT_PATH);
        }
    }
}

/// Index of the custom profile applied last, if it still exists.
pub fn remembered_profile(customs: &[CustomProfile]) -> Option<usize> {
    let name = std::fs::read_to_string(HINT_PATH).ok()?;
    customs.iter().position(|c| c.name == name.trim())
}

/// `current`, with the profile applied last as the tie-break: what is running now.
pub fn running(
    ec: &mut dyn EcAccess,
    stock: &[Preset; 3],
    customs: &[CustomProfile],
) -> HwResult<Option<Profile>> {
    current(ec, stock, customs, remembered_profile(customs))
}

/// Which profile the EC is running, judged by its PL1/PL2 setting registers. `applied_last` (a custom
/// profile) wins when its limits still match, even over a stock profile with the same limits;
/// otherwise stock profiles win over custom ones. Both registers read 0 until something applies
/// limits (fresh boot, or the EC still in the state Windows left it): then the EC runs its own
/// profile and its mode byte (the button's lights) says which, so the profile is read from there.
pub fn current(
    ec: &mut dyn EcAccess,
    stock: &[Preset; 3],
    customs: &[CustomProfile],
    applied_last: Option<usize>,
) -> HwResult<Option<Profile>> {
    let now = (ec.read(addr::PL1_SETTING)?, ec.read(addr::PL2_SETTING)?);
    let last = applied_last.filter(|i| {
        customs
            .get(*i)
            .is_some_and(|c| (c.preset.pl1, c.preset.pl2) == now)
    });
    if let Some(i) = last {
        return Ok(Some(Profile::Custom(i)));
    }
    let mut by_limits = STOCK_MODES
        .into_iter()
        .zip(stock)
        .find(|(_, p)| (p.pl1, p.pl2) == now)
        .map(|(m, _)| Profile::Stock(m));
    if by_limits.is_none() {
        by_limits = custom_by_limits(ec, customs, now)?.map(Profile::Custom);
    }
    if by_limits.is_some() || now != (0, 0) {
        return Ok(by_limits);
    }
    Ok(Profile::from_mode_byte(ec.read(addr::MAFAN_CONTROL_BYTE)?))
}

/// The custom profile with these limits; when several share them, the one whose PL4 the EC reports.
fn custom_by_limits(
    ec: &mut dyn EcAccess,
    customs: &[CustomProfile],
    now: (u8, u8),
) -> HwResult<Option<usize>> {
    let hits: Vec<usize> = customs
        .iter()
        .enumerate()
        .filter(|(_, c)| (c.preset.pl1, c.preset.pl2) == now)
        .map(|(i, _)| i)
        .collect();
    if hits.len() < 2 {
        return Ok(hits.first().copied());
    }
    let pl4 = ec.read(addr::PL4_SETTING)?;
    Ok(hits
        .iter()
        .copied()
        .find(|i| customs[*i].preset.pl4 == pl4)
        .or_else(|| hits.first().copied()))
}

pub fn preset_for(stock: &[Preset; 3], customs: &[CustomProfile], p: Profile) -> Preset {
    match p {
        Profile::Stock(m) => stock[m as usize],
        Profile::Custom(i) => customs.get(i).map_or_else(default_custom, |c| c.preset),
    }
}

/// Start-up sync (daemon and GUI): find the profile the EC is running, write its limits when none
/// are set yet (so the EC enforces what the UI shows, as the stock Windows service does on boot),
/// and put the button's lights on it.
pub fn adopt(
    ec: &mut dyn EcAccess,
    sysfs: Option<&SysfsEc>,
    stock: &[Preset; 3],
    customs: &[CustomProfile],
) -> HwResult<Option<Profile>> {
    let Some(p) = running(ec, stock, customs)? else {
        return Ok(None);
    };
    if (ec.read(addr::PL1_SETTING)?, ec.read(addr::PL2_SETTING)?) == (0, 0) {
        if let Some(why) = apply(ec, sysfs, preset_for(stock, customs, p))? {
            eprintln!("PL4 not applied: {why}");
        }
    }
    // what the EC's own byte already announces (Windows leaves Turbo as 0x10) is not rewritten
    if Profile::from_mode_byte(ec.read(addr::MAFAN_CONTROL_BYTE)?) != Some(p) {
        set_mode_byte(ec, p)?;
    }
    Ok(Some(p))
}
