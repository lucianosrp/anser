//! OKX (EEA) read-only client + ledger fetch, same event format as Binance.
//! Includes the Binance->OKX transfer
//! netting (an OKX deposit matching a recent Binance withdrawal becomes
//! 'transfer-in', not an unknown-cost-basis inflow).
//!
//! OKX's endpoints only look back ~7 days (bills) / ~3 months (archive), so
//! the ledger is MERGED: the last 80 days are wiped and rebuilt, everything
//! older survives. Signed GETs only; the key is read-only.

use std::collections::{BTreeMap, HashSet};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use rusqlite::Connection;
use serde_json::Value;
use sha2::Sha256;

use super::http::{self, now_ms};
use crate::db::{self, Event};
use super::{f, num_i64, Log};
use crate::config;

pub const BASE: &str = "https://eea.okx.com"; // regional endpoint (EEA account)
pub const ASSETS: [&str; 3] = ["BTC", "ETH", "BNB"];
/// EUR counts ~1:1 as USD here (approximate).
const STABLE: [&str; 7] = ["USDT", "USDC", "USD", "EUR", "DAI", "TUSD", "FDUSD"];
/// Exchange cash tracked with 'cash-*' kinds, mirroring binance::CASH.
pub const CASH: [&str; 7] = ["EUR", "USDT", "USDC", "DAI", "TUSD", "FDUSD", "BUSD"];
const DAY_MS: i64 = 86_400_000;

