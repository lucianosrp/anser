//! Binance read-only client + ledger fetch -> crypto_events (exchange='binance').
//!
//! Incremental by default: exchange history is immutable, so a sync only
//! re-walks the span since the last successful pull plus a 90-day overlap
//! (late-settling rows: deposits credit after insertTime, fiat orders flip
//! status, withdrawal completeTime lags). The full 2020-onward walk happens
//! only into an empty ledger or with `--full`. Events land via merge_events
//! (wipe just the re-fetched window, natural-key dedupe), and the reconcile
//! anchors are re-derived from the STORED ledger after the merge, so the
//! present stays exact either way.
//!
//! The live refresh uses [`fetch_quick`] instead: only the day-to-day
//! endpoints, in parallel, leaving the cursor and the anchors to the next
//! full sync (see [`QUICK_KINDS`]).
//!
//! Signed GETs only. The key is read-only, and this module has no way to
//! place an order or withdraw: there is no POST in fetch::http.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use hmac::{Hmac, KeyInit, Mac};
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::Sha256;

use super::http::{self, now_ms, urlencode};
use crate::db::{self, Event};
use super::{f, istr, num_i64, stored_str, truthy, Log};
use crate::config;

pub const BASE: &str = "https://api.binance.com";
pub const ASSETS: [&str; 3] = ["BTC", "BNB", "ETH"];
const QUOTES: [&str; 10] = ["USDT", "BUSD", "USDC", "FDUSD", "TUSD", "EUR", "DAI", "BTC", "ETH", "BNB"];
const STABLE: [&str; 7] = ["USDT", "BUSD", "USDC", "FDUSD", "TUSD", "DAI", "USD"];
/// Exchange cash: the EUR fiat balance + stablecoins parked on Binance. They
/// get their own 'cash-*' ledger legs so net worth sees money sitting on the
/// exchange that isn't in a tracked crypto position.
pub const CASH: [&str; 7] = ["EUR", "USDT", "BUSD", "USDC", "FDUSD", "TUSD", "DAI"];
/// 2020-01-01 00:00 UTC. (Older rows were walked from LOCAL midnight; the
/// few hours' difference only moves the floor of the full walk.)
pub const HISTORY_START_MS: i64 = 1_577_836_800_000;
const DAY_MS: i64 = 86_400_000;
/// Incremental syncs re-walk this much before the last successful fetch.
/// Generous on purpose: everything Binance settles late resolves well inside
/// 90 days, and the natural-key UNIQUE makes the overlap free of duplicates.
const OVERLAP_MS: i64 = 90 * DAY_MS;

fn is_asset(a: &str) -> bool {
    ASSETS.contains(&a)
}
fn is_cash(a: &str) -> bool {
    CASH.contains(&a)
}
fn ours(a: &str) -> bool {
    is_asset(a) || is_cash(a)
}
fn usd_of(amount: f64, asset: &str) -> Option<f64> {
    STABLE.contains(&asset).then_some(amount)
}
/// Delisted stables can't exist today, so their reconcile residual is booked
/// at the delisting date (BUSD support ended 2024-03-01), not now -- else a
/// phantom BUSD balance would inflate every month since the sunset.
fn sunset_ms(asset: &str) -> Option<i64> {
    (asset == "BUSD").then_some(1_709_251_200_000)
}

type HmacSha256 = Hmac<Sha256>;

/// HMAC-signed REST client. Read-only GET endpoints only.
pub struct Client {
    key: String,
    secret: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        let (Some(key), Some(secret)) = (config::env("BINANCE_API_KEY"), config::env("BINANCE_API_SECRET")) else {
            bail!("Missing BINANCE_API_KEY / BINANCE_API_SECRET {}", crate::secrets::where_to_set());
        };
        Ok(Self::new(&key, &secret))
    }

    /// A client on explicit keys (the source test, before they are saved).
    pub fn new(key: &str, secret: &str) -> Self {
        Client { key: key.into(), secret: secret.into(), agent: http::agent(Duration::from_secs(30)) }
    }

    fn req(&self, path: &str, params: &[(&str, String)], signed: bool) -> Result<Value> {
        let mut params = params.to_vec();
        if signed {
            params.push(("timestamp", now_ms().to_string()));
            params.push(("recvWindow", "60000".into()));
        }
        let query = urlencode(&params);
        let mut url = format!("{BASE}{path}?{query}");
        if signed {
            let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes()).expect("hmac takes any key");
            mac.update(query.as_bytes());
            url.push_str(&format!("&signature={}", hex::encode(mac.finalize().into_bytes())));
        }
        for attempt in 0..6u32 {
            let r = http::get(&self.agent, &url, &[("X-MBX-APIKEY", &self.key)], path)?;
            if r.status == 200 {
                return r.json();
            }
            if r.status == 429 || r.status == 418 {
                // rate limited / banned: back off
                let wait = r.retry_after.unwrap_or(0).max(1 << attempt).max(3);
                eprintln!("    (rate limited, waiting {wait}s...)");
                sleep(Duration::from_secs(wait));
                continue;
            }
            bail!("{}: {}", r.status, r.text());
        }
        bail!("{path}: still rate-limited after retries")
    }

    pub fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        self.req(path, params, true)
    }
}

/// The read-only GET surface a pull walks: the signed [`Client`], or a
/// scripted fake in the tests. `Sync` so the quick pull can fan out.
pub trait Api: Sync {
    fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value>;
    /// The politeness gap between consecutive calls of one walk.
    fn pause(&self, d: Duration) {
        sleep(d)
    }
}

impl Api for Client {
    fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        self.req(path, params, true)
    }
}

/// Backwards walk in `window_days` spans down to the floor. `start_ms`
/// floors it for endpoints that reject old windows outright.
fn windowed(api: &dyn Api, path: &str, base: &[(&str, String)], window_days: i64, key: Option<&str>, start_ms: i64) -> Result<Vec<Value>> {
    let floor = start_ms.max(HISTORY_START_MS);
    let (mut out, mut end, w) = (Vec::new(), now_ms(), window_days * DAY_MS);
    while end > floor {
        let start = (end - w).max(floor);
        let mut p = base.to_vec();
        p.push(("startTime", start.to_string()));
        p.push(("endTime", end.to_string()));
        let data = api.get(path, &p)?;
        let rows = match key {
            Some(k) if data.is_object() => data.get(k).cloned().unwrap_or(json!([])),
            _ => data,
        };
        if let Value::Array(rows) = rows {
            out.extend(rows);
        }
        end = start - 1;
        if end > floor {
            api.pause(Duration::from_millis(400));
        }
    }
    Ok(out)
}

