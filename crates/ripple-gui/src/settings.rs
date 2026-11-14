//! GUI preferences (theme, font), saved next to the hardware config. The GUI runs as root,
//! so `/etc/ripple-nova/` is the natural place.

use std::process::Command;

use serde::{Deserialize, Serialize};

use ripple_hw::notify::GUI_SETTINGS as PATH;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Light theme instead of the default dark one.
    #[serde(default)]
    pub light: bool,
    /// Font family; `None` = the system monospace font.
    #[serde(default)]
    pub font: Option<String>,
    /// Desktop popups when a toggle, the profile or fan boost changes. On unless switched off:
    /// a missing key (an old file) must read as on, not as serde's `false`.
    #[serde(default = "yes")]
    pub popups: bool,
    /// Open at login without showing the window (it starts minimized). On unless switched off.
    #[serde(default = "yes")]
    pub start_minimized: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            light: false,
            font: None,
            popups: true,
            start_minimized: true,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(PATH)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Save the window's own preferences. The popups key is not ours: the tray switches it too (as
    /// root, through `rcc popups`), so it is left as it is in the file, and the keys are set under the
    /// same lock the tray's write takes.
    pub fn save(&self) -> std::io::Result<()> {
        ripple_hw::store::set_json_keys(
            std::path::Path::new(PATH),
            &[
                ("light", self.light.into()),
                ("font", self.font.clone().into()),
                ("start_minimized", self.start_minimized.into()),
            ],
        )
    }
}

fn fontconfig(args: &[&str]) -> Vec<String> {
    Command::new(args[0])
        .args(&args[1..])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The system default monospace family (fontconfig), first entry only.
pub fn system_mono() -> String {
    fontconfig(&["fc-match", "-f", "%{family}\n", "monospace"])
        .first()
        .and_then(|f| f.split(',').next().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| "monospace".into())
}

/// Every installed font family name (fontconfig), sorted and de-duplicated. A font can carry several
/// ("DejaVu Sans,DejaVu Sans Condensed"); all of them select it, so all are listed.
pub fn installed_fonts() -> Vec<String> {
    family_names(&fontconfig(&["fc-list", ":", "family"]))
}

/// The distinct names in `fc-list : family` lines, comma separated, in case-insensitive order.
fn family_names(lines: &[String]) -> Vec<String> {
    let mut v: Vec<String> = lines
        .iter()
        .flat_map(|l| l.split(','))
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();
    v.sort_by_key(|f| f.to_lowercase());
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_of_a_font_is_listed_once() {
        let lines = [
            "DejaVu Sans,DejaVu Sans Condensed".to_string(),
            "DejaVu Sans".to_string(),
            " JetBrainsMono NF , JetBrainsMono NF Light ".to_string(),
            "".to_string(),
            "abc".to_string(),
        ];
        assert_eq!(
            family_names(&lines),
            [
                "abc",
                "DejaVu Sans",
                "DejaVu Sans Condensed",
                "JetBrainsMono NF",
                "JetBrainsMono NF Light"
            ]
        );
    }

    #[test]
    fn defaults_are_dark_system_font_and_roundtrip() {
        let d = Settings::default();
        assert!(!d.light && d.font.is_none() && d.popups && d.start_minimized);
        let s = Settings {
            light: true,
            font: Some("Fira Code".into()),
            popups: false,
            start_minimized: false,
        };
        assert_eq!(
            serde_json::from_str::<Settings>(&serde_json::to_string(&s).unwrap()).unwrap(),
            s
        );
        // old/partial files (even with the removed all_fonts key) still load
        assert_eq!(serde_json::from_str::<Settings>("{}").unwrap(), d);
        assert_eq!(
            serde_json::from_str::<Settings>(r#"{"all_fonts":true}"#).unwrap(),
            d
        );
        // a file written before the option existed keeps popups on
        assert!(
            serde_json::from_str::<Settings>(r#"{"light":true,"font":null}"#)
                .unwrap()
                .popups
        );
        assert!(
            !serde_json::from_str::<Settings>(r#"{"popups":false}"#)
                .unwrap()
                .popups
        );
        assert!(
            serde_json::from_str::<Settings>("{}")
                .unwrap()
                .start_minimized
        );
        assert!(
            !serde_json::from_str::<Settings>(r#"{"start_minimized":false}"#)
                .unwrap()
                .start_minimized
        );
    }
}
