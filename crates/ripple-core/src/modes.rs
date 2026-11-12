//! Domain enums for performance modes, fan modes, battery protection.

use serde::{Deserialize, Serialize};

/// The big mode switch (OEM `OperatingMode`). The OEM enum also has Benchmark=3, but
/// this hardware has no such mode (3 button lights: 0=Office, 1=Gaming, 2=Turbo).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingMode {
    Office = 0,
    Gaming = 1,
    Turbo = 2,
}

impl OperatingMode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Office),
            1 => Some(Self::Gaming),
            2 => Some(Self::Turbo),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Office => "office",
            Self::Gaming => "gaming",
            Self::Turbo => "turbo",
        }
    }

    /// What the hardware mode button does: Office → Gaming → Turbo → Office.
    pub fn next(self) -> Self {
        match self {
            Self::Office => Self::Gaming,
            Self::Gaming => Self::Turbo,
            Self::Turbo => Self::Office,
        }
    }
}

impl std::str::FromStr for OperatingMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "office" | "0" => Ok(Self::Office),
            "gaming" | "1" => Ok(Self::Gaming),
            "turbo" | "2" => Ok(Self::Turbo),
            other => Err(format!("unknown mode '{other}'")),
        }
    }
}

/// Fan preset (OEM `Fan` enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanPreset {
    Performance = 1,
    Standard = 2,
    Quiet = 3,
    Benchmark = 4,
}

/// Battery protection profile (OEM `Protection_Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionStatus {
    PerformanceMode = 0,
    BalancedMode = 1,
    HealthyMode = 2,
}

impl ProtectionStatus {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::PerformanceMode),
            1 => Some(Self::BalancedMode),
            2 => Some(Self::HealthyMode),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PerformanceMode => "performance",
            Self::BalancedMode => "balanced",
            Self::HealthyMode => "healthy",
        }
    }
}

impl std::str::FromStr for ProtectionStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "performance" | "0" => Ok(Self::PerformanceMode),
            "balanced" | "1" => Ok(Self::BalancedMode),
            "healthy" | "2" => Ok(Self::HealthyMode),
            other => Err(format!("unknown profile '{other}'")),
        }
    }
}

/// Values written to `ADDR_MAFAN_CONTROL_BYTE` (OEM `MyFanCTLByteFlag`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanControlByte {
    Normal,
    Turbo,
    FanBoost,
    User,
    /// User levels 1..5 → 0x81..0x85
    UserLevel(u8),
    UserHiMode,
}

impl FanControlByte {
    pub fn to_byte(self) -> u8 {
        match self {
            Self::Normal => 0x00,
            Self::Turbo => 0x10,
            Self::FanBoost => 0x40,
            Self::User => 0x80,
            Self::UserLevel(n) => 0x80 | n.min(5),
            Self::UserHiMode => 0xA0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_roundtrip() {
        for m in [
            OperatingMode::Office,
            OperatingMode::Gaming,
            OperatingMode::Turbo,
        ] {
            let s = m.as_str();
            assert_eq!(s.parse::<OperatingMode>().unwrap(), m);
        }
        assert_eq!(OperatingMode::from_u8(1), Some(OperatingMode::Gaming));
        assert_eq!(OperatingMode::from_u8(3), None, "no Benchmark mode on this hardware");
        assert_eq!(OperatingMode::from_u8(9), None);
    }

    #[test]
    fn fan_control_bytes() {
        assert_eq!(FanControlByte::Normal.to_byte(), 0x00);
        assert_eq!(FanControlByte::Turbo.to_byte(), 0x10);
        assert_eq!(FanControlByte::FanBoost.to_byte(), 0x40);
        assert_eq!(FanControlByte::User.to_byte(), 0x80);
        assert_eq!(FanControlByte::UserLevel(3).to_byte(), 0x83);
        assert_eq!(FanControlByte::UserHiMode.to_byte(), 0xA0);
    }
}
