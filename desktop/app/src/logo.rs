//! Logos for banks, exchanges, coins, tickers and well-known merchants --
//! single-colour (Simple Icons, CC0; see assets/logos/LICENSE.simple-icons.md),
//! bundled into the binary so nothing is fetched at runtime and no logo
//! service learns which banks or merchants the owner uses.
//!
//! They are drawn tinted with the theme's text colour (gpui renders SVGs as
//! a single-colour mask), so they follow dark/light and the accent like the
//! rest of the UI. A security without a brand logo gets an asset-class
//! glyph (Lucide, ISC: ETF/fund, stock, T-bill or government bond, other
//! bond, cash, coin, commodity) from `finance_core::exposure::class`; only
//! what nothing classifies falls back to a monogram tile.
//!
//! Fund issuers: Simple Icons 16.33.0 (the pinned version) carries HSBC but
//! no Vanguard, Invesco, iShares/BlackRock, State Street/SPDR or Charles
//! Schwab mark, so those ETFs show the class glyph; [`issuer_slug`] is where
//! a CC0 issuer logo would plug in.
//! Optional: `logos = false` in theme.toml (or the Settings switch) turns
//! them off; every call site then renders exactly as without them.

use std::borrow::Cow;

use finance_core::exposure::class::{self as class, AssetClass, BondKind, Classification, Issuer, Vehicle};
use finance_core::{Entry, Kind};
use gpui_kit::assets::{IconName as Lucide, icon_assets};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::*;
use crate::zoom::px;

pub const LOGOS: &[(&str, &[u8])] = &[
    ("airbnb", include_bytes!("../assets/logos/airbnb.svg")),
    ("airfrance", include_bytes!("../assets/logos/airfrance.svg")),
    ("algorand", include_bytes!("../assets/logos/algorand.svg")),
    ("alipay", include_bytes!("../assets/logos/alipay.svg")),
    ("anthropic", include_bytes!("../assets/logos/anthropic.svg")),
    ("apple", include_bytes!("../assets/logos/apple.svg")),
    ("binance", include_bytes!("../assets/logos/binance.svg")),
    ("bitcoin", include_bytes!("../assets/logos/bitcoin.svg")),
    ("bnbchain", include_bytes!("../assets/logos/bnbchain.svg")),
    ("bookingdotcom", include_bytes!("../assets/logos/bookingdotcom.svg")),
    ("cardano", include_bytes!("../assets/logos/cardano.svg")),
    ("circle", include_bytes!("../assets/logos/circle.svg")),
    ("claude", include_bytes!("../assets/logos/claude.svg")),
    ("deliveroo", include_bytes!("../assets/logos/deliveroo.svg")),
    ("easyjet", include_bytes!("../assets/logos/easyjet.svg")),
    ("ethereum", include_bytes!("../assets/logos/ethereum.svg")),
    ("google", include_bytes!("../assets/logos/google.svg")),
    ("googlepay", include_bytes!("../assets/logos/googlepay.svg")),
    ("hsbc", include_bytes!("../assets/logos/hsbc.svg")),
    ("ikea", include_bytes!("../assets/logos/ikea.svg")),
    ("lufthansa", include_bytes!("../assets/logos/lufthansa.svg")),
    ("mcdonalds", include_bytes!("../assets/logos/mcdonalds.svg")),
    ("meta", include_bytes!("../assets/logos/meta.svg")),
    ("mtr", include_bytes!("../assets/logos/mtr.svg")),
    ("n26", include_bytes!("../assets/logos/n26.svg")),
    ("netflix", include_bytes!("../assets/logos/netflix.svg")),
    ("nordvpn", include_bytes!("../assets/logos/nordvpn.svg")),
    ("nvidia", include_bytes!("../assets/logos/nvidia.svg")),
    ("okx", include_bytes!("../assets/logos/okx.svg")),
    ("paypal", include_bytes!("../assets/logos/paypal.svg")),
    ("revolut", include_bytes!("../assets/logos/revolut.svg")),
    ("ryanair", include_bytes!("../assets/logos/ryanair.svg")),
    ("sncf", include_bytes!("../assets/logos/sncf.svg")),
    ("solana", include_bytes!("../assets/logos/solana.svg")),
    ("spacex", include_bytes!("../assets/logos/spacex.svg")),
    ("spotify", include_bytes!("../assets/logos/spotify.svg")),
    ("starbucks", include_bytes!("../assets/logos/starbucks.svg")),
    ("tether", include_bytes!("../assets/logos/tether.svg")),
    ("trezor", include_bytes!("../assets/logos/trezor.svg")),
    ("tripdotcom", include_bytes!("../assets/logos/tripdotcom.svg")),
    ("turkishairlines", include_bytes!("../assets/logos/turkishairlines.svg")),
    ("uber", include_bytes!("../assets/logos/uber.svg")),
    ("ubereats", include_bytes!("../assets/logos/ubereats.svg")),
    ("uniqlo", include_bytes!("../assets/logos/uniqlo.svg")),
    ("wechat", include_bytes!("../assets/logos/wechat.svg")),
    ("wise", include_bytes!("../assets/logos/wise.svg")),
];