fn is_asset(a: &str) -> bool {
    ASSETS.contains(&a)
}
fn is_cash(a: &str) -> bool {
    CASH.contains(&a)
}
fn usd(amount: f64, ccy: &str) -> Option<f64> {
    STABLE.contains(&ccy).then_some(amount)
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

type HmacSha256 = Hmac<Sha256>;

pub struct Client {
    key: String,
    secret: String,
    passphrase: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        let (Some(key), Some(secret), Some(passphrase)) =
            (config::env("OKX_API_KEY"), config::env("OKX_API_SECRET"), config::env("OKX_PASSPHRASE"))
        else {
            bail!("Missing OKX_API_KEY / OKX_API_SECRET / OKX_PASSPHRASE {}", crate::secrets::where_to_set());
        };
        Ok(Self::new(&key, &secret, &passphrase))
    }

    /// A client on explicit keys (the source test, before they are saved).
    pub fn new(key: &str, secret: &str, passphrase: &str) -> Self {
        Client { key: key.into(), secret: secret.into(), passphrase: passphrase.into(), agent: http::agent(Duration::from_secs(30)) }
    }

    /// Signed GET; returns the `data` array. `params` with an empty value are
    /// dropped.
    pub fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Vec<Value>> {
        let q: Vec<String> = params.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")).collect();
        let req = if q.is_empty() { path.to_string() } else { format!("{path}?{}", q.join("&")) };
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let sign = sign(&self.secret, &ts, &req);
        let headers = [
            ("OK-ACCESS-KEY", self.key.as_str()),
            ("OK-ACCESS-SIGN", sign.as_str()),
            ("OK-ACCESS-TIMESTAMP", ts.as_str()),
            ("OK-ACCESS-PASSPHRASE", self.passphrase.as_str()),
            ("Content-Type", "application/json"),
        ];
        for attempt in 0..5u32 {
            let r = http::get(&self.agent, &format!("{BASE}{req}"), &headers, path)?;
            if r.status == 429 {
                sleep(Duration::from_secs(1 << attempt));
                continue;
            }
            let d = r.json().map_err(|e| anyhow!("{path}: HTTP {} {e}", r.status))?;
            let code = match d.get("code") {
                Some(Value::String(c)) => c.clone(),
                Some(other) => other.to_string(),
                None => "None".into(),
            };
            if code != "0" {
                bail!("{path}: code={code} {}", d.get("msg").and_then(Value::as_str).unwrap_or(""));
            }
            return Ok(d.get("data").and_then(Value::as_array).cloned().unwrap_or_default());
        }
        bail!("{path}: rate-limited")
    }

    /// Up to ten pages of 100 (small accounts), cursoring on `id_field`.
    pub fn paged(&self, path: &str, id_field: &str) -> Result<Vec<Value>> {
        let (mut out, mut after) = (Vec::new(), String::new());
        for _ in 0..10 {
            let data = self.get(path, &[("after", after.clone()), ("limit", "100".into())])?;
            let n = data.len();
            if let Some(last) = data.last() {
                after = [last.get(id_field), last.get("ts")]
                    .into_iter()
                    .flatten()
                    .find(|v| super::truthy(v))
                    .map(|v| super::stored_str(Some(v)))
                    .unwrap_or_default();
            }
            out.extend(data);
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }
}

/// OK-ACCESS-SIGN: base64(HMAC-SHA256(secret, ts + "GET" + path?query)).
pub fn sign(secret: &str, ts: &str, req: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac takes any key");
    mac.update(format!("{ts}GET{req}").as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// The endpoint payloads one fetch reads.
#[derive(Debug, Default, Clone)]
pub struct Pull {
    pub bills: Vec<Value>,
    pub convert: Vec<Value>,
    pub deposits: Vec<Value>,
    pub withdrawals: Vec<Value>,
}

/// Events from one pull (before the transfer netting).
pub fn build_events(p: &Pull) -> Vec<Event> {
    let mut ev = Vec::new();
    // a priced leg on an X-Y instrument in one of our coins = a real trade
    // (recurring buys are bill type 30, regular trades type 2)
    for b in &p.bills {
        let (ccy, inst) = (s(b, "ccy"), s(b, "instId"));
        let px = f(b.get("px"));
        let bal = f(b.get("balChg"));
        if is_asset(ccy) && px > 0.0 && inst.contains('-') && bal != 0.0 {
            let quote = inst.split_once('-').map(|x| x.1).unwrap_or("");
            let u = STABLE.contains(&quote).then(|| bal.abs() * px);
            ev.push(Event::new(num_i64(&b["ts"]), ccy, bal, u, if bal > 0.0 { "spot-buy" } else { "spot-sell" }, quote));
        }
    }
    for c in &p.convert {
        let (base, quote) = (s(c, "baseCcy"), s(c, "quoteCcy"));
        let (bsz, qsz) = (f(c.get("fillBaseSz")), f(c.get("fillQuoteSz")));
        let ts = num_i64(c.get("ts").unwrap_or(&Value::Null));
        let (fa, fam, ta, tam) = if s(c, "side") == "buy" { (quote, qsz, base, bsz) } else { (base, bsz, quote, qsz) };
        if is_asset(ta) {
            ev.push(Event::new(ts, ta, tam, usd(fam, fa), "convert-in", fa));
        }
        if is_asset(fa) {
            ev.push(Event::new(ts, fa, -fam, usd(tam, ta), "convert-out", ta));
        }
        // cash legs ('cash-*'; profit math never sees them). Trade-bill cash
        // legs are deliberately NOT recorded: a convert can also surface as a
        // priced bill, and double counting is worse than missing tiny legs.
        if is_cash(ta) {
            ev.push(Event::new(ts, ta, tam, None, "cash-convert-in", fa));
        }
        if is_cash(fa) {
            ev.push(Event::new(ts, fa, -fam, None, "cash-convert-out", ta));
        }
    }
    for x in &p.deposits {
        let state = super::stored_str(x.get("state"));
        if (is_asset(s(x, "ccy")) || is_cash(s(x, "ccy"))) && matches!(state.as_str(), "1" | "2" | "3") {
            ev.push(Event::new(num_i64(&x["ts"]), s(x, "ccy"), f(x.get("amt")), None, "deposit", &trunc(s(x, "chain"), 16)));
        }
    }
    for x in &p.withdrawals {
        if is_asset(s(x, "ccy")) || is_cash(s(x, "ccy")) {
            let to = if !s(x, "to").is_empty() { s(x, "to") } else { s(x, "chain") };
            ev.push(Event::new(num_i64(&x["ts"]), s(x, "ccy"), -f(x.get("amt")), Some(0.0), "withdraw", &trunc(to, 16)));
        }
    }
    ev
}

/// Relabel OKX deposits matching a Binance withdrawal (same asset, amount
/// within 2%, landing 0-4 days later) as 'transfer-in'. `bwd` = (ts, asset,
/// amount). Returns how many matched.
pub fn net_transfers(events: &mut [Event], bwd: &[(i64, String, f64)]) -> usize {
    let mut matched = 0;
    for e in events.iter_mut() {
        if e.kind != "deposit" || e.delta <= 0.0 {
            continue;
        }
        for (wts, asset, amt) in bwd {
            if *asset == e.asset && (amt - e.delta).abs() <= (e.delta * 0.02).max(1e-6) && (0..=4 * DAY_MS).contains(&(e.ts - wts)) {
                e.kind = "transfer-in".into();
                e.detail = "from Binance".into();
                matched += 1;
                break;
            }
        }
    }
    matched
}

/// Reconcile anchors against live truth, every asset in CASH | ASSETS
/// (sorted) at `now`.
pub fn reconcile(stored: &BTreeMap<String, f64>, live: &BTreeMap<String, f64>, now: i64) -> Vec<Event> {
    let mut out = Vec::new();
    for (a, sum) in stored {
        let diff = live.get(a).copied().unwrap_or(0.0) - sum;
        if is_cash(a) && diff.abs() > 0.01 {
            out.push(Event::new(now, a, diff, None, "cash-adjust", "reconcile"));
        } else if is_asset(a) && diff.abs() > 1e-8 {
            out.push(Event::new(now, a, diff, None, "asset-adjust", "reconcile"));
        }
    }
    out
}

pub fn fetch(conn: &Connection, log: Log) -> Result<()> {
    let api = Client::from_env()?;
    // fatal on failure: the merge wipes+rebuilds the last 80 days, so an
    // empty list from a transient error would delete 80 days of trades
    let mut p = Pull::default();
    let mut seen = HashSet::new();
    for ep in ["/api/v5/account/bills", "/api/v5/account/bills-archive"] {
        for b in api.paged(ep, "billId")? {
            let id = s(&b, "billId").to_string();
            if !id.is_empty() && seen.insert(id) {
                p.bills.push(b);
            }
        }
    }
    p.convert = api.paged("/api/v5/asset/convert/history", "ts")?;
    p.deposits = api.paged("/api/v5/asset/deposit-history", "depId")?;
    p.withdrawals = api.paged("/api/v5/asset/withdrawal-history", "wdId")?;
    let mut events = build_events(&p);

    let bwd: Vec<(i64, String, f64)> = conn
        .prepare("SELECT ts, asset, -delta FROM crypto_events WHERE exchange='binance' AND kind='withdraw'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let matched = net_transfers(&mut events, &bwd);
    if matched > 0 {
        log(&format!("  OKX: netted {matched} Binance->OKX transfer(s)"));
    }

    let cutoff = now_ms() - 80 * DAY_MS;
    db::merge_events(conn, "okx", &events, Some(cutoff), &[], &["cash-adjust", "asset-adjust"])?;
    for (name, payload) in [("trade_bills", &p.bills), ("convert", &p.convert), ("deposits", &p.deposits), ("withdrawals", &p.withdrawals)] {
        db::upsert_raw(conn, "okx", name, &Value::Array(payload.clone()).to_string())?;
    }

    // anchor against live truth, on the STORED ledger (older events live
    // only in the DB, not in this pull)
    match live_balances_with(&api) {
        Err(e) => log(&format!("  OKX: warning: cash reconcile skipped, prior adjustments kept ({e})")),
        Ok(live) => {
            conn.execute("DELETE FROM crypto_events WHERE exchange='okx' AND kind IN ('cash-adjust','asset-adjust')", [])?;
            let mut stored = BTreeMap::new();
            for a in CASH.iter().chain(ASSETS.iter()) {
                stored.insert(a.to_string(), db::ledger_sum(conn, "okx", a)?);
            }
            let adj = reconcile(&stored, &live, now_ms());
            for a in &adj {
                if a.kind == "cash-adjust" {
                    log(&format!("  OKX: cash adjustment {} {:+.2}", a.asset, a.delta));
                } else {
                    log(&format!("  OKX: asset adjustment {} {:+.8}", a.asset, a.delta));
                }
            }
            if !adj.is_empty() {
                db::merge_events(conn, "okx", &adj, None, &[], &[])?;
                events.extend(adj);
            }
        }
    }
    let n = super::prices::backfill_deposit_cost(conn)?;
    log(&format!("  OKX: saved {} events (backfilled cost basis for {n} deposits)", events.len()));
    Ok(())
}

/// Sum the trading (cashBal), funding (bal) and savings (amt) views.
pub fn parse_balances(trading: &[Value], funding: &[Value], savings: &[Value]) -> BTreeMap<String, f64> {
    let mut held: BTreeMap<String, f64> = BTreeMap::new();
    for acct in trading {
        for c in acct.get("details").and_then(Value::as_array).into_iter().flatten() {
            if !s(c, "ccy").is_empty() {
                *held.entry(s(c, "ccy").into()).or_default() += f(c.get("cashBal"));
            }
        }
    }
    for (rows, field) in [(funding, "bal"), (savings, "amt")] {
        for c in rows {
            if !s(c, "ccy").is_empty() {
                *held.entry(s(c, "ccy").into()).or_default() += f(c.get(field));
            }
        }
    }
    held.retain(|_, v| *v > 0.0);
    held
}

fn live_balances_with(api: &Client) -> Result<BTreeMap<String, f64>> {
    // strict: a partial read would be treated as exact truth downstream
    let trading = api.get("/api/v5/account/balance", &[])?;
    let funding = api.get("/api/v5/asset/balances", &[])?;
    let savings = api.get("/api/v5/finance/savings/balance", &[])?;
    Ok(parse_balances(&trading, &funding, &savings))
}

/// Every positive balance across trading + funding + savings, crypto AND cash.
pub fn live_balances() -> Result<BTreeMap<String, f64>> {
    live_balances_with(&Client::from_env()?).map_err(|e| anyhow!("okx: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn signature_is_okx_documented_shape() {
        // HMAC-SHA256 base64 is 44 chars; deterministic for the same input
        let a = sign("secret", "2020-12-08T09:08:57.715Z", "/api/v5/account/balance");
        assert_eq!(a.len(), 44);
        assert_eq!(a, sign("secret", "2020-12-08T09:08:57.715Z", "/api/v5/account/balance"));
        assert_ne!(a, sign("secret", "2020-12-08T09:08:57.716Z", "/api/v5/account/balance"));
    }

    #[test]
    fn synthetic_convert_and_netting() {
        let p = Pull {
            convert: vec![json!({"baseCcy": "BTC", "quoteCcy": "USDC", "side": "buy", "fillBaseSz": "0.001", "fillQuoteSz": "60", "ts": "1000"})],
            deposits: vec![json!({"ccy": "ETH", "amt": "0.99", "state": "2", "ts": "100000000", "chain": "ETH-Arbitrum One"})],
            withdrawals: vec![json!({"ccy": "USDT", "amt": "5", "ts": "2000", "to": "", "chain": "USDT-TRC20"})],
            bills: vec![json!({"ccy": "BTC", "instId": "BTC-EUR", "px": "50000", "balChg": "0.01", "ts": "3000"})],
        };
        let mut ev = build_events(&p);
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["spot-buy", "convert-in", "cash-convert-out", "deposit", "withdraw"]);
        assert_eq!(ev[0].usd, Some(500.0), "EUR quote counts as stable");
        assert_eq!(ev[1].usd, Some(60.0));
        assert_eq!(ev[3].detail, "ETH-Arbitrum One");
        assert_eq!(ev[4].detail, "USDT-TRC20");
        let n = net_transfers(&mut ev, &[(100000000 - DAY_MS, "ETH".into(), 1.0)]);
        assert_eq!(n, 1);
        assert_eq!((ev[3].kind.as_str(), ev[3].detail.as_str()), ("transfer-in", "from Binance"));
    }

    #[test]
    fn balances_sum_three_views() {
        let b = parse_balances(
            &[json!({"details": [{"ccy": "BTC", "cashBal": "0.1"}]})],
            &[json!({"ccy": "BTC", "bal": "0.2"}), json!({"ccy": "EUR", "bal": "0"})],
            &[json!({"ccy": "USDC", "amt": "10"})],
        );
        assert!((b["BTC"] - 0.3).abs() < 1e-12);
        assert_eq!(b["USDC"], 10.0);
        assert!(!b.contains_key("EUR"));
    }

    /// Recorded bills/deposits/withdrawals rebuild the stored rows. Only
    /// events inside the payloads' own span are checked against the ledger
    /// (the 80-day window); 'transfer-in' relabels are matched loosely.
    #[test]
    fn recorded_payloads_match_parity_ledger() {
        let Some(conn) = crate::fetch::parity_db() else { return };
        let raw = |src: &str| -> Vec<Value> {
            conn.query_row("SELECT payload FROM crypto_raw_sources WHERE exchange='okx' AND source=?", [src], |r| r.get::<_, String>(0))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default()
        };
        let p = Pull { bills: raw("trade_bills"), convert: raw("convert"), deposits: raw("deposits"), withdrawals: raw("withdrawals") };
        let mut ev = build_events(&p);
        let bwd: Vec<(i64, String, f64)> = conn
            .prepare("SELECT ts, asset, -delta FROM crypto_events WHERE exchange='binance' AND kind='withdraw'")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        net_transfers(&mut ev, &bwd);
        assert!(!ev.is_empty());
        let mut missing = 0;
        for e in &ev {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM crypto_events WHERE exchange='okx' AND ts=? AND asset=? AND delta=? AND kind=? AND detail=? AND (usd IS ? OR kind IN ('deposit','transfer-in'))",
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
}
