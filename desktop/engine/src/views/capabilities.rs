//! What this server has: every data source (configured, has data, its first
//! and last row, the last sync) and the features they make possible, so a
//! client shows only the pages that have something to show. Served as
//! GET /api/capabilities and as the bundle's `capabilities` section (Rust
//! only and additive: older clients ignore it, and a client reading an
//! older server's bundle, which lacks it, takes every feature as on).
//!
//! Never a secret: an API source reports whether its .env keys are set, not
//! their values. The per-source rows are memoized on the db fingerprint;
//! `configured` and `last_sync` (the .env and data/fetch_status.json, which
//! the fingerprint doesn't see) are read on every call, both cheap.

use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};

use super::{cache, receivables, Ctx};
use crate::fetch::status;
use crate::sources::{self, BalanceMethod, Kind};

/// The API sources: key, label, kind, group, the .env keys they need.
pub const API_SOURCES: [(&str, &str, &str, &str, &[&str]); 3] = [
    ("binance", "Binance", "exchange", "crypto", &["BINANCE_API_KEY", "BINANCE_API_SECRET"]),
    ("okx", "OKX", "exchange", "crypto", &["OKX_API_KEY", "OKX_API_SECRET", "OKX_PASSPHRASE"]),
    ("ibkr", "Interactive Brokers", "broker", "broker", &["IBKR_FLEX_TOKEN", "IBKR_FLEX_QUERY_ID"]),
];

/// The features a client turns pages on with.
pub const FEATURES: [&str; 8] = ["banks", "spending", "crypto", "broker", "fund_proxy", "receivables", "travel", "cashflow"];

fn day_of_ms(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.date_naive().to_string())
}

/// (rows, first, last) of one query returning COUNT(*), MIN, MAX as text.
fn span(ctx: &Ctx, sql: &str, key: &str) -> (i64, Option<String>, Option<String>) {
    ctx.conn
        .query_row(sql, [key], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)))
        .unwrap_or((0, None, None))
}

fn span_ms(ctx: &Ctx, sql: &str, key: &str) -> (i64, Option<String>, Option<String>) {
    let (n, a, b) = ctx.conn
        .query_row(sql, [key], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?)))
        .unwrap_or((0, None, None));
    (n, a.and_then(day_of_ms), b.and_then(day_of_ms))
}

fn source(key: &str, kind: &str, group: &str, label: &str, (rows, first, last): (i64, Option<String>, Option<String>)) -> Value {
    json!({"key": key, "kind": kind, "group": group, "label": label, "configured": true, "has_data": rows > 0,
           "rows": rows, "first": first.map(|d| d.chars().take(10).collect::<String>()),
           "last": last.map(|d| d.chars().take(10).collect::<String>()), "last_sync": null})
}

/// The db-derived part (memoized): sources with their rows, and features.
fn data(ctx: &Ctx) -> anyhow::Result<Value> {
    let mut out = Vec::new();
    for s in sources::file_sources() {
        let spec = s.spec();
        let (kind, group, rows) = match spec.kind {
            Kind::Bank if spec.balance_method == BalanceMethod::None => ("card", "cards",
                span(ctx, "SELECT COUNT(*), MIN(txn_date), MAX(txn_date) FROM bank_transactions WHERE source = ?", spec.key)),
            Kind::Bank => ("bank", "banks",
                span(ctx, "SELECT COUNT(*), MIN(txn_date), MAX(txn_date) FROM bank_transactions WHERE source = ?", spec.key)),
            // the fund executions carry no source column: they are all hsbc_sms's
            Kind::FundUnits => ("fund_units", "funds",
                span(ctx, "SELECT COUNT(*), MIN(trade_date), MAX(trade_date) FROM fund_subscriptions WHERE ? <> ''", spec.key)),
            Kind::CryptoWallet => ("crypto_wallet", "crypto",
                span_ms(ctx, "SELECT COUNT(*), MIN(ts), MAX(ts) FROM crypto_events WHERE exchange = ?", spec.key)),
        };
        // a format definition (built in or installed) is a source only once
        // it has rows: an unused one is a parser, not an account
        if rows.0 == 0 && sources::format::get(spec.key).is_some() {
            continue;
        }
        out.push(source(spec.key, kind, group, spec.label, rows));
    }
    for (key, label, kind, group, _) in API_SOURCES {
        let rows = if key == "ibkr" {
            span_ms(ctx, "SELECT COUNT(*), MIN(ts), MAX(ts) FROM ibkr_trades WHERE ? <> ''", key)
        } else {
            span_ms(ctx, "SELECT COUNT(*), MIN(ts), MAX(ts) FROM crypto_events WHERE exchange = ?", key)
        };
        let mut v = source(key, kind, group, label, rows);
        if key == "ibkr" && v["rows"] == 0 {
            // a broker holding positions bought before the statement window
            let pos: i64 = ctx.conn.query_row("SELECT COUNT(*) FROM ibkr_positions", [], |r| r.get(0)).unwrap_or(0);
            v["has_data"] = json!(pos > 0);
        }
        out.push(v);
    }

    let has = |pred: &dyn Fn(&Value) -> bool| out.iter().any(|s| pred(s) && s["has_data"] == true);
    let banks = has(&|s| s["kind"] == "bank");
    let spending = banks || has(&|s| s["kind"] == "card");
    let crypto = has(&|s| s["group"] == "crypto");
    let broker = has(&|s| s["kind"] == "broker");
    let fund_proxy = cache::flexinvest(ctx, None, None)
        .map(|f| f.get("rows").and_then(Value::as_array).is_some_and(|r| !r.is_empty()))
        .unwrap_or(false);
    let receivables = receivables::has_rows(ctx);
    let mut features = Map::new();
    for (k, on) in [("banks", banks), ("spending", spending), ("crypto", crypto), ("broker", broker), ("fund_proxy", fund_proxy),
                    ("receivables", receivables), ("travel", spending), ("cashflow", banks)] {
        features.insert(k.into(), json!(on));
    }
    Ok(json!({"sources": out, "features": features}))
}

