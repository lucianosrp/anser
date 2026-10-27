//! Monthly FX from frankfurter.dev, cached in `fx_rates`.
//!
//! Closed months are cached forever and never refetched; the current
//! calendar month is refetched on every call (its rate still moves). Missing
//! months cost one ranged HTTP call per pair (views::fx::fx_pairs: the
//! currencies in the data, quoted in the base currency), never one
//! per month, and a fully cached range without the current month costs none.
//! Offline or unreachable: whatever is cached is served (the balance sheet
//! forward-fills a gap in the current month), so pages never 500 on FX.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{Datelike, NaiveDate};
use rusqlite::{params, Connection};
use serde_json::Value;

use super::http;
use crate::config;

pub fn month_start(d: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap()
}

fn next_month(d: NaiveDate) -> NaiveDate {
    if d.month() == 12 {
        NaiveDate::from_ymd_opt(d.year() + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1).unwrap()
    }
}

pub fn month_end(d: NaiveDate) -> NaiveDate {
    next_month(month_start(d)).pred_opt().unwrap()
}

pub fn month_range(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    let (mut cur, end) = (month_start(start), month_start(end));
    let mut out = Vec::new();
    while cur <= end {
        out.push(cur);
        cur = next_month(cur);
    }
    out
}

/// frankfurter's `{rates: {YYYY-MM-DD: {SYM: rate}}}` -> {month start: last
/// rate of that month}.
pub fn parse_rates(v: &Value, symbol: &str) -> BTreeMap<NaiveDate, f64> {
    let mut daily: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    if let Some(rates) = v.get("rates").and_then(Value::as_object) {
        for (d, r) in rates {
            if let (Ok(day), Some(rate)) =
                (NaiveDate::parse_from_str(d, "%Y-%m-%d"), r.get(symbol).and_then(Value::as_f64))
            {
                daily.insert(day, rate);
            }
        }
    }
    // ascending by day, so the last insert per month is its last fixing
    let mut out = BTreeMap::new();
    for (d, r) in daily {
        out.insert(month_start(d), r);
    }
    out
}

pub fn fetch_exchange_rates(start: NaiveDate, end: NaiveDate, base: &str, symbol: &str) -> Result<BTreeMap<NaiveDate, f64>> {
    let agent = http::agent(Duration::from_secs(30));
    let url = format!("https://api.frankfurter.dev/v1/{start}..{end}?base={base}&symbols={symbol}");
    let r = http::get(&agent, &url, &[], "frankfurter")?;
    if r.status != 200 {
        bail!("frankfurter: HTTP {}", r.status);
    }
    Ok(parse_rates(&r.json().context("frankfurter")?, symbol))
}

fn upsert_rates(conn: &Connection, base: &str, quote: &str, rates: &BTreeMap<NaiveDate, f64>) -> Result<()> {
    let now = http::now_ms();
    for (m, r) in rates {
        conn.execute(
            "INSERT INTO fx_rates(base,quote,month,rate,fetched_at) VALUES(?,?,?,?,?) \
             ON CONFLICT(base,quote,month) DO UPDATE SET rate=excluded.rate, fetched_at=excluded.fetched_at",
            params![base, quote, m.to_string(), r, now],
        )?;
    }
    Ok(())
}

/// One month of X/base rates; `None` where nothing is cached.
#[derive(Debug, Clone, PartialEq)]
pub struct FxMonth {
    pub month: NaiveDate,
    /// keyed by the pair's currency (EUR, GBP, USD, JPY, ...)
    pub rates: BTreeMap<String, Option<f64>>,
}

/// Monthly X/base for every pair (views::fx::fx_pairs) over start..=end (by month).
/// One row per month with every pair's rate (eur_hkd, gbp_hkd, ...).
pub fn get_monthly_rates(conn: &Connection, start: NaiveDate, end: NaiveDate) -> Result<Vec<FxMonth>> {
    let months = month_range(start, end);
    let (Some(&lo), Some(&hi)) = (months.first(), months.last()) else { return Ok(vec![]) };
    let today = config::utc_today();
    let current = month_start(today);
    let mut out: Vec<FxMonth> = months.iter().map(|m| FxMonth { month: *m, rates: BTreeMap::new() }).collect();
    for (base, quote) in crate::views::fx::fx_pairs(conn) {
        let (base, quote) = (base.as_str(), quote.as_str());
        let cached = |conn: &Connection| -> Result<BTreeMap<String, f64>> {
            let mut st = conn.prepare(
                "SELECT month, rate FROM fx_rates WHERE base=? AND quote=? AND month BETWEEN ? AND ?",
            )?;
            let rows = st.query_map(params![base, quote, lo.to_string(), hi.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let have = cached(conn)?;
        let missing: Vec<NaiveDate> =
            months.iter().copied().filter(|m| !have.contains_key(&m.to_string()) || *m == current).collect();
        if let (Some(&first), Some(&last)) = (missing.iter().min(), missing.iter().max()) {
            let fetch_end = month_end(last).min(today);
            // offline / unreachable: serve whatever is cached
            if let Ok(fetched) = fetch_exchange_rates(first, fetch_end, base, quote) {
                upsert_rates(conn, base, quote, &fetched)?;
            }
        }
        let have = cached(conn)?;
        for row in out.iter_mut() {
            row.rates.insert(base.to_string(), have.get(&row.month.to_string()).copied());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    use serde_json::json;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn last_fixing_per_month() {
        let v = json!({"base": "EUR", "rates": {
            "2026-01-30": {"HKD": 9.1}, "2026-01-02": {"HKD": 9.0},
            "2026-02-27": {"HKD": 9.3}, "2026-02-02": {"HKD": 9.2}}});
        let r = parse_rates(&v, "HKD");
        assert_eq!(r, [(d("2026-01-01"), 9.1), (d("2026-02-01"), 9.3)].into());
    }

    #[test]
    fn month_helpers() {
        assert_eq!(month_end(d("2024-02-10")), d("2024-02-29"));
        assert_eq!(month_range(d("2025-11-15"), d("2026-02-01")).len(), 4);
    }

    #[test]
    fn closed_months_served_from_cache_offline() {
        std::env::set_var("FINANCE_OFFLINE", "1");
        let c = crate::db::memory().unwrap();
        c.execute_batch(
            "INSERT INTO fx_rates VALUES('EUR','HKD','2020-01-01',8.6,0),('USD','HKD','2020-01-01',7.77,0);",
        )
        .unwrap();
        let rows = get_monthly_rates(&c, d("2020-01-01"), d("2020-02-01")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rates["EUR"], Some(8.6));
        assert_eq!(rows[0].rates.get("GBP"), None, "no GBP in the data: no GBP pair");
        assert_eq!(rows[1].rates["USD"], None);
    }
}
