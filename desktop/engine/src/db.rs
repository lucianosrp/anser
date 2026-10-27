//! The single SQLite store: bank transactions, the
//! crypto ledger, the FX cache, IBKR broker state.
//!
//! Tables:
//!   bank_transactions        raw bank rows (source of truth mirrored from CSVs)
//!   ingested_files           (path, hash) bookkeeping -> idempotent ingest
//!   crypto_events            unified ledger: binance + okx + trezor + revolut (BTC)
//!   crypto_prices_monthly    month-end closes per asset (historical valuation)
//!   crypto_raw_sources       raw API payloads for audit
//!   crypto_balance_snapshots point-in-time live balances+prices (hourly)
//!   fx_rates                 frankfurter monthly rates cache (closed months immutable)
//!
//! SCHEMA is frozen text and `migrate()` replays the same ALTERs in the same
//! order: SQLite stores each CREATE/ALTER's text in sqlite_master, so every
//! finance.db (the server's, a fresh one, an old one opened again) has the
//! identical schema, which the golden and identity tests compare.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::config;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS bank_transactions (
  id          INTEGER PRIMARY KEY,
  source      TEXT NOT NULL,
  account     TEXT NOT NULL DEFAULT '',
  currency    TEXT NOT NULL,
  txn_date    TEXT NOT NULL,
  amount      REAL,
  balance     REAL,
  description TEXT NOT NULL DEFAULT '',
  raw_hash    TEXT NOT NULL,
  source_file TEXT NOT NULL,
  ingested_at INTEGER NOT NULL,
  UNIQUE (source, raw_hash)
);
CREATE INDEX IF NOT EXISTS idx_bank_txn_source_date ON bank_transactions(source, txn_date);

CREATE TABLE IF NOT EXISTS ingested_files (
  path        TEXT NOT NULL,
  source      TEXT NOT NULL,
  file_hash   TEXT NOT NULL,
  row_count   INTEGER NOT NULL,  -- rows parsed from the file
  rows_added  INTEGER,           -- rows that survived dedup (NULL pre-feature)
  ingested_at INTEGER NOT NULL,
  PRIMARY KEY (path, file_hash)
);

CREATE TABLE IF NOT EXISTS crypto_events (
  id         INTEGER PRIMARY KEY,
  exchange   TEXT NOT NULL,
  ts         INTEGER NOT NULL,
  asset      TEXT NOT NULL,
  delta      REAL NOT NULL,
  usd        REAL,
  kind       TEXT NOT NULL,
  detail     TEXT NOT NULL DEFAULT '',
  source_ref TEXT,
  UNIQUE (exchange, ts, asset, delta, kind, detail)
);
CREATE INDEX IF NOT EXISTS idx_crypto_events_ts ON crypto_events(ts);
CREATE INDEX IF NOT EXISTS idx_crypto_events_ex ON crypto_events(exchange);
CREATE INDEX IF NOT EXISTS idx_crypto_events_asset ON crypto_events(asset);

CREATE TABLE IF NOT EXISTS crypto_prices_monthly (
  asset TEXT NOT NULL,
  month TEXT NOT NULL,
  close REAL NOT NULL,
  PRIMARY KEY (asset, month)
);

-- daily closes for an equity proxy (e.g. URTH = iShares MSCI World), used to
-- mark investment contributions to market. Cached so history isn't refetched.
CREATE TABLE IF NOT EXISTS equity_prices (
  symbol TEXT NOT NULL,
  date   TEXT NOT NULL,
  close  REAL NOT NULL,
  currency TEXT,  -- quote currency of the resolved Yahoo listing (major unit)
  open REAL, high REAL, low REAL,  -- daily bar; NULL on rows cached before OHLC
  PRIMARY KEY (symbol, date)
);

CREATE TABLE IF NOT EXISTS crypto_raw_sources (
  exchange   TEXT NOT NULL,
  source     TEXT NOT NULL,
  payload    TEXT NOT NULL,
  fetched_at INTEGER NOT NULL,
  PRIMARY KEY (exchange, source)
);

CREATE TABLE IF NOT EXISTS crypto_balance_snapshots (
  ts       INTEGER NOT NULL,
  exchange TEXT NOT NULL,
  asset    TEXT NOT NULL,
  amount   REAL NOT NULL,
  price    REAL,
  PRIMARY KEY (ts, exchange, asset)
);

CREATE TABLE IF NOT EXISTS fx_rates (
  base       TEXT NOT NULL,
  quote      TEXT NOT NULL DEFAULT 'HKD',
  month      TEXT NOT NULL,
  rate       REAL NOT NULL,
  fetched_at INTEGER NOT NULL,
  PRIMARY KEY (base, quote, month)
);

