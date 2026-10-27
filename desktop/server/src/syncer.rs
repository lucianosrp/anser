//! Background exchange/broker sync: the
//! same work as `fetch-crypto --exchange all`, on its own thread so a page
//! request never blocks on the minutes-long history refetch.
//!
//! Per tenant (finance_engine::tenant): each user's sync runs on their
//! tree with their keys, and its state, lock and fetch_status.json are
//! theirs. Single-flight twice over per user: inside the process a
//! mutex-guarded flag (one per tree) makes start() a no-op while that
//! user's sync runs, and across processes an exclusive lock on
//! `<their data dir>/sync.lock` keeps `anser-server sync` (CLI, cron) and the
//! server from ever walking the same exchanges at the same time. Across
//! users, at most [`MAX_CONCURRENT`] syncs run at once (a small server's
//! network and CPU); the others wait their turn. The fetches are idempotent anyway
//! (windowed merge + natural-key UNIQUE), so the locks are about not
//! doubling the rate-limit budget, not about data safety.
//!
//! Entry points: POST /sync -> start() for the requester's tree; a data GET
//! (/api/export.json, /api/ledger.json) -> maybe_autostart(), which only
//! fires when that ledger is older than AUTO_SYNC_AFTER and never for a db
//! with no ledger yet; and the scheduler ([`spawn_scheduler`]), which walks
//! every user with configured sources every [`SCHEDULE_EVERY`] and starts
//! the stale ones, so a user who doesn't open the app still syncs.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use serde::Serialize;

use finance_engine::tenant::{self, Tenant};
use finance_engine::{config, fetch};

use crate::log::log;

/// Ledger older than this -> auto-sync on the next data request.
pub const AUTO_SYNC_AFTER: Duration = Duration::from_secs(24 * 3600);
/// Syncs running at once, across users.
pub const MAX_CONCURRENT: usize = 2;
/// How often the scheduler looks at every user's ledger.
pub const SCHEDULE_EVERY: Duration = Duration::from_secs(15 * 60);

/// What the header's sync control shows. Times are unix seconds (floats).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub running: bool,
    pub started: Option<f64>,
    pub finished: Option<f64>,
    pub error: Option<String>,
}

/// One status per tree (its db path).
static STATE: Mutex<BTreeMap<PathBuf, Status>> = Mutex::new(BTreeMap::new());

fn now_s() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn key() -> PathBuf {
    config::db_path()
}

/// The active tenant's sync status.
pub fn status() -> Status {
    let k = key();
    STATE.lock().ok().and_then(|s| s.get(&k).cloned()).unwrap_or_default()
}

