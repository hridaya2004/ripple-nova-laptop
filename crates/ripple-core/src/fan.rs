//! Fan curve model + parser for the OEM `UserFanTables/<MODEL>/M{m}T{t}.json`
//! format.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::Result;

/// One curve point (16 per CPU/GPU table in the OEM files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct CurvePoint {
    pub ID: u8,
    /// Up-threshold (°C) — fan ramps up when temp crosses this.
    #[serde(rename = "UpT")]
    pub up_t: u8,
    /// Down-threshold (°C) — hysteresis; fan ramps down below this.
    #[serde(rename = "DownT")]
    pub down_t: u8,
    /// Duty percent (0–100). EC raw = percent × 2.
    #[serde(rename = "Duty")]
    pub duty: u8,
}

/// A complete OEM fan table file (`M1T1.json`, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct FanTable {
    #[serde(default = "yes")]
    pub Activated: bool,
    #[serde(default)]
    pub Name: String,

    // Power limits — strings in the OEM format ("NA" = disabled).
    #[serde(default)]
    pub PL1: Option<String>,
    #[serde(default)]
    pub PL2: Option<String>,
    #[serde(default)]
    pub PL1_dc: Option<String>,
    #[serde(default)]
    pub PL2_dc: Option<String>,

    /// TCC offset in °C (string, may be negative).
    #[serde(default)]
    pub TCC: Option<String>,
    /// Dynamic Boost watts (string, or "NA").
    #[serde(default)]
    pub DB: Option<String>,
    /// WhisperMode level (string "0"/"1"/"2", or "NA").
    #[serde(default)]
    pub WM: Option<String>,

    #[serde(default)]
    pub CpuTemp_DefaultMaxLevel: Option<u8>,
    #[serde(default)]
    pub GpuTemp_DefaultMaxLevel: Option<u8>,

    #[serde(default)]
    pub CPU: Vec<CurvePoint>,
    #[serde(default)]
    pub GPU: Vec<CurvePoint>,

    /// Per-OEM override block (`*_CustomizeValues`); ignored structurally.
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn yes() -> bool {
    true
}

impl FanTable {
    /// Load from a `M{m}T{t}.json` file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Parse a power-limit field: `"20"` → `20.0`, `"NA"`/missing → `None`.
    pub fn pl_watts(s: Option<&String>) -> Option<f64> {
        s.as_ref()
            .filter(|v| !v.eq_ignore_ascii_case("NA") && !v.is_empty())
            .and_then(|v| v.parse().ok())
    }

    pub fn pl1(&self) -> Option<f64> {
        Self::pl_watts(self.PL1.as_ref())
    }

    pub fn pl2(&self) -> Option<f64> {
        Self::pl_watts(self.PL2.as_ref())
    }
}

/// List the fan-table profiles shipped for a model directory.
pub fn list_profiles(dir: impl AsRef<Path>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".json") {
            out.push(name.trim_end_matches(".json").to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// Detect the model directory name from a DMI product name, e.g.
/// `"PH4ARXX"` → `"PH4ARxx"` (matches `UserFanTables/` casing).
pub fn model_dir_for(product_name: &str) -> Option<&'static str> {
    const MODELS: &[&str] = &[
        "PH4AQxx", "PH4ARxx", "PH4AUxf", "PH4AUxx", "PH4AXxx", "PH4TQx1", "PH4TRX1", "PH4TUX1",
        "PH6AQxx", "PH6ARxx", "PH6TRX1", "PH6AGxx", "PH4PQxx",
    ];
    let upper = product_name.to_ascii_uppercase();
    MODELS
        .iter()
        .find(|m| upper.contains(&m.to_ascii_uppercase()))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "Activated": true,
      "Name": "M1T1",
      "PL1": "20", "PL2": "60", "PL1_dc": "20", "PL2_dc": "60",
      "TCC": "-5", "DB": "15", "WM": "NA",
      "CpuTemp_DefaultMaxLevel": 8, "GpuTemp_DefaultMaxLevel": 8,
      "CPU": [
        {"ID":0,"UpT":0,"DownT":45,"Duty":0},
        {"ID":1,"UpT":54,"DownT":50,"Duty":25}
      ],
      "GPU": [
        {"ID":0,"UpT":0,"DownT":45,"Duty":0}
      ]
    }"#;

    #[test]
    fn parse_sample() {
        let t = serde_json::from_str::<FanTable>(SAMPLE).unwrap();
        assert_eq!(t.Name, "M1T1");
        assert_eq!(t.pl1(), Some(20.0));
        assert_eq!(t.pl2(), Some(60.0));
        assert_eq!(t.CPU.len(), 2);
        assert_eq!(t.CPU[1].up_t, 54);
        assert_eq!(t.TCC.as_deref(), Some("-5"));
    }

    #[test]
    fn na_power_limits() {
        let mut t = serde_json::from_str::<FanTable>(SAMPLE).unwrap();
        t.PL1 = Some("NA".into());
        assert_eq!(t.pl1(), None);
        t.PL1 = None;
        assert_eq!(t.pl1(), None);
    }

    #[test]
    fn model_detect() {
        assert_eq!(model_dir_for("PH4ARXX"), Some("PH4ARxx"));
        assert_eq!(model_dir_for("XMG Neo 16 PH6AQxx"), Some("PH6AQxx"));
        assert_eq!(model_dir_for("ThinkPad X1"), None);
    }

    #[test]
    fn real_oem_file_if_present() {
        // Runs against the shipped OEM package when it exists in the repo.
        let p = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ControlCenter_4.9.47.13/UserFanTables/PH4AQxx/M1T1.json"
        ));
        if p.exists() {
            let t = FanTable::load(p).unwrap();
            assert_eq!(t.Name, "M1T1");
            assert_eq!(t.CPU.len(), 16);
            assert_eq!(t.GPU.len(), 16);
        }
    }
}
