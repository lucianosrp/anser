//! Combined investments: FlexInvest, IBKR,
//! other holdings and the crypto ledger as ONE HKD story of net money in vs
//! market value, the annualized money-weighted return, a time-weighted
//! growth series, and the same flows bought into the S&P 500 (^GSPC) as a
//! benchmark (the valuation model: desktop/server/README.md, Valuation and receivables).

use std::collections::HashMap;

use chrono::NaiveDate;
use serde_json::{json, Map, Value};

use super::crypto::{cash_flow_usd, compute, Export, MarketRow, ASSETS};
use super::fx::Rates;
use super::holdings::{ibkr_performance, investments_hkd_by_holding, investment_flows};
use super::prices::{equity_price_hist, live_or_cached_prices};
use super::{month_date, ms_to_ym, next_month, num, opt_num, r2, round_n, ym_of, Ctx};

pub const CRYPTO: &str = "Crypto";

/// irr_annual_pct: money-weighted annualized return in %: the monthly r
/// solving sum(flow_i * (1+r)^(n-i)) == final_value (bisection on
/// [-50%, +50%]), then (1+r)^12 - 1. None when undefined.
pub fn irr_annual_pct(flows: &[f64], final_value: f64) -> Option<f64> {
    if flows.len() < 2 || flows.iter().filter(|f| **f > 0.0).fold(0.0, |a, f| a + f) <= 0.0 || final_value <= 0.0 {
        return None;
    }
    let n = flows.len() - 1;
    // powf, not powi: the stored figures were computed with pow()
    let f = |r: f64| flows.iter().enumerate().fold(0.0, |a, (i, fl)| a + fl * (1.0 + r).powf((n - i) as f64)) - final_value;
    let (mut lo, mut hi) = (-0.5f64, 0.5f64);
    let mut flo = f(lo);
    let fhi = f(hi);
    if flo * fhi > 0.0 {
        return None;
    }
    for _ in 0..80 {
        let mid = (lo + hi) / 2.0;
        let fm = f(mid);
        if flo * fm <= 0.0 {
            hi = mid;
        } else {
            lo = mid;
            flo = fm;
        }
    }
    let r = (lo + hi) / 2.0;
    Some(((1.0 + r).powf(12.0) - 1.0) * 100.0)
}

/// crypto_monthly_flows_hkd: net cash put into crypto per month, HKD (every
/// buy + deposits from elsewhere at cost - sells). Insertion-ordered.
pub fn crypto_monthly_flows_hkd(rates: &Rates, export: &Export) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = Vec::new();
    let mut idx: HashMap<String, usize> = HashMap::new();
    for e in &export.events {
        if !ASSETS.contains(&e.asset.as_str()) {
            continue;
        }
        let cf = cash_flow_usd(&e.kind, e.delta, e.usd);
        if cf != 0.0 {
            let ym = ms_to_ym(e.ts);
            let v = cf * rates.rate("USD", &ym);
            match idx.get(&ym) {
                Some(&i) => out[i].1 += v,
                None => {
                    idx.insert(ym.clone(), out.len());
                    out.push((ym, v));
                }
            }
        }
    }
    out
}

/// Modified Dietz monthly return, flows mid-month; None with no capital.
fn dietz(v0: f64, v1: f64, flow: f64) -> Option<f64> {
    let base = v0 + flow / 2.0;
    (base > 1.0).then(|| (v1 - v0 - flow) / base)
}