-- IBKR broker state from the Flex Web Service (read-only reporting token).
-- Trades accumulate (merged by broker trade id); positions + cash are a
-- point-in-time snapshot of broker truth, wiped and rewritten on every fetch.
CREATE TABLE IF NOT EXISTS ibkr_trades (
  trade_id    TEXT PRIMARY KEY,
  ts          INTEGER NOT NULL,
  symbol      TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  currency    TEXT NOT NULL,
  qty         REAL NOT NULL,
  price       REAL NOT NULL,
  amount      REAL NOT NULL,           -- signed net cash in trade currency (buy < 0)
  commission  REAL NOT NULL DEFAULT 0,
  fx_to_base  REAL NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_ibkr_trades_ts ON ibkr_trades(ts);

CREATE TABLE IF NOT EXISTS ibkr_positions (
  symbol      TEXT PRIMARY KEY,
  description TEXT NOT NULL DEFAULT '',
  currency    TEXT NOT NULL,
  qty         REAL NOT NULL,
  mark_price  REAL,
  value_ccy   REAL NOT NULL,           -- position value in `currency`
  cost_ccy    REAL,                    -- broker cost basis in `currency`
  value_base  REAL NOT NULL,           -- in account base currency
  base_ccy    TEXT NOT NULL,
  as_of       TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ibkr_cash (
  currency    TEXT NOT NULL PRIMARY KEY,
  amount      REAL NOT NULL,
  amount_base REAL NOT NULL,
  base_ccy    TEXT NOT NULL,
  as_of       TEXT NOT NULL
);

-- Broker-reported daily net asset value (base currency), one row per Flex
-- daily statement: end-of-day cash + marked positions. This is the series
-- IBKR's own performance figures are computed from, so time-weighted return
-- can be reproduced exactly instead of estimated from our monthly marks.
CREATE TABLE IF NOT EXISTS ibkr_nav (
  date           TEXT PRIMARY KEY,   -- YYYY-MM-DD (statement toDate)
  cash_base      REAL NOT NULL,
  positions_base REAL NOT NULL,
  nav_base       REAL NOT NULL,
  base_ccy       TEXT NOT NULL
);

-- External cash flows (deposits / withdrawals) as the broker books them --
-- the denominators of every return figure. Dividends and interest are NOT
-- flows (they stay inside the account and show up in NAV).
CREATE TABLE IF NOT EXISTS ibkr_cash_flows (
  flow_id     TEXT PRIMARY KEY,
  date        TEXT NOT NULL,          -- YYYY-MM-DD (booking date)
  type        TEXT NOT NULL,
  currency    TEXT NOT NULL,
  amount      REAL NOT NULL,          -- signed, in `currency`
  amount_base REAL NOT NULL,
  description TEXT NOT NULL DEFAULT ''
);

-- real unit-trust executions from HSBC SMS confirmations ("N UNITS U50004
-- SUBSCRIBED AT HKD x PER UNIT. Pnnnnnn") -- ground truth that replaces the
-- proxy-derived units estimate wherever it exists
CREATE TABLE IF NOT EXISTS fund_subscriptions (
  ref        TEXT PRIMARY KEY,   -- HSBC confirmation ref (P123456)
  trade_date TEXT NOT NULL,      -- YYYY-MM-DD (SMS date; execution +/- a day)
  fund       TEXT NOT NULL,      -- HSBC fund code (U50004)
  units      REAL NOT NULL,      -- signed: negative on redemption
  px         REAL NOT NULL       -- NAV per unit, HKD
);

-- editable categorization rules; first match by ascending priority wins
CREATE TABLE IF NOT EXISTS category_rules (
  id       INTEGER PRIMARY KEY,
  priority INTEGER NOT NULL DEFAULT 100,
  pattern  TEXT NOT NULL,          -- lowercase substring; "=text" = whole description only
  category TEXT NOT NULL,
  is_transfer INTEGER NOT NULL DEFAULT 0, -- 1 = money moving between own accounts/exchanges, not spending
  direction TEXT,                  -- NULL = any sign, 'in' = inflows only, 'out' = outflows only
  share REAL                       -- NULL = 1; fraction of the row that is the owner's own money
);

-- hand categorization of one transaction, beating every rule (a deposit paid
-- to the same payee as the rent, a salary credit that carries a refund).
-- Keyed like bank_transactions' own UNIQUE, so it survives re-ingest.
CREATE TABLE IF NOT EXISTS txn_overrides (
  source     TEXT NOT NULL,
  raw_hash   TEXT NOT NULL,
  category   TEXT NOT NULL,
  share      REAL,                 -- NULL = 1
  note       TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (source, raw_hash)
);
"#;

fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = st.query_map([], |r| r.get::<_, String>("name"))?.collect::<rusqlite::Result<_>>()?;
    Ok(names)
}

/// Idempotent column additions for tables that predate a feature (same
/// statements, same order, every time).
fn migrate(conn: &Connection) -> Result<()> {
    let has = |cs: &[String], c: &str| cs.iter().any(|x| x == c);
    let cols = columns(conn, "bank_transactions")?;
    if !has(&cols, "category") {
        conn.execute_batch("ALTER TABLE bank_transactions ADD COLUMN category TEXT")?;
    }
    let rcols = columns(conn, "category_rules")?;
    if !has(&rcols, "direction") {
        conn.execute_batch("ALTER TABLE category_rules ADD COLUMN direction TEXT")?;
    }
    if !has(&rcols, "share") {
        conn.execute_batch("ALTER TABLE category_rules ADD COLUMN share REAL")?;
    }
    if !has(&cols, "share") {
        // set by categorize_all from the winning rule / override; NULL = 1
        conn.execute_batch("ALTER TABLE bank_transactions ADD COLUMN share REAL")?;
    }
    // stat fingerprint (size + mtime) so auto-ingest can skip unchanged files
    // WITHOUT reading them; rows from before this feature have NULLs and get
    // backfilled on their first (final) re-hash.
    let fcols = columns(conn, "ingested_files")?;
    if !has(&fcols, "size") {
        conn.execute_batch("ALTER TABLE ingested_files ADD COLUMN size INTEGER")?;
        conn.execute_batch("ALTER TABLE ingested_files ADD COLUMN mtime_ns INTEGER")?;
    }
    if !has(&fcols, "rows_added") {
        conn.execute_batch("ALTER TABLE ingested_files ADD COLUMN rows_added INTEGER")?;
    }
    let ecols = columns(conn, "equity_prices")?;
    if !has(&ecols, "currency") {
        conn.execute_batch("ALTER TABLE equity_prices ADD COLUMN currency TEXT")?;
    }
    for col in ["open", "high", "low"] {
        if !has(&ecols, col) {
            conn.execute_batch(&format!("ALTER TABLE equity_prices ADD COLUMN {col} REAL"))?;
        }
    }
    conn.execute_batch("DROP TABLE IF EXISTS investment_valuations")?; // manual marks retired; inferred only
    Ok(())
}

/// Open (creating if needed) the database at `path` with the server's
/// pragmas, schema and migrations applied.
pub fn connect(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let born = !path.exists();
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    // WAL + busy_timeout: the dashboard's background ledger sync writes while
    // page requests read -- without these, readers raise "database is locked"
    // the moment replace_events() commits.
    conn.busy_timeout(Duration::from_secs(10))?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    conn.execute_batch("PRAGMA busy_timeout=10000")?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    if born {
        mark_new(&conn)?;
    }
    Ok(conn)
}

/// `PRAGMA user_version` of a database this code created. An older
/// database predates it (0); the schema itself is unchanged, byte
/// for byte, so either opens the same.
const NEW_TREE: i64 = 1;

fn mark_new(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!("PRAGMA user_version = {NEW_TREE}"))?;
    Ok(())
}

/// Made by this code (db::connect created the file, or db::memory), not
/// an older tree that predates this code (user_version 0).
pub fn is_new_tree(conn: &Connection) -> bool {
    conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).is_ok_and(|v| v >= NEW_TREE)
}

