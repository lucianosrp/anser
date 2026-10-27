//! Market prices: Binance public klines and
//! tickers for crypto, Yahoo's chart API for equities and indexes, the
//! deposit cost-basis backfill, and `refresh-prices`.
//!
//! All public endpoints, no keys. Every function returns an error on
//! transport failure except daily_close and monthly_closes, which swallow
//! it -- the callers' stale-badge fallbacks depend on exactly which ones fail.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use serde_json::Value;

use super::http::{self, escape};
use crate::db;
pub use crate::views::feed::Bar;

pub const BASE: &str = "https://api.binance.com";
pub const STABLE: [&str; 7] = ["USDT", "USDC", "USD", "BUSD", "FDUSD", "TUSD", "DAI"];

pub fn is_stable(a: &str) -> bool {
    STABLE.contains(&a)
}

const DAY_MS: i64 = 86_400_000;

fn ymd(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

pub fn month_key(ms: i64) -> String {
    ymd(ms).chars().take(7).collect()
}

fn f(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Parse a klines response into (open_ms, close) pairs.
pub fn parse_klines(v: &Value) -> Vec<(i64, f64)> {
    v.as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|k| Some((k.get(0)?.as_i64()?, f(k.get(4)?))))
                .collect()
        })
        .unwrap_or_default()
}

fn klines(query: &str, timeout: u64) -> Result<Value> {
    let agent = http::agent(Duration::from_secs(timeout));
    let r = http::get(&agent, &format!("{BASE}/api/v3/klines?{query}"), &[], "binance klines")?;
    if r.status != 200 {
        bail!("binance klines: HTTP {}", r.status);
    }
    r.json()
}

/// USD (USDT) daily close for `asset` on the day containing ts_ms; 0.0 on any
/// failure (the deposit backfill treats 0 as "retry next fetch").
pub fn daily_close(asset: &str, ts_ms: i64) -> f64 {
    if is_stable(asset) {
        return 1.0;
    }
    let start = ts_ms - ts_ms.rem_euclid(DAY_MS);
    klines(&format!("symbol={asset}USDT&interval=1d&startTime={start}&limit=1"), 20)
        .ok()
        .and_then(|v| parse_klines(&v).first().map(|k| k.1))
        .unwrap_or(0.0)
}

/// Month-end USDT closes from public klines: {YYYY-MM: close}. Empty on failure.
pub fn monthly_closes(asset: &str, start_ms: i64) -> BTreeMap<String, f64> {
    if is_stable(asset) {
        return BTreeMap::new();
    }
    klines(&format!("symbol={asset}USDT&interval=1M&startTime={start_ms}&limit=1000"), 20)
        .map(|v| parse_klines(&v).into_iter().map(|(t, c)| (month_key(t), c)).collect())
        .unwrap_or_default()
}

/// {YYYY-MM-DD (Monday candle open, UTC): close} weekly candles; one call
/// covers >19 years. Raises on failure.
pub fn weekly_closes(asset: &str) -> Result<BTreeMap<String, f64>> {
    let v = klines(&format!("symbol={asset}USDT&interval=1w&limit=1000"), 10)?;
    Ok(parse_klines(&v).into_iter().map(|(t, c)| (ymd(t), c)).collect())
}

/// {symbol: price} from a ticker/price response.
pub fn parse_ticker(v: &Value) -> BTreeMap<String, f64> {
    v.as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| Some((r.get("symbol")?.as_str()?.to_string(), f(r.get("price")?))))
                .collect()
        })
        .unwrap_or_default()
}

