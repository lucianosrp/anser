//! Net worth now: reported bank balances + crypto (live exchange balances
//! x live tickers, else the last balance snapshot, stale) + investments per
//! holding + receivables, all HKD.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::fx::{current_fx_rates, get_rate};
use super::holdings::investment_summary;
use super::prices::{add_snapshot, last_snapshot_ts};
use super::sheet::{ledger_currency, monthly_balance};
use super::{bank_key, balance_method, num, title_case, r2, round_n, source_label, Ctx};

/// The DCA core: a missing live price for these is an outage (snapshot
/// fallback); anything else unpriceable is skipped.
const CORE_CRYPTO: [&str; 3] = ["BTC", "ETH", "BNB"];
/// ~USD 1: positions below this are noise, not net worth.
const DUST_HKD: f64 = 8.0;

/// One by_source row.
struct Src {
    key: String,
    label: String,
    kind: &'static str,
    currency: String,
    native_amount: f64,
    hkd_value: f64,
    no_fx: bool,
}

impl Src {
    fn json(&self, total: f64) -> Value {
        let mut m = Map::new();
        m.insert("key".into(), json!(self.key));
        m.insert("label".into(), json!(self.label));
        m.insert("kind".into(), json!(self.kind));
        m.insert("currency".into(), json!(self.currency));
        m.insert("native_amount".into(), num(self.native_amount));
        m.insert("hkd_value".into(), num(self.hkd_value));
        if self.no_fx {
            m.insert("no_fx".into(), json!(true));
        }
        m.insert("pct".into(), num(if total != 0.0 { r2(self.hkd_value / total * 100.0) } else { 0.0 }));
        Value::Object(m)
    }
}

fn crypto_src(exch: &str, asset: &str, amount: f64, hkd_value: f64) -> Src {
    Src {
        key: format!("{exch}_{asset}").to_lowercase(),
        label: format!("{} {asset}", title_case(exch)),
        kind: "crypto",
        currency: asset.to_string(),
        native_amount: round_n(amount, 8),
        hkd_value,
        no_fx: false,
    }
}

/// _crypto_snapshot_fallback: crypto from the last balance snapshot.
fn snapshot_fallback(ctx: &Ctx, usd_hkd: f64) -> (f64, Vec<Src>) {
    let mut total = 0.0;
    let mut out = Vec::new();
    for (exch, assets) in &ctx.snapshot().by_exchange {
        for a in assets {
            // same pre-window clamp as the live path
            let amount = a.amount.unwrap_or(0.0).max(0.0);
            let price = a.price.unwrap_or(0.0);
            let hkd = r2(amount * price * usd_hkd);
            if hkd < DUST_HKD {
                continue;
            }
            total += hkd;
            out.push(crypto_src(&exch, &a.asset, amount, hkd));
        }
    }
    (total, out)
}

