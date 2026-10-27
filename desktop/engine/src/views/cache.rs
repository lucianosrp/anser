//! The process-wide view cache, keyed on the db fingerprint.
//!
//! A computed view is a pure function of the database content, "today" and
//! (for the few views that price crypto) the live tickers. So it is memoized
//! under a [`Fingerprint`]: a handful of COUNT/MAX/SUM queries over every
//! table the views read, including the ones that were once read uncached
//! (balance snapshots, monthly closes, IBKR cash flows,
//! overrides). A sync, an ingest, a recategorization or a new price row
//! changes it, and the next request recomputes.
//!
//! The current month's FX `fetched_at` is left out: the views
//! refetch that month on every compute and rewrite its timestamp even when
//! the rate holds still. Its rate VALUE is in, so a moved rate recomputes.
//!
//! The content queries cost ~1.5 ms on the laptop (~15 ms on a Raspberry Pi), so a
//! stat check stands in front of them: while the db and WAL files keep the
//! same inode, size and mtime, the last fingerprint still holds. Any commit
//! appends WAL frames (and a checkpoint rewrites the db), which moves one of
//! them. Kernel mtimes are coarse (a tick of a few ms) and a WAL commit
//! becomes visible after its frames are written (and synced), so content
//! read within half a second of the files' mtime is not trusted for good
//! ("racy git"): it is reused for half a second, then read again.
//!
//! A Ctx reads its inputs once (Ctx's memos), so a Ctx that saw the
//! fingerprint move under it (a write landed mid-request: say the warm-up
//! pass started before an admin edit committed) still answers, but caches
//! nothing more: its inputs may predate the new fingerprint (Ctx::cacheable).
//!
//! Values are shared (Arc) and never mutated. Errors are never cached.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::NaiveDate;
use rusqlite::types::Value as Sql;
use rusqlite::Connection;
use serde_json::Value;

use super::Ctx;

/// LRU size. Pages ask for a few ranges each, plus month
/// drill-downs; 64 entries of a few hundred KB at most.
const VIEWS_MAX: usize = 64;
/// A file touched more recently than this isn't trusted by the stat check.
/// (100x a 4 ms kernel tick)
const RACY: Duration = Duration::from_millis(500);

/// Everything a cached view depends on besides its own arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Fingerprint {
    path: Arc<str>,
    today: NaiveDate,
    /// the views ran through the cache-only feed (Ctx::offline / FINANCE_OFFLINE)
    offline_feed: bool,
    content: Arc<Vec<Sql>>,
}