/// _ibkr_monthly_returns: {ym: return} chained from the broker's daily NAV
/// (flows removed), in HKD.
fn ibkr_monthly_returns(ctx: &Ctx, rates: &Rates) -> HashMap<String, f64> {
    let Some(perf) = ibkr_performance(ctx.conn) else { return HashMap::new() };
    let base_ccy = perf["base_ccy"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| rates.base().to_string());
    let mut growth: Vec<(String, f64)> = Vec::new();
    let mut prev: Option<f64> = None;
    for r in perf["nav"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let ym = r["date"].as_str().unwrap_or("")[..7].to_string();
        let nav = r["nav"].as_f64().unwrap_or(0.0);
        if let Some(p) = prev {
            let base = p + r["flow"].as_f64().unwrap_or(0.0);
            if base > 0.0 {
                match growth.iter_mut().find(|(k, _)| *k == ym) {
                    Some((_, g)) => *g *= nav / base,
                    None => growth.push((ym, nav / base)),
                }
            }
        }
        prev = Some(nav);
    }
    growth
        .into_iter()
        .map(|(ym, g)| {
            let (y, m): (i32, u32) = (ym[..4].parse().unwrap_or(0), ym[5..7].parse().unwrap_or(1));
            let pym = format!("{:04}-{:02}", y - (m == 1) as i32, (m + 10) % 12 + 1);
            let (fx0, fx1) = (rates.rate(&base_ccy, &pym), rates.rate(&base_ccy, &ym));
            let v = g * (if fx0 > 0.0 { fx1 / fx0 } else { 1.0 }) - 1.0;
            (ym, v)
        })
        .collect()
}

/// time_weighted: cumulative TWR % per month (capital-weighted average of
/// each class's own monthly return: crypto from the transfer-proof market
/// rows, IBKR from the broker NAV, the rest modified Dietz on their own
/// value), plus per month whether any capital was at work.
#[allow(clippy::too_many_arguments)]
fn time_weighted(
    yms: &[String],
    classes: &[String],
    values: &HashMap<String, Vec<f64>>,
    flows: &HashMap<String, HashMap<String, f64>>,
    crypto_market: &HashMap<String, &MarketRow>,
    ibkr_returns: &HashMap<String, f64>,
    ibkr: &str,
    rates: &Rates,
) -> (Vec<f64>, Vec<bool>) {
    let mut growth = 1.0;
    let (mut out, mut active) = (Vec::new(), Vec::new());
    for (i, ym) in yms.iter().enumerate() {
        let (mut num_, mut den) = (0.0, 0.0);
        if i > 0 {
            let pym = &yms[i - 1];
            for h in classes {
                let f = flows.get(h).and_then(|m| m.get(ym)).copied().unwrap_or(0.0);
                let (v0, r);
                if h == CRYPTO && crypto_market.contains_key(ym) {
                    let mr = crypto_market[ym];
                    v0 = mr.v0 * rates.rate("USD", pym);
                    r = if mr.priced { dietz(v0, mr.v1_traded * rates.rate("USD", ym), f) } else { None };
                } else {
                    let series = values.get(h);
                    v0 = series.map(|s| s[i - 1]).unwrap_or(0.0);
                    let mut rr = series.and_then(|s| dietz(v0, s[i], f));
                    if h == ibkr {
                        if let Some(x) = ibkr_returns.get(ym) {
                            rr = Some(*x);
                        }
                    }
                    r = rr;
                }
                let Some(r) = r.filter(|r| *r > -1.0) else { continue }; // unmeasurable
                let w = v0 + f / 2.0;
                if w > 1.0 {
                    num_ += w * r;
                    den += w;
                }
            }
        }
        if den > 0.0 {
            growth *= 1.0 + num_ / den;
        }
        out.push(round_n((growth - 1.0) * 100.0, 3));
        active.push(den > 0.0);
    }
    (out, active)
}

