//! Whose money this is and where they live: the profile's base currency,
//! home country and city, and the country of each account (Phase 4). The
//! pages read it instead of assuming Hong Kong dollars and HSBC = Hong Kong.
//!
//! Process-wide, like the categories table: one profile is on screen at a
//! time. [`crate::Bundle::load`] sets it from the bundle's `profile` and
//! `user_settings` sections; until then (or from an older server with no
//! profile) it is the server's default: [`DEFAULT_CURRENCY`], no home
//! country, and the source catalog's account countries.
//!
//! An account's country is the user's choice (`user_settings.accounts`:
//! `{"hsbc": {"country": "HK"}}`, written by Settings → Sources), else the
//! catalog's default for its source ([`default_country`]: a bank whose
//! export format only exists in one country), else none: its rows are
//! placed by their currency.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;

/// The base currency when no profile says otherwise: neutral, the same as a
/// new user's wizard default without a locale ([`crate::locale::FALLBACK_CURRENCY`]).
/// A bundle that has a profile (the owner's included) always names its own.
pub const DEFAULT_CURRENCY: &str = crate::locale::FALLBACK_CURRENCY;

/// The catalog's country for a file source's account (the server's
/// `catalog::default_country`, kept equal by a server test).
pub fn default_country(source: &str) -> Option<&'static str> {
    match source {
        // Hong Kong statement exports (current account, card, Mox)
        "hsbc" | "hsbc_card" | "mox" => Some("HK"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Home {
    /// ISO 4217, upper case ("HKD")
    pub currency: String,
    /// ISO 3166 alpha-2 ("HK"); the prior when the data can't tell where home is
    pub country: Option<String>,
    pub city: Option<String>,
    /// the Investments page's benchmark (a Yahoo symbol); None: the S&P 500
    pub benchmark: Option<String>,
    /// the fund account (the bundle's `flexinvest` section: the server's
    /// investment setup): its holding name and index proxy
    pub fund: Option<String>,
    pub fund_proxy: Option<String>,
    /// source -> the country the user set (overrides the catalog; None:
    /// no country, the rows are placed by their currency)
    pub accounts: BTreeMap<String, Option<String>>,
}

impl Default for Home {
    fn default() -> Self {
        Home { currency: DEFAULT_CURRENCY.into(), country: None, city: None, benchmark: None, fund: None, fund_proxy: None, accounts: BTreeMap::new() }
    }
}

impl Home {
    /// From a bundle's `profile` and `user_settings` sections (either may be
    /// missing or null).
    pub fn from_sections(profile: &Value, settings: &Value) -> Home {
        let s = |k: &str| profile.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        let mut accounts = BTreeMap::new();
        if let Some(m) = settings.get("accounts").and_then(Value::as_object) {
            for (src, v) in m {
                match v.get("country") {
                    Some(Value::String(cc)) if cc.trim().len() == 2 => {
                        accounts.insert(src.clone(), Some(cc.trim().to_uppercase()));
                    }
                    Some(Value::Null) => {
                        accounts.insert(src.clone(), None);
                    }
                    Some(Value::String(cc)) if cc.trim().is_empty() => {
                        accounts.insert(src.clone(), None);
                    }
                    _ => {}
                }
            }
        }
        Home {
            currency: s("base_currency").map(|c| c.to_uppercase()).unwrap_or_else(|| DEFAULT_CURRENCY.into()),
            country: s("home_country").map(|c| c.to_uppercase()),
            city: s("home_city"),
            benchmark: s("benchmark"),
            fund: None,
            fund_proxy: None,
            accounts,
        }
    }

    /// The fund account's holding name ("" without one).
    pub fn fund_name(&self) -> String {
        self.fund.clone().unwrap_or_default()
    }

    /// The benchmark's name: "S&P 500" for the default (^GSPC), a few
    /// known indices and funds by name, else the symbol.
    pub fn benchmark_label(&self) -> String {
        match self.benchmark.as_deref().unwrap_or("^GSPC") {
            "^GSPC" | "SPY" | "VOO" | "IVV" | "CSPX" => "S&P 500",
            "URTH" | "IWDA" | "SWDA" => "MSCI World",
            "VT" | "VWRA" | "VWRL" => "FTSE All-World",
            "^NDX" | "QQQ" | "EQQQ" => "Nasdaq 100",
            "^STOXX50E" => "Euro Stoxx 50",
            "^FTSE" => "FTSE 100",
            "^GDAXI" => "DAX",
            "^N225" => "Nikkei 225",
            "^HSI" => "Hang Seng",
            other => return other.to_string(),
        }
        .to_string()
    }

    /// The country of a source's account: the user's choice, else the
    /// catalog's, else None.
    pub fn account_country(&self, source: &str) -> Option<String> {
        match self.accounts.get(source) {
            Some(set) => set.clone(),
            None => default_country(source).map(str::to_string),
        }
    }
}

static HOME: RwLock<Option<Arc<Home>>> = RwLock::new(None);

/// What is on screen now (the default before a bundle loaded).
pub fn get() -> Arc<Home> {
    HOME.read().ok().and_then(|h| h.clone()).unwrap_or_else(|| Arc::new(Home::default()))
}

pub fn set(home: Home) {
    *HOME.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(home));
}

/// From a whole bundle (its `profile` and `user_settings`).
pub fn set_from_bundle(bundle: &Value) {
    let null = Value::Null;
    let mut h = Home::from_sections(bundle.get("profile").unwrap_or(&null), bundle.get("user_settings").unwrap_or(&null));
    let fund = |k: &str| bundle.get("flexinvest").and_then(|f| f.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    h.fund = fund("holding");
    h.fund_proxy = fund("proxy");
    set(h);
}

/// The base currency ("HKD").
pub fn currency() -> String {
    get().currency.clone()
}

/// Countries offered for an account (Settings → Sources), besides "by its
/// currency" and the catalog's default.
pub const COUNTRY_CHOICES: [&str; 26] = [
    "HK", "MO", "CN", "TW", "JP", "KR", "SG", "TH", "IN", "AU", "NZ", "US", "CA", "GB", "IE", "FR", "DE", "IT", "ES", "PT", "NL", "BE",
    "AT", "CH", "SE", "AE",
];

/// Set an account's country: `Some(Some("HK"))` a country, `Some(None)` none
/// (its rows are placed by currency), `None` back to the catalog's default.
/// The pages see it at once; the returned future writes
/// `user_settings.accounts` (None when the server keeps no settings).
pub fn set_account_country(source: &str, country: Option<Option<String>>) -> Option<crate::client::BoxFuture> {
    let mut all = crate::user_settings::get("accounts").and_then(|v| v.as_object().cloned()).unwrap_or_default();
    match &country {
        None => {
            all.remove(source);
        }
        Some(c) => {
            all.insert(source.to_string(), serde_json::json!({"country": c}));
        }
    }
    let mut home = (*get()).clone();
    match country {
        None => {
            home.accounts.remove(source);
        }
        Some(c) => {
            home.accounts.insert(source.to_string(), c);
        }
    }
    set(home);
    crate::user_settings::put("accounts", Value::Object(all))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_the_bundle_sections() {
        let h = Home::from_sections(&json!({"base_currency": "eur", "home_country": "it", "home_city": "Rome"}),
                                    &json!({"accounts": {"n26": {"country": "de"}, "bad": {"country": "xyz"}, "hsbc_card": {"country": null}}}));
        assert_eq!((h.currency.as_str(), h.country.as_deref(), h.city.as_deref()), ("EUR", Some("IT"), Some("Rome")));
        assert_eq!(h.account_country("n26").as_deref(), Some("DE"));
        assert_eq!(h.account_country("bad"), None);
        assert_eq!(h.account_country("hsbc").as_deref(), Some("HK"), "the catalog's default");
        assert_eq!(h.account_country("mox").as_deref(), Some("HK"));
        assert_eq!(h.account_country("hsbc_card"), None, "cleared by the user");
        let none = Home::from_sections(&Value::Null, &Value::Null);
        assert_eq!(none, Home::default());
    }
}
