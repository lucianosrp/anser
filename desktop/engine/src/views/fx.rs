//! FX, read side: the fx_rates cache (monthly rates, the rate resolver, the
//! current and latest cached rates). Fetching goes through
//! MarketFeed::fx_monthly; when it fails the cache is served as is.
//!
//! Every amount is converted to the profile's base currency
//! ([`base_currency`], HKD by default). The pairs ([`fx_pairs`]) are derived
//! from the currencies in the data (bank rows, the broker's positions,
//! cash and trades) plus USD (crypto and the index proxies are priced in
//! it) and EUR (the net worth's second figure), each quoted against the
//! base. The `_hkd` names (fields, columns) stay in bundle version 1 and
//! mean "in the base currency".

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use rusqlite::{params, Connection};

use super::{month_range, month_start, next_month, Ctx};

/// The base currency: the profile's, else config::BASE_CURRENCY.
pub fn base_currency(conn: &Connection) -> String {
    crate::profile::get(conn)
        .ok()
        .flatten()
        .map(|p| p.base_currency.trim().to_uppercase())
        .filter(|c| c.len() == 3)
        .unwrap_or_else(|| crate::config::BASE_CURRENCY.to_string())
}

/// The order pairs come in (the bundle's fx section, the fetch): these
/// first, then the rest alphabetically.
const PAIR_ORDER: [&str; 4] = ["EUR", "GBP", "USD", "JPY"];
/// Always quoted (crypto and the proxies are in USD; the net worth's second
/// figure is in EUR), whatever the data holds.
const ALWAYS: [&str; 2] = ["USD", crate::config::SECONDARY_CURRENCY];

/// The FX pairs (currency, base): every currency the data holds, plus
/// [`ALWAYS`], minus the base. HKD data with EUR, GBP, JPY and USD rows
/// gives exactly the four pairs the views always had.
pub fn fx_pairs(conn: &Connection) -> Vec<(String, String)> {
    let base = base_currency(conn);
    let mut seen: Vec<String> = ALWAYS.iter().map(|s| s.to_string()).collect();
    for sql in [
        "SELECT DISTINCT currency FROM bank_transactions",
        "SELECT DISTINCT currency FROM ibkr_positions UNION SELECT DISTINCT base_ccy FROM ibkr_positions",
        "SELECT DISTINCT currency FROM ibkr_cash",
        "SELECT DISTINCT currency FROM ibkr_trades",
    ] {
        if let Ok(mut st) = conn.prepare_cached(sql) {
            if let Ok(rows) = st.query_map([], |r| r.get::<_, Option<String>>(0)) {
                for c in rows.flatten().flatten() {
                    let c = c.trim().to_uppercase();
                    if c.len() == 3 && c.chars().all(|ch| ch.is_ascii_alphabetic()) && !seen.contains(&c) {
                        seen.push(c);
                    }
                }
            }
        }
    }
    seen.retain(|c| *c != base);
    seen.sort_by_key(|c| (PAIR_ORDER.iter().position(|p| p == c).unwrap_or(PAIR_ORDER.len()), c.clone()));
    seen.into_iter().map(|c| (c, base.clone())).collect()
}

/// F(currency, 'YYYY-MM') -> base rate. The base
/// = 1; a missing month falls back to the nearest earlier cached month, then
/// the earliest available; an unknown currency is 0.0. Only pairs quoted in
/// the base are read.
pub struct Rates {
    base: String,
    /// (base, its series): a handful of currencies, so a scan beats hashing
    /// the currency on every one of the thousands of lookups per view
    by_base: Vec<(String, Series)>,
}

/// One base's [(ym, rate)] by month, and the months as integers when they
/// are all 'YYYY-MM' (the same order, compared without touching strings).
struct Series {
    rows: Vec<(String, f64)>,
    keys: Option<Vec<u32>>,
}

/// 'YYYY-MM' as YYYYMM; None for anything else. For fixed-width digit
/// strings the numeric order is the string order.
fn ym_key(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    if b.len() != 7 || b[4] != b'-' {
        return None;
    }
    let d = |i: usize| (b[i] as char).to_digit(10);
    Some((d(0)? * 1000 + d(1)? * 100 + d(2)? * 10 + d(3)?) * 100 + d(5)? * 10 + d(6)?)
}

