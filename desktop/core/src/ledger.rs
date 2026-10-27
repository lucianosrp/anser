//! Every money movement the replica knows, flattened into one in-memory list
//! (a few thousand rows: loading is milliseconds, scanning is microseconds).

// the web build (no `native`) only deserializes: the SQLite helpers go unused
#![cfg_attr(not(feature = "native"), allow(dead_code))]

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::DateTime;
#[cfg(feature = "native")]
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Bank,
    Card,
    Crypto,
    Broker,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Bank => "Bank",
            Kind::Card => "Card",
            Kind::Crypto => "Crypto",
            Kind::Broker => "Broker",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    /// "YYYY-MM-DD"
    pub date: String,
    /// bank source / exchange / "ibkr"
    pub source: String,
    pub account: String,
    pub description: String,
    pub category: String,
    /// native amount (crypto: the asset quantity)
    pub amount: f64,
    /// native currency / crypto asset / broker currency
    pub currency: String,
    /// signed HKD at the month's rate (crypto: from the event's USD value)
    pub hkd: Option<f64>,
    /// the owner's share of the row (shared rent), 1.0 otherwise
    pub share: f64,
    /// lowercased text the search matches against
    pub haystack: String,
    /// the bank row's natural key with `source` (bank and card rows only):
    /// what the category APIs name a row by
    #[serde(default)]
    pub raw_hash: String,
}

/// One broker execution, kept structured for per-holding maths (the Entry
/// for it is the human-readable line in the ledger).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    pub date: String,
    pub symbol: String,
    /// trade currency (the instrument's)
    pub currency: String,
    /// + bought, - sold
    pub qty: f64,
    pub price: f64,
    /// cash in the trade currency: - paid for a buy, + received for a sell
    pub amount: f64,
    /// commission in the trade currency (positive = paid)
    pub commission: f64,
    /// trade currency -> account base currency (HKD) at execution
    pub fx_to_base: f64,
}

/// One crypto ledger event, kept structured (USD intact) for the DCA maths;
/// the Entry for it is the human-readable line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoinEvent {
    pub date: String,
    pub exchange: String,
    pub asset: String,
    /// + received, - sent/sold
    pub delta: f64,
    /// the event's cost/proceeds magnitude in USD, when known
    pub usd: Option<f64>,
    /// "auto-invest", "spot-buy", "reward", "withdraw", ...
    pub kind: String,
}

/// "… | exchange_out=GBP 50.00" -> ("GBP", 50.0)
fn paid_in(detail: &str) -> Option<(String, f64)> {
    let rest = detail.split("exchange_out=").nth(1)?;
    let mut it = rest.split_whitespace();
    let ccy = it.next()?.to_string();
    let amt = it.next()?.trim_end_matches(',').parse().ok()?;
    Some((ccy, amt))
}

/// One daily market bar (major currency unit of the listing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// Serializable as is: the server sends it whole as `/api/ledger.json` for
/// the web build, which has no SQLite ([`Ledger::from_json`]).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Ledger {
    pub entries: Vec<Entry>,
    /// broker executions, oldest first, currency conversions excluded
    pub trades: Vec<Trade>,
    /// daily bars per traded symbol, oldest first (from the price cache;
    /// rows cached before OHLC was stored have open = high = low = close),
    /// plus BTC's weekly candles under "BTC-1W"
    pub bars: HashMap<String, Vec<Bar>>,
    /// crypto events, oldest first
    pub coins: Vec<CoinEvent>,
    /// IBKR dividends, payments in lieu and withholding tax, oldest first
    /// (ibkr_income; absent from older servers' JSON)
    #[serde(default)]
    pub income: Vec<crate::invest::CashFlow>,
    /// the categories table (Phase 3; empty from an older server or replica)
    #[serde(default)]
    pub categories: Vec<crate::categories::Category>,
    /// the rules in effective order (Phase 3; empty before)
    #[serde(default)]
    pub rules: Vec<crate::categories::Rule>,
    /// the server's data revision when this copy was made
    #[serde(default)]
    pub data_rev: i64,
}

