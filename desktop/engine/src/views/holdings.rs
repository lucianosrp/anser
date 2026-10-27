//! Investment holdings:
//! contribution attribution, the per-holding monthly marks behind the
//! balance sheet and the Investments page, the "now" summary, the
//! fund history (real fund units bridged by the index proxy), and the
//! IBKR views (history at real fills, broker performance, market now).
//!
//! Which account a contribution belongs to, its proxy and fund code come
//! from the investment setup (crate::invest_setup, `ctx.invest()`).
//!
//! Valuation priority per holding: broker mark > real units x bridged NAV >
//! index-proxy estimate > cost basis. Network-free except through the feed
//! (a stale proxy series is refreshed when `live`).

use std::collections::HashMap;

use chrono::NaiveDate;
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::fx::Rates;
use super::prices::{equity_price_hist, refresh_before, upsert_equity_bars, Hist};
use super::{get_f64, get_str, month_date, month_range, month_start, ms_to_date, next_month, num, opt_num, query_json,
            r2, round_n, take_chars, ym_of, Ctx};

/// "MSCI World" -> "MSCI": the word the real-units source label uses
/// ("U50004 NAV + MSCI bridge").
fn bridge_word(label: &str) -> &str {
    label.split_whitespace().next().unwrap_or(label)
}

/// One 'Investments' bank row, converted to HKD at its month's rate.
#[derive(Clone, Debug)]
pub struct Contrib {
    pub date: String, // YYYY-MM-DD
    pub ym: String,
    pub holding: String,
    /// money in (a redemption is negative)
    pub hkd: f64,
    pub usd_hkd: f64,
}

/// Every row of an investment category (kind investment: 'Investments')
/// (optionally only `txn_date > after`; by date when `ordered`, else in
/// table order).
pub fn contributions(ctx: &Ctx, rates: &Rates, after: Option<&str>, ordered: bool) -> Vec<Contrib> {
    let setup = ctx.invest();
    let flags = ctx.flags();
    let bank = ctx.bank();
    let mut rows: Vec<&super::store::BankRow> = bank
        .iter()
        .filter(|b| b.category.as_deref().is_some_and(|c| flags.is_investment(c)) && after.is_none_or(|a| b.txn_date.as_str() > a))
        .collect();
    if ordered {
        rows.sort_by(|a, b| a.txn_date.cmp(&b.txn_date));
    }
    rows.into_iter()
        .map(|b| {
            let ym = b.ym().to_string();
            Contrib {
                date: super::take_chars(&b.txn_date, 10).to_string(),
                holding: setup.holding_of(&b.description).to_string(),
                hkd: -b.amount.unwrap_or(0.0) * rates.rate(&b.currency, &ym),
                usd_hkd: rates.rate("USD", &ym),
                ym,
            }
        })
        .collect()
}

/// The broker's own deposits and withdrawals (ibkr_cash_flows), as
/// (date, signed amount, currency) by date. Empty without the table or rows.
fn ibkr_broker_flows(conn: &Connection) -> Vec<(String, f64, String)> {
    conn.prepare_cached("SELECT date, amount, currency FROM ibkr_cash_flows ORDER BY date, flow_id")
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok((get_str(r, 0).unwrap_or_default(), get_f64(r, 1).unwrap_or(0.0), get_str(r, 2).unwrap_or_default()))
            })?
            .collect()
        })
        .unwrap_or_default()
}

/// Days a bank row may precede the broker's booking of the same deposit.
const LAG_DAYS: i64 = 10;

/// contributions() with the IBKR holding's money in taken from the broker's
/// own deposits and withdrawals (ibkr_cash_flows) from its first recorded flow
/// onward; bank rows into IBKR before that date stay (the Flex history may
/// not reach the account's start) unless one is the lagged twin of a broker flow
/// (same amount, booked within LAG_DAYS after), and those on or after it are dropped so a
/// deposit is never counted twice (the bank export can lag the broker). Without
/// broker flows this is exactly contributions(). Used wherever money in is
/// compared with the broker's value (investment history, summary).
pub fn investment_flows(ctx: &Ctx, rates: &Rates, after: Option<&str>, ordered: bool) -> Vec<Contrib> {
    let mut rows = contributions(ctx, rates, after, ordered);
    let broker = ibkr_broker_flows(ctx.conn);
    let Some(cutover) = broker.first().map(|f| take_chars(&f.0, 10).to_string()) else { return rows };
    let setup = ctx.invest();
    let name = setup.ibkr_name();
    // a bank row just before the cutover is the same deposit as a broker flow
    // booked a few days later (the lag): drop it when one matches, one to one
    let mut matched = vec![false; broker.len()];
    let day = |d: &str| NaiveDate::parse_from_str(take_chars(d, 10), "%Y-%m-%d").ok();
    let mut lagged = vec![false; rows.len()];
    for (i, c) in rows.iter().enumerate() {
        if !setup.is_ibkr(&c.holding) || c.date.as_str() >= cutover.as_str() {
            continue;
        }
        let Some(d) = day(&c.date) else { continue };
        let hit = broker.iter().enumerate().position(|(j, (bd, amount, ccy))| {
            let hkd = amount * rates.rate(ccy, bd.get(..7).unwrap_or_default());
            let gap = day(bd).map(|b| (b - d).num_days());
            !matched[j]
                && gap.is_some_and(|g| (0..=LAG_DAYS).contains(&g))
                && hkd * c.hkd > 0.0
                && (hkd - c.hkd).abs() <= 0.02 * c.hkd.abs()
        });
        if let Some(j) = hit {
            matched[j] = true;
            lagged[i] = true;
        }
    }
    let mut i = 0;
    rows.retain(|c| {
        i += 1;
        !lagged[i - 1] && !(setup.is_ibkr(&c.holding) && c.date.as_str() >= cutover.as_str())
    });
    for (date, amount, ccy) in broker {
        let date = take_chars(&date, 10).to_string();
        if after.is_some_and(|a| date.as_str() <= a) {
            continue;
        }
        let ym = date.get(..7).unwrap_or_default().to_string();
        rows.push(Contrib {
            holding: name.clone(),
            hkd: amount * rates.rate(&ccy, &ym),
            usd_hkd: rates.rate("USD", &ym),
            date,
            ym,
        });
    }
    if ordered {
        rows.sort_by(|a, b| a.date.cmp(&b.date));
    }
    rows
}