/// A private in-memory database with the full schema (tests).
pub fn memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    apply_schema(&conn)?;
    mark_new(&conn)?;
    Ok(conn)
}

/// Create any missing table and run the column migrations: brings a copy of
/// an older database (the desktop replica) up to this schema. Idempotent.
pub fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    migrate(conn)
}

/// connect() on the configured database (FINANCE_DB / data/finance.db).
/// The server opens it a few times per request, so the schema and the
/// migrations run once per process per database file (same path, same
/// inode); after that an open is just the connection and its busy timeout
/// (WAL mode is stored in the file).
pub fn open() -> Result<Connection> {
    let path = config::db_path();
    if let Some(id) = file_id(&path) {
        if MIGRATED.lock().is_ok_and(|m| m.contains(&(path.clone(), id))) {
            let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
            conn.busy_timeout(Duration::from_secs(10))?;
            return Ok(conn);
        }
    }
    std::fs::create_dir_all(config::data_dir())?;
    let conn = connect(&path)?;
    if let (Some(id), Ok(mut m)) = (file_id(&path), MIGRATED.lock()) {
        m.retain(|(p, _)| *p != path);
        m.push((path, id));
    }
    Ok(conn)
}

/// Databases this process already brought up to the schema.
static MIGRATED: std::sync::Mutex<Vec<(std::path::PathBuf, FileId)>> = std::sync::Mutex::new(Vec::new());

type FileId = (u64, u64, std::time::SystemTime);

