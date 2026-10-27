//! Computed views: the valuation + analytics stack behind the export bundle
//! and the MCP tools.
//!
//! Every public view returns a serde_json::Value whose shape is a contract
//! (keys and key order, rounding, null vs missing, list order): the desktop
//! app, the web app and the identity tests consume that JSON. Inside,
//! the math runs on plain structs, iterators and BTreeMaps; JSON happens at
//! the edge.
//!
//! The network never appears here directly. Every network read
//! (frankfurter FX, Yahoo closes, Binance tickers, exchange balances) goes
//! through the [`feed::MarketFeed`] seam; the server plugs its fetchers in,
//! and [`feed::Offline`] (forced under FINANCE_OFFLINE=1) makes every fetch
//! fail so the views take their cache fallback.
//!
//! The modules:
//!   api.rs                   the get_* views
//!   sheet.rs                 the monthly balance sheet
//!   holdings.rs              investment holdings and their marks
//!   networth.rs              the net worth
//!   analytics.rs             PnL, growth, allocation, spending
//!   investments.rs           all investments as one portfolio
//!   receivables.rs           money owed to the owner
//!   costs.rs                 fees and cash drag
//!   crypto.rs                crypto profit and holdings
//!   fx.rs, prices.rs         FX and price caches, read side

pub mod analytics;
pub mod api;
pub mod cache;
pub mod capabilities;
pub mod costs;
pub mod crypto;
pub mod feed;
pub mod fx;
pub mod holdings;
pub mod investments;
pub mod networth;
pub mod prices;
pub mod receivables;
pub mod sheet;
pub mod store;

use std::sync::Arc;

use chrono::{Datelike, NaiveDate};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row};
use serde_json::{Map, Value};

pub use feed::{MarketFeed, Offline};

/// Everything a view needs: the store, the market-data seam and the pinned
/// clock (config::utc_today(), so FINANCE_TODAY freezes every view at once).
pub struct Ctx<'a> {
    pub conn: &'a Connection,
    pub feed: &'a dyn MarketFeed,
    pub today: NaiveDate,
    /// fetching through the cache-only feed (part of the view-cache key)
    feed_offline: bool,
    memo: Memo,
}

/// Request-scoped memos: a Ctx lives for one request / one bundle, during
/// which the data under these doesn't change (a refetched FX month clears
/// the rates).
#[derive(Default)]
struct Memo {
    rates: std::cell::RefCell<Option<Arc<fx::Rates>>>,
    ledgers: std::cell::OnceCell<Arc<Vec<sheet::Ledger>>>,
    sheet: std::cell::OnceCell<Arc<sheet::Sheet>>,
    net_worth_offline: std::cell::OnceCell<Arc<Value>>,
    bank: std::cell::OnceCell<Arc<Vec<store::BankRow>>>,
    export: std::cell::OnceCell<Arc<crypto::Export>>,
    fingerprint: cache::FpMemo,
    /// the first fingerprint this Ctx saw: every memo above was read at or
    /// after it
    first_fp: std::cell::OnceCell<Option<cache::Fingerprint>>,
    snapshot: std::cell::OnceCell<Arc<prices::Snapshot>>,
    ibkr_trades: std::cell::OnceCell<Arc<Vec<Value>>>,
    equity: std::cell::RefCell<std::collections::HashMap<String, Arc<prices::Hist>>>,
    flags: std::cell::OnceCell<Arc<crate::categories::Flags>>,
    invest: std::cell::OnceCell<Arc<crate::invest_setup::Setup>>,
}

static OFFLINE: Offline = Offline;

impl<'a> Ctx<'a> {
    /// A context on `conn` fetching through `feed`, except under
    /// FINANCE_OFFLINE=1 where every fetch is the cache-only fallback.
    pub fn new(conn: &'a Connection, feed: &'a dyn MarketFeed) -> Self {
        let offline = crate::config::offline();
        let feed: &'a dyn MarketFeed = if offline { &OFFLINE } else { feed };
        Ctx { conn, feed, today: crate::config::utc_today(), feed_offline: offline, memo: Memo::default() }
    }

