//! Network fetchers: the exchange and broker sources, market prices, FX and
//! the live-price helpers.
//!
//! * [`binance`], [`okx`], [`ibkr`] -- ledger/broker history -> SQLite, plus
//!   each source's `live_balances()`
//! * [`prices`] -- Binance public klines/tickers, Yahoo equity closes + OHLC
//! * [`fx`]     -- frankfurter monthly rates, closed months immutable
//! * [`live`]   -- TTL-cached live crypto prices with the snapshot fallback,
//!   and the bounded fan-out over every source's live balances
//! * [`http`]   -- the one GET-only client (offline switch, no secret logging)
//!
//! Read-only by construction: keys come from .env via config::env, every
//! call is a GET, and nothing here can place a trade or move money.

pub mod binance;
pub mod fx;
pub mod http;
pub mod ibkr;
pub mod live;
pub mod okx;
pub mod prices;
pub mod status;
pub mod verify;

use rusqlite::Connection;
use serde_json::Value;

use crate::config;

/// Progress lines. The CLI passes println, the server
/// its timestamped logger.
pub type Log<'a> = &'a dyn Fn(&str);

/// The API sources in sync order.
pub const API_SOURCES: [&str; 3] = ["binance", "okx", "ibkr"];

/// `fetch-crypto` for one source. Only Binance distinguishes a full walk.
/// Every attempt's outcome lands in [`status`] (the Doctor reads it).
pub fn fetch_source(conn: &Connection, key: &str, full: bool, log: Log) -> anyhow::Result<()> {
    let started = chrono::Utc::now().timestamp_millis();
    let res = match key {
        "binance" => binance::fetch(conn, full, log),
        "okx" => okx::fetch(conn, log),
        "ibkr" => ibkr::fetch(conn, log),
        _ => anyhow::bail!("unknown source {key}"),
    };
    // offline runs (tests, parity) never reached the network: nothing to record
    if !config::offline() {
        status::record(key, started, &res);
    }
    res
}

/// The live refresh's fetch of one source: Binance's quick pull (the
/// day-to-day endpoints; the full one stays the scheduled sync's job), the
/// usual fetch for the others. A quick pull isn't recorded in [`status`]:
/// it must not make a failing full sync look healthy to the Doctor.
pub fn refresh_source(conn: &Connection, key: &str, log: Log) -> anyhow::Result<()> {
    match key {
        "binance" => binance::fetch_quick(conn, log),
        _ => fetch_source(conn, key, false, log),
    }
}

/// Current holdings {asset: amount} for one source; raises on network/auth
/// failure (the caller falls back to the last snapshot + stale badge).
pub fn live_balances(key: &str) -> anyhow::Result<std::collections::BTreeMap<String, f64>> {
    match key {
        "binance" => binance::live_balances(),
        "okx" => okx::live_balances(),
        "ibkr" => Ok(ibkr::live_balances()),
        _ => anyhow::bail!("unknown source {key}"),
    }
}

// --- JSON helpers for loosely typed API payloads (numbers as strings) ------

/// A str-or-number field as f64; missing/null/garbage -> 0.
pub(crate) fn f(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        Some(Value::Bool(b)) => *b as u8 as f64,
        _ => 0.0,
    }
}

/// A str-or-number timestamp as i64.
pub(crate) fn num_i64(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|x| x as i64)).unwrap_or(0),
        Value::String(s) => s.trim().parse::<i64>().ok().or_else(|| s.trim().parse::<f64>().ok().map(|x| x as i64)).unwrap_or(0),
        _ => 0,
    }
}

/// Whether a JSON value counts as set: not null, false, 0, "", [] or {}.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// A value as stored text: null/missing -> "None", booleans -> "True" /
/// "False" (the forms the stored ids already carry).
pub(crate) fn stored_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.into(),
        Some(other) => other.to_string(),
    }
}

/// A value as stored text, `default` only when the key is absent.
pub(crate) fn istr(v: Option<&Value>, default: &str) -> String {
    match v {
        None => default.into(),
        some => stored_str(some),
    }
}

/// The parity snapshot (read-only), or None when it isn't on this machine --
/// tests that need personal data skip then.
#[cfg(test)]
pub(crate) fn parity_db() -> Option<Connection> {
    let Some(dir) = std::env::var_os("FINANCE_PARITY_DIR") else {
        eprintln!("FINANCE_PARITY_DIR unset, skipping");
        return None;
    };
    let path = std::path::Path::new(&dir).join("prod.db");
    if !path.exists() {
        eprintln!("parity snapshot absent ({}), skipping", path.display());
        return None;
    }
    Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}
