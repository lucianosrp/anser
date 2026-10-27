//! The dashboard-facing views: every get_* as a serde_json::Value in the
//! bundle's shape. These always compute; the memoized versions (keyed on
//! the db fingerprint)
//! are in views::cache, which the server and the bundle call.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::analytics::{self as an, compute_cashflows, compute_growth_rate, compute_pnl, compute_portfolio_pct,
                       compute_rolling_avg};
use super::fx::monthly_rates;
use super::holdings::{ibkr_history, ibkr_market_now_hkd, ibkr_performance, ibkr_snapshot,
                      investment_cost_basis};
use super::sheet::build_balance_sheet;
use super::{get_str, ms_to_ym, num, r2, take_chars, Ctx};

type R = anyhow::Result<Value>;

/// get_month_range: {"min", "max"} 'YYYY-MM' across bank + crypto data.
pub fn get_month_range(ctx: &Ctx) -> R {
    let conn = ctx.conn;
    // MIN/MAX(txn_date) over the rows already in memory (txn_date is NOT
    // NULL, so the plain string min/max is SQLite's)
    let (bmn, bmx) = txn_date_span(ctx);
    let (cmn, cmx): (Option<i64>, Option<i64>) =
        conn.query_row("SELECT MIN(ts) mn, MAX(ts) mx FROM crypto_events", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let (mut mins, mut maxs) = (Vec::new(), Vec::new());
    if let (Some(mn), Some(mx)) = (bmn.filter(|s| !s.is_empty()), bmx) {
        mins.push(take_chars(&mn, 7).to_string());
        maxs.push(take_chars(&mx, 7).to_string());
    }
    if let (Some(mn), Some(mx)) = (cmn, cmx) {
        mins.push(ms_to_ym(mn));
        maxs.push(ms_to_ym(mx));
    }
    if mins.is_empty() {
        let t = ctx.ym_now();
        return Ok(json!({"min": t, "max": t}));
    }
    Ok(json!({"min": mins.iter().min(), "max": maxs.iter().max()}))
}

/// (MIN(txn_date), MAX(txn_date)) of bank_transactions, from ctx.bank().
fn txn_date_span(ctx: &Ctx) -> (Option<String>, Option<String>) {
    let bank = ctx.bank();
    let mn = bank.iter().map(|b| b.txn_date.as_str()).min().map(String::from);
    let mx = bank.iter().map(|b| b.txn_date.as_str()).max().map(String::from);
    (mn, mx)
}

fn freshness(ctx: &Ctx) -> Value {
    let latest: Option<String> =
        txn_date_span(ctx).1.filter(|s| !s.is_empty()).map(|s| take_chars(&s, 7).to_string());
    let fetch: Option<i64> =
        ctx.conn.query_row("SELECT MAX(fetched_at) m FROM crypto_raw_sources", [], |r| r.get(0)).ok().flatten();
    json!({"latest_bank_month": latest, "crypto_last_fetch_ts": fetch})
}

/// get_net_worth_now: the live valuation (live=false: snapshot, no network).
pub fn get_net_worth_now(ctx: &Ctx, live: bool) -> R {
    Ok(if live { super::networth::net_worth_now(ctx, true) } else { ctx.net_worth_offline().clone() })
}

/// get_overview: net worth + the balance sheet + total PnL + freshness.
pub fn get_overview(ctx: &Ctx, from: Option<&str>, to: Option<&str>, live: bool) -> R {
    let net_worth = get_net_worth_now(ctx, live)?;
    let sheet = build_balance_sheet(ctx, from, to);
    let bank_keys: Vec<String> = sheet.columns.iter().filter(|c| !c.no_fx).map(|c| c.key.clone()).collect();
    let pnl: Vec<Value> = compute_pnl(&sheet, &bank_keys)
        .into_iter()
        .map(|r| json!({"month": r["month"], "total_hkd": r["total_hkd"]}))
        .collect();
    Ok(json!({"net_worth": net_worth,
              "sheet": {"columns": sheet.columns_json(), "rows": sheet.rows_json()},
              "pnl": pnl, "freshness": freshness(ctx)}))
}

/// get_receivables: deposits held, the partner's balance, reimbursable spend.
pub fn get_receivables(ctx: &Ctx) -> R {
    let fx = super::fx::current_fx_rates(ctx, false);
    super::receivables::compute(ctx, Some(&fx))
}

/// get_banks: the sheet plus PnL, growth, allocation, cashflows, rolling
/// average and each ledger's last activity.
pub fn get_banks(ctx: &Ctx, from: Option<&str>, to: Option<&str>) -> R {
    let sheet = build_balance_sheet(ctx, from, to);
    let mut pnl_keys: Vec<String> = sheet.columns.iter().filter(|c| !c.no_fx).map(|c| c.key.clone()).collect();
    pnl_keys.push("crypto_hkd".into());
    let mut currency_map: Vec<(String, String)> =
        sheet.columns.iter().map(|c| (c.key.clone(), c.currency.clone())).collect();
    currency_map.push(("crypto_hkd".into(), "CRYPTO".into()));

    let pnl = compute_pnl(&sheet, &pnl_keys);
    let growth = compute_growth_rate(&sheet);
    let portfolio = compute_portfolio_pct(&sheet, &currency_map)?;
    let rolling = compute_rolling_avg(&sheet);
    let rates = (!sheet.is_empty()).then(|| monthly_rates(ctx, sheet.months[0], sheet.months[sheet.len() - 1]));
    let cashflows = compute_cashflows(ctx, rates.as_ref());

    // last activity per ledger: the Banks page's staleness flag. SELECT
    // source, account, MAX(txn_date) ... GROUP BY source, account, over the
    // rows in memory (the GROUP BY's sorter orders by memcmp, as BTreeMap)
    let mut latest: std::collections::BTreeMap<(&str, &str), &str> = std::collections::BTreeMap::new();
    let bank = ctx.bank();
    for b in bank.iter() {
        let e = latest.entry((b.source.as_str(), b.account.as_str())).or_insert(b.txn_date.as_str());
        if b.txn_date.as_str() > *e {
            *e = b.txn_date.as_str();
        }
    }
    let mut last_txn = Map::new();
    for ((source, account), mx) in latest {
        last_txn.insert(super::bank_key(source, account), json!(take_chars(mx, 10)));
    }
    Ok(json!({"sheet": {"columns": sheet.columns_json(), "rows": sheet.rows_json()},
              "pnl": pnl, "growth": growth, "portfolio": portfolio, "cashflows": cashflows,
              "rolling": rolling, "last_txn": last_txn}))
}

/// get_crypto_meta: last exchange fetch + event counts per exchange.
pub fn get_crypto_meta(ctx: &Ctx) -> R {
    let conn = ctx.conn;
    let last: Option<i64> = conn.query_row("SELECT MAX(fetched_at) m FROM crypto_raw_sources", [], |r| r.get(0))?;
    let mut by = Map::new();
    let mut stmt = conn.prepare_cached("SELECT exchange, COUNT(*) c FROM crypto_events GROUP BY exchange")?;
    for (ex, c) in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.flatten() {
        by.insert(ex, json!(c));
    }
    Ok(json!({"last_fetch_ts": last, "events_by_exchange": by}))
}

/// get_spending: monthly income / spend / net (+ fixed vs variable, money
/// invested, crypto bought), category breakdown, top merchants, recurring
/// payments and the fixed merchants.
pub fn get_spending(ctx: &Ctx, from: Option<&str>, to: Option<&str>) -> R {
    let rates = ctx.rates();
    let bank = ctx.bank();
    let flags = ctx.flags();
    let windowed = an::spending_rows(&bank, &flags, &rates, from, to);
    let all = if from.is_none() && to.is_none() { None } else { Some(an::spending_rows(&bank, &flags, &rates, None, None)) };
    let all = all.as_deref().unwrap_or(&windowed);
    let by_cat = an::spending_by_category(&windowed);

    // ym -> (income, spend, invested)
    let mut monthly: std::collections::BTreeMap<&str, (f64, f64, f64)> = std::collections::BTreeMap::new();
    for b in bank.iter() {
        let ym = b.ym();
        if from.is_some_and(|f| ym < f) || to.is_some_and(|t| ym > t) {
            continue;
        }
        let cat = b.cat();
        let mut hkd = b.amount.unwrap_or(0.0) * rates.rate(&b.currency, ym);
        let m = monthly.entry(ym).or_insert((0.0, 0.0, 0.0));
        if flags.is_investment(cat) {
            // money sent to FlexInvest / IBKR: a transfer for income/spend,
            // but THE number for the saved-vs-invested split
            m.2 += -hkd;
        }
        if flags.is_transfer(cat) {
            continue;
        }
        hkd *= b.share1(); // only the owner's part of a shared bill
        if hkd > 0.0 {
            m.0 += hkd;
        } else {
            m.1 += -hkd;
        }
    }
    // crypto buys only exist in the exchange ledger
    let export = ctx.export();
    let crypto_flow: HashMap<String, f64> =
        super::investments::crypto_monthly_flows_hkd(&rates, &export).into_iter().collect();
    let groups = an::merchant_groups(all);
    let mut fixed = an::fixed_merchants(&groups, &flags);
    let fv = an::fixed_variable_by_month(all, &fixed);
    let trend: Vec<Value> = monthly
        .iter()
        .map(|(m, (inc, sp, inv))| {
            let (f, v) = fv.get(*m).copied().unwrap_or((0.0, 0.0));
            json!({"month": m, "income": num(r2(*inc)), "spend": num(r2(*sp)), "net": num(r2(inc - sp)),
                   "fixed": num(r2(f)), "variable": num(r2(v)), "invested": num(r2(*inv)),
                   "invested_crypto": num(r2(crypto_flow.get(*m).copied().unwrap_or(0.0)))})
        })
        .collect();
    fixed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(json!({"trend": trend, "by_category": by_cat, "top": an::top_spending(&windowed, 12),
              "recurring": an::recurring_payments(&groups),
              "fixed_merchants": fixed.into_iter().map(|f| f.2).collect::<Vec<_>>()}))
}

/// get_flexinvest: the fund page (flexinvest_history of the first
/// proxy-marked account, invest_setup), rows range-filtered.
pub fn get_flexinvest(ctx: &Ctx, from: Option<&str>, to: Option<&str>) -> R {
    let holding = ctx.invest().fund_account().map(|a| a.name.clone()).unwrap_or_default();
    let mut data = super::holdings::flexinvest_history(ctx, &holding, true);
    if let Some(rows) = data.get_mut("rows").and_then(Value::as_array_mut) {
        rows.retain(|r| {
            let m = r["month"].as_str().unwrap_or("");
            from.is_none_or(|f| m >= f) && to.is_none_or(|t| m <= t)
        });
    }
    Ok(data)
}

/// get_investments: the combined Investments page.
pub fn get_investments(ctx: &Ctx, include_crypto: bool) -> R {
    Ok(super::investments::combined_history(ctx, include_crypto, true))
}

/// get_costs: the Investments page's Costs section. `investments` reuses an
/// already computed get_investments(true) (a cache hit).
pub fn get_costs(ctx: &Ctx, live: bool, investments: Option<&Value>) -> R {
    let owned;
    let inv = match investments {
        Some(v) => v,
        None => {
            owned = get_investments(ctx, true)?;
            &owned
        }
    };
    let platform = ctx.invest().platform().map(|a| a.name.clone());
    let flex = inv["classes"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| platform.as_deref().is_some_and(|p| c["key"] == p)))
        .and_then(|c| c["value"].as_f64())
        .unwrap_or(0.0);
    let cash = ctx.net_worth_offline()["banks_hkd"].as_f64().unwrap_or(0.0);
    Ok(super::costs::investment_costs(ctx, flex, cash, live))
}