    /// Cache only: what the bundle and every instant-paint path use when no
    /// fetcher is wired in.
    pub fn offline(conn: &'a Connection) -> Self {
        Ctx { conn, feed: &OFFLINE, today: crate::config::utc_today(), feed_offline: true, memo: Memo::default() }
    }

    /// The view-cache fingerprint of this context (cache::fingerprint):
    /// re-checked per call, so a write made during this request counts.
    pub fn fingerprint(&self) -> Option<cache::Fingerprint> {
        let fp = cache::fingerprint(self.conn, self.today, self.feed_offline, &self.memo.fingerprint);
        self.memo.first_fp.get_or_init(|| fp.clone());
        fp
    }

    /// Whether what this Ctx computes may be cached under `fp`: only while
    /// the database is still at the fingerprint the Ctx started from. Its
    /// memos (the investment setup, the bank rows, the rates...) are read
    /// once; after a write that moved the fingerprint mid-request they can
    /// predate `fp`, and a view computed from them would be served stale to
    /// every later request under the new fingerprint.
    pub fn cacheable(&self, fp: &cache::Fingerprint) -> bool {
        self.memo.first_fp.get().is_some_and(|f| f.as_ref() == Some(fp))
    }

    /// The rate resolver over the fx_rates cache, loaded once.
    pub fn rates(&self) -> Arc<fx::Rates> {
        if let Some(r) = self.memo.rates.borrow().as_ref() {
            return r.clone();
        }
        let r = cache::shared(self, "rates", "", || fx::Rates::load(self.conn));
        *self.memo.rates.borrow_mut() = Some(r.clone());
        r
    }

    /// Forget the cached rates (after an FX upsert).
    pub fn invalidate_rates(&self) {
        self.memo.rates.borrow_mut().take();
    }

    /// A symbol's cached closes, read once per symbol.
    pub fn equity(&self, symbol: &str) -> Arc<prices::Hist> {
        if let Some(h) = self.memo.equity.borrow().get(symbol) {
            return h.clone();
        }
        let h = cache::shared(self, "equity", symbol, || prices::load_equity_prices(self.conn, symbol));
        self.memo.equity.borrow_mut().insert(symbol.to_string(), h.clone());
        h
    }

    /// Forget a symbol's closes (after an upsert or delete of its rows).
    pub fn invalidate_equity(&self, symbol: &str) {
        self.memo.equity.borrow_mut().remove(symbol);
    }

    /// The last crypto balance snapshot, read once.
    pub fn snapshot(&self) -> Arc<prices::Snapshot> {
        self.memo.snapshot.get_or_init(|| cache::shared(self, "snapshot", "", || prices::latest_snapshot(self.conn))).clone()
    }

    /// holdings::ibkr_trades (every trade as a row object, forex legs
    /// dropped), read once.
    pub fn ibkr_trades(&self) -> Arc<Vec<Value>> {
        self.memo
            .ibkr_trades
            .get_or_init(|| cache::shared(self, "ibkr_trades", "", || holdings::ibkr_trades(self.conn)))
            .clone()
    }

    /// What the categories mean (the table's flags, else the old lists), read once.
    pub fn flags(&self) -> Arc<crate::categories::Flags> {
        self.memo.flags.get_or_init(|| cache::shared(self, "flags", "", || crate::categories::Flags::load(self.conn))).clone()
    }

    /// The investment accounts and instruments (the tables, else the
    /// defaults), read once.
    pub fn invest(&self) -> Arc<crate::invest_setup::Setup> {
        self.memo.invest.get_or_init(|| cache::shared(self, "invest", "", || crate::invest_setup::Setup::load(self.conn))).clone()
    }

    /// Every bank_transactions row, read once.
    pub fn bank(&self) -> Arc<Vec<store::BankRow>> {
        self.memo.bank.get_or_init(|| cache::shared(self, "bank", "", || store::load_bank(self.conn))).clone()
    }

