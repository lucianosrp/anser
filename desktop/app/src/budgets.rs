//! Monthly budgets per spending category, kept on this machine next to
//! theme.toml:
//!
//!   ~/.config/finance-desktop/budgets.toml
//!   [monthly]
//!   Dining = 2200
//!   "Utilities / Telecom" = 500
//!
//! `FINANCE_DESKTOP_BUDGETS=/path/budgets.toml` uses another file (dev and
//! screenshots). The file is only written from the Budgets page. The numbers
//! are a gpui Global, so any page (Overview's tile) can read them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;
use gpui_kit::Global;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Budgets {
    /// category -> HKD per month
    pub monthly: BTreeMap<String, f64>,
}

impl Global for Budgets {}

/// The user_settings key.
pub const KEY: &str = "budgets";

/// Where saves go: "the server" or the file.
pub fn saved_where() -> String {
    if finance_core::user_settings::server_side() { "the server".into() } else { path().display().to_string() }
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("FINANCE_DESKTOP_BUDGETS") {
        return PathBuf::from(p);
    }
    dirs::config_dir().unwrap_or_default().join("finance-desktop").join("budgets.toml")
}

impl Budgets {
    /// The saved budgets: the server's (user_settings), else the local
    /// file; none when neither has any.
    pub fn load() -> Self {
        if let Some(v) = finance_core::user_settings::get(KEY) {
            return serde_json::from_value(v).unwrap_or_default();
        }
        std::fs::read_to_string(path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    /// To the server when it keeps settings (shared by every device), else
    /// the local file.
    pub fn save(&self) -> anyhow::Result<()> {
        if finance_core::user_settings::save(KEY, serde_json::to_value(self)?) {
            return Ok(());
        }
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, toml::to_string(self)?)?;
        Ok(())
    }
}

/// Today, for the running month and the running year.
/// `FINANCE_DESKTOP_TODAY=2026-09-18` pins it (dev and screenshots).
pub fn today() -> NaiveDate {
    std::env::var("FINANCE_DESKTOP_TODAY").ok()
        .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
        .unwrap_or_else(|| chrono::Local::now().date_naive())
}

/// "2,200", "HK$ 2200", " 2200.5 " -> Some(2200.5); "" -> None (no budget).
pub fn parse_amount(s: &str) -> Option<f64> {
    let clean: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    clean.parse::<f64>().ok().filter(|v| *v > 0.)
}

#[cfg(test)]
mod tests {
    use super::{parse_amount, Budgets};

    #[test]
    fn amounts_and_the_file_format() {
        assert_eq!(parse_amount("2,200"), Some(2200.));
        assert_eq!(parse_amount("HK$ 2200.5"), Some(2200.5));
        assert_eq!(parse_amount(""), None);
        assert_eq!(parse_amount("0"), None);
        let b: Budgets = toml::from_str("[monthly]\nDining = 2200\n\"Utilities / Telecom\" = 500.0\n").unwrap();
        assert_eq!(b.monthly.get("Dining"), Some(&2200.));
        assert_eq!(b.monthly.get("Utilities / Telecom"), Some(&500.));
        let back: Budgets = toml::from_str(&toml::to_string(&b).unwrap()).unwrap();
        assert_eq!(back, b);
    }
}
