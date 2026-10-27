//! The sync contract for local-first clients: every computed view as one
//! JSON document. The desktop app
//! (desktop/core/src/bundle.rs) caches it and renders offline.
//!
//! BUNDLE_VERSION bumps on any breaking change to the bundle's shape; new
//! sections are additive ([`ADDED_SECTIONS`]: `capabilities`, the sources and
//! features views::capabilities derives).
//!
//! The server sends [`bundle_bytes`]: every section after `net_worth` is a
//! pure function of the db fingerprint and the crypto tickers, so that part
//! is kept serialized (views::cache explains the fingerprint) and only the
//! head -- version, generated_at, the live net worth -- is written per
//! request. The one wall-clock field inside the cached part,
//! overview.net_worth.as_of, is stamped with the request time on the way
//! out, so the bytes equal a fresh serde_json serialization of [`bundle`].

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use crate::views::{cache, crypto, num, prices, Ctx};

pub const BUNDLE_VERSION: i64 = 1;

/// Sections added without a version bump (a client that doesn't know one
/// ignores it). The frozen view goldens predate them, so the golden
/// checks skip these.
pub const ADDED_SECTIONS: [&str; 4] = ["profile", "user_settings", "data_rev", "capabilities"];

/// The per-user head sections after net_worth (profile.rs): tiny, written on
/// every request, never cached, so a settings write shows on the next pull.
fn user_head(ctx: &Ctx) -> [(&'static str, Value); 3] {
    [("profile", crate::profile::bundle_profile(ctx.conn)), ("user_settings", crate::profile::bundle_settings(ctx.conn)),
     ("data_rev", json!(crate::categories::data_rev(ctx.conn)))]
}

/// _safe: one failing section must not sink the whole bundle -- the client
/// shows that view as unavailable and everything else still syncs. A panic
/// counts as a failure too.
fn safe(f: impl FnOnce() -> anyhow::Result<Arc<Value>>) -> Arc<Value> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => Arc::new(json!({"error": truncate(&format!("Error: {e:#}"), 300)})),
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            Arc::new(json!({"error": truncate(&format!("Panic: {msg}"), 300)}))
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// live_or_cached_prices(ASSETS): what the crypto section is priced with.
type Px = (Vec<(String, f64)>, bool, Option<String>);

/// The crypto section: DCA profit + holdings at live-or-cached prices.
fn crypto_section(ctx: &Ctx, px: &Px) -> anyhow::Result<Value> {
    let export = ctx.export();
    let (px, stale, as_of) = px;
    match crypto::compute(&export, px, &ctx.ym_now()) {
        None => Ok(json!({"empty": true})),
        Some(res) => Ok(res.bundle_json(*stale, as_of.as_deref())),
    }
}

/// Current-month X->base rates from the fx_rates cache (network-free: the
/// latest cached month on or before this one), so clients convert with
/// these instead of deriving a rate from some holding. `base` is the
/// profile's base currency.
fn fx_section(ctx: &Ctx) -> anyhow::Result<Value> {
    let ym = ctx.ym_now();
    let rates = ctx.rates();
    let (m, f): (Option<String>, Option<i64>) = ctx.conn.query_row(
        "SELECT MAX(month) m, MAX(fetched_at) f FROM fx_rates WHERE month <= ?",
        [format!("{ym}-01")],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut out = Map::new();
    out.insert("base".into(), json!(rates.base()));
    for (ccy, _) in crate::views::fx::fx_pairs(ctx.conn) {
        let v = rates.rate(&ccy, &ym);
        if v > 0.0 {
            out.insert(ccy, num(v));
        }
    }
    let month: String = m.unwrap_or_default().chars().take(7).collect();
    out.insert("month".into(), if month.is_empty() { Value::Null } else { json!(month) });
    out.insert("as_of".into(), json!(f));
    Ok(Value::Object(out))
}

fn ibkr_section(ctx: &Ctx) -> anyhow::Result<Value> {
    let d = cache::ibkr(ctx)?;
    let mut out = Map::new();
    for k in ["snapshot", "performance", "cost_hkd", "value_hkd", "gain_hkd"] {
        out.insert(k.into(), d.get(k).cloned().ok_or_else(|| anyhow::anyhow!("KeyError: '{k}'"))?);
    }
    Ok(Value::Object(out))
}

/// Every section after net_worth, in the bundle's fixed order (a couple of
/// fallbacks write to the cache, so order is part of the contract). The
/// memoized views come from views::cache.
fn sections(ctx: &Ctx, px: &Px) -> Vec<(&'static str, Arc<Value>)> {
    let own = |f: &dyn Fn() -> anyhow::Result<Value>| safe(|| f().map(Arc::new));
    let mut out = Vec::with_capacity(11);
    out.push(("overview", safe(|| cache::overview(ctx, None, None))));
    out.push(("banks", safe(|| cache::banks(ctx, None, None))));
    out.push(("spending", safe(|| cache::spending(ctx, None, None))));
    out.push(("investments", safe(|| cache::investments(ctx, true))));
    out.push(("costs", safe(|| cache::costs(ctx))));
    out.push(("flexinvest", safe(|| cache::flexinvest(ctx, None, None))));
    out.push(("ibkr", own(&|| ibkr_section(ctx))));
    out.push(("crypto", own(&|| crypto_section(ctx, px))));
    out.push(("month_range", safe(|| cache::month_range(ctx))));
    out.push(("receivables", safe(|| cache::receivables(ctx))));
    out.push(("fx", own(&|| fx_section(ctx))));
    out.push(("capabilities", own(&|| Ok(crate::views::capabilities::capabilities(ctx)))));
    out
}

/// bundle(): all computed views. `net_worth` lets the server pass its
/// TTL-cached live valuation; otherwise the network-free snapshot valuation
/// is used.
pub fn bundle(ctx: &Ctx, net_worth: Option<Value>) -> Value {
    let mut b = Map::new();
    b.insert("version".into(), json!(BUNDLE_VERSION));
    b.insert("generated_at".into(), json!(chrono::Utc::now().timestamp_millis()));
    let nw = net_worth.unwrap_or_else(|| {
        let mut nw = (*safe(|| cache::net_worth_offline(ctx))).clone();
        if let Some(Value::String(s)) = nw.get_mut("as_of") {
            *s = crate::views::networth::as_of(chrono::Utc::now()); // computed "now"
        }
        nw
    });
    b.insert("net_worth".into(), nw);
    for (k, v) in user_head(ctx) {
        b.insert(k.into(), v);
    }
    let px = prices::live_or_cached_prices(ctx, &crypto::ASSETS);
    for (k, v) in sections(ctx, &px) {
        b.insert(k.into(), (*v).clone());
    }
    restamp(&mut b);
    Value::Object(b)
}

/// overview.net_worth.as_of = now, as an uncached compute would have it.
fn restamp(b: &mut Map<String, Value>) {
    if let Some(Value::String(s)) = b.get_mut("overview").and_then(|o| o.get_mut("net_worth")).and_then(|n| n.get_mut("as_of")) {
        *s = crate::views::networth::as_of(chrono::Utc::now());
    }
}

/// The serialized sections after net_worth: `,"overview":{..},...,"fx":{..}}`,
/// and where overview.net_worth.as_of sits in it.
struct Tail {
    bytes: Vec<u8>,
    as_of_at: Vec<usize>,
    as_of_len: usize,
}

/// (fingerprint, prices key) -> tail. One slot: the bundle is one document.
static TAIL: Mutex<Option<(cache::Fingerprint, String, Arc<Tail>)>> = Mutex::new(None);

#[cfg(feature = "test-hooks")]
static TAIL_BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Times the serialized tail was built (a cache miss). Tests use this to
/// check a warm-up and the requests that join it compute once.
#[cfg(feature = "test-hooks")]
pub fn tail_builds() -> usize {
    use std::sync::atomic::Ordering;
    TAIL_BUILDS.load(Ordering::SeqCst)
}

/// Drop the serialized tail (tests).
#[cfg(feature = "test-hooks")]
pub fn clear_tail() {
    if let Ok(mut slot) = TAIL.lock() {
        *slot = None;
    }
}

fn build_tail(ctx: &Ctx, px: &Px) -> (Tail, bool) {
    let secs = sections(ctx, px);
    let ok = secs.iter().all(|(_, v)| v.get("error").is_none());
    let mut bytes = Vec::with_capacity(256 * 1024);
    for (k, v) in &secs {
        bytes.push(b',');
        serde_json::to_writer(&mut bytes, k).expect("write to vec");
        bytes.push(b':');
        serde_json::to_writer(&mut bytes, &**v).expect("write to vec");
    }
    bytes.push(b'}');
    let as_of = secs[0].1.get("net_worth").and_then(|n| n.get("as_of")).and_then(Value::as_str).unwrap_or("");
    let mut as_of_at = Vec::new();
    if as_of.len() >= 26 {
        // a microsecond timestamp: unique in the document
        let needle = format!("\"{as_of}\"").into_bytes();
        let mut i = 0;
        while let Some(p) = find(&bytes[i..], &needle) {
            as_of_at.push(i + p + 1);
            i += p + needle.len();
        }
    }
    (Tail { bytes, as_of_at, as_of_len: as_of.len() }, ok)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The bundle as the server sends it: byte for byte what
/// `serde_json::to_vec(&bundle(ctx, Some(net_worth)))` would produce,
/// with everything after net_worth served from the serialized cache while
/// the db fingerprint and the crypto tickers hold.
pub fn bundle_bytes(ctx: &Ctx, net_worth: &Value) -> Vec<u8> {
    let px = prices::live_or_cached_prices(ctx, &crypto::ASSETS);
    let px_key = format!("{px:?}");
    let fp = ctx.fingerprint();
    let hit = fp.as_ref().and_then(|fp| {
        let slot = TAIL.lock().ok()?;
        let (f, k, t) = slot.as_ref()?;
        (f == fp && *k == px_key).then(|| t.clone())
    });
    let tail = hit.unwrap_or_else(|| {
        #[cfg(feature = "test-hooks")]
        TAIL_BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (t, ok) = build_tail(ctx, &px);
        let t = Arc::new(t);
        if let (Some(fp), true) = (fp.filter(|fp| ctx.cacheable(fp)), ok) {
            if let Ok(mut slot) = TAIL.lock() {
                *slot = Some((fp, px_key, t.clone()));
            }
        }
        t
    });

    let mut out = Vec::with_capacity(tail.bytes.len() + 16 * 1024);
    out.extend_from_slice(b"{\"version\":");
    serde_json::to_writer(&mut out, &BUNDLE_VERSION).expect("write to vec");
    out.extend_from_slice(b",\"generated_at\":");
    serde_json::to_writer(&mut out, &chrono::Utc::now().timestamp_millis()).expect("write to vec");
    out.extend_from_slice(b",\"net_worth\":");
    serde_json::to_writer(&mut out, net_worth).expect("write to vec");
    for (k, v) in user_head(ctx) {
        out.push(b',');
        serde_json::to_writer(&mut out, k).expect("write to vec");
        out.push(b':');
        serde_json::to_writer(&mut out, &v).expect("write to vec");
    }
    let base = out.len();
    out.extend_from_slice(&tail.bytes);
    let now = crate::views::networth::as_of(chrono::Utc::now());
    if now.len() == tail.as_of_len {
        for &at in &tail.as_of_at {
            out[base + at..base + at + now.len()].copy_from_slice(now.as_bytes());
        }
    }
    out
}