/// Current USD price per asset via one batched public ticker call.
///
/// Raises on transport/HTTP failure: callers rely on it to switch to the
/// snapshot fallback + stale badge, so an outage must never become a silent
/// 0.0 valuation. A symbol legitimately absent from a good response is 0.0.
pub fn live_prices(assets: &[String]) -> Result<BTreeMap<String, f64>> {
    let mut out: BTreeMap<String, f64> =
        assets.iter().filter(|a| is_stable(a)).map(|a| (a.clone(), 1.0)).collect();
    let symbols: Vec<String> = assets.iter().filter(|a| !is_stable(a)).map(|a| format!("\"{a}USDT\"")).collect();
    if symbols.is_empty() {
        return Ok(out);
    }
    let agent = http::agent(Duration::from_secs(5));
    // Binance's symbols param rejects whitespace: compact separators
    let q = escape(&format!("[{}]", symbols.join(",")));
    let mut r = http::get(&agent, &format!("{BASE}/api/v3/ticker/price?symbols={q}"), &[], "binance ticker")?;
    if r.status == 400 {
        // ONE unknown symbol (delisted dust swept up by a full-balance fetch)
        // fails the whole batch; the unfiltered list can't be broken by it.
        r = http::get(&agent, &format!("{BASE}/api/v3/ticker/price"), &[], "binance ticker")?;
    }
    if r.status != 200 {
        bail!("binance ticker: HTTP {}", r.status);
    }
    let by_symbol = parse_ticker(&r.json()?);
    for a in assets.iter().filter(|a| !is_stable(a)) {
        out.insert(a.clone(), by_symbol.get(&format!("{a}USDT")).copied().unwrap_or(0.0));
    }
    Ok(out)
}

// --- equities (Yahoo Finance chart API) -------------------------------------

pub const YF_BASE: &str = "https://query1.finance.yahoo.com/v8/finance/chart/";

/// Listing suffixes in preference order: bare (US), London, the main EUR
/// venues, Swiss, Paris. A UCITS ETF is cross-listed in DIFFERENT currencies
/// (EQQQ.L GBp, EQQQ.DE EUR, EQQQ.SW USD), so when the caller knows the
/// position's currency the matching listing wins.
const YF_SUFFIXES: [&str; 7] = ["", ".L", ".DE", ".AS", ".MI", ".SW", ".PA"];

fn yf_chart(symbol: &str, range: &str) -> Result<Value> {
    let agent = http::agent(Duration::from_secs(8));
    let url = format!("{YF_BASE}{}?interval=1d&range={}", escape(symbol), escape(range));
    let r = http::get(&agent, &url, &[], &format!("yahoo {symbol}"))?;
    if r.status != 200 {
        bail!("yahoo {symbol}: HTTP {}", r.status);
    }
    let v = r.json()?;
    v.pointer("/chart/result/0").filter(|x| !x.is_null()).cloned().ok_or_else(|| anyhow!("yahoo {symbol}: no result"))
}

/// Fold Yahoo's pence quote (GBp) onto its major unit for matching.
fn norm_ccy(c: &str) -> &str {
    if c == "GBp" {
        "GBP"
    } else {
        c
    }
}

/// Is this chart result a usable listing? Returns its scale to major units
/// (GBp lines need /100, else a GBX ETF is valued 100x) and its currency.
/// Dead pink-sheet shells (exchangeName PNK) and zero quotes are rejected.
pub fn listing_scale(res: &Value) -> Option<(f64, String)> {
    let meta = res.get("meta")?;
    if meta.get("exchangeName").and_then(Value::as_str) == Some("PNK") {
        return None;
    }
    let px = meta.get("regularMarketPrice").and_then(Value::as_f64).unwrap_or(0.0);
    if px <= 0.0 {
        return None;
    }
    let ccy = meta.get("currency").and_then(Value::as_str).unwrap_or("").to_string();
    Some((if ccy == "GBp" { 0.01 } else { 1.0 }, ccy))
}