    /// The crypto ledger + monthly closes, read once.
    pub fn export(&self) -> Arc<crypto::Export> {
        self.memo.export.get_or_init(|| cache::shared(self, "export", "", || crypto::load_export(self.conn))).clone()
    }

    /// Every bank ledger.
    pub fn ledgers(&self) -> Arc<Vec<sheet::Ledger>> {
        self.memo
            .ledgers
            .get_or_init(|| cache::shared(self, "ledgers", "", || sheet::load_ledgers(self.conn, &self.bank())))
            .clone()
    }

    /// The full-range balance sheet, built once.
    pub fn full_sheet(&self) -> &sheet::Sheet {
        self.memo.sheet.get_or_init(|| cache::shared(self, "full_sheet", "", || sheet::build_full(self)))
    }

    /// The network-free net worth (live=false), computed once.
    /// Its as_of is the time it was first computed under this fingerprint.
    pub fn net_worth_offline(&self) -> &Value {
        self.memo
            .net_worth_offline
            .get_or_init(|| cache::shared(self, "net_worth_offline", "", || networth::net_worth_now(self, false)))
    }

    /// 'YYYY-MM' of today.
    pub fn ym_now(&self) -> String {
        ym_of(self.today)
    }
}

// ---------------------------------------------------------------------------
// Numeric and text helpers (the forms the bundle carries)
// ---------------------------------------------------------------------------

/// round(x, nd): correctly rounded on the exact binary value, ties
/// to even. The fast path is exact whenever x*10^nd is not within a hair of
/// a .5; near a tie the decimal formatter (same exact semantics) decides.
pub fn round_n(x: f64, nd: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let m = 10f64.powi(nd);
    let y = x * m;
    if y.abs() >= 1e15 {
        return x;
    }
    let frac = (y - y.trunc()).abs();
    if (frac - 0.5).abs() > 1e-6 {
        return y.round() / m;
    }
    format!("{:.*}", nd.max(0) as usize, x).parse().unwrap_or(x)
}

/// round(x, 2) -- money.
pub fn r2(x: f64) -> f64 {
    round_n(x, 2)
}

/// A float as JSON (NaN/inf -> null).
pub fn num(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}

pub fn opt_num(x: Option<f64>) -> Value {
    x.map(num).unwrap_or(Value::Null)
}

/// Title case: a cased char is upper after an uncased one, lower
/// after a cased one ("okx" -> "Okx", "o'neil" -> "O'Neil").
pub fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        let cased = c.is_lowercase() || c.is_uppercase();
        if cased {
            if prev_cased {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
        } else {
            out.push(c);
        }
        prev_cased = cased;
    }
    out
}

/// The first n chars of s, by code points, not bytes.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Insertion point after any equal entries, on sorted date strings.
pub fn bisect_right(v: &[String], x: &str) -> usize {
    v.partition_point(|e| e.as_str() <= x)
}

/// Insertion point before any equal entries, on sorted date strings.
pub fn bisect_left(v: &[String], x: &str) -> usize {
    v.partition_point(|e| e.as_str() < x)
}

// ---------------------------------------------------------------------------
// Months
// ---------------------------------------------------------------------------

pub fn ym_of(d: NaiveDate) -> String {
    let (y, m) = (d.year(), d.month());
    if !(0..=9999).contains(&y) {
        return format!("{y:04}-{m:02}");
    }
    // format! is a measurable share of the views (a call per row per series)
    let y = y as u32;
    let b = [
        b'0' + (y / 1000) as u8,
        b'0' + (y / 100 % 10) as u8,
        b'0' + (y / 10 % 10) as u8,
        b'0' + (y % 10) as u8,
        b'-',
        b'0' + (m / 10) as u8,
        b'0' + (m % 10) as u8,
    ];
    String::from_utf8(b.to_vec()).expect("ascii digits")
}

