//! RGB keyboard / lightbar state model + parser for the OEM
//! `RGBKeyboard.reg` (UTF-16 `.reg` export).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::Result;

/// Effect bank (`RGBKB_Mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bank {
    Lighting = 1,
    Welcome = 2,
    Night = 3,
}

/// Effect id (`RGBKB_Effect` — a subset of the OEM list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    Single = 0,
    Breathing = 1,
    Wave = 2,
    Reactive = 3,
    Rainbow = 4,
    Ripple = 5,
    Raindrop = 6,
    Neon = 7,
    Marquee = 8,
    Stack = 9,
    Impact = 10,
    Spark = 11,
    Aurora = 12,
    Music = 13,
    UserMode = 14,
    Gaming = 15,
    Flash = 16,
    Mix = 17,
    RippleO = 18,
    Alphabet = 20,
    StarHitting = 21,
    StarSpark = 22,
    Thinking = 23,
    Manual = 24,
    BatteryPercent = 25,
}

impl Effect {
    pub fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            0 => Self::Single,
            1 => Self::Breathing,
            2 => Self::Wave,
            3 => Self::Reactive,
            4 => Self::Rainbow,
            5 => Self::Ripple,
            6 => Self::Raindrop,
            7 => Self::Neon,
            8 => Self::Marquee,
            9 => Self::Stack,
            10 => Self::Impact,
            11 => Self::Spark,
            12 => Self::Aurora,
            13 => Self::Music,
            14 => Self::UserMode,
            15 => Self::Gaming,
            16 => Self::Flash,
            17 => Self::Mix,
            18 => Self::RippleO,
            20 => Self::Alphabet,
            21 => Self::StarHitting,
            22 => Self::StarSpark,
            23 => Self::Thinking,
            24 => Self::Manual,
            25 => Self::BatteryPercent,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Breathing => "breathing",
            Self::Wave => "wave",
            Self::Reactive => "reactive",
            Self::Rainbow => "rainbow",
            Self::Ripple => "ripple",
            Self::Raindrop => "raindrop",
            Self::Neon => "neon",
            Self::Marquee => "marquee",
            Self::Stack => "stack",
            Self::Impact => "impact",
            Self::Spark => "spark",
            Self::Aurora => "aurora",
            Self::Music => "music",
            Self::UserMode => "user",
            Self::Gaming => "gaming",
            Self::Flash => "flash",
            Self::Mix => "mix",
            Self::RippleO => "rippleo",
            Self::Alphabet => "alphabet",
            Self::StarHitting => "star_hitting",
            Self::StarSpark => "star_spark",
            Self::Thinking => "thinking",
            Self::Manual => "manual",
            Self::BatteryPercent => "battery_percent",
        }
    }
}

/// One saved effect (the JSON stored under `<idx>_LastEffect`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)] // field names mirror the OEM JSON exactly
pub struct SavedEffect {
    #[serde(default)]
    pub save_mode: u8,
    #[serde(default)]
    pub bSaved: bool,
    #[serde(default)]
    pub save_effect: u8,
    #[serde(default)]
    pub save_light: i64,
    #[serde(default)]
    pub save_speed: i64,
    #[serde(default)]
    pub save_direction: i64,
    #[serde(default)]
    pub save_layout_color: Option<LayoutColor>,
    #[serde(default)]
    pub save_layout_backgroundcolor: Option<i64>,
    #[serde(default)]
    pub save_layout_alphabet: Option<String>,
    #[serde(default)]
    pub save_power_status: Option<i64>,
    /// SingleZone variant extras (`MonochromeIndex`, `ManualIndexN`, …).
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)] // field names mirror the OEM JSON exactly
pub struct LayoutColor {
    #[serde(default)]
    pub isCircular: bool,
    #[serde(default)]
    pub ColorBlocks: u32,
    #[serde(default)]
    pub ColorBuffer: Vec<ColorBlock>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct ColorBlock {
    #[serde(default)]
    pub ID: u8,
    #[serde(default)]
    pub R: u8,
    #[serde(default)]
    pub G: u8,
    #[serde(default)]
    pub B: u8,
}

/// One value inside a zone sub-key: either a parsed saved-effect JSON blob or
/// a plain string (e.g. `*_LastWelcomeEffect` stored as raw JSON text).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ZoneValue {
    Effect(SavedEffect),
    Raw(String),
}

/// Parsed `RGBKeyboard.reg`: global switches + per-zone saved effects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RgbRegState {
    /// Global string values (`PowerSwitch`, `DCLight`, `ACLight`, `Lightbar*`).
    pub globals: BTreeMap<String, String>,
    /// Zone → key → value (see [`ZoneValue`]).
    pub zones: BTreeMap<String, BTreeMap<String, ZoneValue>>,
}

