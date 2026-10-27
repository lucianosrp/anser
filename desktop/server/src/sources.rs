//! `GET /api/sources` (finance_core::sources has the shape): per data
//! source, whether its keys are configured, the last fetch and its error,
//! and how fresh the data is; for IBKR also the Flex sections, the newest
//! trade and positions snapshot, a Flex error in plain words, and the
//! pricing plan the commissions fit.
//!
//! Never a secret: a key is reported as configured or not, and error text
//! was scrubbed when engine::fetch::status stored it (scrubbed again here,
//! in case the file predates that).

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use finance_core::sources::{ApiSource, FieldState, FileSource, FlexError, Ibkr, Sections, Sources};
use finance_engine::{config, secrets};
use finance_engine::fetch::status::{self, SourceStatus};

/// The API sources, their labels and the .env keys each reads.
const API: [(&str, &str, &[&str]); 3] = [
    ("ibkr", "IBKR Flex", &["IBKR_FLEX_TOKEN", "IBKR_FLEX_QUERY_ID"]),
    ("binance", "Binance", &["BINANCE_API_KEY", "BINANCE_API_SECRET"]),
    ("okx", "OKX", &["OKX_API_KEY", "OKX_API_SECRET", "OKX_PASSPHRASE"]),
];

/// `hints`: give each key's last 4 characters (a paired device asks).
pub fn sources(conn: &Connection, hints: bool) -> Result<Sources> {
    let mut s = build(conn, &status::load(), &|k| config::env(k).is_some_and(|v| !v.trim().is_empty()), chrono::Utc::now().timestamp_millis())?;
    for a in s.api.iter_mut() {
        a.fields = a.env_keys.iter().map(|k| {
            let found = secrets::lookup(k);
            FieldState {
                key: k.clone(),
                set: found.is_some(),
                via: found.as_ref().map(|(_, v)| v.as_str().to_string()),
                hint: found.filter(|_| hints).map(|(v, _)| secrets::hint(&v)),
            }
        }).collect();
        a.configured_via = a.fields.iter().find_map(|f| f.via.clone());
    }
    Ok(s)
}

fn day_of_ms(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.date_naive().to_string())
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