/// The cross-process lock: held for the whole sync, released on drop.
pub struct SyncLock(#[allow(dead_code)] File);

/// The active tenant's lock (`<data dir>/sync.lock`).
pub fn try_lock() -> Result<SyncLock> {
    let dir = config::data_dir();
    std::fs::create_dir_all(&dir)?;
    let f = File::options().create(true).truncate(false).write(true).open(dir.join("sync.lock"))?;
    match f.try_lock() {
        Ok(()) => Ok(SyncLock(f)),
        Err(std::fs::TryLockError::WouldBlock) => bail!("another sync is already running"),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// The global semaphore: at most [`MAX_CONCURRENT`] permits.
static RUNNING: Mutex<usize> = Mutex::new(0);
static FREED: Condvar = Condvar::new();

struct Permit;

fn acquire() -> Permit {
    let mut n = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    while *n >= MAX_CONCURRENT {
        n = FREED.wait(n).unwrap_or_else(|p| p.into_inner());
    }
    *n += 1;
    Permit
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut n = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        *n = n.saturating_sub(1);
        FREED.notify_one();
    }
}

/// How many syncs hold a permit now (tests).
pub fn running_now() -> usize {
    *RUNNING.lock().unwrap_or_else(|p| p.into_inner())
}

/// Every API source in order; one failing source doesn't stop the others,
/// but the run reports all failures. Caller holds the lock.
pub fn run_all(log_line: &dyn Fn(&str)) -> Result<()> {
    let conn = finance_engine::db::open()?;
    let mut errors = Vec::new();
    for key in fetch::API_SOURCES {
        log_line(&format!("[sync] fetching {key} ..."));
        if let Err(e) = fetch::fetch_source(&conn, key, false, log_line) {
            errors.push(format!("{key}: {}", e.to_string().chars().take(120).collect::<String>()));
        }
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    Ok(())
}

fn finish(k: &PathBuf, err: Option<String>) {
    if let Ok(mut all) = STATE.lock() {
        let s = all.entry(k.clone()).or_default();
        s.running = false;
        s.finished = Some(now_s());
        s.error = err.map(|e| e.chars().take(200).collect());
    }
}

/// "[sync] ..." for the owner (as before), "[sync user 2] ..." for others.
fn tag() -> String {
    match tenant::current().filter(|t| !t.legacy) {
        Some(t) => format!("[sync {}]", t.label()),
        None => "[sync]".into(),
    }
}

/// Start a background sync of the active tenant's tree unless one is
/// running (here or in another process). True if started. The thread
/// waits for a permit of the global semaphore before it fetches.
pub fn start() -> bool {
    let k = key();
    {
        let Ok(mut all) = STATE.lock() else { return false };
        let s = all.entry(k.clone()).or_default();
        if s.running {
            return false;
        }
        *s = Status { running: true, started: Some(now_s()), finished: s.finished, error: None };
    }
    let kk = k.clone();
    // the tenant goes with the thread: its tree, its keys, its lock
    let spawned = tenant::spawn("ledger-sync", move || {
        let tag = tag();
        let _permit = acquire();
        let line = |l: &str| log(&l.replacen("[sync]", &tag, 1));
        let result = try_lock().and_then(|_lock| run_all(&line));
        match result {
            Ok(()) => {
                finish(&kk, None);
                log(&format!("{tag} done"));
                // the ledger moved: recompute the views now, not on the next visit
                crate::server::warm_async();
            }
            Err(e) => {
                log(&format!("{tag} failed: {e}"));
                finish(&kk, Some(e.to_string()));
            }
        }
    });
    if spawned.is_err() {
        finish(&k, Some("could not start the sync thread".into()));
        return false;
    }
    true
}

/// One source's outcome in a [`refresh`].
#[derive(Debug, Clone, Serialize)]
pub struct SourceResult {
    pub source: String,
    pub ok: bool,
    pub ms: u64,
    pub error: Option<String>,
}

/// Longest one source may take in a [`refresh`]; a slower one is reported
/// and left to finish (or fail) on its own.
pub const REFRESH_SOURCE_TIMEOUT: Duration = Duration::from_secs(45);
/// How long a refresh waits for a running sync (or another refresh) of the
/// same tree before it gives up.
const REFRESH_WAIT: Duration = Duration::from_secs(120);

fn failure(source: &str, error: impl Into<String>) -> SourceResult {
    SourceResult { source: source.into(), ok: false, ms: 0, error: Some(error.into()) }
}

/// "Pull the latest now" (Ctrl+Shift+K): the quick fetch of every
/// configured source ([`fetch::refresh_source`]: Binance only its
/// day-to-day endpoints), in parallel and each under its own timeout, then the
/// live valuation (balances, tickers) recomputed from fresh tickers. Single
/// flight with [`start`] and `anser-server sync`: if one is running it waits
/// for it. A source never synced before is left to a deliberate first Sync
/// (that is the minutes-long full walk). Never errs; each source reports.
pub fn refresh() -> Vec<SourceResult> {
    let k = key();
    // claim the in-process flag (the same one start() honours), waiting our turn
    let deadline = Instant::now() + REFRESH_WAIT;
    loop {
        {
            let mut all = STATE.lock().unwrap_or_else(|p| p.into_inner());
            let s = all.entry(k.clone()).or_default();
            if !s.running {
                *s = Status { running: true, started: Some(now_s()), finished: s.finished, error: None };
                break;
            }
        }
        if Instant::now() >= deadline {
            return vec![failure("sync", "another sync is still running")];
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let results = refresh_claimed(deadline);
    let failed: Vec<&str> = results.iter().filter(|r| !r.ok).map(|r| r.source.as_str()).collect();
    finish(&k, (!failed.is_empty()).then(|| format!("refresh failed: {}", failed.join(", "))));
    results
}

fn refresh_claimed(deadline: Instant) -> Vec<SourceResult> {
    let tag = tag();
    let _permit = acquire();
    // the cross-process lock (a CLI or cron sync): wait for it too
    let lock = loop {
        match try_lock() {
            Ok(l) => break std::sync::Arc::new(l),
            Err(e) if e.to_string().contains("already running") && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return vec![failure("sync", e.to_string())],
        }
    };
    let conn = finance_engine::db::open().ok();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut pending = Vec::new();
    let mut out = Vec::new();
    for entry in finance_engine::catalog::api() {
        if !entry.fields.iter().all(|f| config::env(f.key).is_some()) {
            continue;
        }
        let key = entry.key;
        let seen = conn.as_ref().and_then(|c| finance_engine::db::last_fetch_ms(c, Some(key)).ok().flatten());
        if seen.is_none() && !config::offline() {
            out.push(failure(key, "never synced: use Sync now once first"));
            continue;
        }
        pending.push(key);
        let (tx, lock, tag) = (tx.clone(), lock.clone(), tag.clone());
        // the lock rides with each fetch: it is held until the last one ends,
        // even one we stopped waiting for
        let _ = tenant::spawn(&format!("refresh-{key}"), move || {
            let _lock = lock;
            let t = Instant::now();
            let line = |l: &str| log(&l.replacen("[sync]", &tag, 1));
            let r = finance_engine::db::open().and_then(|c| fetch::refresh_source(&c, key, &line));
            let _ = tx.send((key, t.elapsed(), r));
        });
    }
    drop(tx);
    drop(lock);
    let started = Instant::now();
    while !pending.is_empty() {
        let left = REFRESH_SOURCE_TIMEOUT.saturating_sub(started.elapsed());
        let Ok((key, took, r)) = rx.recv_timeout(left) else { break };
        pending.retain(|k| *k != key);
        let error = r.err().map(|e| e.to_string().chars().take(160).collect::<String>());
        out.push(SourceResult { source: key.into(), ok: error.is_none(), ms: took.as_millis() as u64, error });
    }
    for key in pending {
        out.push(SourceResult { ms: REFRESH_SOURCE_TIMEOUT.as_millis() as u64, ..failure(key, "timed out") });
    }
    // the ledger moved and the tickers are stale: value it again, now
    let t = Instant::now();
    fetch::live::clear_price_cache();
    crate::live::refresh();
    out.push(SourceResult { source: "prices".into(), ok: true, ms: t.elapsed().as_millis() as u64, error: None });
    out.sort_by(|a, b| a.source.cmp(&b.source));
    out
}

/// MAX(fetched_at) over the raw sources: "when did the ledger last sync".
/// None on any failure (fresh db, mid-merge).
pub fn last_fetch_ts_ms() -> Option<i64> {
    let conn = finance_engine::db::open().ok()?;
    finance_engine::db::last_fetch_ms(&conn, None).ok().flatten()
}

/// Age-gated auto-sync; never for a fresh ledger, never for an empty one
/// (that first, minutes-long fetch should be a deliberate act), never
/// under FINANCE_OFFLINE.
pub fn maybe_autostart(last_fetch_ms: Option<i64>) -> bool {
    // offline (golden runs, tests) every fetch refuses the network anyway
    let Some(ms) = last_fetch_ms.filter(|_| !config::offline()) else { return false };
    if now_s() - ms as f64 / 1000.0 < AUTO_SYNC_AFTER.as_secs_f64() {
        return false;
    }
    start()
}

/// The active tenant has an API source configured (a key in their store,
/// or for the owner .env / the environment too).
pub fn has_sources() -> bool {
    finance_engine::catalog::api().iter().any(|e| e.fields.iter().all(|f| config::env(f.key).is_some()))
}

/// Every user with a tree, as tenants (the owner first).
pub fn tenants() -> Vec<Tenant> {
    crate::auth::users().unwrap_or_default().iter().filter_map(|u| Tenant::for_user(u.id, &u.home)).collect()
}

/// One pass of the scheduler: each user with configured sources and a
/// stale ledger gets a sync. Returns the uids started.
pub fn schedule_once() -> Vec<i64> {
    let mut started = vec![];
    for t in tenants() {
        let go = t.enter(|| has_sources() && maybe_autostart(last_fetch_ts_ms()));
        if go {
            started.push(t.uid);
        }
    }
    started
}

/// The scheduler thread (the server starts it): a pass every
/// [`SCHEDULE_EVERY`], the first a minute after startup. Not under
/// FINANCE_OFFLINE.
pub fn spawn_scheduler() {
    if config::offline() {
        return;
    }
    let _ = std::thread::Builder::new().name("sync-scheduler".into()).spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            let started = schedule_once();
            if !started.is_empty() {
                log(&format!("[sync] scheduled: user(s) {}", started.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(", ")));
            }
            std::thread::sleep(SCHEDULE_EVERY);
        }
    });
}

/// `anser-server sync`: the same run in the foreground, refusing to overlap.
pub fn run_foreground(log_line: &dyn Fn(&str)) -> Result<()> {
    let _lock = try_lock().map_err(|e| anyhow!("{e} (the server or another `anser-server sync`)"))?;
    run_all(log_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_age_gate() {
        assert!(!maybe_autostart(None), "no ledger yet: never");
        let fresh = (now_s() * 1000.0) as i64 - 3_600_000;
        assert!(!maybe_autostart(Some(fresh)));
    }

    #[test]
    fn lock_is_exclusive_per_tree() {
        let dir = std::env::temp_dir().join(format!("finance-lock-test-{}", std::process::id()));
        let a = Tenant::user_at(2, &dir.join("users/2"));
        let b = Tenant::user_at(3, &dir.join("users/3"));
        let la = a.enter(try_lock).unwrap();
        assert!(a.enter(try_lock).is_err());
        let lb = b.enter(try_lock);
        assert!(lb.is_ok(), "another user's sync isn't blocked by this one's");
        drop(la);
        assert!(a.enter(try_lock).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_semaphore_admits_two() {
        let p1 = acquire();
        let p2 = acquire();
        assert_eq!(running_now(), 2);
        let (tx, rx) = std::sync::mpsc::channel();
        let h = std::thread::spawn(move || {
            let _p = acquire();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "a third waits");
        drop(p1);
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "and runs once one is freed");
        h.join().unwrap();
        drop(p2);
        assert_eq!(running_now(), 0);
    }
}
