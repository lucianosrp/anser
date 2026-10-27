//! The live, network-bound reads the pages make, bounded so a slow exchange
//! can never stall a request:
//!
//! * [`cached_live_prices`] -- the live half of
//!   views::prices::live_or_cached_prices: bounded, TTL-cached tickers (the
//!   last-snapshot fallback is views::prices').
//! * [`swr_live_prices`] -- the same cache read stale-while-revalidate, for
//!   request paths that must never wait on the network.
//! * [`sweep`] -- the fan-out half of the live net worth (views::networth):
//!   every source's live_balances() plus the equity-proxy quote in parallel,
//!   with ONE total deadline. The valuation (pricing, dust, snapshots) stays
//!   with the views; this only gathers.
//!
//! Threads that miss the deadline are left to finish on their own and their
//! results are dropped.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{prices, API_SOURCES};

const PRICE_TTL: Duration = Duration::from_secs(60);

type PriceCache = Mutex<BTreeMap<Vec<String>, (Instant, BTreeMap<String, f64>)>>;

fn price_cache() -> &'static PriceCache {
    static CACHE: std::sync::OnceLock<PriceCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Forget the cached tickers: the next read fetches (a manual refresh).
pub fn clear_price_cache() {
    if let Ok(mut cache) = price_cache().lock() {
        cache.clear();
    }
}

/// Run `f` on its own thread; None if it errs or misses the deadline.
fn bounded<T: Send + 'static>(timeout: Duration, f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok().and_then(Result::ok)
}

/// prices::live_prices within `timeout`; a success is reused for 60s per
/// asset set. Err when the call fails, times out or comes back empty -- the
/// caller then serves the last balance snapshot, flagged stale.
pub fn cached_live_prices(assets: &[String], timeout: Duration) -> anyhow::Result<BTreeMap<String, f64>> {
    let key = assets.to_vec();
    if let Ok(cache) = price_cache().lock() {
        if let Some((at, p)) = cache.get(&key) {
            if at.elapsed() < PRICE_TTL {
                return Ok(p.clone());
            }
        }
    }
    let want = key.clone();
    let p = bounded(timeout, move || prices::live_prices(&want))
        .filter(|p| !p.is_empty())
        .ok_or_else(|| anyhow::anyhow!("live prices unavailable"))?;
    if let Ok(mut cache) = price_cache().lock() {
        cache.insert(key, (Instant::now(), p.clone()));
    }
    Ok(p)
}

/// The request-path read of the same cache, stale-while-revalidate: a
/// fresh entry is returned as is; an entry past the TTL but younger than
/// `max_age` is returned too, while one background thread refetches it; no
/// entry (or a too old one) is an Err right away -- the caller serves the
/// snapshot fallback, flagged stale -- with the refetch started. Never
/// blocks on the network.
pub fn swr_live_prices(assets: &[String], timeout: Duration, max_age: Duration) -> anyhow::Result<BTreeMap<String, f64>> {
    let key = assets.to_vec();
    let cached = price_cache().lock().ok().and_then(|c| c.get(&key).map(|(at, p)| (at.elapsed(), p.clone())));
    if let Some((age, p)) = &cached {
        if *age < PRICE_TTL {
            return Ok(p.clone());
        }
    }
    refresh_in_background(key, timeout);
    match cached {
        Some((age, p)) if age < max_age => Ok(p),
        _ => anyhow::bail!("live prices not fetched yet"),
    }
}

/// One refetch per asset set at a time.
fn refresh_in_background(key: Vec<String>, timeout: Duration) {
    static INFLIGHT: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
    {
        let Ok(mut inflight) = INFLIGHT.lock() else { return };
        if inflight.contains(&key) {
            return;
        }
        inflight.push(key.clone());
    }
    let spawned = std::thread::Builder::new().name("price-refresh".into()).spawn({
        let key = key.clone();
        move || {
            let _ = cached_live_prices(&key, timeout);
            if let Ok(mut inflight) = INFLIGHT.lock() {
                inflight.retain(|k| *k != key);
            }
        }
    });
    if spawned.is_err() {
        if let Ok(mut inflight) = INFLIGHT.lock() {
            inflight.retain(|k| *k != key);
        }
    }
}

/// What one bounded fan-out gathered.
#[derive(Debug, Clone, Default)]
pub struct Sweep {
    /// source key -> {asset: amount}, for every source that answered in time
    pub balances: BTreeMap<String, BTreeMap<String, f64>>,
    /// false when any source failed or missed the deadline (the caller then
    /// values crypto from the last snapshot, flagged stale)
    pub ok: bool,
    /// the fund proxy's quote (symbol, price), when it arrived in time
    pub equity_now: Option<(String, f64)>,
}

// The index proxy the sweep quotes too (the investment setup's fund
// account; none: no quote). Set by the live net worth before it sweeps,
// on the same thread: per thread, so two tenants valuing at once don't
// swap proxies.
thread_local! {
    static SWEEP_PROXY: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

pub fn set_sweep_proxy(symbol: Option<String>) {
    SWEEP_PROXY.with(|p| *p.borrow_mut() = symbol);
}

/// Every source's live_balances() and the equity-proxy quote, in parallel,
/// with one total deadline.
pub fn sweep(timeout: Duration) -> Sweep {
    enum Msg {
        Bal(&'static str, anyhow::Result<BTreeMap<String, f64>>),
        Equity(String, anyhow::Result<f64>),
    }
    let (tx, rx) = mpsc::channel();
    for key in API_SOURCES {
        let tx = tx.clone();
        // the keys are the tenant's: carry it to the thread
        std::thread::spawn(crate::tenant::carry(move || {
            let _ = tx.send(Msg::Bal(key, super::live_balances(key)));
        }));
    }
    let proxy = SWEEP_PROXY.with(|p| p.borrow().clone());
    if let Some(sym) = proxy.clone() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let r = prices::equity_now(&sym);
            let _ = tx.send(Msg::Equity(sym, r));
        });
    }
    drop(tx);
    let deadline = Instant::now() + timeout;
    let mut out = Sweep { ok: true, ..Default::default() };
    let mut pending = API_SOURCES.len() + proxy.is_some() as usize;
    let mut answered = 0;
    while pending > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Bal(key, Ok(b))) => {
                out.balances.insert(key.to_string(), b);
                answered += 1;
            }
            Ok(Msg::Bal(_, Err(_))) => {
                out.ok = false;
                answered += 1;
            }
            Ok(Msg::Equity(sym, r)) => out.equity_now = r.ok().map(|px| (sym, px)),
            Err(_) => break,
        }
        pending -= 1;
    }
    if answered < API_SOURCES.len() {
        out.ok = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_sweep_is_not_ok_and_fast() {
        std::env::set_var("FINANCE_OFFLINE", "1");
        let t = Instant::now();
        let s = sweep(Duration::from_secs(3));
        assert!(!s.ok);
        assert!(s.equity_now.is_none());
        assert!(t.elapsed() < Duration::from_secs(2));
    }
}