// the asset-class glyphs (only these are embedded, not the whole catalog)
icon_assets!(pub ClassIcons, [Layers, ChartCandlestick, Landmark, ScrollText, Banknote, Coins, Gem, Building2]);

/// Serves `logos/<slug>.svg` from the table above, the category icons
/// (`category::CategoryIcons`), and everything else from gpui-kit's own icon set.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some(slug) = path.strip_prefix("logos/").and_then(|p| p.strip_suffix(".svg")) {
            return Ok(LOGOS.iter().find(|(s, _)| *s == slug).map(|(_, b)| Cow::Borrowed(*b)));
        }
        if let Some(icon) = crate::map::TravelIcons.load(path)? {
            return Ok(Some(icon));
        }
        if let Some(svg) = crate::category::CategoryIcons.load(path)? {
            return Ok(Some(svg));
        }
        if let Some(svg) = crate::filterbar::FilterIcons.load(path)? {
            return Ok(Some(svg));
        }
        if let Some(svg) = ClassIcons.load(path)? {
            return Ok(Some(svg));
        }
        if let Some(svg) = crate::pages::onboarding::WizardIcons.load(path)? {
            return Ok(Some(svg));
        }
        if let Some(svg) = crate::codeslots::CodeIcons.load(path)? {
            return Ok(Some(svg));
        }
        #[cfg(target_family = "wasm")]
        return crate::web_assets::KitIcons.load(path);
        #[cfg(not(target_family = "wasm"))]
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        #[cfg(target_family = "wasm")]
        let mut out = crate::web_assets::KitIcons.list(path)?;
        #[cfg(not(target_family = "wasm"))]
        let mut out = gpui_kit::assets::Assets.list(path)?;
        out.extend(crate::category::CategoryIcons.list(path)?);
        out.extend(crate::filterbar::FilterIcons.list(path)?);
        out.extend(ClassIcons.list(path)?);
        out.extend(crate::pages::onboarding::WizardIcons.list(path)?);
        out.extend(crate::codeslots::CodeIcons.list(path)?);
        out.sort();
        out.dedup();
        if "logos/".starts_with(path) || path.starts_with("logos") {
            out.extend(LOGOS.iter().map(|(s, _)| SharedString::from(format!("logos/{s}.svg"))));
        }
        Ok(out)
    }
}

/// Whether logos are on (theme.toml `logos`), set by theme::apply.
pub struct LogoPref(pub bool);
impl Global for LogoPref {}

pub fn enabled(cx: &App) -> bool {
    cx.try_global::<LogoPref>().is_none_or(|p| p.0)
}

/// What to draw: a bundled logo, an asset-class glyph, or a monogram.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    Logo(&'static str),
    Icon(Lucide),
    Monogram(String),
}

