//! EC register offsets (decimal), extracted from the OEM `GCUService.exe`
//! (`Define.ECSpec`, `Define.RamFan*_ECSpec` consts).

/// Core EC registers (from `Define.ECSpec`).
pub mod addr {
    // --- fans ------------------------------------------------------------
    /// Fan control byte (write mode flags, see [`crate::modes::FanControlByte`]).
    pub const MAFAN_CONTROL_BYTE: u16 = 1873;
    /// Left / main fan duty (0–200 = 0–100 %).
    pub const MAIN_FAN_L_DUTY: u16 = 1883;
    /// Right fan duty (0–200 = 0–100 %).
    pub const MAIN_FAN_R_DUTY: u16 = 1884;
    /// Main fan tach byte 1.
    pub const MAIN_FAN_RPM1: u16 = 1124;
    /// Main fan tach byte 2.
    pub const MAIN_FAN_RPM2: u16 = 1125;
    /// Second fan tach byte 1 (**note: byte order swapped vs main**).
    pub const SECOND_FAN_RPM1: u16 = 1132;
    /// Second fan tach byte 2.
    pub const SECOND_FAN_RPM2: u16 = 1131;
    /// Fan alarm flags.
    pub const FAN_ALERT: u16 = 1857;
    /// Tau (fan ramp time).
    pub const MYFAN3_CPU_TAU: u16 = 1848;
    /// Fan floor: min speed.
    pub const FAN_MIN_SPEED: u16 = 1950;
    /// Fan floor: min temp.
    pub const FAN_MIN_TEMP: u16 = 1951;
    /// Fan floor: extra speed.
    pub const FAN_EXTRA_SPEED: u16 = 1952;
    /// Active fan profile slot.
    pub const FAN_MODE_INDEX: u16 = 1963;
    /// Fan profile slots 1..3.
    pub const FAN_PROFILE1: u16 = 1968;
    pub const FAN_PROFILE2: u16 = 1969;
    pub const FAN_PROFILE3: u16 = 1970;
    /// Liquid-cooling fan / pump levels (Oasis).
    pub const LC_FAN_LEVEL: u16 = 2022;
    pub const LC_PUMP_LEVEL: u16 = 2023;

    // --- power / performance --------------------------------------------
    pub const GAMING_PL1_DEFAULT: u16 = 1840;
    pub const GAMING_PL2_DEFAULT: u16 = 1841;
    pub const GAMING_PL4_DEFAULT: u16 = 1842;
    pub const GAMING_D_DEFAULT: u16 = 1843;
    pub const OFFICE_PL1_DEFAULT: u16 = 1844;
    pub const OFFICE_PL2_DEFAULT: u16 = 1845;
    pub const OFFICE_PL4_DEFAULT: u16 = 1846;
    pub const OFFICE_D_DEFAULT: u16 = 1847;
    pub const BATTERYSAVER_PL1_DEFAULT: u16 = 1959;
    pub const BATTERYSAVER_PL2_DEFAULT: u16 = 1960;
    pub const BATTERYSAVER_PL4_DEFAULT: u16 = 1961;
    pub const BATTERYSAVER_D_DEFAULT: u16 = 1962;
    /// Currently applied PL settings.
    pub const PL1_SETTING: u16 = 1923;
    pub const PL2_SETTING: u16 = 1924;
    pub const PL4_SETTING: u16 = 1925;
    pub const GAMING_TCC_OFFSET_DEFAULT: u16 = 2008;
    pub const OFFICE_TCC_OFFSET_DEFAULT: u16 = 2009;
    pub const TURBO_TCC_OFFSET_DEFAULT: u16 = 2010;
    pub const COMPLEX_POWER_STATUS: u16 = 1996;
    pub const CONFIGURABLE_TGP_CTRL: u16 = 1859;
    pub const CONFIGURABLE_TGP_VALUE: u16 = 1860;
    pub const DYNAMIC_BOOST_CTRL: u16 = 1861;
    pub const DYNAMIC_BOOST_VALUE: u16 = 1862;
    pub const CPU_VRM_CURRENT_LIMIT: u16 = 1875;
    pub const CPU_VRM_MAXI_CURRENT_LIMIT: u16 = 1876;

    // --- battery ---------------------------------------------------------
    /// Charge limit (upper threshold, %).
    pub const BATTERY_CHARGE_LIMIT_UP: u16 = 1977;
    /// Charge limit (lower threshold, %).
    pub const BATTERY_CHARGE_LIMIT_DOWN: u16 = 2000;
    pub const BATTERY_ALERT: u16 = 1172;
    pub const EC_BST_BPR1: u16 = 1076;
    pub const EC_BST_BPR2: u16 = 1077;
    pub const EC_BST_BRC1: u16 = 1078;
    pub const EC_BST_BRC2: u16 = 1079;
    pub const EC_BST_BPV1: u16 = 1080;
    pub const EC_BST_BPV2: u16 = 1081;
    pub const BT1_CYCLE_COUNT1: u16 = 1190;
    pub const BT1_CYCLE_COUNT2: u16 = 1191;