/// month ("YYYY-MM") -> currency -> base currency per unit, and the base
/// (the profile's, HKD by default): every entry's `hkd` is in it.
struct Fx(HashMap<String, HashMap<String, f64>>, String);

/// The replica's base currency: its profile's, else the default.
#[cfg(feature = "native")]
fn base_currency(conn: &Connection) -> String {
    conn.query_row("SELECT base_currency FROM profile WHERE id = 1", [], |r| r.get::<_, String>(0))
        .ok()
        .map(|c| c.trim().to_uppercase())
        .filter(|c| c.len() == 3)
        .unwrap_or_else(|| crate::base::DEFAULT_CURRENCY.to_string())
}

impl Fx {
    #[cfg(feature = "native")]
    fn load(conn: &Connection) -> Result<Self> {
        let base = base_currency(conn);
        let mut map: HashMap<String, HashMap<String, f64>> = HashMap::new();
        let mut stmt = conn.prepare("SELECT base, substr(month,1,7), rate FROM fx_rates WHERE quote = ?1")?;
        for row in stmt.query_map([&base], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?)))? {
            let (ccy, month, rate) = row?;
            map.entry(month).or_default().insert(ccy, rate);
        }
        Ok(Fx(map, base))
    }

    /// Rate for the month, else the nearest earlier month that has one.
    fn rate(&self, ccy: &str, month: &str) -> Option<f64> {
        if ccy == self.1 {
            return Some(1.0);
        }
        if let Some(r) = self.0.get(month).and_then(|m| m.get(ccy)) {
            return Some(*r);
        }
        let mut months: Vec<&String> = self.0.keys().filter(|m| m.as_str() <= month).collect();
        months.sort();
        months.iter().rev().find_map(|m| self.0[*m].get(ccy).copied())
    }
}

