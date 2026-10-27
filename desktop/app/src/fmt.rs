//! Number formatting and the two colors that carry meaning everywhere:
//! money in is green, money out is red -- independent of the accent theme.

use gpui_kit::{Hsla, hsla};

/// The money colors' tone: 0 the usual, 1 terminal dark (phosphor green),
/// 2 terminal light (the same idea, dark enough for paper). theme::apply sets it.
static MONEY_TONE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_money_tone(tone: u8) {
    MONEY_TONE.store(tone, std::sync::atomic::Ordering::Relaxed);
}

pub fn money_in() -> Hsla {
    match MONEY_TONE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => hsla(138. / 360., 0.52, 0.60, 1.),
        2 => hsla(138. / 360., 0.62, 0.30, 1.),
        _ => hsla(145. / 360., 0.45, 0.58, 1.),
    }
}

pub fn money_out() -> Hsla {
    match MONEY_TONE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => hsla(4. / 360., 0.60, 0.64, 1.),
        2 => hsla(4. / 360., 0.60, 0.44, 1.),
        _ => hsla(6. / 360., 0.68, 0.66, 1.),
    }
}

pub fn signed_color(v: f64) -> Hsla {
    if v >= 0. { money_in() } else { money_out() }
}

fn group(int: u64) -> String {
    let s = int.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Privacy mode (Ctrl+H): every amount and quantity renders as MASK, so the
/// app can be shown or screen-shared without revealing numbers. Percentages,
/// dates and counts stay. Set from the theme config at start and on toggle.
static PRIVATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub const MASK: &str = "•••••";

pub fn private() -> bool {
    PRIVATE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_private(on: bool) {
    PRIVATE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// A count of things (rows, files, buys): never masked.
pub fn count(v: f64) -> String {
    let s = group(v.abs().round() as u64);
    if v < -0.5 { format!("−{s}") } else { s }
}

/// 1234567.8 -> "1,234,568"
pub fn int(v: f64) -> String {
    if private() {
        return MASK.into();
    }
    let s = group(v.abs().round() as u64);
    if v < -0.5 { format!("−{s}") } else { s }
}

/// 1234.5 -> "1,234.50"
pub fn dec2(v: f64) -> String {
    if private() {
        return MASK.into();
    }
    let cents = (v.abs() * 100.).round() as u64;
    let s = format!("{}.{:02}", group(cents / 100), cents % 100);
    if v < 0. && cents > 0 { format!("−{s}") } else { s }
}

/// The base currency's code: the profile's (USD until a bundle names one).
pub fn base_ccy() -> String {
    finance_core::base::currency()
}

/// A currency's symbol as the amounts show it: "HK$", "€", "£", "US$" ...
pub fn symbol_of(ccy: &str) -> String {
    match ccy {
        "HKD" => "HK$",
        "EUR" => "€",
        "GBP" => "£",
        "USD" => "US$",
        "JPY" => "¥",
        "CNY" => "CN¥",
        "SGD" => "S$",
        "AUD" => "A$",
        "CAD" => "C$",
        "NZD" => "NZ$",
        "TWD" => "NT$",
        "MOP" => "MOP$",
        "INR" => "₹",
        "KRW" => "₩",
        other => return other.to_string(),
    }
    .to_string()
}

/// The base currency's symbol ("HK$").
pub fn sym() -> String {
    symbol_of(&base_ccy())
}

/// The symbol and its space ("HK$ "), to strip from a formatted amount.
pub fn sym_space() -> String {
    format!("{} ", sym())
}

/// A text written with Hong Kong dollars and the S&P 500 as the
/// benchmark in mind, for this profile: "HK$" becomes the base currency's symbol,
/// "HKD" its code, "S&P 500" the benchmark's name. Unchanged for HKD and the
/// default benchmark.
pub fn local(text: &str) -> String {
    let home = finance_core::base::get();
    local_for(&home.currency, &home.benchmark_label(), text)
}

fn local_for(ccy: &str, bench: &str, text: &str) -> String {
    let mut s = text.to_string();
    if ccy != "HKD" {
        s = s.replace("HK$", &symbol_of(ccy)).replace("HKD", ccy);
    }
    if bench != "S&P 500" {
        s = s.replace("S&P 500", bench);
    }
    s
}

/// [`local`] for a table header that must be `&'static str`: each distinct
/// text is leaked once per base currency (a handful in all).
pub fn local_static(text: &'static str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static LEAKED: Mutex<Option<HashMap<(String, String, &'static str), &'static str>>> = Mutex::new(None);
    let home = finance_core::base::get();
    let (ccy, bench) = (home.currency.clone(), home.benchmark_label());
    let out = local_for(&ccy, &bench, text);
    if out == text {
        return text;
    }
    let mut g = LEAKED.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    m.entry((ccy, bench, text)).or_insert_with(|| Box::leak(out.into_boxed_str()))
}

/// An amount in the base currency: "HK$ 1,234".
pub fn hkd(v: f64) -> String {
    amount_in(&base_ccy(), v)
}

/// An amount in any currency: "€ 1,234".
pub fn amount_in(ccy: &str, v: f64) -> String {
    format!("{} {}", symbol_of(ccy), int(v))
}

pub fn signed_hkd(v: f64) -> String {
    signed_in(&base_ccy(), v)
}

fn signed_in(ccy: &str, v: f64) -> String {
    let s = symbol_of(ccy);
    if private() {
        return format!("{s} {MASK}");
    }
    let sign = if v >= 0. { "+" } else { "−" };
    format!("{sign}{s} {}", int(v.abs()))
}

/// 1_348_629 -> "1.35M", 23_450 -> "23.5k"
pub fn compact(v: f64) -> String {
    if private() {
        return MASK.into();
    }
    let a = v.abs();
    let sign = if v < 0. { "−" } else { "" };
    if a >= 1e6 {
        format!("{sign}{:.2}M", a / 1e6)
    } else if a >= 1e3 {
        format!("{sign}{:.1}k", a / 1e3)
    } else {
        format!("{sign}{a:.0}")
    }
}

/// A signed figure with `dp` decimals and a real minus sign, like the
/// amounts: "+1.2", "−0.6" ("{:+}" gives a hyphen, narrower and lower than
/// "+" in the same column). What rounds to zero is "+0.0", not "−0.0".
pub fn signed(v: f64, dp: usize) -> String {
    let s = format!("{:.*}", dp, v.abs());
    let zero = s.bytes().all(|b| b == b'0' || b == b'.');
    format!("{}{s}", if v < 0. && !zero { "−" } else { "+" })
}

pub fn pct(v: f64) -> String {
    format!("{}%", signed(v, 1))
}

/// "synced 3 min ago"
pub fn ago(ms: i64) -> String {
    let secs = (chrono::Utc::now().timestamp_millis() - ms).max(0) / 1000;
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// "2026-09" -> "Sep 26"
pub fn month_label(ym: &str) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mo: usize = ym.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
    format!("{} {}", M[mo.clamp(1, 12) - 1], ym.get(2..4).unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_currency_formats_its_own_way() {
        assert_eq!(amount_in("HKD", 1234.4), "HK$ 1,234");
        assert_eq!(signed_in("HKD", -50.), "−HK$ 50");
        assert_eq!(local_for("HKD", "S&P 500", "Paid HK$, in HKD, vs the S&P 500"), "Paid HK$, in HKD, vs the S&P 500");
        assert_eq!((symbol_of("EUR"), symbol_of("GBP"), symbol_of("CHF")), ("€".to_string(), "£".to_string(), "CHF".to_string()));
        assert_eq!(amount_in("EUR", 1234.4), "€ 1,234");
        assert_eq!(signed_in("EUR", 50.), "+€ 50");
        assert_eq!(local_for("EUR", "MSCI World", "Paid HK$, in HKD, vs the S&P 500"), "Paid €, in EUR, vs the MSCI World");
        // the default base (no bundle loaded in a unit test) is neutral: USD
        assert_eq!(symbol_of("USD"), "US$");
        assert_eq!(local_static("In HK$"), "In US$");
    }

    #[test]
    fn formats() {
        assert_eq!(int(1_348_629.78), "1,348,630");
        assert_eq!(int(-950.0), "−950");
        assert_eq!(dec2(12345.0), "12,345.00");
        assert_eq!(compact(1_348_629.0), "1.35M");
        assert_eq!(month_label("2026-09"), "Sep 26");
        assert_eq!((pct(3.24), pct(-11.2), pct(-0.04)), ("+3.2%".into(), "−11.2%".into(), "+0.0%".into()));
        assert_eq!(signed(-47.63, 1), "−47.6");
    }
}