/// One real unit-trust execution (fund_subscriptions).
#[derive(Clone, Debug)]
pub struct Sub {
    pub trade_date: String,
    pub fund: String,
    pub units: f64,
    pub px: f64,
}

/// A fund's subscriptions, by trade date.
pub fn load_fund_subscriptions(conn: &Connection, fund: &str) -> Vec<Sub> {
    conn.prepare_cached("SELECT trade_date, fund, units, px FROM fund_subscriptions WHERE fund=? ORDER BY trade_date")
        .and_then(|mut s| {
            s.query_map([fund], |r| {
                Ok(Sub {
                    trade_date: get_str(r, 0).unwrap_or_default(),
                    fund: get_str(r, 1).unwrap_or_default(),
                    units: get_f64(r, 2).unwrap_or(0.0),
                    px: get_f64(r, 3).unwrap_or(0.0),
                })
            })?
            .collect()
        })
        .unwrap_or_default()
}

/// One month of a real-execution fund position (_fund_value_series row).
pub struct FundMonth {
    pub ym: String,
    pub contrib: f64,
    pub cost: f64,
    pub value: f64,
    pub close: f64,
}

/// Month-end value of a real-execution fund:
/// exact units x (latest confirmed NAV scaled by the proxy's move since),
/// plus `pending` debits newer than the last confirmation bridged as proxy
/// units bought on their debit date (so the days between a debit and its
/// SMS confirmation don't read as a loss of the whole tranche).
pub fn fund_value_series(subs: &[Sub], hist: &Hist, months: &[NaiveDate], pending: &[(String, f64)]) -> Vec<FundMonth> {
    let mut by_month: HashMap<&str, Vec<&Sub>> = HashMap::new();
    for s in subs {
        by_month.entry(super::take_chars(&s.trade_date, 7)).or_default().push(s);
    }
    let mut pend_by_month: HashMap<&str, Vec<&(String, f64)>> = HashMap::new();
    for p in pending {
        pend_by_month.entry(super::take_chars(&p.0, 7)).or_default().push(p);
    }
    let mut rows = Vec::with_capacity(months.len());
    let (mut cum_units, mut cum_cost, mut pend_units) = (0.0, 0.0, 0.0);
    let mut anchor: Option<&Sub> = None;
    for m in months {
        let ym = ym_of(*m);
        let buys = by_month.get(ym.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        cum_units += buys.iter().fold(0.0, |a, s| a + s.units);
        let mut contrib = buys.iter().fold(0.0, |a, s| a + s.units * s.px);
        for (d, hkd) in pend_by_month.get(ym.as_str()).map(Vec::as_slice).unwrap_or(&[]).iter().map(|p| (&p.0, p.1)) {
            let px0 = hist.on_or_before(d);
            if px0 > 0.0 {
                pend_units += hkd / px0;
                contrib += hkd;
            }
        }
        cum_cost += contrib;
        if let Some(last) = buys.last() {
            anchor = Some(last);
        }
        let close = hist.before(&next_month(*m).to_string());
        let base = anchor.map(|a| hist.on_or_before(&a.trade_date)).unwrap_or(0.0);
        let nav = match anchor {
            Some(a) if base > 0.0 => a.px * (close / base),
            _ => 0.0,
        };
        rows.push(FundMonth { ym, contrib, cost: cum_cost, value: cum_units * nav + pend_units * close, close });
    }
    rows
}

/// Monthly HKD value of every
/// investment account, keyed by holding (first-seen order): real units x
/// bridged NAV when fund_subscriptions has executions, else proxy units
/// marked at month-end closes, else cost (IBKR's current month pinned to
/// the broker mark).
pub fn investments_hkd_by_holding(ctx: &Ctx, months: &[NaiveDate]) -> Vec<(String, Vec<f64>)> {
    let conn = ctx.conn;
    let rates = ctx.rates();
    let setup = ctx.invest();
    let mut contribs: Vec<(String, Vec<Contrib>)> = Vec::new();
    for c in investment_flows(ctx, &rates, None, false) {
        match contribs.iter_mut().find(|(h, _)| *h == c.holding) {
            Some((_, v)) => v.push(c),
            None => contribs.push((c.holding.clone(), vec![c])),
        }
    }
    let month_keys: Vec<String> = months.iter().map(|m| ym_of(*m)).collect();
    let mut out: Vec<(String, Vec<f64>)> = Vec::new();
    for (h, rows) in &contribs {
        let mut series = vec![0.0; months.len()];
        let hist = setup.proxy(h).map(|p| ctx.equity(p)).unwrap_or_default();
        let subs = setup.fund(h).map(|f| load_fund_subscriptions(conn, f)).unwrap_or_default();
        if !subs.is_empty() && !hist.is_empty() {
            let last = &subs[subs.len() - 1].trade_date;
            let pending: Vec<(String, f64)> = rows
                .iter()
                .filter(|c| c.date.as_str() > last.as_str() && c.hkd > 0.0)
                .map(|c| (c.date.clone(), c.hkd))
                .collect();
            for (i, r) in fund_value_series(&subs, &hist, months, &pending).iter().enumerate() {
                series[i] += r.value;
            }
        } else if !hist.is_empty() {
            let mut units_by_month: HashMap<&str, f64> = HashMap::new();
            for c in rows {
                let px = hist.on_or_before(&c.date);
                if c.usd_hkd > 0.0 && px > 0.0 {
                    *units_by_month.entry(&c.date[..7]).or_insert(0.0) += (c.hkd / c.usd_hkd) / px;
                }
            }
            let mut cum = 0.0;
            for (i, m) in months.iter().enumerate() {
                cum += units_by_month.get(month_keys[i].as_str()).copied().unwrap_or(0.0);
                if cum != 0.0 {
                    let close = hist.before(&next_month(*m).to_string());
                    series[i] += cum * close * rates.rate("USD", &month_keys[i]);
                }
            }
        } else {
            let mut hkd_by_month: HashMap<&str, f64> = HashMap::new();
            for c in rows {
                *hkd_by_month.entry(&c.date[..7]).or_insert(0.0) += c.hkd;
            }
            // IBKR: history stays at cost, but "now" matches the broker mark
            let broker_now = if setup.is_ibkr(h) && month_keys.last().is_some_and(|k| *k == ctx.ym_now()) {
                ibkr_market_now_hkd(ctx, &rates)
            } else {
                None
            };
            let mut running = 0.0;
            for i in 0..months.len() {
                running += hkd_by_month.get(month_keys[i].as_str()).copied().unwrap_or(0.0);
                match broker_now {
                    Some(b) if b > 0.0 && i == months.len() - 1 => series[i] += b,
                    _ => series[i] += running,
                }
            }
        }
        out.push((h.to_string(), series));
    }
    out
}

/// HKD contributed to date (net of
/// redemptions, floored at 0), for one holding or all.
pub fn investment_cost_basis(ctx: &Ctx, holding: Option<&str>) -> f64 {
    let rates = ctx.rates();
    let total = investment_flows(ctx, &rates, None, false)
        .iter()
        .filter(|c| holding.is_none_or(|h| c.holding == h))
        .fold(0.0, |a, c| a + c.hkd);
    r2(total.max(0.0))
}

/// (market_hkd, source label) of a holding anchored
/// on real subscriptions: units x last NAV x the proxy's move since, plus
/// pending debits bridged as proxy units. None without real executions.
pub fn fund_market_now(ctx: &Ctx, holding: &str, hist: &Hist, cur: f64, rates: Option<&Rates>) -> Option<(f64, String)> {
    let setup = ctx.invest();
    let fund = setup.fund(holding)?;
    if hist.is_empty() || cur == 0.0 {
        return None;
    }
    let subs = load_fund_subscriptions(ctx.conn, fund);
    let last = subs.last()?;
    let base_px = hist.on_or_before(&last.trade_date);
    if base_px <= 0.0 {
        return None;
    }
    let units = subs.iter().fold(0.0, |a, s| a + s.units);
    let mut market = units * last.px * (cur / base_px);
    if let Some(rates) = rates {
        let mut pending = 0.0;
        for c in contributions(ctx, rates, Some(&last.trade_date), false) {
            if c.holding != holding {
                continue;
            }
            let px = hist.on_or_before(&c.date);
            if c.hkd <= 0.0 || px <= 0.0 {
                continue;
            }
            pending += c.hkd * (cur / px);
        }
        market += pending;
    }
    Some((r2(market), format!("{fund} NAV + {} bridge", bridge_word(&setup.proxy_label(holding)))))
}

/// One investment_summary holding.
#[derive(Clone, Debug)]
pub struct Holding {
    pub holding: String,
    pub cost: f64,
    pub market: f64,
    pub gain: f64,
    pub source: String,
}

impl Holding {
    pub fn json(&self) -> Value {
        json!({"holding": self.holding, "cost": num(self.cost), "market": num(self.market),
               "gain": num(self.gain), "source": self.source})
    }
}

pub struct Summary {
    pub total_cost: f64,
    pub total_market: f64,
    pub total_gain: f64,
    pub holdings: Vec<Holding>,
}

/// Per-holding market value now. live=false
/// marks the proxy to its last cached close; `equity_price` (prefetched by
/// the live net worth) wins over a fetch.
pub fn investment_summary(ctx: &Ctx, usd_hkd_now: f64, live: bool, equity_price: Option<f64>) -> Summary {
    let conn = ctx.conn;
    let rates = ctx.rates();
    let setup = ctx.invest();
    let mut cost: Vec<(String, f64)> = Vec::new();
    let mut contribs: HashMap<String, Vec<(String, f64, f64)>> = HashMap::new();
    for c in investment_flows(ctx, &rates, None, false) {
        match cost.iter_mut().find(|(h, _)| *h == c.holding) {
            Some((_, v)) => *v += c.hkd,
            None => cost.push((c.holding.clone(), c.hkd)),
        }
        let usd = if c.usd_hkd != 0.0 { c.usd_hkd } else { usd_hkd_now };
        contribs.entry(c.holding).or_default().push((c.date, c.hkd, usd));
    }

    // each proxy's series + current price, only for the proxied holdings
    // there are; `equity_price` (prefetched) is the fund account's proxy's
    let prefetched_for = setup.fund_account().and_then(|a| a.proxy.clone());
    let mut proxies: HashMap<String, (std::sync::Arc<Hist>, Option<f64>)> = HashMap::new();
    if usd_hkd_now != 0.0 {
        for (h, _) in &cost {
            let Some(p) = setup.proxy(h) else { continue };
            if proxies.contains_key(p) {
                continue;
            }
            let mut hist = ctx.equity(p);
            if hist.is_empty() && live {
                if let Ok(bars) = ctx.feed.equity_ohlc(p, "10y", None) {
                    upsert_equity_bars(conn, p, &bars, Some("USD"));
                    ctx.invalidate_equity(p);
                    hist = ctx.equity(p);
                }
            }
            let cur = hist.last_close().map(|last| match equity_price.filter(|_| prefetched_for.as_deref() == Some(p)) {
                Some(px) => px,
                None if live => ctx.feed.equity_now(p).unwrap_or(last),
                None => last,
            });
            proxies.insert(p.to_string(), (hist, cur));
        }
    }
    let empty: (std::sync::Arc<Hist>, Option<f64>) = Default::default();
    let proxy_of = |h: &str| setup.proxy(h).and_then(|p| proxies.get(p)).unwrap_or(&empty);

    let est_market = |h: &str| -> Option<f64> {
        let (hist, cur) = proxy_of(h);
        let cur = cur.filter(|c| *c != 0.0)?;
        if hist.is_empty() {
            return None;
        }
        let mut units = 0.0;
        for (d, hkd, usd_then) in contribs.get(h).map(Vec::as_slice).unwrap_or(&[]) {
            if *hkd <= 0.0 || *usd_then <= 0.0 {
                continue;
            }
            let px = hist.on_or_before(d);
            if px > 0.0 {
                units += (hkd / usd_then) / px;
            }
        }
        Some(r2(units * cur * usd_hkd_now))
    };

    // IBKR: broker truth beats both the proxy estimate and cost basis; the
    // holding may exist without a matching bank transfer
    let ibkr_mkt = ibkr_market_now_hkd(ctx, &rates).filter(|v| *v != 0.0);
    if ibkr_mkt.is_some() && !cost.iter().any(|(h, _)| setup.is_ibkr(h)) {
        cost.push((setup.ibkr_name(), 0.0));
    }

    let mut order: Vec<(String, f64)> = cost.clone();
    order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut holdings = Vec::new();
    for (h, c) in order {
        let c = r2(c);
        if setup.is_ibkr(&h) {
            if let Some(m) = ibkr_mkt {
                holdings.push(Holding { holding: h, cost: c, market: m, gain: r2(m - c), source: "IBKR broker".into() });
                continue;
            }
        }
        let (hist, cur) = proxy_of(&h);
        let anchored = match cur {
            Some(cv) if *cv != 0.0 => fund_market_now(ctx, &h, hist, *cv, Some(&rates)),
            _ => None,
        };
        let (market, src) = match anchored {
            Some(a) => a,
            None => match setup.proxy(&h).and_then(|_| est_market(&h)) {
                Some(m) => (m, format!("{} est.", setup.proxy_label(&h))),
                None => (c, "cost".to_string()),
            },
        };
        holdings.push(Holding { holding: h, cost: c, market, gain: r2(market - c), source: src });
    }
    let total_cost = r2(holdings.iter().fold(0.0, |a, h| a + h.cost));
    let total_market = r2(holdings.iter().fold(0.0, |a, h| a + h.market));
    Summary { total_cost, total_market, total_gain: r2(total_market - total_cost), holdings }
}

/// The FlexInvest page. Real executions when
/// fund_subscriptions has them (proxy only bridges since the last one),
/// else simulated proxy units per contribution. live refreshes a proxy
/// series older than 3 days through the feed; offline it is served stale.
pub fn flexinvest_history(ctx: &Ctx, holding: &str, live: bool) -> Value {
    let conn = ctx.conn;
    let rates = ctx.rates();
    let setup = ctx.invest();
    let proxy = setup.proxy(holding);
    let purchases: Vec<Contrib> =
        contributions(ctx, &rates, None, true).into_iter().filter(|c| c.holding == holding).collect();
    let empty = json!({"holding": holding, "proxy": proxy, "stale": true, "last_close_date": null,
                       "rows": [], "purchases": []});
    let Some(proxy) = proxy else { return empty };
    if purchases.is_empty() {
        return empty;
    }
    let mut hist = ctx.equity(proxy);
    let cutoff = refresh_before(ctx);
    let old = |h: &Hist| h.last_date().is_none_or(|d| d < cutoff.as_str());
    let mut stale = false;
    if live && old(&hist) {
        match ctx.feed.equity_ohlc(proxy, "10y", None) {
            Ok(bars) => {
                upsert_equity_bars(conn, proxy, &bars, Some("USD"));
                ctx.invalidate_equity(proxy);
                hist = ctx.equity(proxy);
            }
            Err(_) => stale = true,
        }
    } else if old(&hist) {
        stale = true;
    }
    if hist.is_empty() {
        return empty;
    }
    let last_close_date = hist.last_date().unwrap_or_default().to_string();

    let subs = setup.fund(holding).map(|f| load_fund_subscriptions(conn, f)).unwrap_or_default();
    if let Some(last) = subs.last() {
        let pending: Vec<(String, f64)> = contributions(ctx, &rates, Some(&last.trade_date), true)
            .into_iter()
            .filter(|c| c.holding == holding && c.hkd > 0.0)
            .map(|c| (c.date, c.hkd))
            .collect();
        let months = month_range(month_date(&subs[0].trade_date), month_start(ctx.today));
        let mut rows = Vec::new();
        let mut prev_value = 0.0;
        for r in fund_value_series(&subs, &hist, &months, &pending) {
            rows.push(json!({"month": r.ym, "contrib": num(r2(r.contrib)), "cost": num(r2(r.cost)),
                             "value": num(r2(r.value)), "gain": num(r2(r.value - r.cost)),
                             "market": num(r2(r.value - prev_value - r.contrib)), "close": num(r2(r.close))}));
            prev_value = r.value;
        }
        let purchases: Vec<Value> = subs
            .iter()
            .map(|s| json!({"date": s.trade_date, "hkd": num(r2(s.units * s.px)), "px": num(s.px),
                            "units": num(round_n(s.units, 4))}))
            .collect();
        return json!({"holding": holding, "proxy": proxy, "stale": stale, "last_close_date": last_close_date,
                      "real": true, "fund": last.fund, "last_nav": num(last.px), "nav_date": last.trade_date,
                      "rows": rows, "purchases": purchases});
    }

    // simulated: each contribution buys proxy units at that day's close
    let units: Vec<(f64, f64)> = purchases
        .iter()
        .map(|p| {
            let px = hist.on_or_before(&p.date);
            let u = if p.usd_hkd > 0.0 && px > 0.0 { (p.hkd / p.usd_hkd) / px } else { 0.0 };
            (r2(px), u)
        })
        .collect();
    let months = month_range(month_date(&purchases[0].ym), month_start(ctx.today));
    let mut rows = Vec::new();
    let (mut cum_units, mut cum_cost, mut prev_value) = (0.0, 0.0, 0.0);
    for m in &months {
        let ym = ym_of(*m);
        let mut contrib = 0.0;
        let mut du = 0.0;
        for (p, (_, u)) in purchases.iter().zip(&units) {
            if p.ym == ym {
                contrib += p.hkd;
                du += u;
            }
        }
        cum_units += du;
        cum_cost += contrib;
        let close = hist.before(&next_month(*m).to_string());
        let value = cum_units * close * rates.rate("USD", &ym);
        rows.push(json!({"month": ym, "contrib": num(r2(contrib)), "cost": num(r2(cum_cost)),
                         "value": num(r2(value)), "gain": num(r2(value - cum_cost)),
                         "market": num(r2(value - prev_value - contrib)), "close": num(r2(close))}));
        prev_value = value;
    }
    let purchases: Vec<Value> = purchases
        .iter()
        .zip(&units)
        .map(|(p, (px, u))| json!({"date": p.date, "hkd": num(r2(p.hkd)), "px": num(*px), "units": num(round_n(*u, 4))}))
        .collect();
    json!({"holding": holding, "proxy": proxy, "stale": stale, "last_close_date": last_close_date,
           "rows": rows, "purchases": purchases})
}

// ---------------------------------------------------------------------------
// IBKR
// ---------------------------------------------------------------------------

/// is_ibkr_fx_symbol: forex conversion legs ("USD.HKD") are cash, not holdings.
pub fn is_ibkr_fx_symbol(symbol: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z]{3}\.[A-Z]{3}$").unwrap()).is_match(symbol)
}