/// (device, inode, birth time): a db replaced by another file (even one
/// reusing the inode number) is migrated again. None where the filesystem
/// has no birth time: then every open migrates, as before.
fn file_id(path: &Path) -> Option<FileId> {
    let md = std::fs::metadata(path).ok()?;
    let born = md.created().ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((md.dev(), md.ino(), born))
    }
    #[cfg(not(unix))]
    {
        Some((0, 0, born))
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// --- bank side -------------------------------------------------------------

/// One bank_transactions row as inserted (id and category are the db's).
#[derive(Debug, Clone, PartialEq)]
pub struct NewBankTxn {
    pub source: String,
    pub account: String,
    pub currency: String,
    pub txn_date: String,
    pub amount: Option<f64>,
    pub balance: Option<f64>,
    pub description: String,
    pub raw_hash: String,
    pub source_file: String,
    pub ingested_at: i64,
}

/// INSERT OR IGNORE against UNIQUE(source, raw_hash). Returns rows added.
pub fn insert_bank_transactions(conn: &Connection, rows: &[NewBankTxn]) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut added = 0;
    {
        let mut st = tx.prepare(
            "INSERT OR IGNORE INTO bank_transactions\
             (source,account,currency,txn_date,amount,balance,description,raw_hash,source_file,ingested_at)\
             VALUES(?,?,?,?,?,?,?,?,?,?)",
        )?;
        for r in rows {
            added += st.execute(params![
                r.source,
                r.account,
                r.currency,
                r.txn_date,
                r.amount,
                r.balance,
                r.description,
                r.raw_hash,
                r.source_file,
                r.ingested_at
            ])?;
        }
    }
    tx.commit()?;
    Ok(added)
}

pub fn file_already_ingested(conn: &Connection, path: &str, file_hash: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM ingested_files WHERE path=? AND file_hash=?", params![path, file_hash], |_| Ok(()))
        .optional()?
        .is_some())
}

/// True when this exact (path, size, mtime) was already ingested -- the
/// cheap pre-check that lets auto-ingest skip a file without reading it.
pub fn file_stat_known(conn: &Connection, path: &str, size: i64, mtime_ns: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM ingested_files WHERE path=? AND size=? AND mtime_ns=?",
            params![path, size, mtime_ns],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Backfill/refresh the stat fingerprint on an already-ingested file (same
/// content, but touched or recorded before stat tracking existed).
pub fn update_ingested_stat(conn: &Connection, path: &str, file_hash: &str, size: i64, mtime_ns: i64) -> Result<()> {
    conn.execute(
        "UPDATE ingested_files SET size=?, mtime_ns=? WHERE path=? AND file_hash=?",
        params![size, mtime_ns, path, file_hash],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn record_ingested_file(
    conn: &Connection,
    path: &str,
    source: &str,
    file_hash: &str,
    row_count: i64,
    size: Option<i64>,
    mtime_ns: Option<i64>,
    rows_added: Option<i64>,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO ingested_files(path,source,file_hash,row_count,ingested_at,size,mtime_ns,rows_added)\
         VALUES(?,?,?,?,?,?,?,?)",
        params![path, source, file_hash, row_count, now_ms(), size, mtime_ns, rows_added],
    )?;
    Ok(())
}

/// Real unit-trust executions keyed on HSBC's confirmation ref -- re-ingested
/// SMS backups overlap heavily, the PK collapses duplicates.
pub fn upsert_fund_subscriptions(conn: &Connection, rows: &[crate::sources::FundRow]) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut added = 0;
    {
        let mut st =
            tx.prepare("INSERT OR IGNORE INTO fund_subscriptions(ref,trade_date,fund,units,px) VALUES(?,?,?,?,?)")?;
        for r in rows {
            added += st.execute(params![r.reference, r.trade_date, r.fund, r.units, r.px])?;
        }
    }
    tx.commit()?;
    Ok(added)
}

// --- crypto side --------------------------------------------------------------

/// One ledger row before it is tagged with its exchange
/// (`[ts_ms, asset, delta, usd, kind, detail]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub ts: i64,
    pub asset: String,
    pub delta: f64,
    pub usd: Option<f64>,
    pub kind: String,
    pub detail: String,
}

impl Event {
    pub fn new(ts: i64, asset: &str, delta: f64, usd: Option<f64>, kind: &str, detail: &str) -> Self {
        Event { ts, asset: asset.into(), delta, usd, kind: kind.into(), detail: detail.into() }
    }
}

fn insert_events(conn: &Connection, exchange: &str, events: &[Event]) -> Result<usize> {
    let mut st = conn.prepare(
        "INSERT OR IGNORE INTO crypto_events(exchange,ts,asset,delta,usd,kind,detail,source_ref) \
         VALUES(?,?,?,?,?,?,?,NULL)",
    )?;
    let mut added = 0;
    for e in events {
        added += st.execute(params![exchange, e.ts, e.asset, e.delta, e.usd, e.kind, e.detail])?;
    }
    Ok(added)
}

/// INSERT OR IGNORE file-sourced ledger rows (ingest's crypto_wallet kind).
/// Returns rows added.
pub fn insert_crypto_events(conn: &Connection, exchange: &str, events: &[Event]) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let added = insert_events(&tx, exchange, events)?;
    tx.commit()?;
    Ok(added)
}

