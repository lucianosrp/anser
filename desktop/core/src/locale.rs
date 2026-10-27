//! A new user's currency and country, guessed from the device's locale.
//!
//! The setup wizard pre-fills "Base currency" and "Home country" from here;
//! the user can change both. Nothing is assumed beyond the locale: a tag
//! with no region (or one this table doesn't know) gives [`FALLBACK_CURRENCY`]
//! and no country. Existing profiles never come through here.

use std::sync::OnceLock;

/// The base currency when the locale says nothing.
pub const FALLBACK_CURRENCY: &str = "USD";

/// The browser's language tag (navigator.language), set once by the web
/// build; the desktop reads its environment instead.
static BROWSER_TAG: OnceLock<String> = OnceLock::new();

pub fn set_browser_tag(tag: String) {
    let _ = BROWSER_TAG.set(tag);
}

/// The device's locale tag ("de_DE.UTF-8", "en-GB"), if it has one: on the
/// desktop LC_ALL, LC_MONETARY, then LANG ("C" and "POSIX" are no locale);
/// in the browser navigator.language.
pub fn device_tag() -> Option<String> {
    if let Some(t) = BROWSER_TAG.get() {
        return Some(t.clone());
    }
    #[cfg(not(target_family = "wasm"))]
    for k in ["LC_ALL", "LC_MONETARY", "LANG"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.is_empty() && v != "C" && v != "POSIX") {
            return Some(v);
        }
    }
    None
}

/// The region of a locale tag as an upper-case ISO 3166 alpha-2 code:
/// "de_DE.UTF-8" -> DE, "en-GB" -> GB, "zh-Hant-HK" -> HK, "en" -> None.
pub fn region(tag: &str) -> Option<String> {
    let base = tag.split(['.', '@']).next().unwrap_or("");
    base.split(['_', '-'])
        .skip(1)
        .find(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
        .map(|p| p.to_ascii_uppercase())
}

/// The currency a country uses, for the countries this knows (the euro
/// area and the larger economies that the FX provider (ECB) has rates for);
/// None for the rest.
pub fn currency_of(country: &str) -> Option<&'static str> {
    Some(match country {
        "AT" | "BE" | "CY" | "DE" | "EE" | "ES" | "FI" | "FR" | "GR" | "HR" | "IE" | "IT" | "LT" | "LU" | "LV" | "MT" | "NL" | "PT" | "SI" | "SK" => "EUR",
        "US" => "USD",
        "GB" => "GBP",
        "CH" | "LI" => "CHF",
        "HK" => "HKD",
        "SG" => "SGD",
        "JP" => "JPY",
        "CN" => "CNY",
        "KR" => "KRW",
        "IN" => "INR",
        "AU" => "AUD",
        "NZ" => "NZD",
        "CA" => "CAD",
        "MX" => "MXN",
        "BR" => "BRL",
        "SE" => "SEK",
        "NO" => "NOK",
        "DK" => "DKK",
        "PL" => "PLN",
        "CZ" => "CZK",
        "HU" => "HUF",
        "TR" => "TRY",
        "ZA" => "ZAR",
        "TH" => "THB",
        "MY" => "MYR",
        "ID" => "IDR",
        "PH" => "PHP",
        _ => return None,
    })
}

/// (base currency, home country) for a locale tag: the region's country and
/// currency, else USD and no country. A region with no known currency keeps
/// the country and falls back to USD.
pub fn infer(tag: Option<&str>) -> (String, String) {
    let Some(cc) = tag.and_then(region) else {
        return (FALLBACK_CURRENCY.into(), String::new());
    };
    let ccy = currency_of(&cc).unwrap_or(FALLBACK_CURRENCY);
    (ccy.into(), cc)
}

/// [`infer`] for this device.
pub fn detect() -> (String, String) {
    infer(device_tag().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_region_is_read_from_the_tag() {
        assert_eq!(region("de_DE.UTF-8").as_deref(), Some("DE"));
        assert_eq!(region("en-GB").as_deref(), Some("GB"));
        assert_eq!(region("zh-Hant-HK").as_deref(), Some("HK"));
        assert_eq!(region("sr_RS@latin").as_deref(), Some("RS"));
        assert_eq!(region("en"), None);
        assert_eq!(region("C"), None);
        assert_eq!(region(""), None);
    }

    #[test]
    fn currency_and_country_follow_the_locale() {
        assert_eq!(infer(Some("de_DE.UTF-8")), ("EUR".into(), "DE".into()));
        assert_eq!(infer(Some("en-GB")), ("GBP".into(), "GB".into()));
        assert_eq!(infer(Some("ja_JP")), ("JPY".into(), "JP".into()));
        assert_eq!(infer(Some("en-HK")), ("HKD".into(), "HK".into()));
    }

    #[test]
    fn no_locale_is_neutral_not_hong_kong() {
        assert_eq!(infer(None), ("USD".into(), String::new()));
        assert_eq!(infer(Some("en")), ("USD".into(), String::new()));
        // a country this table has no currency for: keep it, price in USD
        assert_eq!(infer(Some("fr_NC")), ("USD".into(), "NC".into()));
        // the FX provider (ECB) has no AED or TWD rates: price in USD
        assert_eq!(infer(Some("zh_TW")), ("USD".into(), "TW".into()));
        assert_eq!(infer(Some("ar_AE")), ("USD".into(), "AE".into()));
    }
}