fn list_of(v: &Value, key: &str) -> Vec<Value> {
    v.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// Everything one pull returned, endpoint by endpoint (what crypto_raw_sources
/// stores, and what build_events turns into ledger rows).
#[derive(Debug, Default, Clone)]
pub struct Pull {
    pub converts: Vec<Value>,
    pub auto: Vec<Value>,
    pub plans: Vec<Value>,
    pub trades: Vec<Value>,
    pub cash_trades: Vec<Value>,
    pub fiat_pay: Vec<Value>,
    pub fiat_sells: Vec<Value>,
    pub deposits: Vec<Value>,
    pub withdrawals: Vec<Value>,
    pub rewards: Vec<Value>,
    pub c2c: Vec<Value>,
    pub pay: Vec<Value>,
    pub fiat_dep: Vec<Value>,
    pub fiat_wd: Vec<Value>,
    pub forced: Vec<Value>,
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Withdrawal time: completeTime, else applyTime; epoch ms or a
/// "YYYY-MM-DD HH:MM:SS" string, read as UTC.
fn withdraw_ts(w: &Value) -> i64 {
    let v = [w.get("completeTime"), w.get("applyTime")].into_iter().flatten().find(|v| truthy(v));
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(t)) if t.chars().all(|c| c.is_ascii_digit()) => t.parse().unwrap_or(0),
        Some(Value::String(t)) => chrono::NaiveDateTime::parse_from_str(t.trim(), "%Y-%m-%d %H:%M:%S")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(t.trim(), "%Y-%m-%dT%H:%M:%S"))
            .map(|d| d.and_utc().timestamp_millis())
            .unwrap_or(0),
        _ => 0,
    }
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Turn one pull into ledger events (the event-building half of
/// BinanceSource.fetch, pure so it can be checked against recorded payloads).
pub fn build_events(p: &Pull) -> Vec<Event> {
    let mut ev = Vec::new();
    let e = |ev: &mut Vec<Event>, ts: i64, a: &str, d: f64, usd: Option<f64>, k: &str, det: &str| {
        ev.push(Event::new(ts, a, d, usd, k, det))
    };

    // Convert: BOTH legs -- buying our asset (convert-in) and selling it (convert-out)
    for c in &p.converts {
        let ms = num_i64(&c["createTime"]);
        let (fa, ta) = (s(c, "fromAsset"), s(c, "toAsset"));
        let (fam, tam) = (f(c.get("fromAmount")), f(c.get("toAmount")));
        if is_asset(ta) {
            e(&mut ev, ms, ta, tam, usd_of(fam, fa), "convert-in", &stored_str(c.get("fromAsset")));
        }
        if is_asset(fa) {
            e(&mut ev, ms, fa, -fam, usd_of(tam, ta), "convert-out", &stored_str(c.get("toAsset")));
        }
    }
    for a in p.auto.iter().filter(|a| is_asset(s(a, "targetAsset"))) {
        e(
            &mut ev,
            num_i64(&a["transactionDateTime"]),
            s(a, "targetAsset"),
            f(a.get("targetAssetAmount")),
            usd_of(f(a.get("sourceAssetAmount")), s(a, "sourceAsset")),
            "auto-invest",
            &stored_str(a.get("sourceAsset")),
        );
    }
    // spot: balance-correct -- both legs (when ours) + commission
    for t in &p.trades {
        let (ms, base, quote) = (num_i64(&t["time"]), s(t, "_base"), s(t, "_quote"));
        let (qty, qq) = (f(t.get("qty")), f(t.get("quoteQty")));
        let buy = t.get("isBuyer").is_some_and(truthy);
        e(&mut ev, ms, base, if buy { qty } else { -qty }, usd_of(qq, quote), if buy { "spot-buy" } else { "spot-sell" }, quote);
        if ours(quote) {
            e(&mut ev, ms, quote, if buy { -qq } else { qq }, None, "spot-quote", base);
        }
        let (comm, ca) = (f(t.get("commission")), s(t, "commissionAsset"));
        if ours(ca) && comm != 0.0 {
            e(&mut ev, ms, ca, -comm, None, "spot-fee", base);
        }
    }
    for r in p.rewards.iter().filter(|r| ours(s(r, "asset"))) {
        let info = r.get("enInfo").map(|v| stored_str(Some(v))).unwrap_or_default();
        e(&mut ev, num_i64(&r["divTime"]), s(r, "asset"), f(r.get("amount")), Some(0.0), "reward", &trunc(&info, 24));
    }
    for o in p.c2c.iter().filter(|o| ours(s(o, "asset")) && s(o, "orderStatus") == "COMPLETED") {
        let amt = f(o.get("amount"));
        let buy = s(o, "_tradeType") == "BUY";
        e(
            &mut ev,
            num_i64(&o["createTime"]),
            s(o, "asset"),
            if buy { amt } else { -amt },
            buy.then(|| f(o.get("totalPrice"))),
            if buy { "p2p-buy" } else { "p2p-sell" },
            &istr(o.get("fiat"), ""),
        );
    }
    for x in p.fiat_pay.iter().filter(|x| is_asset(s(x, "cryptoCurrency")) && s(x, "status") == "Completed") {
        e(
            &mut ev,
            num_i64(&x["createTime"]),
            s(x, "cryptoCurrency"),
            f(x.get("obtainAmount")),
            Some(f(x.get("sourceAmount"))),
            "fiat",
            &stored_str(x.get("fiatCurrency")),
        );
    }
    for x in p.pay.iter().filter(|x| ours(s(x, "currency"))) {
        // amount is signed: positive = received, negative = sent
        let amt = f(x.get("amount"));
        let det = x.get("orderType").map(|v| stored_str(Some(v))).unwrap_or_default();
        e(&mut ev, num_i64(&x["transactionTime"]), s(x, "currency"), amt, None, if amt > 0.0 { "pay-recv" } else { "pay-send" }, &det);
    }
    for d in p.deposits.iter().filter(|d| ours(s(d, "coin"))) {
        // cost basis for pre-owned coins is filled later by backfill_deposit_cost
        e(&mut ev, num_i64(&d["insertTime"]), s(d, "coin"), f(d.get("amount")), None, "deposit", &istr(d.get("network"), ""));
    }
    for w in p.withdrawals.iter().filter(|w| ours(s(w, "coin"))) {
        e(&mut ev, withdraw_ts(w), s(w, "coin"), -f(w.get("amount")), Some(0.0), "withdraw", &trunc(s(w, "address"), 12));
    }

    // ---- exchange cash ledger (EUR fiat balance + stablecoins) ----
    // 'cash-*' kinds (or asset-filtered), so the profit math -- BTC/ETH/BNB
    // only -- is untouched; these legs exist to reconstruct exchange cash.
    let ok_status = |x: &Value| matches!(s(x, "status"), "Successful" | "Completed" | "Finished");
    let fiat_amount = |x: &Value| {
        f([x.get("amount"), x.get("indicatedAmount")].into_iter().flatten().find(|v| truthy(v)))
    };
    let det16 = |x: &Value| trunc(&x.get("method").map(|v| stored_str(Some(v))).unwrap_or_default(), 16);
    for x in p.fiat_dep.iter().filter(|x| ok_status(x) && is_cash(s(x, "fiatCurrency"))) {
        e(&mut ev, num_i64(&x["createTime"]), s(x, "fiatCurrency"), fiat_amount(x), None, "cash-deposit", &det16(x));
    }
    for x in p.fiat_wd.iter().filter(|x| ok_status(x) && is_cash(s(x, "fiatCurrency"))) {
        e(&mut ev, num_i64(&x["createTime"]), s(x, "fiatCurrency"), -fiat_amount(x), None, "cash-withdraw", &det16(x));
    }
    // fiat BUYS debit the cash balance only when paid from it -- a card buy
    // never touches the exchange balance
    for x in &p.fiat_pay {
        if s(x, "status") != "Completed" || !is_cash(s(x, "fiatCurrency")) {
            continue;
        }
        let method = x.get("paymentMethod").map(|v| stored_str(Some(v))).unwrap_or_default();
        if method.to_lowercase().contains("balance") {
            e(&mut ev, num_i64(&x["createTime"]), s(x, "fiatCurrency"), -f(x.get("sourceAmount")), None, "cash-buy", &istr(x.get("cryptoCurrency"), ""));
        }
    }
    for x in p.fiat_sells.iter().filter(|x| s(x, "status") == "Completed") {
        let ts = num_i64(&x["createTime"]);
        if is_cash(s(x, "fiatCurrency")) {
            e(&mut ev, ts, s(x, "fiatCurrency"), f(x.get("obtainAmount")), None, "cash-sell", &istr(x.get("cryptoCurrency"), ""));
        }
        if is_asset(s(x, "cryptoCurrency")) {
            let got = f(x.get("obtainAmount"));
            e(&mut ev, ts, s(x, "cryptoCurrency"), -f(x.get("sourceAmount")), (got != 0.0).then_some(got), "fiat-sell", &istr(x.get("fiatCurrency"), ""));
        }
    }
    for c in &p.converts {
        let ms = num_i64(&c["createTime"]);
        let (fa, ta) = (s(c, "fromAsset"), s(c, "toAsset"));
        if is_cash(fa) {
            e(&mut ev, ms, fa, -f(c.get("fromAmount")), None, "cash-convert-out", &stored_str(c.get("toAsset")));
        }
        if is_cash(ta) {
            e(&mut ev, ms, ta, f(c.get("toAmount")), None, "cash-convert-in", &stored_str(c.get("fromAsset")));
        }
    }
    for a in &p.auto {
        let (sa, amt) = (s(a, "sourceAsset"), f(a.get("sourceAssetAmount")));
        if is_cash(sa) && amt != 0.0 {
            e(&mut ev, num_i64(&a["transactionDateTime"]), sa, -amt, None, "cash-auto-invest", &istr(a.get("targetAsset"), ""));
        }
    }
    for t in &p.cash_trades {
        let ms = num_i64(&t["time"]);
        let (qty, qq) = (f(t.get("qty")), f(t.get("quoteQty")));
        let buy = t.get("isBuyer").is_some_and(truthy);
        e(&mut ev, ms, s(t, "_base"), if buy { qty } else { -qty }, None, "cash-spot", s(t, "_quote"));
        e(&mut ev, ms, s(t, "_quote"), if buy { -qq } else { qq }, None, "cash-spot-quote", s(t, "_base"));
    }
    for fc in &p.forced {
        let ms = fc.get("time").filter(|v| truthy(v)).map(num_i64).unwrap_or(0);
        let (da, ta) = (s(fc, "deductedAsset"), s(fc, "targetAsset"));
        if ms != 0 && is_cash(da) {
            e(&mut ev, ms, da, -f(fc.get("deductedAmount")), None, "cash-forced-out", &istr(fc.get("targetAsset"), ""));
        }
        if ms != 0 && is_cash(ta) {
            e(&mut ev, ms, ta, f(fc.get("targetAmount")), None, "cash-forced-in", &istr(fc.get("deductedAsset"), ""));
        }
    }
    ev.sort_by_key(|e| e.ts); // stable: same-ts events keep fetch order
    ev
}

/// The reconcile anchors: one adjust event per asset whose STORED ledger
/// sum differs from live truth. Cash residuals above a cent, coin residuals
/// above 1e-8 (usd None: unknown provenance claims no profit or basis).
pub fn reconcile(stored: &BTreeMap<String, f64>, live: &BTreeMap<String, f64>, now: i64) -> Vec<Event> {
    let mut out = Vec::new();
    for (a, s) in stored {
        let diff = live.get(a).copied().unwrap_or(0.0) - s;
        if is_cash(a) && diff.abs() > 0.01 {
            out.push(Event::new(sunset_ms(a).unwrap_or(now).min(now), a, diff, None, "cash-adjust", "reconcile"));
        } else if is_asset(a) && diff.abs() > 1e-8 {
            out.push(Event::new(now, a, diff, None, "asset-adjust", "reconcile"));
        }
    }
    out
}

/// Run one endpoint; on failure record it and return []. fetch() refuses to
/// write when anything failed: the merge wipes the re-fetched window, so a
/// partial pull would silently destroy that endpoint's events.
fn attempt(name: &str, log: Log, failed: &mut Vec<String>, f: impl FnOnce() -> Result<Vec<Value>>) -> Vec<Value> {
    log(&format!("  Binance: fetching {name}..."));
    match f() {
        Ok(v) => v,
        Err(e) => {
            log(&format!("  Binance: !! {name} failed ({e})"));
            failed.push(name.to_string());
            vec![]
        }
    }
}

fn invalid_symbol(e: &anyhow::Error) -> bool {
    let m = e.to_string();
    m.contains("-1121") || m.contains("Invalid symbol")
}

/// One pair's latest trades (myTrades has no cursor worth using: a span is
/// capped at 24 h, so it is the newest 1000), tagged with the pair. A pair
/// Binance doesn't list is simply empty.
fn pair_trades(api: &dyn Api, sym: &str, base: &str, quote: &str) -> Result<Vec<Value>> {
    let trades = match api.get("/api/v3/myTrades", &[("symbol", sym.into()), ("limit", "1000".into())]) {
        Ok(t) => t,
        Err(e) if invalid_symbol(&e) => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut rows = trades.as_array().cloned().unwrap_or_default();
    for t in rows.iter_mut() {
        if let Some(o) = t.as_object_mut() {
            o.insert("_symbol".into(), json!(sym));
            o.insert("_base".into(), json!(base));
            o.insert("_quote".into(), json!(quote));
        }
    }
    Ok(rows)
}

fn my_trades(api: &dyn Api, pairs: &[(String, &str, &str)]) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for (sym, base, quote) in pairs {
        rows.extend(pair_trades(api, sym, base, quote)?);
        api.pause(Duration::from_millis(300));
    }
    Ok(rows)
}

/// Every DCA pair: our coins against each quote.
fn spot_pairs() -> Vec<(String, &'static str, &'static str)> {
    ["BTC", "ETH", "BNB"]
        .iter()
        .flat_map(|a| QUOTES.iter().filter(move |q| *q != a).map(move |q| (format!("{a}{q}"), *a, *q)))
        .collect()
}

/// cash-to-cash pairs (EUR -> USDT before a buy): neither leg is in the DCA pairs
fn cash_pairs() -> Vec<(String, &'static str, &'static str)> {
    vec![("EURUSDT".into(), "EUR", "USDT"), ("EURUSDC".into(), "EUR", "USDC")]
}

/// Auto-Invest executions down to `floor_ms`. Windowed + paginated: the
/// endpoint defaults to a short lookback, so explicit startTime/endTime are
/// needed for the full DCA history.
fn auto_history(api: &dyn Api, floor_ms: i64) -> Result<Vec<Value>> {
    let (mut rows, mut end, mut seen) = (Vec::new(), now_ms(), HashSet::new());
    while end > floor_ms {
        let start = (end - 30 * DAY_MS).max(floor_ms);
        let mut page = 1;
        loop {
            let data = api.get(
                "/sapi/v1/lending/auto-invest/history/list",
                &[("size", "100".into()), ("current", page.to_string()), ("startTime", start.to_string()), ("endTime", end.to_string())],
            )?;
            let batch = list_of(&data, "list");
            for b in &batch {
                let tid = match b.get("id").filter(|v| truthy(v)) {
                    Some(id) => id.to_string(),
                    None => format!("{}|{}|{}", b.get("transactionDateTime").unwrap_or(&Value::Null), s(b, "targetAsset"), b.get("targetAssetAmount").unwrap_or(&Value::Null)),
                };
                if seen.insert(tid) {
                    rows.push(b.clone());
                }
            }
            if batch.len() < 100 {
                break;
            }
            page += 1;
        }
        end = start - 1;
        if end > floor_ms {
            api.pause(Duration::from_millis(300));
        }
    }
    Ok(rows)
}

fn deposits(api: &dyn Api, floor_ms: i64) -> Result<Vec<Value>> {
    windowed(api, "/sapi/v1/capital/deposit/hisrec", &[], 90, None, floor_ms)
}

fn withdrawals(api: &dyn Api, floor_ms: i64) -> Result<Vec<Value>> {
    windowed(api, "/sapi/v1/capital/withdraw/history", &[], 90, None, floor_ms)
}

/// staking, Simple Earn interest, airdrops, Launchpool
fn rewards(api: &dyn Api, floor_ms: i64) -> Result<Vec<Value>> {
    windowed(api, "/sapi/v1/asset/assetDividend", &[("limit", "500".into())], 90, Some("rows"), floor_ms)
}

fn converts(api: &dyn Api, floor_ms: i64) -> Result<Vec<Value>> {
    windowed(api, "/sapi/v1/convert/tradeFlow", &[("limit", "1000".into())], 30, Some("list"), floor_ms)
}

/// Every endpoint, walked down to `floor_ms`.
fn pull(api: &dyn Api, floor_ms: i64, log: Log, failed: &mut Vec<String>) -> Pull {
    let mut p = Pull::default();
    p.converts = attempt("Convert history", log, failed, || converts(api, floor_ms));
    p.auto = attempt("Auto-Invest (recurring buy) full history", log, failed, || auto_history(api, floor_ms));
    p.plans = attempt("Auto-Invest plan configuration", log, failed, || {
        let mut out = Vec::new();
        for pt in ["SINGLE", "PORTFOLIO", "INDEX"] {
            if let Ok(data) = api.get("/sapi/v1/lending/auto-invest/plan/list", &[("planType", pt.into())]) {
                out.extend(list_of(&data, "plan"));
            }
        }
        Ok(out)
    });
    p.trades = attempt("Spot market trades", log, failed, || my_trades(api, &spot_pairs()));
    p.cash_trades = attempt("EUR/stable spot trades", log, failed, || my_trades(api, &cash_pairs()));
    // fiat endpoints: without beginTime/endTime they return only recent
    // activity and their weight is huge if windowed -- but they accept the
    // whole history as ONE span, so pass it and paginate within it
    let fiat_paged = |path: &str, tt: &str| -> Result<Vec<Value>> {
        let (mut rows, mut page, now) = (Vec::new(), 1, now_ms());
        loop {
            let data = api.get(
                path,
                &[("transactionType", tt.into()), ("rows", "500".into()), ("page", page.to_string()), ("beginTime", floor_ms.to_string()), ("endTime", now.to_string())],
            )?;
            let batch = list_of(&data, "data");
            let n = batch.len();
            rows.extend(batch);
            if n < 500 {
                return Ok(rows);
            }
            page += 1;
            api.pause(Duration::from_secs(1));
        }
    };
    p.fiat_pay = attempt("Fiat payments (card/bank buys)", log, failed, || fiat_paged("/sapi/v1/fiat/payments", "0"));
    p.fiat_sells = attempt("Fiat payments (sells)", log, failed, || fiat_paged("/sapi/v1/fiat/payments", "1"));
    p.deposits = attempt("Crypto deposits", log, failed, || deposits(api, floor_ms));
    p.withdrawals = attempt("Withdrawals", log, failed, || withdrawals(api, floor_ms));
    p.rewards = attempt("Rewards / distributions (assetDividend)", log, failed, || rewards(api, floor_ms));
    p.c2c = attempt("P2P (C2C) orders", log, failed, || {
        let mut rows = Vec::new();
        for tt in ["BUY", "SELL"] {
            let mut page = 1;
            loop {
                let data = api.get(
                    "/sapi/v1/c2c/orderMatch/listUserOrderHistory",
                    &[("tradeType", tt.into()), ("page", page.to_string()), ("rows", "100".into())],
                )?;
                let mut batch = list_of(&data, "data");
                for b in batch.iter_mut() {
                    if let Some(o) = b.as_object_mut() {
                        o.insert("_tradeType".into(), json!(tt));
                    }
                }
                let n = batch.len();
                // newest-first pages: once a page dips below the floor,
                // everything further back is already in the ledger
                let oldest = batch.iter().map(|b| b.get("createTime").filter(|v| truthy(v)).map(num_i64).unwrap_or(0)).min();
                rows.extend(batch);
                if n < 100 || oldest.is_some_and(|m| m < floor_ms) {
                    break;
                }
                page += 1;
            }
        }
        Ok(rows)
    });
    // Binance Pay only serves ~18 months; older windows fail with 403004
    // and used to void the whole Pay fetch. Floor at 17 months.
    let pay_floor = (now_ms() - 510 * DAY_MS).max(floor_ms);
    p.pay = attempt("Binance Pay transactions", log, failed, || {
        windowed(api, "/sapi/v1/pay/transactions", &[("limit", "100".into())], 30, Some("data"), pay_floor)
    });
    p.fiat_dep = attempt("Fiat deposit orders", log, failed, || fiat_paged("/sapi/v1/fiat/orders", "0"));
    p.fiat_wd = attempt("Fiat withdrawal orders", log, failed, || fiat_paged("/sapi/v1/fiat/orders", "1"));
    // forced conversions (the BUSD sunset) never show in convert/tradeFlow;
    // max span per call 365 days
    p.forced = attempt("Forced conversions (BUSD sunset)", log, failed, || {
        let (mut rows, mut end) = (Vec::new(), now_ms());
        while end > floor_ms {
            let start = (end - 360 * DAY_MS).max(floor_ms);
            let data = api.get(
                "/sapi/v1/asset/convert-transfer/queryByPage",
                &[("size", "100".into()), ("current", "1".into()), ("startTime", start.to_string()), ("endTime", end.to_string())],
            )?;
            rows.extend(list_of(&data, "rows"));
            end = start - 1;
            api.pause(Duration::from_millis(400));
        }
        Ok(rows)
    });
    p
}

// ---- quick pull (the live refresh) ------------------------------------------
//
// Ctrl+Shift+K wants today's DCA, trades and transfers in seconds, not the
// ~20 s sequential walk of every endpoint. A quick pull walks only the
// endpoints that move day to day, since the full-sync cursor, a few calls at
// a time. Everything it skips -- plan config, fiat payments and orders, P2P,
// Pay, the BUSD forced conversions, monthly prices -- waits for the next
// scheduled sync, which a quick pull can't hide from: it writes no
// crypto_raw_sources row (so the cursor, MAX(fetched_at), only moves on a
// complete pull), replaces only its own kinds' events, and leaves the
// reconcile anchors alone (a live balance against a ledger missing the
// skipped endpoints would book their activity as a bogus adjustment).

/// Every kind the quick endpoints build (convert, Auto-Invest, spot and
/// EUR/stable trades, deposits, withdrawals, rewards) and no other endpoint
/// does: the quick merge rebuilds exactly these.
pub const QUICK_KINDS: [&str; 15] = [
    "convert-in", "convert-out", "cash-convert-in", "cash-convert-out",
    "auto-invest", "cash-auto-invest",
    "spot-buy", "spot-sell", "spot-quote", "spot-fee", "cash-spot", "cash-spot-quote",
    "deposit", "withdraw", "reward",
];
/// Calls in flight at once. myTrades weighs 20 and the rest at most 10, so
/// four never come near the 6000/min IP budget; 429s still back off in req().
const QUICK_PARALLEL: usize = 4;
/// The quick merge rebuilds its kinds from the cursor minus this...
const QUICK_OVERLAP_MS: i64 = DAY_MS;
/// ...from rows fetched this much further back: a withdrawal is listed by
/// applyTime but booked at completeTime, so one applied just before the
/// rebuild window must still be in hand.
const QUICK_SLACK_MS: i64 = 7 * DAY_MS;

/// `jobs` on at most [`QUICK_PARALLEL`] threads; results in job order.
fn par<'a, T: Send>(jobs: Vec<Box<dyn FnOnce() -> T + Send + 'a>>) -> Vec<T> {
    let n = jobs.len();
    let queue = std::sync::Mutex::new(jobs.into_iter().enumerate());
    let done = std::sync::Mutex::new(Vec::with_capacity(n));
    std::thread::scope(|sc| {
        for _ in 0..QUICK_PARALLEL.min(n) {
            sc.spawn(|| loop {
                let Some((i, job)) = queue.lock().unwrap_or_else(|p| p.into_inner()).next() else { break };
                let r = job();
                done.lock().unwrap_or_else(|p| p.into_inner()).push((i, r));
            });
        }
    });
    let mut done = done.into_inner().unwrap_or_else(|p| p.into_inner());
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// The quick endpoints, walked down to `floor_ms` in parallel (each pair's
/// myTrades its own job). Same all-or-nothing contract as [`pull`].
fn pull_quick(api: &dyn Api, floor_ms: i64, failed: &mut Vec<String>) -> Pull {
    type Job<'a> = Box<dyn FnOnce() -> Result<Vec<Value>> + Send + 'a>;
    let mut names: Vec<&str> = vec!["Convert history", "Auto-Invest executions", "Crypto deposits", "Withdrawals", "Rewards / distributions"];
    let mut jobs: Vec<Job> = vec![
        Box::new(move || converts(api, floor_ms)),
        Box::new(move || auto_history(api, floor_ms)),
        Box::new(move || deposits(api, floor_ms)),
        Box::new(move || withdrawals(api, floor_ms)),
        Box::new(move || rewards(api, floor_ms)),
    ];
    for (name, pairs) in [("Spot market trades", spot_pairs()), ("EUR/stable spot trades", cash_pairs())] {
        for (sym, base, quote) in pairs {
            names.push(name);
            jobs.push(Box::new(move || pair_trades(api, &sym, base, quote)));
        }
    }
    let mut p = Pull::default();
    for (name, r) in names.into_iter().zip(par(jobs)) {
        let rows = match r {
            Ok(rows) => rows,
            Err(e) => {
                if !failed.iter().any(|f| f.starts_with(name)) {
                    failed.push(format!("{name} ({e})"));
                }
                continue;
            }
        };
        let slot = match name {
            "Convert history" => &mut p.converts,
            "Auto-Invest executions" => &mut p.auto,
            "Crypto deposits" => &mut p.deposits,
            "Withdrawals" => &mut p.withdrawals,
            "Rewards / distributions" => &mut p.rewards,
            "Spot market trades" => &mut p.trades,
            _ => &mut p.cash_trades,
        };
        slot.extend(rows);
    }
    p
}

/// The raw payloads crypto_raw_sources keeps, by source name (only the rows
/// that touch our assets/cash).
fn raw_payloads(p: &Pull) -> Vec<(&'static str, Vec<Value>)> {
    let keep = |rows: &[Value], f: &dyn Fn(&Value) -> bool| rows.iter().filter(|r| f(r)).cloned().collect::<Vec<_>>();
    vec![
        ("convert", keep(&p.converts, &|c| is_asset(s(c, "toAsset")))),
        ("auto_invest", keep(&p.auto, &|a| is_asset(s(a, "targetAsset")))),
        ("spot_trades", p.trades.clone()),
        ("fiat_buys", keep(&p.fiat_pay, &|x| is_asset(s(x, "cryptoCurrency")) && s(x, "status") == "Completed")),
        ("deposits", keep(&p.deposits, &|d| ours(s(d, "coin")))),
        ("withdrawals", keep(&p.withdrawals, &|w| ours(s(w, "coin")))),
        ("rewards", keep(&p.rewards, &|r| ours(s(r, "asset")))),
        ("c2c", keep(&p.c2c, &|o| ours(s(o, "asset")) && s(o, "orderStatus") == "COMPLETED")),
        ("pay", keep(&p.pay, &|x| ours(s(x, "currency")))),
        ("fiat_deposits", p.fiat_dep.clone()),
        ("fiat_withdrawals", p.fiat_wd.clone()),
        ("fiat_sells", p.fiat_sells.clone()),
        ("cash_trades", p.cash_trades.clone()),
        ("forced_conversions", p.forced.clone()),
        ("auto_invest_plans", p.plans.clone()),
    ]
}

/// History pull -> crypto_events, crypto_raw_sources, crypto_prices_monthly.
pub fn fetch(conn: &Connection, full: bool, log: Log) -> Result<()> {
    let say = log;
    let events = sync(conn, &Client::from_env()?, full, log)?;

    say("  Binance: fetching monthly price history...");
    // EUR closes (EURUSDT) value the cash ledger's EUR. Anchored on the STORED
    // ledger's first event: an incremental pull only reaches the floor.
    let first_ms: i64 = conn
        .query_row("SELECT MIN(ts) FROM crypto_events WHERE exchange='binance'", [], |r| r.get::<_, Option<i64>>(0))?
        .unwrap_or(HISTORY_START_MS);
    let monthly: BTreeMap<String, BTreeMap<String, f64>> =
        ["BTC", "ETH", "BNB", "EUR"].iter().map(|a| (a.to_string(), super::prices::monthly_closes(a, first_ms))).collect();
    db::upsert_prices(conn, &monthly)?;
    let n = super::prices::backfill_deposit_cost(conn)?;
    say(&format!("  Binance: merged {} event(s) from this pull (backfilled cost basis for {n} deposits)", events.len()));
    Ok(())
}

/// The live refresh's pull (see [`QUICK_KINDS`]): the day-to-day endpoints
/// since the last full sync, merged without touching its cursor, the other
/// endpoints' events or the reconcile anchors. Needs a prior full sync.
pub fn fetch_quick(conn: &Connection, log: Log) -> Result<()> {
    let n = sync_quick(conn, &Client::from_env()?, log)?;
    // the deposits it rebuilt lost their backfilled basis: one kline each
    let b = super::prices::backfill_deposit_cost(conn)?;
    log(&format!("  Binance: quick refresh merged {n} event(s) (backfilled cost basis for {b} deposits)"));
    Ok(())
}

fn sync_quick(conn: &Connection, api: &dyn Api, log: Log) -> Result<usize> {
    let Some(last) = db::last_fetch_ms(conn, Some("binance"))? else {
        bail!("never synced: a quick refresh needs one full sync first");
    };
    let since = (last - QUICK_OVERLAP_MS).max(HISTORY_START_MS);
    let day = chrono::DateTime::from_timestamp_millis(since).map(|d| d.date_naive().to_string()).unwrap_or_default();
    log(&format!("  Binance: quick refresh since {day} (convert, Auto-Invest, spot trades, deposits, withdrawals, rewards)"));
    let mut failed = Vec::new();
    let p = pull_quick(api, since - QUICK_SLACK_MS, &mut failed);
    if !failed.is_empty() {
        bail!("quick refresh incomplete, ledger left untouched -- failed: {}", failed.join(", "));
    }
    let events = build_events(&p);
    // rebuild only the quick kinds from `since`: every other kind stored
    // (the skipped endpoints', the reconcile anchors) is preserved as is
    let others: Vec<String> = conn
        .prepare(&format!(
            "SELECT DISTINCT kind FROM crypto_events WHERE exchange='binance' AND kind NOT IN ({})",
            vec!["?"; QUICK_KINDS.len()].join(",")
        ))?
        .query_map(rusqlite::params_from_iter(QUICK_KINDS), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let others: Vec<&str> = others.iter().map(String::as_str).collect();
    db::merge_events(conn, "binance", &events, Some(since), &[], &others)?;
    Ok(events.len())
}

/// Pull + merge + raw payloads + reconcile: the network-free-testable core
/// of [`fetch`] (the price history after it goes to the public API).
fn sync(conn: &Connection, api: &dyn Api, full: bool, log: Log) -> Result<Vec<Event>> {
    let say = log;

    // fetched_at is only written after a fully successful pull, so it is a
    // safe "everything before this is in the ledger" marker, minus the overlap
    let last = db::last_fetch_ms(conn, Some("binance"))?;
    let floor_ms = match last {
        Some(l) if !full => HISTORY_START_MS.max(l - OVERLAP_MS),
        _ => HISTORY_START_MS,
    };
    let full_walk = floor_ms <= HISTORY_START_MS;
    if !full_walk {
        let since = chrono::DateTime::from_timestamp_millis(floor_ms).map(|d| d.date_naive().to_string()).unwrap_or_default();
        say(&format!("  Binance: incremental sync since {since} (use --full to rebuild)"));
    }

    let mut failed = Vec::new();
    let p = pull(api, floor_ms, say, &mut failed);
    // all-or-nothing: see attempt()
    if !failed.is_empty() {
        bail!("fetch incomplete, ledger left untouched -- failed endpoints: {}", failed.join(", "));
    }
    let mut events = build_events(&p);

    // MERGE, don't wipe: only the span this fetch covers is rebuilt; prior
    // adjust rows survive until the reconcile below has live truth in hand
    db::merge_events(conn, "binance", &events, Some(if full_walk { 0 } else { floor_ms }), &[], &["cash-adjust", "asset-adjust"])?;
    for (name, payload) in raw_payloads(&p) {
        db::upsert_raw(conn, "binance", name, &Value::Array(payload).to_string())?;
    }

    // anchor: reconcile the STORED ledger to the live balance. The endpoints
    // can't see everything (card spends, dust sweeps, promos); history stays
    // best-effort, the present is exact.
    match live_balances_with(api) {
        Err(e) => say(&format!("  Binance: warning: reconcile skipped, prior adjustments kept ({e})")),
        Ok(live) => {
            conn.execute("DELETE FROM crypto_events WHERE exchange='binance' AND kind IN ('cash-adjust','asset-adjust')", [])?;
            let held: BTreeSet<String> = conn
                .prepare("SELECT DISTINCT asset FROM crypto_events WHERE exchange='binance'")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut stored = BTreeMap::new();
            for a in CASH.iter().map(|s| s.to_string()).chain(held) {
                let sum = db::ledger_sum(conn, "binance", &a)?;
                stored.insert(a, sum);
            }
            let adj = reconcile(&stored, &live, now_ms());
            if !adj.is_empty() {
                let list: Vec<String> = adj.iter().map(|a| format!("{} {:+.8}", a.asset, a.delta)).collect();
                say(&format!("  Binance: reconcile adjustments: {}", list.join(", ")));
                db::merge_events(conn, "binance", &adj, None, &[], &[])?;
                events.extend(adj);
            }
        }
    }
    Ok(events)
}

/// Sum spot (free + locked, minus LD* legacy Savings wrappers -- the Earn
/// sweep counts the underlying) and Simple-Earn flexible rows.
pub fn parse_balances(account: &Value, earn_pages: &[Value]) -> BTreeMap<String, f64> {
    let mut held: BTreeMap<String, f64> = BTreeMap::new();
    for b in account.get("balances").and_then(Value::as_array).into_iter().flatten() {
        let asset = s(b, "asset");
        let amt = f(b.get("free")) + f(b.get("locked"));
        if amt > 0.0 && !(asset.len() > 3 && asset.starts_with("LD")) {
            *held.entry(asset.to_string()).or_default() += amt;
        }
    }
    for page in earn_pages {
        for r in list_of(page, "rows") {
            if !s(&r, "asset").is_empty() {
                *held.entry(s(&r, "asset").to_string()).or_default() += f(r.get("totalAmount"));
            }
        }
    }
    held.retain(|_, v| *v > 0.0);
    held
}

fn live_balances_with(api: &dyn Api) -> Result<BTreeMap<String, f64>> {
    let account = api.get("/api/v3/account", &[])?;
    // strict on purpose: a swallowed Earn failure would be a partial balance
    // treated as exact truth (under-reported net worth, phantom adjustments)
    let (mut pages, mut page) = (Vec::new(), 1);
    loop {
        let data = api.get("/sapi/v1/simple-earn/flexible/position", &[("size", "100".into()), ("current", page.to_string())])?;
        let n = list_of(&data, "rows").len();
        pages.push(data);
        if n < 100 {
            break;
        }
        page += 1;
    }
    Ok(parse_balances(&account, &pages))
}

/// EVERY positive spot + Simple-Earn flexible balance, crypto AND exchange
/// cash. The caller prices them and drops dust / unpriceable assets.
pub fn live_balances() -> Result<BTreeMap<String, f64>> {
    live_balances_with(&Client::from_env()?).map_err(|e| anyhow!("binance: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_pull_builds_every_leg() {
        let p = Pull {
            converts: vec![json!({"createTime": 1000, "fromAsset": "USDT", "toAsset": "BTC", "fromAmount": "100", "toAmount": "0.002"})],
            auto: vec![json!({"transactionDateTime": 2000, "targetAsset": "ETH", "targetAssetAmount": "0.01", "sourceAsset": "EUR", "sourceAssetAmount": "30"})],
            trades: vec![json!({"time": 3000, "_base": "BNB", "_quote": "USDT", "qty": "1", "quoteQty": "500", "isBuyer": true, "commission": "0.001", "commissionAsset": "BNB"})],
            withdrawals: vec![json!({"coin": "BTC", "amount": "0.5", "completeTime": "2024-01-02 03:04:05", "address": "bc1qabcdefghijklmn"})],
            fiat_dep: vec![json!({"createTime": 500, "status": "Successful", "fiatCurrency": "EUR", "amount": "0.00", "indicatedAmount": "200", "method": "BANK_TRANSFER_SEPA_EXTRA"})],
            forced: vec![json!({"time": 4000, "deductedAsset": "BUSD", "targetAsset": "USDC", "deductedAmount": "10", "targetAmount": "10"})],
            ..Default::default()
        };
        let ev = build_events(&p);
        let find = |k: &str| ev.iter().find(|e| e.kind == k).unwrap_or_else(|| panic!("no {k}"));
        assert_eq!(find("convert-in").usd, Some(100.0));
        assert_eq!(find("cash-convert-out").delta, -100.0);
        assert_eq!(find("auto-invest").usd, None, "EUR is not a stable");
        assert_eq!(find("cash-auto-invest").delta, -30.0);
        assert_eq!(find("spot-buy").usd, Some(500.0));
        assert_eq!(find("spot-quote").delta, -500.0);
        assert_eq!(find("spot-fee").delta, -0.001);
        let wd = find("withdraw");
        assert_eq!((wd.ts, wd.detail.as_str()), (1704164645000, "bc1qabcdefgh"));
        // `amount`, else `indicatedAmount` only when amount is missing/empty: the string "0.00" counts as present
        assert_eq!(find("cash-deposit").delta, 0.0);
        assert_eq!(find("cash-deposit").detail, "BANK_TRANSFER_SE", "method cut to 16 chars");
        assert_eq!(find("cash-forced-in").asset, "USDC");
        assert!(ev.windows(2).all(|w| w[0].ts <= w[1].ts));
    }

    #[test]
    fn reconcile_thresholds_and_sunset() {
        let stored: BTreeMap<String, f64> =
            [("EUR".into(), 10.0), ("BUSD".into(), 5.0), ("BTC".into(), 1.0), ("ETH".into(), 2.0), ("SOL".into(), 3.0)].into();
        let live: BTreeMap<String, f64> = [("EUR".into(), 10.005), ("BTC".into(), 1.5), ("ETH".into(), 2.0)].into();
        let adj = reconcile(&stored, &live, 2_000_000_000_000);
        assert_eq!(adj.len(), 2, "{adj:?}");
        assert_eq!((adj[0].asset.as_str(), adj[0].kind.as_str(), adj[0].ts), ("BTC", "asset-adjust", 2_000_000_000_000));
        assert_eq!((adj[1].asset.as_str(), adj[1].delta, adj[1].ts), ("BUSD", -5.0, 1_709_251_200_000));
    }

    #[test]
    fn balances_skip_ld_wrappers() {
        let acct = json!({"balances": [{"asset": "BTC", "free": "0.1", "locked": "0.05"}, {"asset": "LDBTC", "free": "1", "locked": "0"}, {"asset": "EUR", "free": "0", "locked": "0"}]});
        let earn = json!({"rows": [{"asset": "BTC", "totalAmount": "0.2"}, {"asset": "USDC", "totalAmount": "50"}]});
        let b = parse_balances(&acct, &[earn]);
        assert!((b["BTC"] - 0.35).abs() < 1e-12);
        assert_eq!(b["USDC"], 50.0);
        assert!(!b.contains_key("LDBTC") && !b.contains_key("EUR"));
    }

    /// The recorded payloads in the parity snapshot rebuild exactly the rows
    /// stored from them.
    #[test]
    fn recorded_payloads_match_parity_ledger() {
        let Some(conn) = crate::fetch::parity_db() else { return };
        let raw = |src: &str| -> Vec<Value> {
            conn.query_row("SELECT payload FROM crypto_raw_sources WHERE exchange='binance' AND source=?", [src], |r| r.get::<_, String>(0))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default()
        };
        let p = Pull { trades: raw("spot_trades"), rewards: raw("rewards"), deposits: raw("deposits"), withdrawals: raw("withdrawals"), c2c: raw("c2c"), pay: raw("pay"), ..Default::default() };
        let ev = build_events(&p);
        assert!(!ev.is_empty());
        let mut missing = 0;
        for e in &ev {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM crypto_events WHERE exchange='binance' AND ts=? AND asset=? AND delta=? AND kind=? AND detail=? AND usd IS ?",
                    rusqlite::params![e.ts, e.asset, e.delta, e.kind, e.detail, e.usd],
                    |r| r.get(0),
                )
                .unwrap();
            if n == 0 {
                missing += 1;
                eprintln!("not in parity ledger: {e:?}");
            }
        }
        assert_eq!(missing, 0, "{missing} of {} rebuilt events missing", ev.len());
    }

    // ---- quick pull vs full sync, on a scripted exchange --------------------

    use std::sync::Mutex;

    /// path, the row field its startTime/endTime (or beginTime) filter on,
    /// and the key the rows are wrapped in.
    const SPEC: [(&str, &str, Option<&str>); 14] = [
        ("/sapi/v1/convert/tradeFlow", "createTime", Some("list")),
        ("/sapi/v1/lending/auto-invest/history/list", "transactionDateTime", Some("list")),
        ("/sapi/v1/lending/auto-invest/plan/list", "", Some("plan")),
        ("/api/v3/myTrades", "", None),
        ("/sapi/v1/fiat/payments", "createTime", Some("data")),
        ("/sapi/v1/fiat/orders", "createTime", Some("data")),
        ("/sapi/v1/capital/deposit/hisrec", "insertTime", None),
        ("/sapi/v1/capital/withdraw/history", "applyTime", None),
        ("/sapi/v1/asset/assetDividend", "divTime", Some("rows")),
        ("/sapi/v1/c2c/orderMatch/listUserOrderHistory", "", Some("data")),
        ("/sapi/v1/pay/transactions", "transactionTime", Some("data")),
        ("/sapi/v1/asset/convert-transfer/queryByPage", "time", Some("rows")),
        ("/sapi/v1/simple-earn/flexible/position", "", Some("rows")),
        ("/api/v3/account", "", Some("balances")),
    ];
    const QUICK_PATHS: [&str; 6] = [
        "/sapi/v1/convert/tradeFlow",
        "/sapi/v1/lending/auto-invest/history/list",
        "/api/v3/myTrades",
        "/sapi/v1/capital/deposit/hisrec",
        "/sapi/v1/capital/withdraw/history",
        "/sapi/v1/asset/assetDividend",
    ];
    const LISTED: [&str; 3] = ["BTCUSDT", "ETHEUR", "EURUSDT"];

    /// A Binance that serves `rows` per path (a row's "_q" must match the
    /// symbol / transactionType / tradeType asked for), honours the time
    /// windows, and records every call. Never sleeps.
    #[derive(Default)]
    struct Fake {
        rows: Mutex<Vec<(&'static str, Value)>>,
        calls: Mutex<Vec<String>>,
        fail: Option<&'static str>,
    }

    impl Fake {
        fn add(&self, path: &'static str, row: Value) {
            assert!(SPEC.iter().any(|s| s.0 == path), "{path}");
            self.rows.lock().unwrap().push((path, row));
        }
        fn called(&self) -> BTreeSet<String> {
            self.calls.lock().unwrap().iter().cloned().collect()
        }
    }

    impl Api for Fake {
        fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
            self.calls.lock().unwrap().push(path.to_string());
            if self.fail == Some(path) {
                bail!("503: maintenance");
            }
            let p = |k: &str| params.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
            let (_, field, wrap) = SPEC.iter().find(|s| s.0 == path).ok_or_else(|| anyhow!("unscripted {path}"))?;
            let q = p("symbol").or(p("transactionType")).or(p("tradeType"));
            if path == "/api/v3/myTrades" && !LISTED.contains(&q.as_deref().unwrap_or("")) {
                bail!("400: {{\"code\":-1121,\"msg\":\"Invalid symbol.\"}}");
            }
            let first_page = p("current").or(p("page")).is_none_or(|n| n == "1");
            let span = p("startTime").or(p("beginTime")).zip(p("endTime")).map(|(a, b)| (a.parse::<i64>().unwrap(), b.parse::<i64>().unwrap()));
            let rows: Vec<Value> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(at, _)| *at == path && first_page)
                .filter(|(_, r)| q.is_none() || r.get("_q").and_then(Value::as_str) == q.as_deref())
                .filter(|(_, r)| field.is_empty() || span.is_none_or(|(a, b)| (a..=b).contains(&num_i64(&r[*field]))))
                .map(|(_, r)| r.clone())
                .collect();
            Ok(match wrap {
                Some(k) => json!({ *k: rows }),
                None => Value::Array(rows),
            })
        }
        fn pause(&self, _: Duration) {}
    }

    const H: i64 = 3_600_000;

    /// History before the first sync, on every endpoint; `t0` is "now".
    fn history(x: &Fake, t0: i64) {
        x.add("/sapi/v1/convert/tradeFlow", json!({"createTime": t0 - 400 * DAY_MS, "fromAsset": "USDT", "toAsset": "BTC", "fromAmount": "100", "toAmount": "0.004"}));
        x.add("/sapi/v1/lending/auto-invest/history/list", json!({"id": 1, "transactionDateTime": t0 - 200 * DAY_MS, "targetAsset": "ETH", "targetAssetAmount": "0.05", "sourceAsset": "EUR", "sourceAssetAmount": "100"}));
        x.add("/sapi/v1/lending/auto-invest/history/list", json!({"id": 2, "transactionDateTime": t0 - 20 * H, "targetAsset": "BTC", "targetAssetAmount": "0.001", "sourceAsset": "EUR", "sourceAssetAmount": "60"}));
        x.add("/sapi/v1/lending/auto-invest/plan/list", json!({"_q": "SINGLE", "planId": 7}));
        x.add("/api/v3/myTrades", json!({"_q": "BTCUSDT", "time": t0 - 10 * DAY_MS, "qty": "0.01", "quoteQty": "600", "isBuyer": true, "commission": "0", "commissionAsset": "BNB"}));
        x.add("/api/v3/myTrades", json!({"_q": "EURUSDT", "time": t0 - 9 * DAY_MS, "qty": "100", "quoteQty": "108", "isBuyer": false}));
        x.add("/sapi/v1/fiat/payments", json!({"_q": "0", "createTime": t0 - 300 * DAY_MS, "status": "Completed", "cryptoCurrency": "BTC", "fiatCurrency": "EUR", "obtainAmount": "0.01", "sourceAmount": "400", "paymentMethod": "Card"}));
        x.add("/sapi/v1/fiat/orders", json!({"_q": "0", "createTime": t0 - 500 * DAY_MS, "status": "Successful", "fiatCurrency": "EUR", "amount": "1000", "method": "SEPA"}));
        x.add("/sapi/v1/capital/deposit/hisrec", json!({"insertTime": t0 - 100 * DAY_MS, "coin": "ETH", "amount": "0.5", "network": "ETH"}));
        // applied before the quick fetch floor would be without the slack,
        // completed inside the quick rebuild window
        x.add("/sapi/v1/capital/withdraw/history", json!({"applyTime": t0 - 3 * DAY_MS, "completeTime": t0 - 12 * H, "coin": "BTC", "amount": "0.002", "address": "bc1qwithdrawaladdr"}));
        x.add("/sapi/v1/asset/assetDividend", json!({"divTime": t0 - 5 * DAY_MS, "asset": "BNB", "amount": "0.001", "enInfo": "Simple Earn"}));
        // a skipped endpoint's row inside the quick rebuild window: must survive
        x.add("/sapi/v1/c2c/orderMatch/listUserOrderHistory", json!({"_q": "BUY", "createTime": t0 - 6 * H, "asset": "USDT", "amount": "200", "totalPrice": "1500", "fiat": "HKD", "orderStatus": "COMPLETED"}));
        x.add("/sapi/v1/pay/transactions", json!({"transactionTime": t0 - 60 * DAY_MS, "currency": "USDT", "amount": "-5", "orderType": "C2C"}));
        x.add("/sapi/v1/asset/convert-transfer/queryByPage", json!({"time": 1_709_251_200_000i64 + DAY_MS, "deductedAsset": "BUSD", "targetAsset": "USDC", "deductedAmount": "3", "targetAmount": "3"}));
        x.add("/api/v3/account", json!({"asset": "BTC", "free": "0.0125", "locked": "0"}));
        x.add("/api/v3/account", json!({"asset": "EUR", "free": "740", "locked": "0"}));
        x.add("/sapi/v1/simple-earn/flexible/position", json!({"asset": "USDC", "totalAmount": "3"}));
    }

    /// What happened after the first sync: some on quick endpoints, some on
    /// skipped ones.
    fn activity(x: &Fake, t: i64) {
        x.add("/sapi/v1/lending/auto-invest/history/list", json!({"id": 3, "transactionDateTime": t, "targetAsset": "BTC", "targetAssetAmount": "0.0009", "sourceAsset": "EUR", "sourceAssetAmount": "60"}));
        x.add("/api/v3/myTrades", json!({"_q": "ETHEUR", "time": t, "qty": "0.02", "quoteQty": "50", "isBuyer": true, "commission": "0.00002", "commissionAsset": "ETH"}));
        x.add("/sapi/v1/capital/deposit/hisrec", json!({"insertTime": t, "coin": "BNB", "amount": "1", "network": "BSC"}));
        x.add("/sapi/v1/asset/assetDividend", json!({"divTime": t, "asset": "USDC", "amount": "0.01", "enInfo": "Simple Earn"}));
        x.add("/sapi/v1/c2c/orderMatch/listUserOrderHistory", json!({"_q": "SELL", "createTime": t, "asset": "USDT", "amount": "50", "fiat": "HKD", "orderStatus": "COMPLETED"}));
        x.add("/sapi/v1/fiat/orders", json!({"_q": "0", "createTime": t, "status": "Successful", "fiatCurrency": "EUR", "amount": "300", "method": "SEPA"}));
        x.add("/sapi/v1/fiat/payments", json!({"_q": "0", "createTime": t, "status": "Completed", "cryptoCurrency": "ETH", "fiatCurrency": "EUR", "obtainAmount": "0.1", "sourceAmount": "250", "paymentMethod": "Card"}));
    }

    fn quiet(_: &str) {}

    /// The ledger as comparable rows (an anchor's ts is "when reconciled").
    fn ledger(conn: &Connection) -> Vec<String> {
        let mut rows: Vec<String> = conn
            .prepare("SELECT ts, asset, delta, usd, kind, detail FROM crypto_events WHERE exchange='binance'")
            .unwrap()
            .query_map([], |r| {
                let kind: String = r.get(4)?;
                let ts: i64 = if kind.ends_with("-adjust") { 0 } else { r.get(0)? };
                Ok(format!("{ts} {} {:.8} {:?} {kind} {}", r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, Option<f64>>(3)?, r.get::<_, String>(5)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        rows
    }

    fn raw(conn: &Connection) -> Vec<(String, String, i64)> {
        conn.prepare("SELECT source, payload, fetched_at FROM crypto_raw_sources WHERE exchange='binance' ORDER BY source")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn kinds(rows: &[String]) -> BTreeSet<String> {
        rows.iter().map(|r| r.split(' ').nth(4).unwrap().to_string()).collect()
    }

    /// Quick builds only QUICK_KINDS, and no skipped endpoint builds any:
    /// the kind-scoped rebuild can't touch what it didn't fetch.
    #[test]
    fn quick_kinds_partition_the_endpoints() {
        let (x, t0) = (Fake::default(), now_ms());
        history(&x, t0);
        activity(&x, t0);
        let mut failed = vec![];
        let all = pull(&x, HISTORY_START_MS, &quiet, &mut failed);
        assert!(failed.is_empty(), "{failed:?}");
        let quick = Pull { converts: all.converts.clone(), auto: all.auto.clone(), trades: all.trades.clone(), cash_trades: all.cash_trades.clone(), deposits: all.deposits.clone(), withdrawals: all.withdrawals.clone(), rewards: all.rewards.clone(), ..Default::default() };
        let skipped = Pull { plans: all.plans.clone(), fiat_pay: all.fiat_pay.clone(), fiat_sells: all.fiat_sells.clone(), c2c: all.c2c.clone(), pay: all.pay.clone(), fiat_dep: all.fiat_dep.clone(), fiat_wd: all.fiat_wd.clone(), forced: all.forced.clone(), ..Default::default() };
        let q = build_events(&quick);
        assert!(q.len() > 8);
        assert!(q.iter().all(|e| QUICK_KINDS.contains(&e.kind.as_str())), "{q:?}");
        let s = build_events(&skipped);
        assert!(s.len() > 4);
        assert!(s.iter().all(|e| !QUICK_KINDS.contains(&e.kind.as_str())), "{s:?}");
    }

    /// Quick walks only its endpoints, leaves the cursor, the raw payloads,
    /// the anchors and every skipped endpoint's events alone, and a full
    /// sync after it lands exactly where a full sync alone does.
    #[test]
    fn quick_then_full_matches_full_only() {
        let (a, b) = (db::memory().unwrap(), db::memory().unwrap());
        let x = Fake::default();
        let t0 = now_ms();
        history(&x, t0);
        sync(&a, &x, false, &quiet).unwrap();
        sync(&b, &x, false, &quiet).unwrap();
        assert_eq!(ledger(&a), ledger(&b));
        let first = ledger(&a);
        assert!(kinds(&first).contains("cash-adjust") || kinds(&first).contains("asset-adjust"), "anchors in play");
        let raw_before = raw(&a);

        let t = now_ms().max(raw_before.iter().map(|r| r.2).max().unwrap()) + 1;
        while now_ms() < t {
            std::thread::yield_now();
        }
        activity(&x, t);
        x.calls.lock().unwrap().clear();
        let n = sync_quick(&a, &x, &quiet).unwrap();
        assert!(n > 0);

        // only the quick endpoints, and each pair's trades
        let called = x.called();
        assert!(called.iter().all(|c| QUICK_PATHS.contains(&c.as_str())), "{called:?}");
        assert_eq!(called.len(), QUICK_PATHS.len(), "{called:?}");
        let pairs = x.calls.lock().unwrap().iter().filter(|c| *c == "/api/v3/myTrades").count();
        assert_eq!(pairs, spot_pairs().len() + cash_pairs().len());
        // the cursor and the payloads didn't move
        assert_eq!(raw(&a), raw_before);

        let after = ledger(&a);
        let quick_kind = |r: &&String| QUICK_KINDS.contains(&r.split(' ').nth(4).unwrap());
        // every non-quick row (anchors, P2P, fiat, Pay, forced) exactly as before
        let keep = |rows: &[String]| rows.iter().filter(|r| !quick_kind(r)).cloned().collect::<Vec<_>>();
        assert_eq!(keep(&after), keep(&first));
        // quick rows: the old ones still there once (the late withdrawal too), plus the new
        let old_quick: Vec<&String> = first.iter().filter(quick_kind).collect();
        for r in &old_quick {
            assert_eq!(after.iter().filter(|x| x == r).count(), 1, "{r}");
        }
        let new: Vec<&String> = after.iter().filter(|r| !first.contains(r)).collect();
        assert_eq!(kinds(&new.iter().map(|r| r.to_string()).collect::<Vec<_>>()), ["auto-invest", "cash-auto-invest", "deposit", "reward", "spot-buy", "spot-fee", "spot-quote"].map(String::from).into());
        assert!(after.iter().all(|r| !r.contains(" p2p-sell ")), "P2P waits for the full sync");

        // the next full sync catches up on everything quick skipped
        sync(&a, &x, false, &quiet).unwrap();
        sync(&b, &x, false, &quiet).unwrap();
        let (fa, fb) = (ledger(&a), ledger(&b));
        assert_eq!(fa, fb);
        assert!(fa.iter().any(|r| r.contains(" p2p-sell ")) && fa.iter().any(|r| r.contains(&format!("{t} EUR")) && r.contains(" cash-deposit ")));
    }

    #[test]
    fn quick_is_all_or_nothing() {
        let conn = db::memory().unwrap();
        let x = Fake { fail: Some("/sapi/v1/capital/deposit/hisrec"), ..Default::default() };
        assert!(sync_quick(&conn, &x, &quiet).unwrap_err().to_string().contains("full sync first"), "never synced: refuses");
        let ok = Fake::default();
        history(&ok, now_ms());
        sync(&conn, &ok, false, &quiet).unwrap();
        let before = ledger(&conn);
        *x.rows.lock().unwrap() = ok.rows.lock().unwrap().clone();
        activity(&x, now_ms());
        let e = sync_quick(&conn, &x, &quiet).unwrap_err().to_string();
        assert!(e.contains("Crypto deposits"), "{e}");
        assert_eq!(ledger(&conn), before);
    }
}