/// investments.combined_history.
pub fn combined_history(ctx: &Ctx, include_crypto: bool, live: bool) -> Value {
    let rates = ctx.rates();
    let cur_ym = ctx.ym_now();

    // --- net money in, per class per month (HKD), first-seen class order ---
    let mut flow_order: Vec<String> = Vec::new();
    let mut flows: HashMap<String, HashMap<String, f64>> = HashMap::new();
    // each class's months in first-seen order: the sum runs in that order,
    // and a different float order can flip a zero gain's sign
    let mut flow_yms: HashMap<String, Vec<String>> = HashMap::new();
    let mut add_flow = |h: &str, ym: &str, v: f64, flows: &mut HashMap<String, HashMap<String, f64>>| {
        if !flows.contains_key(h) {
            flow_order.push(h.to_string());
        }
        let m = flows.entry(h.to_string()).or_default();
        if !m.contains_key(ym) {
            flow_yms.entry(h.to_string()).or_default().push(ym.to_string());
        }
        *m.entry(ym.to_string()).or_insert(0.0) += v;
    };
    for c in investment_flows(ctx, &rates, None, false) {
        add_flow(&c.holding, &c.ym, c.hkd, &mut flows);
    }
    let mut crypto_value_usd: Vec<(String, f64)> = Vec::new();
    let mut crypto_stale = false;
    let mut comp = None;
    if include_crypto {
        let export = ctx.export();
        for (ym, hkd) in crypto_monthly_flows_hkd(&rates, &export) {
            add_flow(CRYPTO, &ym, hkd, &mut flows);
        }
        let (px, stale, _) = live_or_cached_prices(ctx, &ASSETS);
        crypto_stale = stale;
        comp = compute(&export, &px, &cur_ym);
        if let Some(c) = &comp {
            crypto_value_usd = c.rows.iter().map(|r| (r.month.clone(), r.total_value.unwrap_or(0.0))).collect();
        }
    }

    let mut starts: Vec<&String> = flows.values().filter_map(|d| d.keys().min()).collect();
    if let Some(m) = crypto_value_usd.iter().map(|(m, _)| m).min() {
        starts.push(m);
    }
    let Some(first) = starts.into_iter().min().cloned() else {
        return json!({"empty": true, "include_crypto": include_crypto});
    };
    let mut months: Vec<NaiveDate> = Vec::new();
    let (mut m, end) = (month_date(&first), month_date(&cur_ym));
    while m <= end {
        months.push(m);
        m = next_month(m);
    }
    let yms: Vec<String> = months.iter().map(|m| ym_of(*m)).collect();
    let n = yms.len();

    // --- market value per class per month (HKD) ---
    let mut values: HashMap<String, Vec<f64>> = investments_hkd_by_holding(ctx, &months).into_iter().collect();
    if include_crypto && !crypto_value_usd.is_empty() {
        let by: HashMap<&str, f64> = crypto_value_usd.iter().map(|(m, v)| (m.as_str(), *v)).collect();
        values.insert(CRYPTO.into(),
                      yms.iter().map(|ym| by.get(ym.as_str()).copied().unwrap_or(0.0) * rates.rate("USD", ym)).collect());
    }

    let mut classes: Vec<String> = flow_order
        .iter()
        .filter(|h| flows[*h].values().any(|v| *v != 0.0))
        .cloned()
        .collect();
    for h in values.keys() {
        if !classes.contains(h) {
            classes.push(h.clone());
        }
    }
    // display order: the marked accounts (fund, broker), then anything
    // else, crypto last (invest_setup::Setup::order)
    let setup = ctx.invest();
    classes.sort_by(|a, b| (setup.order(a, CRYPTO), a).cmp(&(setup.order(b, CRYPTO), b)));
    let flow_total: Vec<f64> = yms
        .iter()
        .map(|ym| classes.iter().fold(0.0, |a, h| a + flows.get(h).and_then(|m| m.get(ym)).copied().unwrap_or(0.0)))
        .collect();
    let mut invested_total = Vec::with_capacity(n);
    let mut run = 0.0;
    for f in &flow_total {
        run += f;
        invested_total.push(run);
    }
    let value_total: Vec<f64> =
        (0..n).map(|i| classes.iter().fold(0.0, |a, h| a + values.get(h).map(|v| v[i]).unwrap_or(0.0))).collect();

    // --- time-weighted growth ---
    let crypto_market: HashMap<String, &MarketRow> = match (&comp, include_crypto && !crypto_value_usd.is_empty()) {
        (Some(c), true) => c.market_rows.iter().map(|r| (r.month.clone(), r)).collect(),
        _ => HashMap::new(),
    };
    let ibkr_ret = if classes.iter().any(|h| setup.is_ibkr(h)) { ibkr_monthly_returns(ctx, &rates) } else { HashMap::new() };
    let (twr, twr_active) = time_weighted(&yms, &classes, &values, &flows, &crypto_market, &ibkr_ret, &setup.ibkr_name(), &rates);

    // --- S&P 500 counterfactual: same flows buy ^GSPC mid-month ---
    let mut spx = Value::Null;
    let mut spx_series: Vec<Option<f64>> = vec![None; n];
    let mut spx_twr: Vec<Option<f64>> = vec![None; n];
    // the profile's benchmark (the S&P 500 unless it says otherwise)
    let benchmark = crate::invest_setup::benchmark(ctx.conn);
    let (hist, spx_stale) = equity_price_hist(ctx, &benchmark, live, None, "10y");
    if !hist.is_empty() {
        let close_on_or_before = |day: &str| hist.on_or_before(day);
        let mut units = 0.0;
        let mut series: Vec<f64> = Vec::with_capacity(n);
        let (mut spx_growth, mut prev_mark) = (1.0, 0.0);
        for (i, (m, ym)) in months.iter().zip(&yms).enumerate() {
            let usdhkd = rates.rate("USD", ym);
            let buy_px = close_on_or_before(&format!("{ym}-15"));
            let mark_px = close_on_or_before(&(next_month(*m) - chrono::Duration::days(1)).to_string());
            let (units0, mut du) = (units, 0.0);
            if usdhkd > 0.0 && buy_px > 0.0 {
                du = (flow_total[i] / usdhkd) / buy_px;
                units += du;
            }
            let mark = if mark_px > 0.0 { mark_px * usdhkd } else { prev_mark };
            let cap = units0 * prev_mark + du * buy_px * usdhkd;
            if cap > 1.0 && mark > 0.0 && twr_active[i] {
                spx_growth *= 1.0 + (units0 * (mark - prev_mark) + du * (mark - buy_px * usdhkd)) / cap;
            }
            spx_twr[i] = Some(round_n((spx_growth - 1.0) * 100.0, 3));
            prev_mark = mark;
            let v = if mark_px > 0.0 { units * mark_px * usdhkd } else { series.last().copied().unwrap_or(0.0) };
            series.push(v);
        }
        let last = *series.last().unwrap();
        spx_series = series.iter().map(|v| Some(*v)).collect();
        let inv = invested_total[n - 1];
        spx = json!({"value": num(r2(last)), "gain": num(r2(last - inv)),
                     "roi_pct": num(if inv > 0.0 { r2((last - inv) / inv * 100.0) } else { 0.0 }),
                     "irr_pct": opt_num(irr_annual_pct(&flow_total, last)),
                     "last_close_date": hist.last_date(), "stale": spx_stale});
    }

    let (invested, value) = (invested_total[n - 1], value_total[n - 1]);
    let gain = value - invested;
    let totals = json!({"invested": num(r2(invested)), "value": num(r2(value)), "gain": num(r2(gain)),
                        "roi_pct": num(if invested > 0.0 { r2(gain / invested * 100.0) } else { 0.0 }),
                        "irr_pct": opt_num(irr_annual_pct(&flow_total, value)),
                        "first_month": yms[0], "months": n});

    let class_rows: Vec<Value> = classes
        .iter()
        .map(|h| {
            let c_in = match (flows.get(h), flow_yms.get(h)) {
                (Some(m), Some(order)) => order.iter().fold(0.0, |a, ym| a + m[ym]),
                _ => 0.0,
            };
            let c_val = values.get(h).map(|v| v[n - 1]).unwrap_or(c_in);
            json!({"key": h, "label": h, "invested": num(r2(c_in)), "value": num(r2(c_val)),
                   "gain": num(r2(c_val - c_in)),
                   "roi_pct": num(if c_in > 0.0 { r2((c_val - c_in) / c_in * 100.0) } else { 0.0 }),
                   "share_pct": num(if value > 0.0 { round_n(c_val / value * 100.0, 1) } else { 0.0 })})
        })
        .collect();

    let rows: Vec<Value> = yms
        .iter()
        .enumerate()
        .map(|(i, ym)| {
            let mut cls = Map::new();
            for h in &classes {
                if let Some(v) = values.get(h) {
                    cls.insert(h.clone(), num(r2(v[i])));
                }
            }
            json!({"month": ym, "flow": num(r2(flow_total[i])), "invested": num(r2(invested_total[i])),
                   "value": num(r2(value_total[i])), "spx_value": opt_num(spx_series[i].map(r2)),
                   "twr_pct": num(twr[i]), "spx_twr_pct": opt_num(spx_twr[i]), "classes": cls})
        })
        .collect();
    json!({"empty": false, "include_crypto": include_crypto, "rows": rows, "classes": class_rows,
           "totals": totals, "spx": spx, "crypto_prices_stale": crypto_stale})
}