/// The broker trades as the desktop ledger reads them (finance_core::ledger).
fn trades(conn: &Connection) -> Result<Vec<finance_core::ledger::Trade>> {
    let mut stmt = conn.prepare(
        "SELECT ts, symbol, currency, qty, COALESCE(price,0), COALESCE(amount,0), COALESCE(fx_to_base,1), COALESCE(commission,0)
         FROM ibkr_trades ORDER BY ts")?;
    let rows = stmt.query_map([], |r| Ok(finance_core::ledger::Trade {
        date: day_of_ms(r.get(0)?).unwrap_or_default(),
        symbol: r.get(1)?,
        currency: r.get(2)?,
        qty: r.get(3)?,
        price: r.get(4)?,
        amount: r.get(5)?,
        fx_to_base: r.get(6)?,
        commission: r.get::<_, f64>(7)?.abs(),
    }))?;
    // IBKR's own currency conversions ("USD.HKD", no cash) aren't trades
    Ok(rows.collect::<Result<Vec<_>, _>>()?.into_iter().filter(|t| !(t.symbol.contains('.') && t.amount == 0.0)).collect())
}

/// `configured`: is this .env key set (the value never leaves here).
pub fn build(conn: &Connection, log: &std::collections::BTreeMap<String, SourceStatus>, configured: &dyn Fn(&str) -> bool,
             now_ms: i64) -> Result<Sources> {
    let api = API.iter().map(|(key, label, keys)| {
        let st = log.get(*key).cloned().unwrap_or_default();
        let raw: Option<i64> = conn
            .query_row("SELECT MAX(fetched_at) FROM crypto_raw_sources WHERE exchange = ?", [key], |r| r.get(0))
            .optional().ok().flatten().flatten();
        ApiSource {
            key: key.to_string(),
            label: label.to_string(),
            configured: keys.iter().all(|k| configured(k)),
            env_keys: keys.iter().map(|k| k.to_string()).collect(),
            last_ok_ms: st.last_ok_ms.max(raw),
            last_attempt_ms: st.last_attempt_ms,
            last_error: st.last_error.map(|e| status::scrub(&e, &[])),
            configured_via: None,
            fields: vec![],
        }
    }).collect::<Vec<_>>();

    let mut files = Vec::new();
    let mut stmt = conn.prepare("SELECT source, MAX(txn_date), COUNT(*) FROM bank_transactions GROUP BY source ORDER BY source")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?)))? {
        let (source, newest, rows) = row?;
        files.push(FileSource { source, kind: "bank".into(), newest: newest.map(|d| d.chars().take(10).collect()), rows });
    }
    let mut stmt = conn.prepare(
        "SELECT exchange, MAX(ts), COUNT(*) FROM crypto_events WHERE exchange NOT IN ('binance', 'okx') GROUP BY exchange ORDER BY exchange")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, i64>(2)?)))? {
        let (source, ts, rows) = row?;
        files.push(FileSource { source, kind: "wallet".into(), newest: ts.and_then(day_of_ms), rows });
    }

    let ib = log.get("ibkr").cloned().unwrap_or_default();
    let latest_trade = conn.query_row("SELECT MAX(ts) FROM ibkr_trades", [], |r| r.get::<_, Option<i64>>(0)).ok().flatten().and_then(day_of_ms);
    let positions_as_of = conn.query_row("SELECT MAX(as_of) FROM ibkr_positions", [], |r| r.get::<_, Option<String>>(0)).ok().flatten();
    let sections = match &ib.sections {
        Some(s) => Sections {
            from: "statement".into(),
            trades: s.trades > 0,
            open_positions: s.open_positions > 0,
            cash_report: s.cash_report > 0,
            nav: s.nav > 0,
            cash_transactions: s.cash_transactions > 0,
            dividends: Some(s.dividends > 0),
            withholding: Some(s.withholding > 0),
        },
        // before the first logged fetch: what the tables show
        None => Sections {
            from: "stored".into(),
            trades: count(conn, "SELECT COUNT(*) FROM ibkr_trades") > 0,
            open_positions: count(conn, "SELECT COUNT(*) FROM ibkr_positions") > 0,
            cash_report: count(conn, "SELECT COUNT(*) FROM ibkr_cash") > 0,
            nav: count(conn, "SELECT COUNT(*) FROM ibkr_nav") > 0,
            cash_transactions: count(conn, "SELECT COUNT(*) FROM ibkr_cash_flows") > 0,
            // stored income proves the query has them; none proves nothing yet
            dividends: (count(conn, "SELECT COUNT(*) FROM ibkr_income WHERE kind NOT LIKE '%withholding%'") > 0).then_some(true),
            withholding: (count(conn, "SELECT COUNT(*) FROM ibkr_income WHERE kind LIKE '%withholding%'") > 0).then_some(true),
        },
    };
    let flex_error = ib.last_error.as_deref().and_then(status::flex_error).map(|(code, kind, words)| FlexError {
        code, kind: kind.into(), words: words.into(),
    });
    let today = day_of_ms(now_ms).unwrap_or_default();
    let plan = trades(conn).ok().and_then(|t| finance_core::fees::infer_plan(&finance_core::fees::reprice(&t), &today));
    let has_ibkr = latest_trade.is_some() || positions_as_of.is_some() || api[0].configured || ib.last_attempt_ms.is_some();
    let ibkr = has_ibkr.then_some(Ibkr { latest_trade, positions_as_of, sections, flex_error, plan });
    Ok(Sources { generated_ms: now_ms, api, files, ibkr })
}