fn day(ts_ms: i64) -> String {
    DateTime::from_timestamp_millis(ts_ms)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn haystack(parts: &[&str]) -> String {
    parts.join(" ").to_lowercase()
}

impl Ledger {
    /// The wire format of `/api/ledger.json`.
    pub fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let l: Ledger = serde_json::from_slice(bytes).context("parsing ledger.json")?;
        crate::categories::set(&l.categories);
        Ok(l)
    }

    /// Web build: no replica file, so `load` hands out the ledger the page
    /// fetched before the app started (see [`crate::preload`]).
    #[cfg(not(feature = "native"))]
    pub fn load(_db: &Path) -> Result<Self> {
        crate::preload::ledger()
    }

    #[cfg(feature = "native")]
    pub fn load(db: &Path) -> Result<Self> {
        // the file, or the encrypted replica decrypted into memory
        let conn = crate::replica::open_db(db)?;
        let fx = Fx::load(&conn)?;
        let mut entries = Vec::with_capacity(8_000);

        let mut stmt = conn.prepare(
            "SELECT source, account, currency, txn_date, COALESCE(amount,0), description,
                    COALESCE(category,'Other'), COALESCE(share,1), raw_hash FROM bank_transactions")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                r.get::<_, String>(3)?, r.get::<_, f64>(4)?, r.get::<_, String>(5)?,
                r.get::<_, String>(6)?, r.get::<_, f64>(7)?, r.get::<_, String>(8)?))
        })?;
        for row in rows {
            let (source, account, currency, date, amount, description, category, share, raw_hash) = row?;
            let date = date.chars().take(10).collect::<String>();
            let hkd = fx.rate(&currency, &date[..date.len().min(7)]).map(|r| amount * r);
            let kind = if source.ends_with("_card") { Kind::Card } else { Kind::Bank };
            let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
            let amount_txt = format!("{:.2}", amount.abs());
            entries.push(Entry {
                haystack: haystack(&[&description, &category, &source, &account, &currency, &date, &amount_txt]),
                kind, date, source, account, description, category, amount, currency, hkd, share, raw_hash,
            });
        }

        let mut stmt = conn.prepare(
            "SELECT exchange, ts, asset, delta, usd, kind, COALESCE(detail,'') FROM crypto_events")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?, r.get::<_, Option<f64>>(4)?, r.get::<_, String>(5)?,
                r.get::<_, String>(6)?))
        })?;
        let mut coins = Vec::new();
        for row in rows {
            let (exchange, ts, asset, delta, usd, kind, detail) = row?;
            let date = day(ts);
            // the Revolut BTC ledger has no USD, only what was paid ("exchange_out=GBP 50.00")
            let usd = usd.or_else(|| paid_in(&detail).and_then(|(ccy, amt)| {
                let m = &date[..date.len().min(7)];
                Some(amt * fx.rate(&ccy, m)? / fx.rate("USD", m)?)
            }));
            coins.push(CoinEvent { date: date.clone(), exchange: exchange.clone(), asset: asset.clone(), delta, usd, kind: kind.clone() });
            let month = &date[..date.len().min(7)];
            // usd is the event's cost/proceeds magnitude; sign it like the delta
            let hkd = usd.zip(fx.rate("USD", month)).map(|(u, r)| u.abs() * r * delta.signum());
            let description = if detail.is_empty() { kind.clone() } else { format!("{kind} · {detail}") };
            let qty = format!("{delta}");
            entries.push(Entry {
                haystack: haystack(&[&description, &asset, &exchange, "crypto", &date, &qty]),
                kind: Kind::Crypto, date, source: exchange, account: String::new(), description,
                category: kind, amount: delta, currency: asset, hkd, share: 1.0, raw_hash: String::new(),
            });
        }

        let mut trades = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT ts, symbol, COALESCE(description,''), currency, qty, COALESCE(price,0),
                    COALESCE(amount,0), COALESCE(fx_to_base,1), COALESCE(commission,0)
             FROM ibkr_trades ORDER BY ts")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                r.get::<_, String>(3)?, r.get::<_, f64>(4)?, r.get::<_, f64>(5)?,
                r.get::<_, f64>(6)?, r.get::<_, f64>(7)?, r.get::<_, f64>(8)?))
        })?;
        for row in rows {
            let (ts, symbol, desc, currency, qty, price, amount, fx_base, commission) = row?;
            // IBKR's own currency conversions ("USD.HKD", no cash amount) are
            // broker bookkeeping, not trades the owner made
            if symbol.contains('.') && amount == 0.0 {
                continue;
            }
            let date = day(ts);
            trades.push(Trade {
                date: date.clone(), symbol: symbol.clone(), currency: currency.clone(), qty, price,
                amount, commission: commission.abs(), fx_to_base: fx_base,
            });
            let side = if qty >= 0.0 { "Buy" } else { "Sell" };
            let description = format!("{side} {} {symbol} @ {price:.2} {currency}", qty.abs());
            entries.push(Entry {
                haystack: haystack(&[&description, &desc, "ibkr broker trade", &date]),
                kind: Kind::Broker, date, source: "ibkr".into(), account: String::new(), description,
                category: "Trade".into(), amount, currency, hkd: Some(amount * fx_base), share: 1.0, raw_hash: String::new(),
            });
        }

        // daily bars for every symbol the broker traded, plus BTC's weekly
        // candles (BTC-1W, refreshed by the server) for the DCA view
        let mut bars: HashMap<String, Vec<Bar>> = HashMap::new();
        let cols: Vec<String> = conn.prepare("PRAGMA table_info(equity_prices)")?
            .query_map([], |r| r.get::<_, String>(1))?.collect::<Result<_, _>>()?;
        let ohlc = ["open", "high", "low"].iter().all(|c| cols.iter().any(|x| x == c));
        let sql = if ohlc {
            "SELECT date, COALESCE(open, close), COALESCE(high, close), COALESCE(low, close), close
             FROM equity_prices WHERE symbol = ? ORDER BY date"
        } else {
            "SELECT date, close, close, close, close FROM equity_prices WHERE symbol = ? ORDER BY date"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut symbols: Vec<&str> = trades.iter().map(|t| t.symbol.as_str()).collect();
        symbols.push(crate::dca::BTC_WEEKLY);
        symbols.sort();
        symbols.dedup();
        for sym in symbols {
            let rows = stmt.query_map([sym], |r| Ok(Bar {
                date: r.get(0)?, open: r.get(1)?, high: r.get(2)?, low: r.get(3)?, close: r.get(4)?,
            }))?;
            let v: Vec<Bar> = rows.collect::<Result<_, _>>()?;
            if !v.is_empty() {
                bars.insert(sym.to_string(), v);
            }
        }
        coins.sort_by(|a, b| a.date.cmp(&b.date));

        // a replica from before IBKR income was stored has no ibkr_income
        let mut income = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT date, kind, symbol, currency, amount, amount_base, description FROM ibkr_income ORDER BY date") {
            let rows = stmt.query_map([], |r| Ok(crate::invest::CashFlow {
                date: r.get(0)?, kind: r.get(1)?, symbol: r.get(2)?, currency: r.get(3)?, amount: r.get(4)?,
                amount_base: r.get(5)?, description: r.get(6)?,
            }))?;
            income = rows.collect::<Result<_, _>>()?;
        }

        // newest first: the natural order for a ledger and the search tiebreak
        entries.sort_by(|a, b| b.date.cmp(&a.date));
        let (categories, rules, data_rev) = load_categories(&conn);
        crate::categories::set(&categories);
        Ok(Ledger { entries, trades, bars, coins, income, categories, rules, data_rev })
    }
}