/// INSERT OR IGNORE events for one exchange WITHOUT wiping its history.
///
/// For sources whose API can only look back a bounded window (OKX bills:
/// ~7d live + ~3mo archive), replace_events() would silently drop every
/// event older than that window on each sync. Instead: optionally wipe only
/// rows newer than replace_since_ms (the span this fetch fully covers, so
/// corrections there win) plus any drop_kinds (point-in-time rows like
/// 'cash-adjust' reconciliations that must never accumulate), then merge --
/// the natural-key UNIQUE collapses re-fetched duplicates, and anything
/// older than the API window survives. preserve_kinds exempts kinds from the
/// replace-window wipe (reconcile anchors re-derive themselves).
pub fn merge_events(
    conn: &Connection,
    exchange: &str,
    events: &[Event],
    replace_since_ms: Option<i64>,
    drop_kinds: &[&str],
    preserve_kinds: &[&str],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    if let Some(since) = replace_since_ms {
        let mut q = String::from("DELETE FROM crypto_events WHERE exchange=? AND ts>=?");
        let mut args: Vec<rusqlite::types::Value> = vec![exchange.to_string().into(), since.into()];
        if !preserve_kinds.is_empty() {
            q += &format!(" AND kind NOT IN ({})", vec!["?"; preserve_kinds.len()].join(","));
            args.extend(preserve_kinds.iter().map(|k| k.to_string().into()));
        }
        tx.execute(&q, rusqlite::params_from_iter(args))?;
    }
    for kind in drop_kinds {
        tx.execute("DELETE FROM crypto_events WHERE exchange=? AND kind=?", params![exchange, kind])?;
    }
    insert_events(&tx, exchange, events)?;
    tx.commit()?;
    Ok(())
}

/// Delete-then-insert all events for one exchange. ONLY correct when the
/// fetcher truly owns full history (Binance: every endpoint windowed back to
/// 2020) -- bounded-lookback sources must use merge_events instead.
pub fn replace_events(conn: &Connection, exchange: &str, events: &[Event]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM crypto_events WHERE exchange=?", params![exchange])?;
    insert_events(&tx, exchange, events)?;
    tx.commit()?;
    Ok(())
}