impl Rates {
    pub fn load(conn: &Connection) -> Rates {
        let base = base_currency(conn);
        let mut by_base: Vec<(String, Vec<(String, f64)>)> = Vec::new();
        if let Ok(mut stmt) = conn.prepare_cached("SELECT base, month, rate FROM fx_rates WHERE quote = ?1 ORDER BY month") {
            let rows = stmt.query_map([&base], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?))
            });
            if let Ok(rows) = rows {
                for (base, month, rate) in rows.flatten() {
                    let ym: String = month.chars().take(7).collect();
                    match by_base.iter_mut().find(|(b, _)| *b == base) {
                        Some((_, v)) => v.push((ym, rate)),
                        None => by_base.push((base, vec![(ym, rate)])),
                    }
                }
            }
        }
        let by_base = by_base
            .into_iter()
            .map(|(b, rows)| {
                let keys = rows.iter().map(|(m, _)| ym_key(m)).collect::<Option<Vec<u32>>>();
                (b, Series { rows, keys })
            })
            .collect();
        Rates { base, by_base }
    }

    /// The currency every rate converts into.
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn rate(&self, currency: &str, ym: &str) -> f64 {
        if currency == self.base {
            return 1.0;
        }
        let Some((_, series)) = self.by_base.iter().find(|(b, _)| b == currency) else { return 0.0 };
        let rows = &series.rows;
        if rows.is_empty() {
            return 0.0;
        }
        // last month <= ym; none -> the earliest
        let i = match (&series.keys, ym_key(ym)) {
            (Some(keys), Some(k)) => keys.partition_point(|m| *m <= k),
            _ => rows.partition_point(|(m, _)| m.as_str() <= ym),
        };
        if i == 0 { rows[0].1 } else { rows[i - 1].1 }
    }
}

/// Per month in start..=end, the cached rate of every
/// FX pair (None where nothing is cached). Refetches missing months and
/// always the current one through the feed first; a failed fetch leaves the
/// cache as is.
pub struct MonthlyRates {
    pub months: Vec<NaiveDate>,
    /// the currency of each column (fx_pairs order), all quoted in `base`
    pub ccys: Vec<String>,
    pub base: String,
    /// [pair][month]
    pub cols: Vec<Vec<Option<f64>>>,
}

impl MonthlyRates {
    /// A currency's column (the base needs none; a
    /// currency with no pair gets flagged no_fx).
    pub fn col(&self, ccy: &str) -> Option<usize> {
        self.ccys.iter().position(|c| c == ccy)
    }

    /// A currency's rates per month, forward-filled, 0 before the first
    /// (the base: 1 throughout).
    pub fn filled(&self, ccy: &str) -> Vec<f64> {
        if ccy == self.base {
            return vec![1.0; self.months.len()];
        }
        let mut last = 0.0;
        match self.col(ccy) {
            Some(i) => self.cols[i]
                .iter()
                .map(|v| {
                    if let Some(x) = v {
                        last = *x;
                    }
                    last
                })
                .collect(),
            None => vec![0.0; self.months.len()],
        }
    }
}