impl Fingerprint {
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// The db fingerprint over every table the views read. One
/// statement: a cross join of one-row aggregates, one pass per table.
const CONTENT_SQL: &str = "SELECT * FROM
  (SELECT COUNT(*), MAX(ingested_at), COUNT(DISTINCT category),
          COALESCE(SUM(LENGTH(category)),0) + ROUND(COALESCE(SUM(share),0),4) FROM bank_transactions),
  (SELECT MAX(fetched_at) FROM crypto_raw_sources),
  (SELECT COUNT(*) FROM crypto_events),
  (SELECT MAX(CASE WHEN month<>?1 THEN fetched_at END), SUM(CASE WHEN month=?1 THEN rate END) FROM fx_rates),
  (SELECT COUNT(*), MAX(date), ROUND(COALESCE(SUM(close),0),4) FROM equity_prices),
  (SELECT COUNT(*) FROM ibkr_trades), (SELECT MAX(as_of) FROM ibkr_positions), (SELECT MAX(as_of) FROM ibkr_cash),
  (SELECT COUNT(*), ROUND(COALESCE(SUM(units*px),0),4) FROM fund_subscriptions),
  (SELECT COUNT(*), MAX(date) FROM ibkr_nav),
  (SELECT COUNT(*), COALESCE(SUM(priority),0),
          COALESCE(SUM(LENGTH(pattern)+LENGTH(category)+is_transfer+COALESCE(LENGTH(direction),0)),0),
          COALESCE(SUM(share),0) FROM category_rules),
  (SELECT COUNT(*), MAX(ts) FROM crypto_balance_snapshots),
  (SELECT COUNT(*), ROUND(COALESCE(SUM(close),0),4) FROM crypto_prices_monthly),
  (SELECT COUNT(*), MAX(date), ROUND(COALESCE(SUM(amount_base),0),4) FROM ibkr_cash_flows),
  (SELECT COUNT(*), MAX(created_at),
          COALESCE(SUM(LENGTH(category)+LENGTH(raw_hash)),0) + ROUND(COALESCE(SUM(share),0),4),
          COALESCE(SUM(LENGTH(note)),0) FROM txn_overrides)";

fn content(conn: &Connection, today: NaiveDate) -> Option<Vec<Sql>> {
    let current = format!("{}-01", super::ym_of(today));
    let mut stmt = conn.prepare_cached(CONTENT_SQL).ok()?;
    let n = stmt.column_count();
    let mut v: Vec<Sql> = stmt.query_row([current], |r| (0..n).map(|i| r.get::<_, Sql>(i)).collect()).ok()?;
    // Phase 3 (absent on an older db): the categories' flags and the rules'
    // enabled/origin change what the views compute without moving a row
    if crate::categories::has_table(conn, "categories") {
        let extra = conn
            .prepare_cached(CATEGORIES_SQL)
            .and_then(|mut s| {
                let n = s.column_count();
                s.query_row([], |r| (0..n).map(|i| r.get::<_, Sql>(i)).collect::<rusqlite::Result<Vec<Sql>>>())
            })
            .ok()?;
        v.extend(extra);
    }
    // Phase 4: the investment setup and the profile's base currency and
    // benchmark change what the views compute without moving a row
    // (and the home setup, whose returned deposits the receivables read)
    for (table, sql) in [("investment_accounts", INVEST_SQL), ("profile", PROFILE_SQL), ("user_settings", HOME_SQL)] {
        if crate::categories::has_table(conn, table) {
            let extra = conn
                .prepare_cached(sql)
                .and_then(|mut s| {
                    let n = s.column_count();
                    s.query_row([], |r| (0..n).map(|i| r.get::<_, Sql>(i)).collect::<rusqlite::Result<Vec<Sql>>>())
                })
                .ok()?;
            v.extend(extra);
        }
    }
    Some(v)
}

/// The home setup (a deposit's `returned_on` changes the receivables).
const HOME_SQL: &str = "SELECT COALESCE((SELECT value FROM user_settings WHERE key = 'home'), '')";

/// The investment accounts and instruments (made by invest_setup::ensure).
const INVEST_SQL: &str = "SELECT * FROM
  (SELECT COUNT(*), COALESCE(GROUP_CONCAT(name || '|' || matches || '|' || sort || '|' || valued_by || '|' || COALESCE(proxy, '') || '|' ||
          COALESCE(proxy_label, '') || '|' || COALESCE(fund_code, '') || '|' || COALESCE(fund_ocf_pct, '') || '|' ||
          COALESCE(platform_flat_month, '') || '|' || COALESCE(platform_cliff, '') || '|' || COALESCE(platform_pct_above, '') || '|' ||
          COALESCE(fee_match, ''), ';'), '') FROM investment_accounts),
  (SELECT COUNT(*), COALESCE(GROUP_CONCAT(symbol || '|' || kind || '|' || COALESCE(ter_pct, ''), ';'), '') FROM instruments)";

/// The profile fields the views read.
const PROFILE_SQL: &str = "SELECT COALESCE(MAX(base_currency), ''), COALESCE(MAX(benchmark), '') FROM profile";

/// The categories table and the managed rules' columns, for the fingerprint.
const CATEGORIES_SQL: &str = "SELECT * FROM
  (SELECT COUNT(*), COALESCE(SUM(LENGTH(name) + LENGTH(kind) * 3 + LENGTH(fixed) * 7 + reimbursable * 11 + hidden * 13 + sort), 0),
          COALESCE(SUM(LENGTH(COALESCE(icon, ''))), 0) FROM categories),
  (SELECT COALESCE(MAX(value), '') FROM app_meta WHERE key = 'data_rev'),
  (SELECT COALESCE(SUM(enabled * id), 0), COALESCE(SUM((origin = 'seed') * id), 0) FROM category_rules)";

/// (dev, inode, size, mtime ns) of the db, and (size, mtime ns) of its WAL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    db: (u64, u64, u64, i128),
    wal: Option<(u64, i128)>,
}