impl RgbRegState {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = std::fs::read(path.as_ref())?;
        Self::parse_bytes(&raw)
    }

    /// Parse a `.reg` file that is either UTF-16LE (as shipped) or UTF-8.
    pub fn parse_bytes(raw: &[u8]) -> Result<Self> {
        let text = decode_reg(raw)?;
        let mut state = RgbRegState::default();
        let mut current: Option<String> = None;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("Windows Registry Editor") {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let key = &line[1..line.len() - 1];
                // Only care about the RGBKeyboard tree.
                current = if key.contains(r"\RGBKeyboard") {
                    // Zone name = last path component.
                    let zone = key.rsplit('\\').next().unwrap_or(key).to_string();
                    Some(zone)
                } else {
                    None
                };
                continue;
            }
            let Some(zone) = current.as_deref() else {
                continue;
            };
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim().trim_matches('"').to_string();
            let value = value.trim();

            if zone == r"HKEY_LOCAL_MACHINE" || zone.contains('\\') {
                // Shouldn't happen (we take last component), but be safe.
                continue;
            }

            if !value.starts_with('"') {
                // REG_DWORD etc. — store raw.
                if zone == "RGBKeyboard" {
                    state.globals.insert(name, value.to_string());
                }
                continue;
            }

            let unquoted = unescape_reg(value);

            // Top-level RGBKeyboard key → globals; sub-keys → zones.
            // The root key itself is named "RGBKeyboard" (last component).
            if name.contains("_LastEffect") || name.contains("_DefaultEffect") {
                // Sub-zone entry: zone is a sub-key like FourZone.
                match serde_json::from_str::<SavedEffect>(&unquoted) {
                    Ok(parsed) => {
                        state
                            .zones
                            .entry(zone.to_string())
                            .or_default()
                            .insert(name, ZoneValue::Effect(parsed));
                    }
                    Err(_) => {
                        // e.g. *_LastWelcomeEffect kept as raw JSON text
                        state
                            .zones
                            .entry(zone.to_string())
                            .or_default()
                            .insert(name, ZoneValue::Raw(unquoted));
                    }
                }
            } else if zone == "RGBKeyboard" {
                state.globals.insert(name, unquoted);
            } else {
                state
                    .zones
                    .entry(zone.to_string())
                    .or_default()
                    .insert(name, ZoneValue::Raw(unquoted));
            }
        }
        Ok(state)
    }
}

fn decode_reg(raw: &[u8]) -> Result<String> {
    if raw.starts_with(&[0xFF, 0xFE]) {
        Ok(String::from_utf16_lossy(
            raw[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect::<Vec<_>>()
                .as_slice(),
        ))
    } else if raw.len() >= 2 && raw[1] == 0 {
        // UTF-16LE without BOM
        Ok(String::from_utf16_lossy(
            raw.as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect::<Vec<_>>()
                .as_slice(),
        ))
    } else {
        Ok(String::from_utf8_lossy(raw).into_owned())
    }
}

/// Unquote a `.reg` string value and resolve `\\` → `\`, `\"` → `"`.
fn unescape_reg(v: &str) -> String {
    let inner = v
        .trim()
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(v);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"Windows Registry Editor Version 5.00

[HKEY_LOCAL_MACHINE\SOFTWARE\OEM\GamingCenter2\RGBKeyboard]
"PowerSwitch"="1"
"DCLight"="0"
"ACLight"="3"

[HKEY_LOCAL_MACHINE\SOFTWARE\OEM\GamingCenter2\RGBKeyboard\FourZone]
"16_LastEffect"="{\"save_mode\":1,\"bSaved\":false,\"save_effect\":5,\"save_light\":36,\"save_speed\":5,\"save_direction\":1,\"save_layout_color\":{\"isCircular\":true,\"ColorBlocks\":7,\"ColorBuffer\":[{\"ID\":0,\"R\":255,\"G\":0,\"B\":0}]},\"save_layout_backgroundcolor\":1,\"save_layout_alphabet\":\"\",\"save_power_status\":0}"
"#;

    #[test]
    fn parse_reg_sample() {
        let state = RgbRegState::parse_bytes(SAMPLE.as_bytes()).unwrap();
        assert_eq!(
            state.globals.get("PowerSwitch").map(String::as_str),
            Some("1")
        );
        assert_eq!(state.globals.get("ACLight").map(String::as_str), Some("3"));
        let fz = &state.zones["FourZone"];
        let ZoneValue::Effect(eff) = &fz["16_LastEffect"] else {
            panic!("expected parsed effect");
        };
        assert_eq!(eff.save_effect, 5);
        assert_eq!(eff.save_speed, 5);
        let buf = &eff.save_layout_color.as_ref().unwrap().ColorBuffer;
        assert_eq!((buf[0].R, buf[0].G, buf[0].B), (255, 0, 0));
    }

    #[test]
    fn real_oem_reg_if_present() {
        let p = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ControlCenter_4.9.47.13/RGBKeyboard.reg"
        ));
        if p.exists() {
            let state = RgbRegState::load(p).unwrap();
            assert!(!state.globals.is_empty(), "expected globals");
            assert!(!state.zones.is_empty(), "expected zones");
        }
    }
}