/// get_ibkr: broker snapshot, every trade, monthly buys per symbol (base
/// currency), the history at real fills, HKD hero figures, performance.
pub fn get_ibkr(ctx: &Ctx) -> R {
    let conn = ctx.conn;
    let snap = ibkr_snapshot(conn);
    let trades = ctx.ibkr_trades();
    let history = ibkr_history(ctx, true);

    let mut monthly: std::collections::BTreeMap<String, Vec<(String, f64)>> = std::collections::BTreeMap::new();
    let mut symbols: Vec<String> = Vec::new();
    for t in trades.iter() {
        let qty = t["qty"].as_f64().unwrap_or(0.0);
        if qty <= 0.0 {
            continue; // the DCA chart shows buys; sells still appear in the table
        }
        let fx = t["fx_to_base"].as_f64().filter(|v| *v != 0.0).unwrap_or(1.0);
        let mut spent = -t["amount"].as_f64().unwrap_or(0.0) * fx;
        if spent <= 0.0 {
            spent = qty * t["price"].as_f64().unwrap_or(0.0) * fx; // netCash missing -> reconstruct
        }
        let sym = t["symbol"].as_str().unwrap_or("").to_string();
        let bucket = monthly.entry(ms_to_ym(t["ts"].as_i64().unwrap_or(0))).or_default();
        match bucket.iter_mut().find(|(s, _)| *s == sym) {
            Some((_, v)) => *v += spent,
            None => bucket.push((sym.clone(), spent)),
        }
        if !symbols.contains(&sym) {
            symbols.push(sym);
        }
    }
    let months: Vec<&String> = monthly.keys().collect();
    let mut series = Map::new();
    for s in &symbols {
        let v: Vec<Value> = monthly
            .values()
            .map(|b| num(r2(b.iter().find(|(k, _)| k == s).map(|(_, v)| *v).unwrap_or(0.0))))
            .collect();
        series.insert(s.clone(), Value::Array(v));
    }
    let buys = json!({"months": months, "symbols": symbols, "series": series});

    let rates = ctx.rates();
    let cost_hkd = investment_cost_basis(ctx, Some(&ctx.invest().ibkr_name()));
    let value_hkd = ibkr_market_now_hkd(ctx, &rates).unwrap_or(0.0);
    let has = snap.is_some() || !trades.is_empty();
    Ok(json!({"snapshot": snap.as_ref().map(|s| s.json()), "trades": &*trades, "buys": buys, "history": history,
              "cost_hkd": num(cost_hkd), "value_hkd": num(value_hkd),
              "gain_hkd": num(if has { r2(value_hkd - cost_hkd) } else { 0.0 }),
              "performance": ibkr_performance(conn)}))
}