/// First day of the month of a 'YYYY-MM' / 'YYYY-MM-DD' string.
pub fn month_date(s: &str) -> NaiveDate {
    let y: i32 = s.get(..4).and_then(|v| v.parse().ok()).unwrap_or(1970);
    let m: u32 = s.get(5..7).and_then(|v| v.parse().ok()).unwrap_or(1);
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or_default()
}

pub fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap()
}

pub fn next_month(d: NaiveDate) -> NaiveDate {
    let (y, m) = if d.month() == 12 { (d.year() + 1, 1) } else { (d.year(), d.month() + 1) };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap()
}

/// Every month start from start's month to end's, inclusive.
pub fn month_range(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    let (mut cur, end) = (month_start(start), month_start(end));
    let mut out = Vec::new();
    while cur <= end {
        out.push(cur);
        cur = next_month(cur);
    }
    out
}

/// UTC 'YYYY-MM' of a unix-ms timestamp.
pub fn ms_to_ym(ms: i64) -> String {
    ym_of(ms_to_date(ms))
}

pub fn ms_to_date(ms: i64) -> NaiveDate {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.date_naive()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

pub fn value_ref_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => num(f),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(hex::encode(b)),
    }
}

/// A row as a JSON object: every column in SELECT order.
pub fn row_json(row: &Row<'_>) -> Value {
    let stmt = row.as_ref();
    let mut m = Map::new();
    for i in 0..stmt.column_count() {
        let name = stmt.column_name(i).unwrap_or("").to_string();
        m.insert(name, value_ref_json(row.get_ref_unwrap(i)));
    }
    Value::Object(m)
}

/// Every row of a query as a JSON object ([`row_json`]).
pub fn query_json(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(args, |r| Ok(row_json(r)))?;
    rows.collect()
}

/// A float column that may be NULL or stored as an integer.
pub fn get_f64(row: &Row<'_>, i: usize) -> Option<f64> {
    match row.get_ref_unwrap(i) {
        ValueRef::Integer(v) => Some(v as f64),
        ValueRef::Real(v) => Some(v),
        ValueRef::Text(t) => std::str::from_utf8(t).ok().and_then(|s| s.parse().ok()),
        _ => None,
    }
}

pub fn get_str(row: &Row<'_>, i: usize) -> Option<String> {
    match row.get_ref_unwrap(i) {
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Integer(v) => Some(v.to_string()),
        ValueRef::Real(v) => Some(v.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Source specs
// ---------------------------------------------------------------------------

/// The registered file source's spec: the code sources
/// (looked up once), then the format definitions in effect.
pub fn spec(source: &str) -> Option<&'static crate::sources::SourceSpec> {
    static SPECS: std::sync::OnceLock<Vec<crate::sources::SourceSpec>> = std::sync::OnceLock::new();
    SPECS
        .get_or_init(|| crate::sources::code_sources().iter().map(|s| s.spec().clone()).collect())
        .iter()
        .find(|s| s.key == source)
        .or_else(|| crate::sources::format::get(source).map(crate::sources::format::FormatSource::spec_static))
}

/// A source's balance method: the spec's, else n26 = cumsum, else reported.
pub fn balance_method(source: &str) -> &'static str {
    use crate::sources::BalanceMethod;
    match spec(source).map(|s| s.balance_method) {
        Some(BalanceMethod::Reported) => "reported",
        Some(BalanceMethod::Cumsum) => "cumsum",
        Some(BalanceMethod::None) => "none",
        None if source == "n26" => "cumsum",
        None => "reported",
    }
}

/// A ledger's display label.
pub fn source_label(source: &str, account: &str) -> String {
    let base = match spec(source) {
        Some(s) if !s.label.is_empty() => s.label.to_string(),
        _ => source.to_string(),
    };
    if account.is_empty() { base } else { format!("{base} · {account}") }
}

/// Ledger column key of a (source, account).
pub fn bank_key(source: &str, account: &str) -> String {
    if account.is_empty() { source.to_string() } else { format!("{source}_{account}") }
}