/// The latest broker snapshot, or None.
pub struct IbkrSnapshot {
    pub as_of: Value,
    pub base_ccy: String,
    pub positions: Vec<Value>,
    pub cash: Vec<Value>,
    pub positions_base: f64,
    pub cash_base: f64,
    pub total_base: f64,
}

impl IbkrSnapshot {
    pub fn json(&self) -> Value {
        json!({"as_of": self.as_of, "base_ccy": self.base_ccy, "positions": self.positions, "cash": self.cash,
               "positions_base": num(self.positions_base), "cash_base": num(self.cash_base),
               "total_base": num(self.total_base)})
    }
}

fn fnum(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

pub fn ibkr_snapshot(conn: &Connection) -> Option<IbkrSnapshot> {
    let positions = query_json(conn, "SELECT * FROM ibkr_positions ORDER BY value_base DESC", []).unwrap_or_default();
    let cash = query_json(conn, "SELECT * FROM ibkr_cash ORDER BY currency", []).unwrap_or_default();
    let meta = positions.first().or(cash.first())?.clone();
    let positions_base = positions.iter().fold(0.0, |a, p| a + fnum(p, "value_base"));
    let cash_base = cash.iter().fold(0.0, |a, c| a + fnum(c, "amount_base"));
    Some(IbkrSnapshot {
        as_of: meta.get("as_of").cloned().unwrap_or(Value::Null),
        base_ccy: meta.get("base_ccy").and_then(Value::as_str).unwrap_or("").to_string(),
        positions,
        cash,
        positions_base: r2(positions_base),
        cash_base: r2(cash_base),
        total_base: r2(positions_base + cash_base),
    })
}

/// Broker cash + positions (Flex snapshot) +
/// a cached-close mark for any held symbol the snapshot misses (at cost when
/// unpriced). None when there is no IBKR data at all. Network-free.
pub fn ibkr_market_now_hkd(ctx: &Ctx, rates: &Rates) -> Option<f64> {
    let conn = ctx.conn;
    let snap = ibkr_snapshot(conn);
    let held: Vec<(String, String, Option<f64>, Option<f64>)> = conn
        .prepare_cached(
            "SELECT symbol, currency, SUM(qty) units, SUM(-amount) cost FROM ibkr_trades GROUP BY symbol, currency",
        )
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok((get_str(r, 0).unwrap_or_default(), get_str(r, 1).unwrap_or_default(), get_f64(r, 2), get_f64(r, 3)))
            })?
            .collect()
        })
        .unwrap_or_default();
    if snap.is_none() && held.is_empty() {
        return None;
    }
    let ym = ctx.ym_now();
    let mut total = 0.0;
    let mut broker_syms: Vec<String> = Vec::new();
    if let Some(s) = &snap {
        total += (s.cash_base + s.positions_base) * rates.rate(&s.base_ccy, &ym);
        broker_syms = s.positions.iter().filter_map(|p| p.get("symbol").and_then(Value::as_str)).map(String::from).collect();
    }
    for (symbol, ccy, units, cost) in held {
        let units = units.unwrap_or(0.0);
        if broker_syms.contains(&symbol) || units <= 0.0 || is_ibkr_fx_symbol(&symbol) {
            continue;
        }
        let hist = ctx.equity(&symbol);
        let mark = match hist.last_close() {
            Some(c) => units * c,
            None => cost.unwrap_or(0.0),
        };
        total += mark * rates.rate(&ccy, &ym);
    }
    Some(r2(total))
}