/// series: date -> close. Idempotent upsert. currency records which quote
/// currency the resolved listing trades in (major unit) so a symbol cached in
/// one currency can never be silently reused as another. ohlc (date ->
/// (open, high, low, close)) fills the daily bar; without it existing bars
/// are kept.
pub fn upsert_equity_prices(
    conn: &Connection,
    symbol: &str,
    series: &BTreeMap<String, f64>,
    currency: Option<&str>,
    ohlc: Option<&BTreeMap<String, (f64, f64, f64, f64)>>,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut st = tx.prepare(
            "INSERT INTO equity_prices(symbol,date,close,currency,open,high,low) VALUES(?,?,?,?,?,?,?) \
             ON CONFLICT(symbol,date) DO UPDATE SET close=excluded.close, \
             currency=COALESCE(excluded.currency, currency), open=COALESCE(excluded.open, open), \
             high=COALESCE(excluded.high, high), low=COALESCE(excluded.low, low)",
        )?;
        for (d, c) in series {
            let bar = ohlc.and_then(|o| o.get(d));
            st.execute(params![symbol, d, c, currency, bar.map(|b| b.0), bar.map(|b| b.1), bar.map(|b| b.2)])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn equity_price_currency(conn: &Connection, symbol: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT currency FROM equity_prices WHERE symbol=? AND currency IS NOT NULL LIMIT 1",
            params![symbol],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn load_equity_prices(conn: &Connection, symbol: &str) -> Result<BTreeMap<String, f64>> {
    let mut st = conn.prepare("SELECT date, close FROM equity_prices WHERE symbol=? ORDER BY date")?;
    let m = st.query_map(params![symbol], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(m)
}

pub fn upsert_prices(conn: &Connection, prices_monthly: &BTreeMap<String, BTreeMap<String, f64>>) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut st = tx.prepare(
            "INSERT INTO crypto_prices_monthly(asset,month,close) VALUES(?,?,?) \
             ON CONFLICT(asset,month) DO UPDATE SET close=excluded.close",
        )?;
        for (a, mm) in prices_monthly {
            for (m, c) in mm {
                st.execute(params![a, m, c])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// Raw API payload for audit (`payload` is the JSON text).
pub fn upsert_raw(conn: &Connection, exchange: &str, source: &str, payload: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO crypto_raw_sources(exchange,source,payload,fetched_at) VALUES(?,?,?,?) \
         ON CONFLICT(exchange,source) DO UPDATE SET payload=excluded.payload, fetched_at=excluded.fetched_at",
        params![exchange, source, payload, now_ms()],
    )?;
    Ok(())
}

/// balances: asset -> amount; prices: asset -> price (0/absent = NULL).
pub fn add_snapshot(
    conn: &Connection,
    ts: i64,
    exchange: &str,
    balances: &BTreeMap<String, f64>,
    prices: &BTreeMap<String, f64>,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut st = tx.prepare(
            "INSERT OR IGNORE INTO crypto_balance_snapshots(ts,exchange,asset,amount,price) VALUES(?,?,?,?,?)",
        )?;
        for (a, amt) in balances {
            let px = prices.get(a).copied().filter(|p| *p != 0.0);
            st.execute(params![ts, exchange, a, amt, px])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn last_snapshot_ts(conn: &Connection) -> Result<i64> {
    let m: Option<i64> = conn.query_row("SELECT MAX(ts) AS m FROM crypto_balance_snapshots", [], |r| r.get(0))?;
    Ok(m.unwrap_or(0))
}

/// MAX(fetched_at) over crypto_raw_sources, optionally for one exchange.
pub fn last_fetch_ms(conn: &Connection, exchange: Option<&str>) -> Result<Option<i64>> {
    Ok(match exchange {
        Some(ex) => conn
            .query_row("SELECT MAX(fetched_at) FROM crypto_raw_sources WHERE exchange=?", [ex], |r| r.get(0))
            .optional()?
            .flatten(),
        None => conn
            .query_row("SELECT MAX(fetched_at) FROM crypto_raw_sources", [], |r| r.get(0))
            .optional()?
            .flatten(),
    })
}

/// SUM(delta) per asset for one exchange in the STORED ledger.
pub fn ledger_sum(conn: &Connection, exchange: &str, asset: &str) -> Result<f64> {
    Ok(conn
        .query_row(
            "SELECT SUM(delta) FROM crypto_events WHERE exchange=? AND asset=?",
            [exchange, asset],
            |r| r.get::<_, Option<f64>>(0),
        )?
        .unwrap_or(0.0))
}

// --- IBKR -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IbkrTrade {
    pub trade_id: String,
    pub ts: i64,
    pub symbol: String,
    pub description: String,
    pub currency: String,
    pub qty: f64,
    pub price: f64,
    pub amount: f64,
    pub commission: f64,
    pub fx_to_base: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IbkrPosition {
    pub symbol: String,
    pub description: String,
    pub currency: String,
    pub qty: f64,
    pub mark_price: Option<f64>,
    pub value_ccy: f64,
    pub cost_ccy: Option<f64>,
    pub value_base: f64,
    pub base_ccy: String,
    pub as_of: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IbkrCash {
    pub currency: String,
    pub amount: f64,
    pub amount_base: f64,
    pub base_ccy: String,
    pub as_of: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IbkrNav {
    pub date: String,
    pub cash_base: f64,
    pub positions_base: f64,
    pub nav_base: f64,
    pub base_ccy: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IbkrFlow {
    pub flow_id: String,
    pub date: String,
    pub kind: String,
    pub currency: String,
    pub amount: f64,
    pub amount_base: f64,
    pub description: String,
}

/// INSERT OR REPLACE keyed on the broker's trade id: re-fetched statements
/// refresh rows in place (corrections win), nothing ever duplicates.
/// Returns the rows written (sqlite's change count).
pub fn upsert_ibkr_trades(conn: &Connection, rows: &[IbkrTrade]) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut n = 0;
    {
        let mut st = tx.prepare(
            "INSERT OR REPLACE INTO ibkr_trades \
             (trade_id,ts,symbol,description,currency,qty,price,amount,commission,fx_to_base) \
             VALUES(?,?,?,?,?,?,?,?,?,?)",
        )?;
        for t in rows {
            n += st.execute(params![
                t.trade_id, t.ts, t.symbol, t.description, t.currency, t.qty, t.price, t.amount, t.commission,
                t.fx_to_base
            ])?;
        }
    }
    tx.commit()?;
    Ok(n)
}

/// Positions and cash are a snapshot of broker truth: wipe and rewrite.
pub fn replace_ibkr_state(conn: &Connection, positions: &[IbkrPosition], cash: &[IbkrCash]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM ibkr_positions", [])?;
    tx.execute("DELETE FROM ibkr_cash", [])?;
    for p in positions {
        tx.execute(
            "INSERT INTO ibkr_positions \
             (symbol,description,currency,qty,mark_price,value_ccy,cost_ccy,value_base,base_ccy,as_of) \
             VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![
                p.symbol, p.description, p.currency, p.qty, p.mark_price, p.value_ccy, p.cost_ccy, p.value_base,
                p.base_ccy, p.as_of
            ],
        )?;
    }
    for c in cash {
        tx.execute(
            "INSERT INTO ibkr_cash(currency,amount,amount_base,base_ccy,as_of) VALUES(?,?,?,?,?)",
            params![c.currency, c.amount, c.amount_base, c.base_ccy, c.as_of],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn upsert_ibkr_nav(conn: &Connection, rows: &[IbkrNav]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for r in rows {
        tx.execute(
            "INSERT INTO ibkr_nav(date,cash_base,positions_base,nav_base,base_ccy) VALUES(?,?,?,?,?) \
             ON CONFLICT(date) DO UPDATE SET cash_base=excluded.cash_base, positions_base=excluded.positions_base, \
             nav_base=excluded.nav_base, base_ccy=excluded.base_ccy",
            params![r.date, r.cash_base, r.positions_base, r.nav_base, r.base_ccy],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// One dividend, payment in lieu, or withholding-tax row (Flex Cash
/// Transactions). Withholding is negative, as IBKR books it.
#[derive(Debug, Clone, PartialEq)]
pub struct IbkrIncome {
    pub income_id: String,
    pub date: String,
    pub symbol: String,
    /// IBKR's type: "Dividends", "Payment In Lieu Of Dividends", "Withholding Tax"
    pub kind: String,
    pub currency: String,
    pub amount: f64,
    pub amount_base: f64,
    pub description: String,
}

/// ibkr_income is not in SCHEMA (the frozen text connect() keeps
/// sqlite_master identical to): it is created here, on the
/// first write, and readers treat a missing table as no income.
pub const IBKR_INCOME_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ibkr_income (
  income_id   TEXT PRIMARY KEY,       -- the broker's transactionID, else a natural key
  date        TEXT NOT NULL,          -- YYYY-MM-DD
  symbol      TEXT NOT NULL DEFAULT '',
  kind        TEXT NOT NULL,
  currency    TEXT NOT NULL,
  amount      REAL NOT NULL,          -- signed, in `currency` (withholding < 0)
  amount_base REAL NOT NULL,
  description TEXT NOT NULL DEFAULT ''
)";

/// Upsert on income_id: a refetched statement refreshes its rows in place.
/// Returns how many rows are new.
pub fn upsert_ibkr_income(conn: &Connection, rows: &[IbkrIncome]) -> Result<usize> {
    conn.execute_batch(IBKR_INCOME_SCHEMA)?;
    let before: i64 = conn.query_row("SELECT COUNT(*) FROM ibkr_income", [], |r| r.get(0))?;
    let tx = conn.unchecked_transaction()?;
    for r in rows {
        tx.execute(
            "INSERT INTO ibkr_income(income_id,date,symbol,kind,currency,amount,amount_base,description) \
             VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(income_id) DO UPDATE SET date=excluded.date, symbol=excluded.symbol, \
             amount=excluded.amount, amount_base=excluded.amount_base, description=excluded.description",
            params![r.income_id, r.date, r.symbol, r.kind, r.currency, r.amount, r.amount_base, r.description],
        )?;
    }
    tx.commit()?;
    let after: i64 = conn.query_row("SELECT COUNT(*) FROM ibkr_income", [], |r| r.get(0))?;
    Ok((after - before) as usize)
}

pub fn upsert_ibkr_cash_flows(conn: &Connection, rows: &[IbkrFlow]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for r in rows {
        tx.execute(
            "INSERT INTO ibkr_cash_flows(flow_id,date,type,currency,amount,amount_base,description) \
             VALUES(?,?,?,?,?,?,?) ON CONFLICT(flow_id) DO UPDATE SET date=excluded.date, \
             amount=excluded.amount, amount_base=excluded.amount_base",
            params![r.flow_id, r.date, r.kind, r.currency, r.amount, r.amount_base, r.description],
        )?;
    }
    tx.commit()?;
    Ok(())
}

// --- stats / backup / snapshot -------------------------------------------------

/// Row counts per table (`anser-server stats`), in a fixed key order.
pub fn stats(conn: &Connection) -> Result<serde_json::Value> {
    let mut out = serde_json::Map::new();
    for name in [
        "bank_transactions",
        "ingested_files",
        "crypto_events",
        "crypto_prices_monthly",
        "crypto_balance_snapshots",
        "fx_rates",
    ] {
        let c: i64 = conn.query_row(&format!("SELECT COUNT(*) c FROM {name}"), [], |r| r.get(0))?;
        out.insert(name.into(), c.into());
    }
    let group = |sql: &str| -> Result<serde_json::Map<String, serde_json::Value>> {
        let mut st = conn.prepare(sql)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut m = serde_json::Map::new();
        for r in rows {
            let (k, v) = r?;
            m.insert(k, v.into());
        }
        Ok(m)
    };
    out.insert(
        "events_by_exchange".into(),
        group("SELECT exchange, COUNT(*) c FROM crypto_events GROUP BY exchange")?.into(),
    );
    out.insert("txns_by_source".into(), group("SELECT source, COUNT(*) c FROM bank_transactions GROUP BY source")?.into());
    Ok(out.into())
}

pub fn backup_dir() -> PathBuf {
    config::data_dir().join("backups")
}

/// Is `name` one of ours (the glob finance-????????-??????.db)?
fn is_backup_name(name: &str) -> bool {
    let c: Vec<char> = name.chars().collect();
    c.len() == 26 && name.starts_with("finance-") && name.ends_with(".db") && c[16] == '-'
}

fn copy_db(src_path: &Path, dest: &Path) -> Result<()> {
    let src = Connection::open(src_path).with_context(|| format!("opening {}", src_path.display()))?;
    src.busy_timeout(Duration::from_secs(10))?;
    src.backup(rusqlite::MAIN_DB, dest, None)?;
    Ok(())
}

/// Consistent online copy of the db (sqlite backup API -- safe while the
/// dashboard and a sync are writing; a plain cp of a WAL db is not) into
/// data/backups/finance-YYYYMMDD-HHMMSS.db, then prune to the newest `keep`.
/// Only files matching that pattern are pruned -- hand-made backups elsewhere
/// in data/ are never touched.
pub fn backup(keep: usize) -> Result<PathBuf> {
    backup_into(&config::db_path(), &backup_dir(), keep)
}

pub fn backup_into(db: &Path, dir: &Path, keep: usize) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(format!("finance-{}.db", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    copy_db(db, &dest)?;
    let mut olds: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| is_backup_name(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    olds.sort();
    let keep = keep.max(1);
    if olds.len() > keep {
        for old in &olds[..olds.len() - keep] {
            std::fs::remove_file(old)?;
        }
    }
    Ok(dest)
}

/// A consistent copy of finance.db as bytes: the /api/db.sqlite payload.
/// Backup API into a temp file, switched to journal_mode=DELETE so it is one
/// self-contained file.
pub fn snapshot_bytes() -> Result<Vec<u8>> {
    snapshot_bytes_of(&config::db_path())
}

pub fn snapshot_bytes_of(db: &Path) -> Result<Vec<u8>> {
    // entirely in memory: no plaintext copy of the data ever lands in /tmp
    let src = Connection::open(db)
        .with_context(|| format!("opening {}", db.display()))?;
    src.busy_timeout(Duration::from_secs(10))?;
    let mut mem = Connection::open_in_memory()?;
    {
        let b = rusqlite::backup::Backup::new(&src, &mut mem)?;
        b.run_to_completion(256, Duration::from_millis(0), None)?;
    }
    let mut bytes = mem.serialize(rusqlite::MAIN_DB)?.to_vec();
    // a self-contained rollback-journal file, as `PRAGMA journal_mode=DELETE`
    // leaves it: header bytes 18/19 (write/read format) 1, not WAL's 2
    if bytes.len() >= 100 && bytes.starts_with(b"SQLite format 3\0") {
        bytes[18] = 1;
        bytes[19] = 1;
    }
    Ok(bytes)
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn snapshot_is_in_memory_and_a_rollback_journal_file() {
        let dir = std::env::temp_dir().join(format!("finance-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("finance.db");
        let c = connect(&db).unwrap();
        c.execute_batch("INSERT INTO fx_rates(month, base, quote, rate, fetched_at) VALUES ('2026-01-01','EUR','HKD',8.5,0)").ok();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (42);").unwrap();
        let before: Vec<_> = std::fs::read_dir(std::env::temp_dir()).unwrap().filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("finance-snapshot-")).map(|e| e.path()).collect();
        let bytes = snapshot_bytes_of(&db).unwrap();
        let after: Vec<_> = std::fs::read_dir(std::env::temp_dir()).unwrap().filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("finance-snapshot-")).map(|e| e.path()).collect();
        assert_eq!(before, after, "no temp file");
        assert!(bytes.starts_with(b"SQLite format 3\0"));
        assert_eq!((bytes[18], bytes[19]), (1, 1), "journal_mode DELETE, not WAL");
        let out = dir.join("copy.db");
        std::fs::write(&out, &bytes).unwrap();
        let o = Connection::open(&out).unwrap();
        let ok: String = o.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(ok, "ok");
        let mode: String = o.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "delete");
        assert_eq!(o.query_row("SELECT x FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 42);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