/// The capabilities: server, sources (with `configured` and `last_sync`
/// current), features.
pub fn capabilities(ctx: &Ctx) -> Value {
    let mut v = cache::view(ctx, "capabilities", "", || data(ctx)).map(|v| (*v).clone())
        .unwrap_or_else(|e| json!({"sources": [], "features": {}, "error": e.to_string()}));
    let log = status::load();
    if let Some(list) = v.get_mut("sources").and_then(Value::as_array_mut) {
        for s in list.iter_mut() {
            let key = s["key"].as_str().unwrap_or("").to_string();
            let last_sync = match API_SOURCES.iter().find(|a| a.0 == key) {
                Some((_, _, _, _, env)) => {
                    let via: Vec<Option<crate::secrets::Via>> = env.iter().map(|k| crate::secrets::configured_via(k)).collect();
                    s["configured"] = json!(via.iter().all(Option::is_some));
                    // where the keys come from: store, .env or env (the
                    // first provider that answers for the first key)
                    s["configured_via"] = json!(via.iter().flatten().next().map(|v| v.as_str()));
                    let raw: Option<i64> = ctx.conn
                        .query_row("SELECT MAX(fetched_at) FROM crypto_raw_sources WHERE exchange = ?", [&key], |r| r.get(0))
                        .optional().ok().flatten().flatten();
                    log.get(&key).and_then(|st| st.last_ok_ms).max(raw)
                }
                // a file source syncs when a file of it is ingested
                None => ctx.conn
                    .query_row("SELECT MAX(ingested_at) FROM ingested_files WHERE source = ?", [&key], |r| r.get::<_, Option<i64>>(0))
                    .ok().flatten(),
            };
            s["last_sync"] = json!(last_sync);
        }
    }
    let mut out = Map::new();
    out.insert("server".into(), json!({"kind": "anser-server", "version": finance_core::VERSION}));
    // moves on every write through the category APIs: a device whose copy
    // carries an older one is behind
    out.insert("data_rev".into(), json!(crate::categories::data_rev(ctx.conn)));
    if let Some(obj) = v.as_object_mut() {
        for (k, val) in std::mem::take(obj) {
            out.insert(k, val);
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::Offline;

    #[test]
    fn empty_db_has_sources_but_no_features() {
        let conn = crate::db::memory().unwrap();
        let ctx = Ctx::new(&conn, &Offline);
        let c = capabilities(&ctx);
        assert_eq!(c["server"]["kind"], "anser-server");
        let srcs = c["sources"].as_array().unwrap();
        assert!(srcs.iter().any(|s| s["key"] == "n26" && s["kind"] == "bank" && s["has_data"] == false));
        assert!(srcs.iter().any(|s| s["key"] == "mox" && s["kind"] == "bank" && s["has_data"] == false));
        assert!(srcs.iter().any(|s| s["key"] == "hsbc_card" && s["kind"] == "card"));
        assert!(srcs.iter().any(|s| s["key"] == "ibkr" && s["kind"] == "broker" && s["configured"].is_boolean()));
        for f in FEATURES {
            assert_eq!(c["features"][f], false, "{f}");
        }
    }

    #[test]
    fn rows_turn_features_on() {
        let conn = crate::db::memory().unwrap();
        conn.execute_batch(
            "INSERT INTO bank_transactions(source, account, txn_date, amount, currency, description, raw_hash, source_file, ingested_at)
               VALUES ('hsbc', 'savings', '2026-01-05', -10, 'HKD', 'coffee', 'h1', 'a.csv', 1790000000000);
             INSERT INTO ingested_files(path, source, file_hash, row_count, rows_added, ingested_at) VALUES ('a.csv', 'hsbc', 'x', 1, 1, 1790000000000);
             INSERT INTO crypto_events(exchange, ts, asset, delta, usd, kind, detail) VALUES ('trezor', 1780000000000, 'BTC', 0.1, 100, 'deposit', '');",
        ).unwrap();
        let ctx = Ctx::new(&conn, &Offline);
        let c = capabilities(&ctx);
        let f = &c["features"];
        assert_eq!((f["banks"].as_bool(), f["spending"].as_bool(), f["crypto"].as_bool(), f["broker"].as_bool()),
                   (Some(true), Some(true), Some(true), Some(false)));
        let hsbc = c["sources"].as_array().unwrap().iter().find(|s| s["key"] == "hsbc").unwrap().clone();
        assert_eq!((hsbc["rows"].as_i64(), hsbc["first"].as_str(), hsbc["last_sync"].as_i64()),
                   (Some(1), Some("2026-01-05"), Some(1_790_000_000_000)));
        assert!(!c.to_string().contains("_API_"), "not even the .env key names");
    }
}