/// Live crypto: exchange balances (feed) + every wallet source's ledger holdings,
/// valued at live tickers. Err -> the caller falls back to the snapshot.
#[allow(clippy::type_complexity)]
fn live_crypto(ctx: &Ctx, usd_hkd: f64) -> anyhow::Result<(f64, Vec<Src>, Vec<(String, Vec<(String, f64)>)>, Vec<(String, f64)>)> {
    if usd_hkd == 0.0 {
        anyhow::bail!("no USD/HKD rate for live crypto valuation");
    }
    let mut holdings: Vec<(String, Vec<(String, f64)>)> = ctx
        .feed
        .exchange_balances()?
        .into_iter()
        .map(|(k, h)| (k, h.into_iter().collect()))
        .collect();
    // every self-custody / ledger wallet source (trezor, revolut), listed
    // in reverse registry order: cold storage first
    let wallets: Vec<&'static str> = crate::sources::file_sources().iter().rev()
        .filter(|s| s.spec().kind == crate::sources::Kind::CryptoWallet).map(|s| s.spec().key).collect();
    for exch in wallets {
        let mut stmt = ctx.conn.prepare_cached("SELECT asset, SUM(delta) amt FROM crypto_events WHERE exchange=? GROUP BY asset")?;
        let rows: Vec<(String, Option<f64>)> =
            stmt.query_map([exch], |r| Ok((r.get(0)?, super::get_f64(r, 1))))?.flatten().collect();
        // a negative net delta predates the ledger window: clamp, don't short
        let h: Vec<(String, f64)> = rows
            .into_iter()
            .filter_map(|(a, amt)| amt.filter(|v| *v != 0.0).map(|v| (a, v.max(0.0))))
            .filter(|(_, v)| *v != 0.0)
            .collect();
        if !h.is_empty() {
            match holdings.iter_mut().find(|(k, _)| k == exch) {
                Some(slot) => slot.1 = h,
                None => holdings.push((exch.to_string(), h)),
            }
        }
    }
    let mut assets: Vec<String> = holdings.iter().flat_map(|(_, h)| h.iter().map(|(a, _)| a.clone())).collect();
    assets.sort();
    assets.dedup();
    let prices: BTreeMap<String, f64> = if assets.is_empty() { BTreeMap::new() } else { ctx.feed.crypto_prices(&assets)? };
    let mut total = 0.0;
    let mut out = Vec::new();
    for (exch, h) in &holdings {
        for (asset, amount) in h {
            if *amount == 0.0 {
                continue;
            }
            let price = prices.get(asset).copied().unwrap_or(0.0);
            if price == 0.0 {
                if CORE_CRYPTO.contains(&asset.as_str()) {
                    anyhow::bail!("no live price for {asset}");
                }
                continue; // unpriceable non-core asset (delisted dust, ...)
            }
            let hkd = r2(amount * price * usd_hkd);
            if hkd < DUST_HKD {
                continue;
            }
            total += hkd;
            out.push(crypto_src(exch, asset, *amount, hkd));
        }
    }
    Ok((total, out, holdings, prices.into_iter().collect()))
}

/// What a mortgage setup adds to net worth: the balance as a liability
/// (negative) and, when the home's value is set, the property. None unless
/// the home setting is in mortgage mode with a loan, so every other
/// household's net worth is as before. `liability` is the negative HKD total.
struct HomeNetWorth {
    sources: Vec<Src>,
    liability_hkd: f64,
    property_hkd: Option<f64>,
}

fn home_net_worth(ctx: &Ctx, base: &str, rate_now: &dyn Fn(&str) -> Option<f64>) -> Option<HomeNetWorth> {
    let raw = crate::profile::setting(ctx.conn, finance_core::home::SETTING_KEY).ok().flatten()?;
    let setup = finance_core::home::HomeSetup::parse(&raw)?;
    let loan = setup.loan()?;
    let ccy = setup.currency.as_deref().map(str::trim).filter(|c| !c.is_empty()).map(str::to_uppercase).unwrap_or_else(|| base.to_string());
    let rate = rate_now(&ccy);
    let rows = finance_core::mortgage::schedule(&loan);
    let balance = finance_core::mortgage::balance_on(&loan, &rows, ctx.today);
    let value = setup.mortgage.home_value.filter(|v| *v > 0.);
    let mut sources = Vec::new();
    let mut src = |key: &str, label: &str, kind: &'static str, native: f64| {
        let mut s = Src { key: key.into(), label: label.into(), kind, currency: ccy.clone(), native_amount: r2(native), hkd_value: 0.0, no_fx: false };
        match rate {
            Some(r) => s.hkd_value = r2(native * r),
            None => {
                s.no_fx = true;
                s.label.push_str(" — no FX rate");
            }
        }
        let hkd = s.hkd_value;
        sources.push(s);
        hkd
    };
    let liability_hkd = src("mortgage", "Mortgage", "liability", -balance);
    let property_hkd = value.map(|v| src("home", "Home", "property", v));
    Some(HomeNetWorth { sources, liability_hkd, property_hkd })
}