fn mtime_ns(md: &std::fs::Metadata) -> i128 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

fn stat(path: &str) -> Option<(Stat, bool)> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let (dev, ino) = (md.dev(), md.ino());
    #[cfg(not(unix))]
    let (dev, ino) = (0u64, 0u64);
    // An empty WAL holds nothing: SQLite creates one on every open and
    // deletes it when the last connection closes (checkpointing any frames
    // into the db first, which moves the db's own mtime).
    let wal = std::fs::metadata(format!("{path}-wal")).ok().filter(|w| w.len() > 0);
    let newest = wal.as_ref().map(mtime_ns).unwrap_or(0).max(mtime_ns(&md));
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
    let settled = now - newest > RACY.as_nanos() as i128;
    Some((Stat { db: (dev, ino, md.len(), mtime_ns(&md)), wal: wal.map(|w| (w.len(), mtime_ns(&w))) }, settled))
}

struct Known {
    path: Arc<str>,
    stat: Stat,
    today: NaiveDate,
    content: Arc<Vec<Sql>>,
    /// the content was read while the files had settled: trusted on an
    /// equal stat from then on
    trusted: bool,
    /// when it was read: unsettled content is still reused for RACY after
    /// that (a commit racing the read is seen at most RACY late), then read
    /// again
    read_at: std::time::Instant,
}

static KNOWN: Mutex<Vec<Known>> = Mutex::new(Vec::new());

/// One request's (Ctx's) last fingerprint and the stat it was read under:
/// within a request an equal stat is enough, racy or not. (A write racing
/// the request itself is caught by a later one: unsettled content is read
/// again once RACY has passed.)
#[derive(Default)]
pub struct FpMemo(std::cell::RefCell<Option<(Stat, Fingerprint)>>);

/// The fingerprint of `conn`'s database for views computed today. None for
/// an in-memory database or one missing a table (nothing gets cached then).
pub fn fingerprint(conn: &Connection, today: NaiveDate, offline_feed: bool, memo: &FpMemo) -> Option<Fingerprint> {
    let path = conn.path().filter(|p| !p.is_empty())?;
    // stat BEFORE the content queries: a commit landing in between moves
    // the stat, so the next call recomputes instead of trusting stale content
    let (st, settled) = stat(path)?;
    if let Some((mst, fp)) = memo.0.borrow().as_ref() {
        if *mst == st && fp.today == today && fp.offline_feed == offline_feed && &*fp.path == path {
            return Some(fp.clone());
        }
    }
    let known = KNOWN.lock().ok().and_then(|known| {
        known
            .iter()
            .find(|k| {
                (k.trusted || k.read_at.elapsed() < RACY) && &*k.path == path && k.stat == st && k.today == today
            })
            .map(|k| (k.path.clone(), k.content.clone()))
    });
    let (path, content) = match known {
        Some(k) => k,
        None => {
            let content = Arc::new(content(conn, today)?);
            let path: Arc<str> = path.into();
            if let Ok(mut known) = KNOWN.lock() {
                known.retain(|k| k.path != path);
                let read_at = std::time::Instant::now();
                known.push(Known { path: path.clone(), stat: st, today, content: content.clone(), trusted: settled, read_at });
            }
            (path, content)
        }
    };
    let fp = Fingerprint { path, today, offline_feed, content };
    *memo.0.borrow_mut() = Some((st, fp.clone()));
    Some(fp)
}

/// (name, args, variant, fingerprint, value). One slot per (name, args):
/// a new variant (say, other crypto tickers) replaces the old one.
type Entry = (&'static str, String, String, Fingerprint, Arc<Value>);

static VIEWS: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());