/// The categories table, the rules in effective order and data_rev from a
/// replica (none of it on a database from before Phase 3).
#[cfg(feature = "native")]
fn load_categories(conn: &Connection) -> (Vec<crate::categories::Category>, Vec<crate::categories::Rule>, i64) {
    use crate::categories::{Category, Rule};
    let has = |t: &str| conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", [t], |_| Ok(())).is_ok();
    if !has("categories") {
        return (Vec::new(), Vec::new(), 0);
    }
    // the Phase 4 flags (absent from an older replica: the names decide)
    let flags = conn
        .query_row("SELECT COUNT(*) FROM pragma_table_info('categories') WHERE name IN ('recurring', 'placeless', 'obligation')", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0)
        == 3;
    let flag_cols = if flags { "c.recurring, c.placeless, c.obligation" } else { "NULL, NULL, NULL" };
    let cats: Vec<Category> = conn
        .prepare(&format!(
            "SELECT c.name, c.icon, c.kind, c.fixed, c.reimbursable, c.hidden, c.sort,
                    (SELECT COUNT(*) FROM category_rules r WHERE r.category = c.name),
                    (SELECT COUNT(*) FROM bank_transactions b WHERE b.category = c.name),
                    {flag_cols}
             FROM categories c ORDER BY c.sort, c.name"
        ))
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok(Category {
                    name: r.get(0)?, icon: r.get(1)?, kind: r.get(2)?, fixed: r.get(3)?, reimbursable: r.get::<_, i64>(4)? != 0,
                    hidden: r.get::<_, i64>(5)? != 0, sort: r.get(6)?, rules: r.get(7)?, rows: r.get(8)?,
                    recurring: r.get(9)?, placeless: r.get::<_, Option<i64>>(10)?.map(|v| v != 0),
                    obligation: r.get::<_, Option<i64>>(11)?.map(|v| v != 0),
                })
            })?
            .collect()
        })
        .unwrap_or_default();
    let mut rules: Vec<Rule> = conn
        .prepare(
            "SELECT id, priority, pattern, category, is_transfer, direction, share, origin, enabled, created_at, note
             FROM category_rules ORDER BY (origin = 'seed'), priority, id",
        )
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok(Rule {
                    id: r.get(0)?, priority: r.get(1)?, pattern: r.get(2)?, category: r.get(3)?, is_transfer: r.get::<_, i64>(4)? != 0,
                    direction: r.get(5)?, share: r.get(6)?, origin: r.get(7)?, enabled: r.get::<_, i64>(8)? != 0,
                    created_at: r.get(9)?, note: r.get(10)?, rank: None, hits: 0,
                })
            })?
            .collect()
        })
        .unwrap_or_default();
    let mut rank = 0;
    for r in rules.iter_mut().filter(|r| r.enabled) {
        r.rank = Some(rank);
        rank += 1;
    }
    let rev = conn
        .query_row("SELECT value FROM app_meta WHERE key = 'data_rev'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    (cats, rules, rev)
}