/// Net worth now. live=false is the instant-paint path: no
/// fetch at all, crypto from the last snapshot, stale=true.
pub fn net_worth_now(ctx: &Ctx, live: bool) -> Value {
    let conn = ctx.conn;
    let now = chrono::Utc::now();
    let fx = current_fx_rates(ctx, live);
    let base = super::fx::base_currency(conn);
    let rate_now = |ccy: &str| if ccy == base { Some(1.0) } else { get_rate(&fx, ccy) };
    let usd_hkd = rate_now("USD").unwrap_or(0.0);
    let eur_hkd = rate_now(crate::config::SECONDARY_CURRENCY).unwrap_or(0.0);

    // -- banks --
    let mut bank_sources = Vec::new();
    let mut banks_hkd = 0.0;
    for l in ctx.ledgers().iter() {
        let (source, account) = (&l.source, &l.account);
        let method = balance_method(source);
        if method == "none" {
            continue; // a liability (credit card)
        }
        let bal = monthly_balance(l, method);
        // a null last balance counts as 0
        let native = bal.last().and_then(|b| b.1).unwrap_or(0.0);
        let ccy = ledger_currency(conn, source, account);
        let rate = rate_now(&ccy);
        let mut s = Src {
            key: bank_key(source, account),
            label: source_label(source, account),
            kind: "bank",
            currency: ccy,
            native_amount: r2(native),
            hkd_value: 0.0,
            no_fx: false,
        };
        match rate {
            None => {
                // no FX rate: excluded from the total, said on the row
                s.no_fx = true;
                s.label.push_str(" — no FX rate");
            }
            Some(r) => {
                s.hkd_value = r2(native * r);
                banks_hkd += s.hkd_value;
            }
        }
        bank_sources.push(s);
    }

    // -- crypto --
    if live {
        // the live sweep fetches the fund proxy's quote alongside the balances
        crate::fetch::live::set_sweep_proxy(ctx.invest().fund_account().and_then(|a| a.proxy.clone()));
    }
    let (stale, crypto_hkd, crypto_sources, live_snap) = if !live {
        let (t, s) = snapshot_fallback(ctx, usd_hkd);
        (true, t, s, None)
    } else {
        match live_crypto(ctx, usd_hkd) {
            Ok((t, s, holdings, prices)) => (false, t, s, Some((holdings, prices))),
            Err(_) => {
                let (t, s) = snapshot_fallback(ctx, usd_hkd);
                (true, t, s, None)
            }
        }
    };
    // the fund account's proxy quote (the live sweep fetched it alongside the balances)
    let proxy = ctx.invest().fund_account().and_then(|a| a.proxy.clone());
    let equity_prefetched = match (&proxy, live) {
        (Some(p), true) => ctx.feed.equity_now(p).ok(),
        _ => None,
    };

    // -- investments: broker mark > real units / proxy estimate > cost --
    let inv = investment_summary(ctx, usd_hkd, live, equity_prefetched);
    let invest_hkd = r2(inv.total_market.max(0.0));
    let mut invest_sources = Vec::new();
    for h in &inv.holdings {
        if h.market <= 0.0 {
            continue;
        }
        invest_sources.push(Src {
            key: format!("invest_{}", h.holding).to_lowercase().replace(' ', "_"),
            label: h.holding.clone(),
            kind: "investment",
            currency: base.clone(),
            native_amount: r2(h.market),
            hkd_value: r2(h.market),
            no_fx: false,
        });
    }

    // -- receivables (net worth must never fail on them) --
    let (rec_sources, rec_hkd) = match super::receivables::compute(ctx, Some(&fx)) {
        Ok(rec) => {
            let total = rec.get("total_hkd").and_then(Value::as_f64).unwrap_or(0.0);
            let rows: Vec<Src> = super::receivables::by_source(&rec, &base)
                .into_iter()
                .map(|r| Src {
                    key: r.0,
                    label: r.1,
                    kind: "receivable",
                    currency: r.2,
                    native_amount: r.3,
                    hkd_value: r.4,
                    no_fx: false,
                })
                .collect();
            (rows, total)
        }
        Err(_) => (vec![], 0.0),
    };

    // -- a mortgage: the loan as a liability, the home as property --
    let home = home_net_worth(ctx, &base, &rate_now);
    let (home_sources, home_hkd) = match &home {
        Some(h) => (h.sources.as_slice(), h.liability_hkd + h.property_hkd.unwrap_or(0.0)),
        None => (&[][..], 0.0),
    };

    let total_hkd = r2(banks_hkd + crypto_hkd + invest_hkd + rec_hkd + home_hkd);
    let total_eur = if eur_hkd != 0.0 { r2(total_hkd / eur_hkd) } else { 0.0 };
    let by_source: Vec<Value> = bank_sources
        .iter()
        .chain(&crypto_sources)
        .chain(&invest_sources)
        .chain(&rec_sources)
        .chain(home_sources)
        .map(|s| s.json(total_hkd))
        .collect();

    if let Some((holdings, prices)) = live_snap {
        let now_ms = now.timestamp_millis();
        if now_ms - last_snapshot_ts(conn) > 3_600_000 {
            for (exch, h) in &holdings {
                add_snapshot(conn, now_ms, exch, h, &prices);
            }
        }
    }

    let mut m = Map::new();
    m.insert("as_of".into(), json!(as_of(now)));
    m.insert("stale".into(), json!(stale));
    m.insert("total_hkd".into(), num(total_hkd));
    m.insert("total_eur".into(), num(total_eur));
    m.insert("banks_hkd".into(), num(r2(banks_hkd)));
    m.insert("crypto_hkd".into(), num(r2(crypto_hkd)));
    m.insert("investments_hkd".into(), num(invest_hkd));
    m.insert("investments_cost_hkd".into(), num(r2(inv.total_cost)));
    m.insert("investments_gain_hkd".into(), num(r2(inv.total_gain)));
    m.insert("investments_holdings".into(), Value::Array(inv.holdings.iter().map(|h| h.json()).collect()));
    m.insert("receivables_hkd".into(), num(r2(rec_hkd)));
    // only with a mortgage setup: the keys are absent otherwise
    if let Some(h) = &home {
        m.insert("liabilities_hkd".into(), num(r2(h.liability_hkd)));
        if let Some(p) = h.property_hkd {
            m.insert("property_hkd".into(), num(r2(p)));
        }
    }
    m.insert("by_source".into(), Value::Array(by_source));
    Value::Object(m)
}