/// Merchant keywords (whole words or phrases, lowercase) -> logo slug.
/// Longer phrases first so "uber eats" wins over "uber".
const MERCHANTS: &[(&str, &str)] = &[
    ("uber eats", "ubereats"), ("ubereats", "ubereats"), ("uber *eats", "ubereats"),
    ("trip.com", "tripdotcom"), ("trip com", "tripdotcom"), ("booking.com", "bookingdotcom"),
    ("air france", "airfrance"), ("turkish airlines", "turkishairlines"), ("google pay", "googlepay"),
    ("spotify", "spotify"), ("uber", "uber"), ("airbnb", "airbnb"), ("starbucks", "starbucks"),
    ("mcdonald's", "mcdonalds"), ("mcdonalds", "mcdonalds"), ("mcdonald", "mcdonalds"),
    ("apple.com", "apple"), ("google", "google"), ("netflix", "netflix"), ("anthropic", "anthropic"),
    ("claude.ai", "claude"), ("deliveroo", "deliveroo"), ("ikea", "ikea"), ("uniqlo", "uniqlo"),
    ("paypal", "paypal"), ("easyjet", "easyjet"), ("ryanair", "ryanair"), ("lufthansa", "lufthansa"),
    ("sncf", "sncf"), ("nordvpn", "nordvpn"), ("a.nordvpn", "nordvpn"), ("oculus", "meta"), ("meta", "meta"),
    ("wechat", "wechat"), ("alipay", "alipay"), ("alipayhk", "alipay"), ("mtr", "mtr"), ("wise", "wise"),
    ("binance", "binance"), ("okx", "okx"), ("revolut", "revolut"), ("hsbc", "hsbc"), ("n26", "n26"),
];

/// Coins, and the tickers of the listed companies whose brand is bundled
/// (the merchants' logos double as their shares') -> slug.
fn asset_slug(symbol: &str) -> Option<&'static str> {
    Some(match symbol.to_ascii_uppercase().as_str() {
        "BTC" => "bitcoin",
        "ETH" => "ethereum",
        "BNB" => "bnbchain",
        "SOL" => "solana",
        "ALGO" => "algorand",
        "ADA" => "cardano",
        "USDT" => "tether",
        "USDC" => "circle",
        "AAPL" => "apple",
        "GOOGL" | "GOOG" => "google",
        "META" => "meta",
        "NFLX" => "netflix",
        "SPOT" => "spotify",
        "ABNB" => "airbnb",
        "UBER" => "uber",
        "MCD" => "mcdonalds",
        "SBUX" => "starbucks",
        "PYPL" => "paypal",
        "BKNG" => "bookingdotcom",
        "NVDA" => "nvidia",
        "SPCX" => "spacex",
        _ => return None,
    })
}

/// A fund issuer's bundled logo. Only HSBC is in the pinned Simple Icons
/// (see the module doc); the others draw the class glyph.
pub fn issuer_slug(i: Issuer) -> Option<&'static str> {
    match i {
        Issuer::Hsbc => Some("hsbc"),
        Issuer::Vanguard | Issuer::Invesco | Issuer::IShares | Issuer::Spdr | Issuer::Schwab => None,
    }
}

/// The glyph for what a holding is: a basket for an ETF or fund, a candle
/// for a stock, a landmark for T-bills and government bonds, a scroll for
/// other bonds, a banknote for cash, coins, a gem for commodities. None
/// when nothing classifies it.
pub fn class_icon(c: &Classification) -> Option<Lucide> {
    Some(match (c.class, c.bond, c.vehicle) {
        (AssetClass::Bond, Some(BondKind::TBill | BondKind::Government), _) => Lucide::Landmark,
        (AssetClass::Bond, _, _) => Lucide::ScrollText,
        (AssetClass::Cash, _, Some(Vehicle::Coin)) => Lucide::Coins,
        (AssetClass::Cash, _, _) => Lucide::Banknote,
        (AssetClass::Crypto, _, _) => Lucide::Coins,
        (AssetClass::Commodity, _, _) => Lucide::Gem,
        (_, _, Some(Vehicle::Stock)) => Lucide::ChartCandlestick,
        (_, _, Some(Vehicle::Etf | Vehicle::Fund)) => Lucide::Layers,
        (AssetClass::Equity, _, _) => Lucide::ChartCandlestick,
        _ => return None,
    })
}