/// SELECT * FROM ibkr_trades ORDER BY ts, forex legs dropped.
pub fn ibkr_trades(conn: &Connection) -> Vec<Value> {
    query_json(conn, "SELECT * FROM ibkr_trades ORDER BY ts", [])
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !is_ibkr_fx_symbol(t.get("symbol").and_then(Value::as_str).unwrap_or("")))
        .collect()
}

/// Every fill adds/removes real units of its symbol
/// at the actual cost; each symbol marked at its own month-end close (flat
/// at average cost while unpriced). Closes refresh through the feed when
/// older than 3 days and `live`.
pub fn ibkr_history(ctx: &Ctx, live: bool) -> Value {
    let rates = ctx.rates();
    let trades = ctx.ibkr_trades();
    if trades.is_empty() {
        return json!({"stale": false, "rows": [], "symbols": [], "purchases": []});
    }
    let s = |t: &Value, k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let f = |t: &Value, k: &str| t.get(k).and_then(Value::as_f64);
    let mut symbols: Vec<String> = trades.iter().map(|t| s(t, "symbol")).collect();
    symbols.sort();
    symbols.dedup();
    let mut ccy_of: HashMap<String, String> = HashMap::new();
    for t in trades.iter() {
        ccy_of.insert(s(t, "symbol"), s(t, "currency")); // fills of a symbol share ccy
    }
    let mut hists: HashMap<String, std::sync::Arc<Hist>> = HashMap::new();
    let mut stale = false;
    for sym in &symbols {
        let (h, st) = equity_price_hist(ctx, sym, live, ccy_of.get(sym).map(String::as_str), "5y");
        stale = stale || st;
        hists.insert(sym.clone(), h);
    }

    struct P {
        date: String,
        ym: String,
        symbol: String,
        units: f64,
        px: f64,
        currency: String,
        cost_ccy: f64,
        hkd: f64,
    }
    let purchases: Vec<P> = trades
        .iter()
        .map(|t| {
            let date = ms_to_date(t.get("ts").and_then(Value::as_i64).unwrap_or(0)).to_string();
            let ym = date[..7].to_string();
            let qty = f(t, "qty").unwrap_or(0.0);
            let mut cost_ccy = -f(t, "amount").unwrap_or(0.0); // buy: netCash < 0 -> positive cost
            if cost_ccy == 0.0 && qty != 0.0 {
                cost_ccy = qty * f(t, "price").unwrap_or(0.0);
            }
            let currency = s(t, "currency");
            let hkd = r2(cost_ccy * rates.rate(&currency, &ym));
            P { date, ym, symbol: s(t, "symbol"), units: qty, px: r2(f(t, "price").unwrap_or(0.0)), currency, cost_ccy, hkd }
        })
        .collect();

    let mut tot_units: HashMap<&str, f64> = HashMap::new();
    let mut tot_cost: HashMap<&str, f64> = HashMap::new();
    for p in &purchases {
        *tot_units.entry(&p.symbol).or_insert(0.0) += p.units;
        *tot_cost.entry(&p.symbol).or_insert(0.0) += p.cost_ccy;
    }
    let avg_cost = |sym: &str| {
        let u = tot_units.get(sym).copied().unwrap_or(0.0);
        if u != 0.0 { tot_cost.get(sym).copied().unwrap_or(0.0) / u } else { 0.0 }
    };

    let months = month_range(month_date(&purchases[0].ym), month_start(ctx.today));
    let mut cum_units: HashMap<&str, f64> = symbols.iter().map(|s| (s.as_str(), 0.0)).collect();
    let mut rows = Vec::new();
    let (mut cum_cost, mut prev_value) = (0.0, 0.0);
    for m in &months {
        let ym = ym_of(*m);
        let buys: Vec<&P> = purchases.iter().filter(|p| p.ym == ym).collect();
        let contrib = buys.iter().fold(0.0, |a, p| a + p.hkd);
        cum_cost += contrib;
        for p in &buys {
            *cum_units.get_mut(p.symbol.as_str()).unwrap() += p.units;
        }
        let nxt = next_month(*m).to_string();
        let mut value = 0.0;
        for sym in &symbols {
            let u = cum_units[sym.as_str()];
            if u == 0.0 {
                continue;
            }
            let h = &hists[sym];
            let unit_px = if h.is_empty() { avg_cost(sym) } else { h.before(&nxt) };
            let ccy = ccy_of.get(sym).map(String::as_str).unwrap_or("USD");
            value += u * unit_px * rates.rate(ccy, &ym);
        }
        rows.push(json!({"month": ym, "contrib": num(r2(contrib)), "cost": num(r2(cum_cost)), "value": num(r2(value)),
                         "gain": num(r2(value - cum_cost)), "market": num(r2(value - prev_value - contrib))}));
        prev_value = value;
    }

    let ym_now = ctx.ym_now();
    let mut sym_rows: Vec<(f64, Value)> = Vec::new();
    for sym in &symbols {
        let units = cum_units[sym.as_str()];
        let cost = tot_cost.get(sym.as_str()).copied().unwrap_or(0.0);
        let h = &hists[sym];
        let ccy = ccy_of.get(sym).cloned().unwrap_or_else(|| "USD".into());
        let (last_close, value, marked) = match h.last_close() {
            Some(c) => (c, units * c, "close"),
            None => (avg_cost(sym), cost, "cost"), // unpriced -> flat at cost
        };
        let value_hkd = r2(value * rates.rate(&ccy, &ym_now));
        sym_rows.push((
            value_hkd,
            json!({"symbol": sym, "currency": ccy, "units": num(round_n(units, 4)),
                   "avg_cost": num(if units != 0.0 { r2(cost / units) } else { 0.0 }),
                   "last_close": num(r2(last_close)), "marked": marked,
                   "cost_ccy": num(r2(cost)), "value_ccy": num(r2(value)), "gain_ccy": num(r2(value - cost)),
                   "roi_pct": num(if cost != 0.0 { r2((value - cost) / cost * 100.0) } else { 0.0 }),
                   "value_hkd": num(value_hkd)}),
        ));
    }
    sym_rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let purchases: Vec<Value> = purchases
        .iter()
        .map(|p| json!({"date": p.date, "ym": p.ym, "symbol": p.symbol, "units": num(p.units), "px": num(p.px),
                        "currency": p.currency, "cost_ccy": num(p.cost_ccy), "hkd": num(p.hkd)}))
        .collect();
    json!({"stale": stale, "rows": rows, "symbols": sym_rows.into_iter().map(|r| r.1).collect::<Vec<_>>(),
           "purchases": purchases})
}