/// compute() memoized under (name, args) while the fingerprint holds; the
/// least recently used entry goes first. A hit refreshes its recency. With
/// no fingerprint (in-memory db), compute() runs every time.
pub fn view(ctx: &Ctx, name: &'static str, args: &str, compute: impl FnOnce() -> anyhow::Result<Value>) -> anyhow::Result<Arc<Value>> {
    view_variant(ctx, name, args, "", compute)
}

/// view() for a value that also depends on something outside the db (the
/// crypto tickers it was priced with): it is served while `variant` holds
/// too, and a new variant takes over the slot instead of piling up.
pub fn view_variant(
    ctx: &Ctx,
    name: &'static str,
    args: &str,
    variant: &str,
    compute: impl FnOnce() -> anyhow::Result<Value>,
) -> anyhow::Result<Arc<Value>> {
    let Some(fp) = ctx.fingerprint() else { return compute().map(Arc::new) };
    if let Some(v) = lookup(name, args, variant, &fp) {
        return Ok(v);
    }
    let v = Arc::new(compute()?);
    if ctx.cacheable(&fp) {
        insert(name, args, variant, fp, v.clone());
    }
    Ok(v)
}

fn lookup(name: &str, args: &str, variant: &str, fp: &Fingerprint) -> Option<Arc<Value>> {
    let mut views = VIEWS.lock().ok()?;
    let i = views.iter().position(|(n, a, _, f, _)| *n == name && a == args && f.path == fp.path)?;
    if views[i].2 != variant || views[i].3 != *fp {
        views.remove(i);
        return None;
    }
    let e = views.remove(i)?;
    let v = e.4.clone();
    views.push_back(e);
    Some(v)
}

fn insert(name: &'static str, args: &str, variant: &str, fp: Fingerprint, v: Arc<Value>) {
    let Ok(mut views) = VIEWS.lock() else { return };
    views.retain(|(n, a, _, f, _)| !(*n == name && a == args && f.path == fp.path));
    views.push_back((name, args.to_string(), variant.to_string(), fp, v));
    while views.len() > VIEWS_MAX {
        views.pop_front();
    }
}

type Shared = (&'static str, String, Fingerprint, Arc<dyn std::any::Any + Send + Sync>);

static SHARED: Mutex<Vec<Shared>> = Mutex::new(Vec::new());

/// The inputs every view starts from (bank rows, ledgers, the crypto
/// ledger, the full sheet, ...), shared by every Ctx under one fingerprint:
/// the live valuation, the bundle and the pages each build their own Ctx,
/// and would otherwise each read and build these again. Entries of an older
/// fingerprint of the same db are dropped on insert.
pub fn shared<T: Send + Sync + 'static>(ctx: &Ctx, name: &'static str, key: &str, load: impl FnOnce() -> T) -> Arc<T> {
    let Some(fp) = ctx.fingerprint() else { return Arc::new(load()) };
    let hit = SHARED.lock().ok().and_then(|s| {
        s.iter().find(|(n, k, f, _)| *n == name && k == key && *f == fp).map(|e| e.3.clone())
    });
    if let Some(v) = hit.and_then(|v| v.downcast::<T>().ok()) {
        return v;
    }
    let v = Arc::new(load());
    if !ctx.cacheable(&fp) {
        return v;
    }
    if let Ok(mut s) = SHARED.lock() {
        s.retain(|(n, k, f, _)| f.path != fp.path || (f.content == fp.content && f.today == fp.today && !(*n == name && k == key)));
        s.push((name, key.to_string(), fp, v.clone()));
    }
    v
}

/// Forget everything (tests, or after an out-of-band change).
pub fn clear() {
    if let Ok(mut s) = SHARED.lock() {
        s.clear();
    }
    if let Ok(mut v) = VIEWS.lock() {
        v.clear();
    }
    if let Ok(mut k) = KNOWN.lock() {
        k.clear();
    }
}

// ---------------------------------------------------------------------------
// The cached views (one key per api::get_* call shape, shared by the pages,
// the bundle and MCP)
// ---------------------------------------------------------------------------