/// The mark for a security: its own logo (a listed brand, a coin), else
/// its issuer's, else its asset-class glyph, else a monogram of the ticker.
/// `description` is the broker's name for it ("VANG FTSE AW USDA"), or "".
pub fn for_security(symbol: &str, description: &str) -> Mark {
    if let Some(slug) = asset_slug(symbol) {
        return Mark::Logo(slug);
    }
    let c = class::classify(symbol, description);
    if let Some(slug) = c.issuer.and_then(issuer_slug) {
        return Mark::Logo(slug);
    }
    class_icon(&c).map(Mark::Icon).unwrap_or_else(|| Mark::Monogram(symbol.chars().take(2).collect::<String>().to_uppercase()))
}

/// The mark for a classified holding (Investments: the classification
/// already knows coins and the account).
pub fn for_class(symbol: &str, c: &Classification) -> Mark {
    if let Some(slug) = asset_slug(symbol).or(c.issuer.and_then(issuer_slug)) {
        return Mark::Logo(slug);
    }
    class_icon(c).map(Mark::Icon).unwrap_or_else(|| Mark::Monogram(symbol.chars().take(2).collect::<String>().to_uppercase()))
}

/// A coin: its logo, else the coins glyph.
pub fn for_coin(symbol: &str) -> Mark {
    for_class(symbol, &class::classify_coin(symbol))
}

/// Bank / exchange / wallet behind a source key -> slug.
pub fn source_slug(source: &str) -> Option<&'static str> {
    let s = source.to_ascii_lowercase();
    [("hsbc", "hsbc"), ("n26", "n26"), ("revolut", "revolut"), ("binance", "binance"), ("okx", "okx"), ("trezor", "trezor")]
        .into_iter()
        .find(|(k, _)| s.starts_with(k))
        .map(|(_, v)| v)
}

/// Lowercase words, punctuation as spaces, padded: " pending uber com ".
/// Matching whole words of this form finds "uber" inside "PENDING.UBER.COM"
/// and "trip com" in "Trip.com", without "uber" matching "suberb".
fn words(text: &str) -> String {
    let cleaned: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    format!(" {} ", cleaned.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// A known merchant in free text (whole-word match).
pub fn merchant_slug(text: &str) -> Option<&'static str> {
    let w = words(text);
    MERCHANTS.iter().find(|(k, _)| w.contains(&words(k))).map(|(_, v)| *v)
}

/// One or two letters for a name: skips card-terminal prefixes and digits.
pub fn monogram(text: &str) -> String {
    const NOISE: &[&str] = &["pos", "mdc", "gpay", "sales:", "kpay", "bbmsl", "qfpay", "yeahpay", "cr", "to", "the"];
    let word = text
        .split(|c: char| c.is_whitespace() || c == '*')
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .find(|w| w.chars().next().is_some_and(|c| c.is_alphabetic()) && !w.contains('(')
            && !NOISE.contains(&w.to_lowercase().as_str()))
        .unwrap_or("?");
    word.chars().filter(|c| c.is_alphanumeric()).take(2).collect::<String>().to_uppercase()
}

/// The mark for a ledger row: the merchant if known; coins by asset;
/// trades by ticker; otherwise a monogram of the description.
pub fn for_entry(e: &Entry) -> Mark {
    match e.kind {
        Kind::Crypto => for_coin(&e.currency),
        Kind::Broker => {
            // "Buy 0.84 QQQ @ 714.24 USD"; the haystack carries the broker's name for it
            let symbol = e.description.split_whitespace().nth(2).unwrap_or("");
            for_security(symbol, &e.haystack)
        }
        _ => merchant_slug(&e.description).map(Mark::Logo).unwrap_or_else(|| Mark::Monogram(monogram(&e.description))),
    }
}

/// The mark for a free-text name (a merchant): a logo when known, else a monogram.
pub fn for_text(text: &str) -> Mark {
    merchant_slug(text).map(Mark::Logo).unwrap_or_else(|| Mark::Monogram(monogram(text)))
}

/// The mark for a ticker / coin symbol (see [`for_security`]).
pub fn for_symbol(symbol: &str) -> Mark {
    for_security(symbol, "")
}

/// The mark for a bank / exchange / wallet source key or label.
pub fn for_source(source: &str) -> Mark {
    source_slug(source).map(Mark::Logo).unwrap_or_else(|| Mark::Monogram(monogram(source)))
}

/// A `size`-px rounded tile with the mark, theme-tinted. None when logos are
/// off, so call sites can `.when_some(logo::tile(..), |d, l| d.child(l))`.
pub fn tile(mark: Mark, size: f32, cx: &App) -> Option<AnyElement> {
    if !enabled(cx) {
        return None;
    }
    let t = cx.theme();
    let base = div()
        .size(px(size))
        .flex_shrink_0()
        .rounded(px(size * 0.28))
        .bg(t.muted)
        .flex()
        .items_center()
        .justify_center();
    Some(match mark {
        Mark::Logo(slug) => base
            .child(svg().path(format!("logos/{slug}.svg")).size(px(size * 0.58)).text_color(t.foreground.opacity(0.85)))
            .into_any_element(),
        Mark::Icon(icon) => base
            .child(Icon::new(icon).size(px(size * 0.6)).text_color(t.muted_foreground))
            .into_any_element(),
        Mark::Monogram(text) => base
            .text_size(px(size * if text.chars().count() > 2 { 0.3 } else { 0.38 }))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.muted_foreground)
            .child(text)
            .into_any_element(),
    })
}

