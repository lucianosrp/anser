//! Optional target weights for the broker holdings, and the contribution the
//! rebalancing math splits, kept on this machine next to theme.toml:
//!
//!   ~/.config/finance-desktop/targets.toml
//!   contribution = 10000          # HKD, the default "next contribution"
//!   [weights]                     # any numbers; they're normalized to 100%
//!   VWRA = 60
//!   EQQQ = 25
//!
//! `FINANCE_DESKTOP_TARGETS=/path/targets.toml` uses another file (dev and
//! screenshots). Written only from Broker › Deep dive › Targets.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Targets {
    /// HKD
    pub contribution: Option<f64>,
    /// symbol -> weight (normalized to 100% when used)
    pub weights: BTreeMap<String, f64>,
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("FINANCE_DESKTOP_TARGETS") {
        return PathBuf::from(p);
    }
    dirs::config_dir().unwrap_or_default().join("finance-desktop").join("targets.toml")
}

const HEADER: &str = "# anser target weights (edited by the app; hand edits are fine).\n\
# contribution: HKD, the default for the split. [weights]: symbol = weight, normalized to 100%.\n\n";

impl Targets {
    /// The saved targets, or none, and why the file couldn't be read.
    pub fn load() -> (Self, Option<String>) {
        let p = path();
        // sealed (.enc) while the local copy is encrypted
        match finance_core::replica::read_text(&p) {
            Some(s) => match toml::from_str(&s) {
                Ok(t) => (t, None),
                Err(e) => (Targets::default(), Some(format!("{} has an error: {e}", p.display()))),
            },
            None => (Targets::default(), None),
        }
    }

    pub fn save(&self) -> anyhow::Result<PathBuf> {
        let p = path();
        finance_core::replica::write_text(&p, &format!("{HEADER}{}", toml::to_string(self)?))?;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::Targets;

    #[test]
    fn file_format() {
        let t: Targets = toml::from_str("contribution = 10000\n[weights]\nVWRA = 60\nEQQQ = 25.5\n").unwrap();
        assert_eq!(t.contribution, Some(10000.));
        assert_eq!(t.weights.get("EQQQ"), Some(&25.5));
        let back: Targets = toml::from_str(&toml::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        assert_eq!(toml::from_str::<Targets>("").unwrap(), Targets::default());
    }
}