use super::api;

type R = anyhow::Result<Arc<Value>>;

fn range(from: Option<&str>, to: Option<&str>) -> String {
    format!("{from:?}|{to:?}")
}

/// get_overview(live=false): the network-free net worth + sheet + PnL.
/// Its net_worth.as_of is the compute time, not the request's.
pub fn overview(ctx: &Ctx, from: Option<&str>, to: Option<&str>) -> R {
    view(ctx, "overview", &range(from, to), || api::get_overview(ctx, from, to, false))
}

/// get_net_worth_now(live=false). as_of is the compute time.
pub fn net_worth_offline(ctx: &Ctx) -> R {
    view(ctx, "net_worth_offline", "", || api::get_net_worth_now(ctx, false))
}

pub fn banks(ctx: &Ctx, from: Option<&str>, to: Option<&str>) -> R {
    view(ctx, "banks", &range(from, to), || api::get_banks(ctx, from, to))
}

pub fn spending(ctx: &Ctx, from: Option<&str>, to: Option<&str>) -> R {
    view(ctx, "spending", &range(from, to), || api::get_spending(ctx, from, to))
}

pub fn flexinvest(ctx: &Ctx, from: Option<&str>, to: Option<&str>) -> R {
    view(ctx, "flexinvest", &range(from, to), || api::get_flexinvest(ctx, from, to))
}

/// With crypto, also keyed on the tickers it is priced with: rather than
/// freezing whatever prices the first compute saw until the data changes, a
/// view filled from the snapshot fallback (say, right after startup)
/// doesn't outlive the first live tickers.
pub fn investments(ctx: &Ctx, include_crypto: bool) -> R {
    if !include_crypto {
        return view(ctx, "investments", "0", || api::get_investments(ctx, false));
    }
    let px = format!("{:?}", super::prices::live_or_cached_prices(ctx, &super::crypto::ASSETS));
    view_variant(ctx, "investments", "1", &px, || api::get_investments(ctx, true))
}

/// get_costs(live=true) over the cached investments (a cache hit).
pub fn costs(ctx: &Ctx) -> R {
    view(ctx, "costs", "", || {
        let inv = investments(ctx, true).ok();
        api::get_costs(ctx, true, inv.as_deref())
    })
}

pub fn ibkr(ctx: &Ctx) -> R {
    view(ctx, "ibkr", "", || api::get_ibkr(ctx))
}

pub fn month_range(ctx: &Ctx) -> R {
    view(ctx, "month_range", "", || api::get_month_range(ctx))
}

pub fn receivables(ctx: &Ctx) -> R {
    view(ctx, "receivables", "", || api::get_receivables(ctx))
}

pub fn month_detail(ctx: &Ctx, month: &str) -> R {
    view(ctx, "month_detail", month, || api::get_month_detail(ctx, month))
}

/// The live-or-cached crypto valuation: `{has_events, prices, stale, as_of,
/// result}` with prices/stale/as_of = prices::live_or_cached_prices(ASSETS)
/// and result = crypto::compute(...), null when there are no events.
/// Memoized on the db fingerprint and the prices it was priced with.
pub fn crypto_page(ctx: &Ctx) -> R {
    use super::{crypto, prices};
    let (px, stale, as_of) = prices::live_or_cached_prices(ctx, &crypto::ASSETS);
    let variant = format!("{px:?}|{stale}|{as_of:?}");
    view_variant(ctx, "crypto_page", "live", &variant, || {
        let export = ctx.export();
        let has_events = !export.events.is_empty();
        let result = if has_events {
            crypto::compute(&export, &px, &ctx.ym_now()).map(|r| r.json()).unwrap_or_else(|| serde_json::json!({"empty": true}))
        } else {
            Value::Null
        };
        let prices: serde_json::Map<String, Value> = px.iter().map(|(a, p)| (a.clone(), serde_json::json!(p))).collect();
        Ok(serde_json::json!({"has_events": has_events, "prices": prices, "stale": stale, "as_of": as_of, "result": result}))
    })
}