/// (chart_result, scale) for the best listing of `symbol`; want_ccy pins a
/// cross-listed UCITS to the listing in that currency. A real listing in the
/// wrong currency is skipped: marking a USD position against a GBP line is
/// worse than the caller's cost-basis fallback.
fn yf_chart_resolved(symbol: &str, range: &str, want_ccy: Option<&str>) -> Result<(Value, f64)> {
    let mut last_err = None;
    for suffix in YF_SUFFIXES {
        let res = match yf_chart(&format!("{symbol}{suffix}"), range) {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        let Some((scale, ccy)) = listing_scale(&res) else { continue };
        if want_ccy.is_none_or(|w| norm_ccy(&ccy) == w) {
            return Ok((res, scale));
        }
    }
    Err(last_err.unwrap_or_else(|| {
        anyhow!("no {}Yahoo listing for {symbol}", if want_ccy.is_some() { "matching-currency " } else { "" })
    }))
}

/// Current price in the symbol's major currency unit. Raises on failure.
pub fn equity_now(symbol: &str) -> Result<f64> {
    let (res, scale) = yf_chart_resolved(symbol, "5d", None)?;
    let px = res.pointer("/meta/regularMarketPrice").and_then(Value::as_f64).context("no regularMarketPrice")?;
    Ok(px * scale)
}

/// Daily bars from one chart result; a missing open/high/low falls back to
/// the close, a null close drops the day.
pub fn parse_ohlc(res: &Value, scale: f64) -> BTreeMap<String, Bar> {
    let mut out = BTreeMap::new();
    let ts = res.get("timestamp").and_then(Value::as_array).cloned().unwrap_or_default();
    let q = res.pointer("/indicators/quote/0").cloned().unwrap_or(Value::Null);
    let col = |k: &str, i: usize| q.get(k).and_then(|a| a.get(i)).and_then(Value::as_f64);
    for (i, t) in ts.iter().enumerate() {
        let Some(t) = t.as_i64() else { continue };
        let Some(c) = col("close", i) else { continue };
        let pick = |k| col(k, i).unwrap_or(c) * scale;
        out.insert(ymd(t * 1000), (pick("open"), pick("high"), pick("low"), c * scale));
    }
    out
}

/// {YYYY-MM-DD: bar} in major currency units. Raises on failure.
pub fn equity_ohlc(symbol: &str, range: &str, want_ccy: Option<&str>) -> Result<BTreeMap<String, Bar>> {
    let (res, scale) = yf_chart_resolved(symbol, range, want_ccy)?;
    Ok(parse_ohlc(&res, scale))
}

/// Fill missing cost basis on deposit rows with the deposit-day close
/// (pre-owned coins landing on an exchange need a market-price basis).
/// Idempotent; a failed lookup (0.0) is skipped, not written, so the row is
/// retried next fetch instead of poisoned with a $0 basis.
pub fn backfill_deposit_cost(conn: &Connection) -> Result<usize> {
    let rows: Vec<(i64, i64, String, f64)> = conn
        .prepare("SELECT id, ts, asset, delta FROM crypto_events WHERE kind='deposit' AND usd IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut n = 0;
    for (id, ts, asset, delta) in rows {
        let px = daily_close(&asset, ts);
        if px <= 0.0 {
            continue;
        }
        conn.execute("UPDATE crypto_events SET usd=? WHERE id=?", rusqlite::params![delta * px, id])?;
        n += 1;
    }
    Ok(n)
}

/// `refresh-prices`: refetch every cached Yahoo symbol with full daily OHLC,
/// so rows cached before bars were stored get their open/high/low. Weekly
/// crypto candles (`*-1W`, from Binance) are skipped. Returns one line per
/// symbol for the CLI.
pub fn refresh_prices(conn: &Connection, range: &str) -> Result<Vec<String>> {
    let syms: Vec<(String, Option<String>)> = conn
        .prepare("SELECT symbol, MAX(currency) FROM equity_prices GROUP BY symbol")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut lines = Vec::new();
    for (sym, ccy) in syms {
        if sym.ends_with("-1W") {
            continue;
        }
        match equity_ohlc(&sym, range, ccy.as_deref()) {
            Ok(bars) => {
                let closes = bars.iter().map(|(d, b)| (d.clone(), b.3)).collect();
                db::upsert_equity_prices(conn, &sym, &closes, ccy.as_deref(), Some(&bars))?;
                lines.push(format!("  {sym:8} {} bars", bars.len()));
            }
            Err(e) => lines.push(format!("  {sym:8} failed: {}", e.to_string().chars().take(80).collect::<String>())),
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn klines_to_months() {
        let v = json!([[1704067200000i64, "1", "2", "0.5", "42000.5", "9"], [1706745600000i64, "1", "2", "0.5", "43000", "9"]]);
        let m: BTreeMap<String, f64> = parse_klines(&v).into_iter().map(|(t, c)| (month_key(t), c)).collect();
        assert_eq!(m, [("2024-01".to_string(), 42000.5), ("2024-02".to_string(), 43000.0)].into());
    }

    #[test]
    fn ticker_parse() {
        let v = json!([{"symbol": "BTCUSDT", "price": "65000.10"}, {"symbol": "ETHUSDT", "price": "3000"}]);
        let t = parse_ticker(&v);
        assert_eq!(t["BTCUSDT"], 65000.10);
        assert_eq!(t["ETHUSDT"], 3000.0);
    }

    #[test]
    fn stables_need_no_network() {
        let p = live_prices(&["USDT".into(), "USDC".into()]).unwrap();
        assert_eq!(p["USDT"], 1.0);
        assert_eq!(daily_close("DAI", 0), 1.0);
    }

    #[test]
    fn yahoo_pence_and_nulls() {
        let res = json!({
            "meta": {"currency": "GBp", "regularMarketPrice": 41000.0, "exchangeName": "LSE"},
            "timestamp": [1735862400i64, 1735948800i64, 1736035200i64],
            "indicators": {"quote": [{
                "open": [40000.0, null, 41000.0],
                "high": [40500.0, 40600.0, null],
                "low": [39900.0, 40000.0, 40900.0],
                "close": [40100.0, 40200.0, null]
            }]}
        });
        let (scale, ccy) = listing_scale(&res).unwrap();
        assert_eq!((scale, ccy.as_str()), (0.01, "GBp"));
        let bars = parse_ohlc(&res, scale);
        assert_eq!(bars.len(), 2, "null close drops the day");
        let b = bars["2025-01-04"];
        assert!((b.0 - 402.0).abs() < 1e-9, "missing open falls back to close");
        assert!((b.1 - 406.0).abs() < 1e-9);
        assert!((bars["2025-01-03"].3 - 401.0).abs() < 1e-9);
    }

    /// Manual smoke check against the PUBLIC endpoints (no keys):
    /// `cargo test -p finance-engine public_endpoints -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn public_endpoints_smoke() {
        std::env::remove_var("FINANCE_OFFLINE");
        let p = live_prices(&["BTC".into(), "ETH".into(), "USDT".into()]).unwrap();
        println!("live: {p:?}");
        assert!(p["BTC"] > 1000.0);
        let m = monthly_closes("BTC", 1_704_067_200_000);
        println!("monthly BTC: {} months, last {:?}", m.len(), m.iter().last());
        assert!(m.len() > 12);
        assert!(daily_close("ETH", 1_704_067_200_000) > 100.0);
        assert!(weekly_closes("BTC").unwrap().len() > 300);
        let now = equity_now("URTH").unwrap();
        let bars = equity_ohlc("EQQQ", "1mo", Some("GBP")).unwrap();
        println!("URTH now {now}; EQQQ GBP last {:?}", bars.iter().last());
        assert!(now > 10.0 && bars.len() > 10);
        let today = crate::config::utc_today();
        let fx = super::super::fx::fetch_exchange_rates(today - chrono::Duration::days(40), today, "USD", "HKD").unwrap();
        println!("USD/HKD: {fx:?}");
        assert!(fx.values().all(|r| *r > 7.0 && *r < 8.0));
    }

    #[test]
    fn pink_sheets_rejected() {
        let res = json!({"meta": {"currency": "USD", "regularMarketPrice": 0.0001, "exchangeName": "PNK"}});
        assert!(listing_scale(&res).is_none());
        let zero = json!({"meta": {"currency": "USD", "regularMarketPrice": 0.0}});
        assert!(listing_scale(&zero).is_none());
    }
}