/// get_ingest_log: recently ingested files (latest per path) and any file in
/// transactions/ no source recognized.
pub fn get_ingest_log(ctx: &Ctx) -> R {
    let conn = ctx.conn;
    let label = |key: &str| super::spec(key).map(|s| s.label.to_string()).unwrap_or_else(|| key.to_string());
    let mut stmt = conn.prepare_cached(
        "SELECT path, source, row_count, rows_added, MAX(ingested_at) ingested_at \
         FROM ingested_files GROUP BY path ORDER BY ingested_at DESC LIMIT 40",
    )?;
    let files: Vec<Value> = stmt
        .query_map([], |r| {
            let path = get_str(r, 0).unwrap_or_default();
            let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            Ok(json!({"name": name, "source": label(&get_str(r, 1).unwrap_or_default()),
                      "rows": super::value_ref_json(r.get_ref_unwrap(2)),
                      "rows_added": super::value_ref_json(r.get_ref_unwrap(3)),
                      "ingested_at": super::value_ref_json(r.get_ref_unwrap(4))}))
        })?
        .flatten()
        .collect();
    let mut known_stmt = conn.prepare_cached("SELECT DISTINCT path FROM ingested_files")?;
    let known: std::collections::HashSet<String> = known_stmt.query_map([], |r| r.get(0))?.flatten().collect();
    let dir = crate::config::transactions_dir();
    let mut unrecognized: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let ok_ext = [".csv", ".xml", ".pdf"].iter().any(|x| name.ends_with(x));
            if ok_ext && !known.contains(&*dir.join(&name).to_string_lossy()) {
                unrecognized.push(name);
            }
        }
    }
    unrecognized.sort();
    Ok(json!({"files": files, "unrecognized": unrecognized}))
}

/// get_month_detail: one month's full drill-down (analytics.month_breakdown).
pub fn get_month_detail(ctx: &Ctx, month: &str) -> R {
    Ok(an::month_breakdown(ctx.conn, month))
}
