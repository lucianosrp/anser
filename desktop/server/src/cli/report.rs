//! `anser-server report [--from YYYY-MM --to YYYY-MM]`: text net-worth
//! summary: the live net worth and the last row
//! of the balance sheet.

use anyhow::Result;
use serde_json::Value;

use finance_engine::db;
use finance_engine::views::{api, sheet, Ctx};
use crate::feed::FEED;

use super::args::Args;

/// The symbol amounts print with: the base currency's ("HK$" for HKD).
fn symbol_of(ccy: &str) -> String {
    match ccy {
        "HKD" => "HK$".into(),
        "EUR" => "€".into(),
        "GBP" => "£".into(),
        "USD" => "US$".into(),
        "JPY" => "¥".into(),
        other => other.into(),
    }
}

#[cfg(test)]
fn hkd(v: f64) -> String {
    money("HK$", v)
}

fn money(sym: &str, v: f64) -> String {
    // HK$ 1,234,567
    let n = v.round() as i64;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{sym} {}{out}", if n < 0 { "-" } else { "" })
}

fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

pub fn run(rest: &[String]) -> Result<()> {
    let a = Args::parse(rest, &["--from", "--to"], &[])?;
    let conn = db::open()?;
    let ctx = Ctx::new(&conn, &FEED);
    let nw = api::get_net_worth_now(&ctx, true)?;
    let rows = sheet::build_balance_sheet(&ctx, a.value("--from").as_deref(), a.value("--to").as_deref()).rows_json();
    let last = rows.as_array().and_then(|r| r.last()).cloned();
    let base = finance_engine::views::fx::base_currency(&conn);
    let sym = symbol_of(&base);
    let hkd = |v: f64| money(&sym, v);

    let stale = nw.get("stale").and_then(Value::as_bool).unwrap_or(false);
    let as_of: String = nw.get("as_of").and_then(Value::as_str).unwrap_or("").chars().take(19).collect();
    println!("\nNet worth as of {as_of}{}", if stale { " (stale — using last snapshot)" } else { "" });
    if base == finance_engine::config::SECONDARY_CURRENCY {
        println!("  {}", hkd(num(&nw, "total_hkd")));
    } else {
        println!("  {}   /   {}", hkd(num(&nw, "total_hkd")), money("€", num(&nw, "total_eur")));
    }
    let owed = if num(&nw, "receivables_hkd") != 0.0 {
        format!("   ·   owed to you {}", hkd(num(&nw, "receivables_hkd")))
    } else {
        String::new()
    };
    println!("  banks {}   ·   crypto {}{owed}\n", hkd(num(&nw, "banks_hkd")), hkd(num(&nw, "crypto_hkd")));
    println!("  By source:");
    let mut src: Vec<&Value> = nw.get("by_source").and_then(Value::as_array).map(|v| v.iter().collect()).unwrap_or_default();
    src.sort_by(|x, y| num(y, "hkd_value").total_cmp(&num(x, "hkd_value")));
    for s in src {
        let label = s.get("label").and_then(Value::as_str).unwrap_or("");
        println!("    {label:22} {:>14}  ({:.1}%)", hkd(num(s, "hkd_value")), num(s, "pct"));
    }
    if let Some(row) = last {
        let month = row.get("month").map(|m| m.as_str().map(String::from).unwrap_or_else(|| m.to_string())).unwrap_or_default();
        println!("\n  Latest month in sheet: {month}  total {}", hkd(num(&row, "total_hkd")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn hkd_format() {
        assert_eq!(super::hkd(1234567.4), "HK$ 1,234,567");
        assert_eq!(super::hkd(-999.6), "HK$ -1,000");
        assert_eq!(super::hkd(12.0), "HK$ 12");
    }
}