/// One ibkr_nav row.
struct Nav {
    date: String,
    cash: f64,
    nav: f64,
    base_ccy: String,
}

/// Book each external flow on whichever
/// of (its date, the next statement day) shows the matching cash jump.
/// Insertion-ordered (the XIRR sums follow it).
fn book_flows(nav: &[Nav], flows: &[(String, f64)]) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = Vec::new();
    for (fdate, amt) in flows {
        let cands: Vec<usize> = (0..nav.len()).filter(|&i| nav[i].date.as_str() >= fdate.as_str()).take(2).collect();
        let Some(&c0) = cands.first() else { continue };
        let best = if c0 == 0 || cands.len() == 1 {
            c0
        } else {
            let err = |i: usize| ((nav[i].cash - nav[i - 1].cash) - amt).abs();
            // min() keeps the first of equal keys
            if err(cands[1]) < err(cands[0]) { cands[1] } else { cands[0] }
        };
        let d = &nav[best].date;
        match out.iter_mut().find(|(k, _)| k == d) {
            Some((_, v)) => *v += amt,
            None => out.push((d.clone(), *amt)),
        }
    }
    out
}

/// The broker's TWR (daily NAV chained, flows
/// removed), money-weighted XIRR and simple gain vs net deposits. None until
/// a Flex fetch stored daily NAV rows.
pub fn ibkr_performance(conn: &Connection) -> Option<Value> {
    let nav: Vec<Nav> = conn
        .prepare_cached("SELECT date, cash_base, nav_base, base_ccy FROM ibkr_nav ORDER BY date")
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok(Nav {
                    date: get_str(r, 0).unwrap_or_default(),
                    cash: get_f64(r, 1).unwrap_or(0.0),
                    nav: get_f64(r, 2).unwrap_or(0.0),
                    base_ccy: get_str(r, 3).unwrap_or_default(),
                })
            })?
            .collect()
        })
        .unwrap_or_default();
    let flows: Vec<(String, f64)> = conn
        .prepare_cached("SELECT date, amount_base FROM ibkr_cash_flows ORDER BY date")
        .and_then(|mut s| s.query_map([], |r| Ok((get_str(r, 0).unwrap_or_default(), get_f64(r, 1).unwrap_or(0.0))))?.collect())
        .unwrap_or_default();
    if nav.len() < 2 || flows.is_empty() {
        return None;
    }
    let booked = book_flows(&nav, &flows);
    let booked_on = |d: &str| booked.iter().find(|(k, _)| k == d).map(|(_, v)| *v);
    let mut twr = 1.0;
    let mut prev: Option<f64> = None;
    for r in &nav {
        if let Some(p) = prev {
            let base = p + booked_on(&r.date).unwrap_or(0.0);
            if base > 0.0 {
                twr *= r.nav / base;
            }
        }
        prev = Some(r.nav);
    }
    let last = nav.last().unwrap();
    let (end_day, end_nav) = (&last.date, last.nav);
    let net_dep = booked.iter().fold(0.0, |a, (_, v)| a + v);
    let date = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default();
    let mut cf: Vec<(NaiveDate, f64)> = booked.iter().map(|(d, a)| (date(d), -a)).collect();
    cf.push((date(end_day), end_nav));
    let t0 = cf.iter().map(|c| c.0).min().unwrap();
    let npv = |r: f64| cf.iter().fold(0.0, |acc, (d, a)| acc + a / (1.0 + r).powf((*d - t0).num_days() as f64 / 365.0));
    let (mut lo, mut hi) = (-0.95f64, 10.0f64);
    let mut mwr = None;
    if npv(lo) * npv(hi) < 0.0 {
        for _ in 0..200 {
            let mid = (lo + hi) / 2.0;
            if npv(lo) * npv(mid) <= 0.0 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        mwr = Some(((lo + hi) / 2.0) * 100.0);
    }
    let days = (date(end_day) - date(&nav[0].date)).num_days();
    let nav_rows: Vec<Value> = nav
        .iter()
        .map(|r| json!({"date": r.date, "nav": num(r.nav), "cash": num(r.cash),
                        "flow": num(r2(booked_on(&r.date).unwrap_or(0.0)))}))
        .collect();
    let mut m = Map::new();
    m.insert("as_of".into(), json!(end_day));
    m.insert("first_day".into(), json!(nav[0].date));
    m.insert("days".into(), json!(days));
    m.insert("nav_base".into(), num(end_nav));
    m.insert("net_deposits_base".into(), num(r2(net_dep)));
    m.insert("gain_base".into(), num(r2(end_nav - net_dep)));
    m.insert("simple_pct".into(), if net_dep > 0.0 { num(r2((end_nav / net_dep - 1.0) * 100.0)) } else { Value::Null });
    m.insert("twr_pct".into(), num(r2((twr - 1.0) * 100.0)));
    m.insert("mwr_pct".into(), opt_num(mwr.map(r2)));
    m.insert("base_ccy".into(), json!(last.base_ccy));
    m.insert("nav".into(), Value::Array(nav_rows));
    Some(Value::Object(m))
}