#[cfg(test)]
mod tests {
    use super::{for_security, for_symbol, merchant_slug, monogram, Lucide, Mark};
    use finance_core::exposure::class::{classify, Issuer};

    #[test]
    fn securities_get_a_logo_or_a_class_glyph() {
        assert_eq!(for_symbol("NVDA"), Mark::Logo("nvidia"));
        assert_eq!(for_symbol("BTC"), Mark::Logo("bitcoin"));
        // issuers without a bundled logo: the class glyph
        assert_eq!(for_symbol("VWRA"), Mark::Icon(Lucide::Layers));
        assert_eq!(for_symbol("IB01"), Mark::Icon(Lucide::Landmark));
        assert_eq!(for_symbol("AGGU"), Mark::Icon(Lucide::ScrollText));
        assert_eq!(for_symbol("GLD"), Mark::Icon(Lucide::Gem));
        assert_eq!(for_security("ABCD", "ACME CORP"), Mark::Icon(Lucide::ChartCandlestick));
        assert_eq!(for_security("XYZ", "HSBC GLOBAL BOND INDEX FUND"), Mark::Logo("hsbc"));
        assert_eq!(for_symbol("ZZZ"), Mark::Monogram("ZZ".into()));
        // the issuer table behind the logos
        assert_eq!(classify("VT", "").issuer, Some(Issuer::Vanguard));
        assert_eq!(classify("EQQQ", "").issuer, Some(Issuer::Invesco));
        assert_eq!(classify("XX", "ISHARES CORE MSCI WORLD").issuer, Some(Issuer::IShares));
    }

    #[test]
    fn merchants_match_whole_words() {
        assert_eq!(merchant_slug("Uber Eats FX Rate"), Some("ubereats"));
        assert_eq!(merchant_slug("UBER *TRIP HELP.UBER.COM"), Some("uber"));
        assert_eq!(merchant_slug("Spotify"), Some("spotify"));
        assert_eq!(merchant_slug("SWISE GARDEN"), None); // no partial words
        assert_eq!(merchant_slug("Trip.com Travel SG"), Some("tripdotcom"));
        assert_eq!(merchant_slug("UBR* PENDING.UBER.COM NL"), Some("uber"));
        assert_eq!(merchant_slug("McDonald's"), Some("mcdonalds"));
        assert_eq!(merchant_slug("Superb Hotel"), None);
    }

    #[test]
    fn monograms_skip_terminal_prefixes() {
        assert_eq!(monogram("BBMSL*042 Sushiro MOS"), "SU");
        assert_eq!(monogram("POS MDC (17JUN24)P WELLCOME"), "WE");
        assert_eq!(monogram("Hikari Bento"), "HI");
    }
}