#[cfg(test)]
mod tests {
    use super::*;
    use finance_engine::fetch::status::FlexSections;
    use std::collections::BTreeMap;

    /// The pages place an account by finance_core::base::default_country;
    /// Settings shows the catalog's: the same map.
    #[test]
    fn the_catalog_and_the_pages_agree_on_account_countries() {
        for f in finance_engine::catalog::files() {
            assert_eq!(f.country, finance_core::base::default_country(f.key), "{}", f.key);
        }
        assert_eq!(finance_core::base::default_country("nonsense"), None);
    }

    fn db() -> Connection {
        let c = finance_engine::db::memory().unwrap();
        c.execute_batch(
            "INSERT INTO bank_transactions(source, account, txn_date, amount, currency, description, raw_hash, source_file, ingested_at)
               VALUES ('n26', 'main', '2026-09-01', -5, 'EUR', 'coffee', 'h1', 'n26.csv', 0);
             INSERT INTO ibkr_trades VALUES ('T1', 1790778600000, 'VT', '', 'USD', 10, 50, -501, -1, 7.8);
             INSERT INTO ibkr_positions VALUES ('VT', '', 'USD', 1, 100, 100, 100, 780, 'HKD', '2026-10-01');
             INSERT INTO crypto_raw_sources VALUES ('okx', 'convert', '{}', 1790000000000);",
        )
        .unwrap();
        c
    }

    #[test]
    fn reports_state_without_secrets() {
        let c = db();
        let mut log = BTreeMap::new();
        log.insert("ibkr".to_string(), SourceStatus {
            last_attempt_ms: Some(1_790_900_000_000),
            last_ok_ms: Some(1_790_000_000_000),
            last_error: Some("ibkr: Flex SendRequest: error 1012: Token has expired.".into()),
            sections: Some(FlexSections { trades: 2, open_positions: 1, cash_report: 3, nav: 1, cash_transactions: 2, dividends: 0, withholding: 0 }),
            runs: vec![],
        });
        // a stale file from before scrubbing: still no URL out
        log.insert("binance".to_string(), SourceStatus { last_error: Some("GET https://api.binance.com/x?signature=abc failed".into()), ..Default::default() });
        let secret = "SUPERSECRETVALUE";
        let s = build(&c, &log, &|k| k.starts_with("IBKR") || k.starts_with("OKX"), 1_791_000_000_000).unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains(secret) && !json.contains("https://") && !json.contains("signature=abc"), "{json}");

        let ib = &s.api[0];
        assert_eq!((ib.key.as_str(), ib.configured), ("ibkr", true));
        assert!(!s.api[1].configured, "binance keys unset");
        assert_eq!(s.api[2].last_ok_ms, Some(1_790_000_000_000), "okx: the newest raw payload when the log has nothing");
        let x = s.ibkr.unwrap();
        assert_eq!(x.latest_trade.as_deref(), Some("2026-09-30"));
        assert_eq!(x.positions_as_of.as_deref(), Some("2026-10-01"));
        assert_eq!((x.sections.from.as_str(), x.sections.dividends), ("statement", Some(false)));
        assert_eq!(x.flex_error.map(|e| e.kind), Some("token_expired".into()));
        assert_eq!(x.plan.map(|p| p.plan), Some("Fixed".into()), "a $1 commission on $500 of VT is Fixed's minimum");
        assert_eq!(s.files[0].source, "n26");
        assert_eq!(s.files[0].newest.as_deref(), Some("2026-09-01"));
    }

    #[test]
    fn sections_from_the_tables_before_any_logged_fetch() {
        let s = build(&db(), &BTreeMap::new(), &|_| false, 1_791_000_000_000).unwrap();
        let x = s.ibkr.unwrap();
        assert_eq!((x.sections.from.as_str(), x.sections.trades, x.sections.cash_report, x.sections.dividends), ("stored", true, false, None));
        assert!(x.flex_error.is_none());
    }
}
