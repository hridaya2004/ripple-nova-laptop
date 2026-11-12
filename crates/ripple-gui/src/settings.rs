//! GUI preferences (theme, font), saved next to the hardware config. The GUI runs as root,
//! so `/etc/ripple-nova/` is the natural place.

use std::process::Command;

use serde::{Deserialize, Serialize};

const PATH: &str = "/etc/ripple-nova/gui.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Light theme instead of the default dark one.
    #[serde(default)]
    pub light: bool,
    /// Font family; `None` = the system monospace font.
    #[serde(default)]
    pub font: Option<String>,
    /// Font picker lists every installed family, not just monospace ones.
    #[serde(default)]
    pub all_fonts: bool,
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(PATH).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all("/etc/ripple-nova")?;
        std::fs::write(PATH, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)
    }
}

fn fontconfig(args: &[&str]) -> Vec<String> {
    Command::new(args[0])
        .args(&args[1..])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// The system default monospace family (fontconfig), first entry only.
pub fn system_mono() -> String {
    fontconfig(&["fc-match", "-f", "%{family}\n", "monospace"])
        .first()
        .and_then(|f| f.split(',').next().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| "monospace".into())
}

/// Installed font families, sorted and de-duplicated; monospace only unless `all`.
pub fn installed_fonts(all: bool) -> Vec<String> {
    let mut v = if all {
        fontconfig(&["fc-list", "-f", "%{family[0]}\n"])
    } else {
        fontconfig(&["fc-list", ":spacing=mono", "-f", "%{family[0]}\n"])
    };
    v.sort_by_key(|f| f.to_lowercase());
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_dark_system_font_and_roundtrip() {
        let d = Settings::default();
        assert!(!d.light && d.font.is_none() && !d.all_fonts);
        let s = Settings { light: true, font: Some("Fira Code".into()), all_fonts: true };
        assert_eq!(serde_json::from_str::<Settings>(&serde_json::to_string(&s).unwrap()).unwrap(), s);
        // old/partial files still load
        assert_eq!(serde_json::from_str::<Settings>("{}").unwrap(), d);
    }
}