/// A valuation's as_of: ISO 8601 UTC with microseconds and +00:00
/// (always 32 chars, e.g. 2026-10-03T08:15:00.123456+00:00).
pub fn as_of(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn worth(c: &rusqlite::Connection) -> Value {
        let mut ctx = Ctx::offline(c);
        ctx.today = "2026-09-30".parse().unwrap();
        net_worth_now(&ctx, false)
    }

    fn mortgage(value: Option<f64>) -> Value {
        json!({"mode": "mortgage", "currency": "HKD", "mortgage": {"principal": 1000000.0, "annual_rate_pct": 3.5, "term_months": 360,
            "start": "2026-01-15", "home_value": value}})
    }

    #[test]
    fn a_mortgage_adds_a_liability_and_the_home() {
        let c = crate::db::memory().unwrap();
        let plain = worth(&c);
        for k in ["liabilities_hkd", "property_hkd"] {
            assert!(plain.get(k).is_none(), "no mortgage: no {k}");
        }
        // a rent setup changes nothing either
        crate::profile::put_setting(&c, "home", &json!({"mode": "rent", "rent": {"monthly": 1000}})).unwrap();
        assert_eq!(worth(&c)["total_hkd"], plain["total_hkd"]);
        assert!(worth(&c).get("liabilities_hkd").is_none());

        // a mortgage without a home value: the liability only
        crate::profile::put_setting(&c, "home", &mortgage(None)).unwrap();
        let w = worth(&c);
        let owed = w["liabilities_hkd"].as_f64().unwrap();
        assert!((-990_000.0..-980_000.0).contains(&owed), "{owed}: the balance after eight payments, negative");
        assert!(w.get("property_hkd").is_none());
        let src = w["by_source"].as_array().unwrap().iter().find(|s| s["kind"] == "liability").unwrap();
        assert_eq!((src["key"].as_str(), src["hkd_value"].as_f64()), (Some("mortgage"), Some(owed)));
        assert_eq!(w["total_hkd"].as_f64().unwrap(), plain["total_hkd"].as_f64().unwrap() + owed);

        // with the home's value: the property too, both in the total
        crate::profile::put_setting(&c, "home", &mortgage(Some(1_400_000.0))).unwrap();
        let w = worth(&c);
        assert_eq!(w["property_hkd"], json!(1_400_000.0));
        assert_eq!(w["total_hkd"].as_f64().unwrap(), r2(plain["total_hkd"].as_f64().unwrap() + owed + 1_400_000.0));
        let kinds: Vec<&str> = w["by_source"].as_array().unwrap().iter().filter_map(|s| s["kind"].as_str()).collect();
        assert!(kinds.contains(&"liability") && kinds.contains(&"property"));
        // the loan not started yet owes nothing
        let mut ctx = Ctx::offline(&c);
        ctx.today = "2025-06-01".parse().unwrap();
        assert_eq!(net_worth_now(&ctx, false)["liabilities_hkd"], json!(0.0));
    }
}