impl Ledger {
    /// Patch this copy with a write's reply (the category APIs): the rows
    /// that moved, the categories and rules as they are now, data_rev.
    /// Returns how many entries changed.
    pub fn apply_write(&mut self, reply: &serde_json::Value) -> usize {
        let changed: Vec<crate::categories::Changed> =
            reply.get("changed").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        let mut by_key: HashMap<(&str, &str), &crate::categories::Changed> = HashMap::new();
        for c in &changed {
            by_key.insert((c.source.as_str(), c.raw_hash.as_str()), c);
        }
        let mut n = 0;
        for e in self.entries.iter_mut() {
            if e.raw_hash.is_empty() {
                continue;
            }
            if let Some(c) = by_key.get(&(e.source.as_str(), e.raw_hash.as_str())) {
                e.category = c.category.clone();
                e.share = c.share.unwrap_or(1.0);
                let amount_txt = format!("{:.2}", e.amount.abs());
                e.haystack = haystack(&[&e.description, &e.category, &e.source, &e.account, &e.currency, &e.date, &amount_txt]);
                n += 1;
            }
        }
        if let Some(cats) = reply.get("categories").cloned().and_then(|v| serde_json::from_value::<Vec<crate::categories::Category>>(v).ok()) {
            crate::categories::set(&cats);
            self.categories = cats;
        }
        if let Some(rules) = reply.get("rules").cloned().and_then(|v| serde_json::from_value(v).ok()) {
            self.rules = rules;
        }
        if let Some(rev) = reply.get("data_rev").and_then(|v| v.as_i64()) {
            self.data_rev = rev;
        }
        n
    }
}

#[cfg(test)]
mod apply_tests {
    use super::*;

    #[test]
    fn a_write_reply_patches_the_rows_it_names() {
        let e = |src: &str, h: &str, cat: &str| Entry {
            kind: Kind::Bank, date: "2026-09-01".into(), source: src.into(), account: String::new(), description: "Sushiro".into(),
            category: cat.into(), amount: -100.0, currency: "HKD".into(), hkd: Some(-100.0), share: 1.0,
            haystack: "sushiro other".into(), raw_hash: h.into(),
        };
        let mut l = Ledger { entries: vec![e("hsbc_card", "a", "Other"), e("hsbc_card", "b", "Other"), e("n26", "a", "Other")], ..Default::default() };
        let n = l.apply_write(&serde_json::json!({
            "changed": [{"source": "hsbc_card", "raw_hash": "a", "category": "Dining", "share": 0.5}],
            "data_rev": 7,
            "rules": [{"id": 3, "pattern": "sushiro", "category": "Dining", "origin": "user", "enabled": true}],
        }));
        assert_eq!(n, 1);
        assert_eq!((l.entries[0].category.as_str(), l.entries[0].share), ("Dining", 0.5));
        assert!(l.entries[0].haystack.contains("dining"));
        assert_eq!((l.entries[1].category.as_str(), l.entries[2].category.as_str()), ("Other", "Other"), "only that source and hash");
        assert_eq!((l.data_rev, l.rules.len()), (7, 1));
    }
}