    // --- RGB / lightbar --------------------------------------------------
    pub const LIGHTBAR_CONTROL: u16 = 1864;
    pub const REDBAR_CONTROL: u16 = 1865;
    pub const GREENBAR_CONTROL: u16 = 1866;
    pub const BLUEBAR_CONTROL: u16 = 1867;
    pub const RGBKB_LEVEL_R: u16 = 1897;
    pub const RGBKB_LEVEL_G: u16 = 1898;
    pub const RGBKB_LEVEL_B: u16 = 1899;
    pub const RGBKB_LEVEL_DEFAULT_R: u16 = 1900;
    pub const RGBKB_LEVEL_DEFAULT_G: u16 = 1901;
    pub const RGBKB_LEVEL_DEFAULT_B: u16 = 1902;
    pub const RGBKB_MUSIC_NO: u16 = 1903;

    // --- identity / capability ------------------------------------------
    pub const PROJECT_ID: u16 = 1856;
    pub const AP_OEM: u16 = 1857;
    pub const SUPPORT_BYTE1: u16 = 1893;
    pub const SUPPORT_BYTE2: u16 = 1894;
    pub const SUPPORT_BYTE5: u16 = 1858;
    pub const TRIGGER_BYTE: u16 = 1895;
    pub const TRIGGER_BYTE2: u16 = 1885;
    pub const STATUS_BYTE: u16 = 1896;
    pub const SYSTEM_ID: u16 = 1110;
    pub const ROMID: u16 = 1905;
    pub const ROMID2: u16 = 1906;
    pub const MODULE_ID: u16 = 2003;
    pub const BIOS_OEM: u16 = 1870;
    pub const BIOS_INFO_3: u16 = 1183;
    pub const SILENTMODE_STATUS: u16 = 1115;
}

/// Fan-curve table layouts in EC RAM. Three variants exist; which one the
/// machine uses is detected at runtime from the support/project bytes.
///
/// Each row is 16 entries × 1 byte; `255` = unused/inherit.
pub mod fan_table {
    /// RamFan1 / RamFan1.5 (single bank) — base offsets.
    pub const CPU_TEMP_UP: u16 = 3840;
    pub const CPU_TEMP_DOWN: u16 = 3856;
    pub const CPU_DUTY: u16 = 3872;
    pub const GPU_TEMP_UP: u16 = 3888;
    pub const GPU_TEMP_DOWN: u16 = 3904;
    pub const GPU_DUTY: u16 = 3920;
    /// RamFan1.5 extras.
    pub const P5_TABLE_STATUS1: u16 = 3933;
    pub const P5_TABLE_STATUS2: u16 = 3934;
    pub const P5_TABLE_CTRL: u16 = 3935;
    /// RamFan2 — three banks F1/F2/F3 (stride 80).
    pub const F1_CPU_TEMP_UP: u16 = 3840;
    pub const F1_CPU_TEMP_DOWN: u16 = 3856;
    pub const F1_DUTY: u16 = 3872;
    pub const F1_GPU_TEMP_UP: u16 = 3888;
    pub const F1_GPU_TEMP_DOWN: u16 = 3904;
    pub const F2_CPU_TEMP_UP: u16 = 3920;
    pub const F2_CPU_TEMP_DOWN: u16 = 3936;
    pub const F2_DUTY: u16 = 3952;
    pub const F2_GPU_TEMP_UP: u16 = 3968;
    pub const F2_GPU_TEMP_DOWN: u16 = 3984;
    pub const F3_CPU_TEMP_UP: u16 = 4000;
    pub const F3_CPU_TEMP_DOWN: u16 = 4016;
    pub const F3_DUTY: u16 = 4032;
    pub const F3_GPU_TEMP_UP: u16 = 4048;
    pub const F3_GPU_TEMP_DOWN: u16 = 4064;

    /// Number of entries per table row.
    pub const ROW_LEN: usize = 16;
}

/// Convert a percent duty (0–100) to the EC's 0–200 scale.
pub fn duty_to_ec(percent: u8) -> u8 {
    percent.min(100).saturating_mul(2)
}

/// Convert the EC's 0–200 duty scale back to percent.
pub fn duty_from_ec(raw: u8) -> u8 {
    raw.min(200) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duty_scale_roundtrip() {
        assert_eq!(duty_to_ec(0), 0);
        assert_eq!(duty_to_ec(30), 60);
        assert_eq!(duty_to_ec(70), 140);
        assert_eq!(duty_to_ec(100), 200);
        assert_eq!(duty_from_ec(60), 30);
        assert_eq!(duty_from_ec(140), 70);
    }

    #[test]
    fn register_offsets_match_oem_consts() {
        assert_eq!(addr::MAFAN_CONTROL_BYTE, 1873);
        assert_eq!(addr::PL1_SETTING, 1923);
        assert_eq!(addr::BATTERY_CHARGE_LIMIT_UP, 1977);
        assert_eq!(fan_table::CPU_TEMP_UP, 3840);
    }
}
