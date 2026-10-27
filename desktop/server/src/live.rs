//! The live valuation behind GET /api/live and the export bundle, kept off
//! the request path.
//!
//! The valuation (exchange-balance sweep, live tickers, equity proxy quote)
//! is computed by a background thread and cached for TTL = 60s. A request
//! never waits on it: it gets the cached valuation, and
//! when that is older than the TTL (or was a stale fallback) a refresh starts
//! behind it -- stale-while-revalidate. One older than LIVE_FOR (no request
//! for a while) is served flagged stale. Before the first refresh lands (right
//! after startup; serve() warms it at once), a request gets the network-free
//! snapshot valuation, flagged stale: the same answer as when the live
//! sweep fails. A stale result is retried at most every RETRY_STALE, not
//! pinned for the whole TTL. Never errors: the client always gets 200 JSON.
//!
//! Under FINANCE_OFFLINE=1 there is no network to wait on: the valuation is
//! computed inline, exactly as before, and memoized on the db fingerprint.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use finance_engine::views::{api, cache, crypto, networth, prices, Ctx, MarketFeed};
use finance_engine::{config, db};

use crate::feed::{FEED, REQUEST_FEED};
use crate::log::log;

pub const TTL: Duration = Duration::from_secs(60);
/// How soon a stale (failed) valuation is retried.
const RETRY_STALE: Duration = Duration::from_secs(10);
/// Past this a cached valuation is served flagged stale (the refresh it
/// triggers lands a second or three later): after an idle hour, the first
/// visit shows "prices cached HH:MM", not an hour-old figure as live.
const LIVE_FOR: Duration = Duration::from_secs(300);

#[derive(Default)]
struct State {
    value: Option<(Instant, Value)>,
    refreshing: bool,
}

/// One valuation per tenant, keyed by its db path (finance_engine::tenant:
/// each user's balances, keys and holdings are their own).
static STATE: Mutex<BTreeMap<PathBuf, State>> = Mutex::new(BTreeMap::new());

fn key() -> PathBuf {
    config::db_path()
}

/// `f` on this tenant's slot.
fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    let k = key();
    STATE.lock().ok().map(|mut m| f(m.entry(k).or_default()))
}

/// The valuation when nothing at all could be computed.
pub fn empty_net_worth() -> Value {
    json!({"stale": true, "as_of": null, "total_hkd": 0.0, "total_eur": 0.0,
           "banks_hkd": 0.0, "crypto_hkd": 0.0, "investments_hkd": 0.0,
           "investments_cost_hkd": 0.0, "investments_gain_hkd": 0.0,
           "investments_holdings": [], "by_source": []})
}

fn is_stale(v: &Value) -> bool {
    v.get("stale").and_then(Value::as_bool).unwrap_or(true)
}

/// The valuation itself: live first; on any failure the
/// network-free snapshot valuation.
fn uncached(feed: &dyn MarketFeed) -> Value {
    let attempt = |live: bool| db::open().ok().and_then(|c| api::get_net_worth_now(&Ctx::new(&c, feed), live).ok());
    attempt(true).or_else(|| attempt(false)).unwrap_or_else(empty_net_worth)
}

/// The all-in crypto gain at live-or-cached
/// prices (the crypto page's tickers). None on any failure or no ledger.
fn crypto_profit_live(feed: &dyn MarketFeed) -> Option<Value> {
    let conn = db::open().ok()?;
    let ctx = Ctx::new(&conn, feed);
    let export = ctx.export();
    if export.events.is_empty() {
        return None;
    }
    let (px, stale, _) = prices::live_or_cached_prices(&ctx, &crypto::ASSETS);
    let totals = crypto::compute(&export, &px, &ctx.ym_now())?.totals;
    let r = |k: &str, nd| totals.get(k).and_then(Value::as_f64).map(|v| finance_engine::views::round_n(v, nd));
    Some(json!({"usd": r("profit_allin", 2)?, "roi_pct": r("roi_allin", 1)?, "stale": stale}))
}

/// Live net worth plus the same-instant crypto gain (priced from the same
/// tickers as the crypto page, so the overview tile can't disagree with it).
fn compute(feed: &dyn MarketFeed) -> Value {
    let mut value = uncached(feed);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("crypto_profit".into(), crypto_profit_live(feed).unwrap_or(Value::Null));
    }
    value
}

/// Stamp a memoized valuation with the time it is served, as a fresh compute.
fn stamp_now(mut v: Value) -> Value {
    if let Some(Value::String(s)) = v.get_mut("as_of") {
        *s = networth::as_of(chrono::Utc::now());
    }
    v
}

/// Offline: no network, so the same computation as online, inline, and
/// memoized on the db fingerprint (it reads nothing else).
fn offline() -> Value {
    let Ok(conn) = db::open() else { return compute(&FEED) };
    let ctx = Ctx::new(&conn, &FEED);
    match cache::view(&ctx, "live_net_worth", "", || Ok(compute(&FEED))) {
        Ok(v) => stamp_now((*v).clone()),
        Err(_) => compute(&FEED),
    }
}

/// Before the first live refresh: the snapshot valuation plus the crypto
/// gain at whatever tickers are cached. No network.
fn fallback() -> Value {
    let Ok(conn) = db::open() else { return empty_net_worth() };
    let ctx = Ctx::new(&conn, &REQUEST_FEED);
    let mut value = match cache::net_worth_offline(&ctx) {
        Ok(v) => stamp_now((*v).clone()),
        Err(_) => empty_net_worth(),
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert("crypto_profit".into(), crypto_profit_live(&REQUEST_FEED).unwrap_or(Value::Null));
    }
    value
}

/// Recompute now, on this thread (the refresher, and serve()'s warm-up).
pub fn refresh() {
    let t = Instant::now();
    let value = compute(&FEED);
    let stale = is_stale(&value);
    with_state(|st| {
        st.value = Some((Instant::now(), value));
        st.refreshing = false;
    });
    let who = finance_engine::tenant::current().filter(|t| !t.legacy).map(|t| format!(" ({})", t.label())).unwrap_or_default();
    log(&format!("[live] valuation refreshed in {:.0}ms{}{who}", t.elapsed().as_secs_f64() * 1000.0, if stale { " (stale)" } else { "" }));
    // fresh tickers (and maybe a new balance snapshot): re-price the views
    // now rather than on the next request
    crate::server::warm_views_async();
}

/// Start a background refresh unless one is running.
pub fn refresh_async() {
    let started = with_state(|st| !std::mem::replace(&mut st.refreshing, true)).unwrap_or(false);
    if !started {
        return;
    }
    // the tenant goes with it: the refresh values this user's tree
    if finance_engine::tenant::spawn("live-refresh", refresh).is_err() {
        with_state(|st| st.refreshing = false);
    }
}

/// The live valuation as of the last refresh; never blocks on the network.
pub fn net_worth() -> Value {
    if config::offline() {
        return offline();
    }
    let (cur, expired) = with_state(|st| match &st.value {
        Some((at, v)) => {
            let ttl = if is_stale(v) { RETRY_STALE } else { TTL };
            let mut v = v.clone();
            if at.elapsed() >= LIVE_FOR {
                // nobody asked for a while: these are no longer live
                // prices, and the badge says so (with their as_of time)
                if let Some(o) = v.as_object_mut() {
                    o.insert("stale".into(), Value::Bool(true));
                }
            }
            (Some(v), at.elapsed() >= ttl)
        }
        None => (None, true),
    })
    .unwrap_or((None, true));
    if expired {
        refresh_async();
    }
    cur.unwrap_or_else(fallback)
}