pub fn monthly_rates(ctx: &Ctx, start: NaiveDate, end: NaiveDate) -> MonthlyRates {
    let conn = ctx.conn;
    let months = month_range(start, end);
    let (lo, hi) = (months[0], *months.last().unwrap());
    let current = month_start(ctx.today);
    let pairs = fx_pairs(conn);
    let mut cols: Vec<Vec<Option<f64>>> = vec![Vec::new(); pairs.len()];
    for (pi, (base, quote)) in pairs.iter().enumerate() {
        let have: HashSet<String> = conn
            .prepare_cached("SELECT month FROM fx_rates WHERE base=? AND quote=? AND month BETWEEN ? AND ?")
            .and_then(|mut s| {
                s.query_map(params![base, quote, lo.to_string(), hi.to_string()], |r| r.get::<_, String>(0))?
                    .collect()
            })
            .unwrap_or_default();
        let missing: Vec<NaiveDate> =
            months.iter().copied().filter(|m| !have.contains(&m.to_string()) || *m == current).collect();
        if let (Some(first), Some(last)) = (missing.first(), missing.last()) {
            let fetch_end = (next_month(*last) - chrono::Duration::days(1)).min(ctx.today);
            if let Ok(rows) = ctx.feed.fx_monthly(base, quote, *first, fetch_end) {
                upsert_rates(conn, base, quote, &rows);
                ctx.invalidate_rates();
            }
        }
        let cached: HashMap<String, f64> = conn
            .prepare_cached("SELECT month, rate FROM fx_rates WHERE base=? AND quote=? AND month BETWEEN ? AND ?")
            .and_then(|mut s| {
                s.query_map(params![base, quote, lo.to_string(), hi.to_string()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })?
                .collect()
            })
            .unwrap_or_default();
        cols[pi] = months.iter().map(|m| cached.get(&m.to_string()).copied()).collect();
    }
    let ccys = pairs.iter().map(|(c, _)| c.clone()).collect();
    MonthlyRates { months, ccys, base: base_currency(conn), cols }
}

/// fx._upsert_rates.
fn upsert_rates(conn: &Connection, base: &str, quote: &str, rows: &[(NaiveDate, f64)]) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    for (m, r) in rows {
        let _ = conn.execute(
            "INSERT INTO fx_rates(base,quote,month,rate,fetched_at) VALUES(?,?,?,?,?) \
             ON CONFLICT(base,quote,month) DO UPDATE SET rate=excluded.rate, fetched_at=excluded.fetched_at",
            params![base, quote, m.to_string(), r, now_ms],
        );
    }
}

/// The newest cached rate per pair.
pub fn latest_cached_fx_rates(conn: &Connection) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for (base, quote) in fx_pairs(conn) {
        let r: Option<f64> = conn
            .query_row(
                "SELECT rate FROM fx_rates WHERE base=? AND quote=? ORDER BY month DESC LIMIT 1",
                params![base, quote],
                |r| r.get(0),
            )
            .ok();
        if let Some(r) = r {
            out.push((base.to_string(), r));
        }
    }
    out
}

/// {ccy: base rate} for valuing "now". The
/// cached current month when every pair has it; else (live) one fetch of
/// this month with a per-currency fallback to the latest cached rate; else
/// the latest cached rates. Ordered like fx_pairs.
pub fn current_fx_rates(ctx: &Ctx, live: bool) -> Vec<(String, f64)> {
    let conn = ctx.conn;
    let current = month_start(ctx.today).to_string();
    let pairs = fx_pairs(conn);
    let mut cached = Vec::new();
    for (base, quote) in &pairs {
        let r: Option<f64> = conn
            .query_row(
                "SELECT rate FROM fx_rates WHERE base=? AND quote=? AND month=?",
                params![base, quote, current],
                |r| r.get(0),
            )
            .ok();
        if let Some(r) = r {
            cached.push((base.to_string(), r));
        }
    }
    if cached.len() == pairs.len() {
        return cached;
    }
    if !live {
        return latest_cached_fx_rates(conn);
    }
    let today = month_start(ctx.today);
    let df = monthly_rates(ctx, today, today);
    let fetched: Vec<Option<f64>> = df.cols.iter().map(|c| c.last().copied().flatten()).collect();
    // a just-started month yields NULL rates: fall back per currency to the
    // last cached rate rather than valuing everything at 0
    if fetched.iter().any(|v| v.is_some_and(|x| x != 0.0)) {
        let latest = latest_cached_fx_rates(conn);
        return pairs
            .iter()
            .zip(fetched)
            .map(|((base, _), v)| {
                let v = match v {
                    Some(x) if x != 0.0 => x,
                    _ => latest.iter().find(|(k, _)| k == base).map(|(_, r)| *r).unwrap_or(0.0),
                };
                (base.clone(), v)
            })
            .collect();
    }
    latest_cached_fx_rates(conn)
}

/// A currency's rate in the ordered rate list.
pub fn get_rate(rates: &[(String, f64)], ccy: &str) -> Option<f64> {
    rates.iter().find(|(k, _)| k == ccy).map(|(_, r)| *r)
}
