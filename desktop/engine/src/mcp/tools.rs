//! The eight tools (assets/mcp_tools.json has their schemas): summary,
//! trend, breakdown, transactions, recurring, holdings, performance,
//! activity.
//!
//! Generic on purpose: nothing names a bank or broker, every account,
//! category and investment account comes from the user's own data (summary
//! lists the exact names the filters take). Outputs are token-lean: the base
//! currency once at the top level, money to 2 decimals, percentages to 1,
//! empty fields left out.
//!
//! No financial math is invented here. Net worth, the monthly balance
//! sheet, investment returns (time-weighted, money-weighted), fees and cash
//! drag come from the views (through the same fingerprint cache the pages
//! use), subscriptions and DCA from finance_core. What this module adds is
//! one definition of income and spending (every non-transfer bank row, the
//! owner's share, at the month's rate: the spending view's trend) so that
//! summary, trend and breakdown always tie out.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use chrono::NaiveDate;
use serde_json::{json, Map, Value};

use super::Source;
use crate::views::{analytics as an, bank_key, cache, capabilities, fx, investments::irr_annual_pct, month_date, month_range, num,
                   r2, round_n, source_label, ym_of, Ctx};

pub fn call(src: &dyn Source, name: &str, args: &Map<String, Value>) -> Result<Value> {
    let env = Env::new(src);
    let a = Args(args);
    match name {
        "summary" => summary(&env),
        "trend" => trend(&env, &a),
        "breakdown" => breakdown(&env, &a),
        "transactions" => transactions(&env, &a),
        "recurring" => recurring(&env),
        "holdings" => holdings(&env, &a),
        "performance" => performance(&env, &a),
        "activity" => activity(&env, &a),
        _ => bail!("Unknown tool: '{name}'"),
    }
}

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

struct Env<'a> {
    src: &'a dyn Source,
    ctx: Ctx<'a>,
    base: String,
}

impl<'a> Env<'a> {
    fn new(src: &'a dyn Source) -> Self {
        Env { src, ctx: Ctx::new(src.conn(), src.feed()), base: fx::base_currency(src.conn()) }
    }

    /// 'YYYY-MM' of the last complete month.
    fn last_complete(&self) -> String {
        shift(&self.ctx.ym_now(), -1)
    }

    /// The first and last month with bank or crypto data.
    fn data_range(&self) -> (String, String) {
        match cache::month_range(&self.ctx) {
            Ok(v) => (v["min"].as_str().unwrap_or("").to_string(), v["max"].as_str().unwrap_or("").to_string()),
            Err(_) => (self.ctx.ym_now(), self.ctx.ym_now()),
        }
    }
}

struct Args<'a>(&'a Map<String, Value>);

impl Args<'_> {
    fn str(&self, k: &str) -> Option<&str> {
        self.0.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
    }

    fn int(&self, k: &str) -> Option<i64> {
        match self.0.get(k) {
            Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|x| x as i64)),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        }
    }

    fn f64(&self, k: &str) -> Option<f64> {
        match self.0.get(k) {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        }
    }

    fn month(&self, k: &str) -> Result<Option<String>> {
        match self.str(k) {
            Some(s) if ym_ok(s) => Ok(Some(s.to_string())),
            Some(s) => bail!("{k} must be YYYY-MM, got '{s}'"),
            None => Ok(None),
        }
    }

    fn date(&self, k: &str) -> Result<Option<String>> {
        match self.str(k) {
            Some(s) if NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() => Ok(Some(s.to_string())),
            Some(s) => bail!("{k} must be YYYY-MM-DD, got '{s}'"),
            None => Ok(None),
        }
    }
}

fn ym_ok(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7 && b[4] == b'-' && b[..4].iter().all(u8::is_ascii_digit) && matches!(&s[5..], "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12")
}

/// 'YYYY-MM' moved by `n` months.
fn shift(ym: &str, n: i32) -> String {
    let d = month_date(ym);
    let i = chrono::Datelike::year(&d) * 12 + chrono::Datelike::month(&d) as i32 - 1 + n;
    format!("{:04}-{:02}", i.div_euclid(12), i.rem_euclid(12) + 1)
}

/// Every month from `from` to `to`, inclusive.
fn months_of(from: &str, to: &str) -> Vec<String> {
    month_range(month_date(from), month_date(to)).into_iter().map(ym_of).collect()
}

/// Money: 2 decimals, never "-0.0".
fn m(x: f64) -> Value {
    let v = r2(x);
    num(if v == 0.0 { 0.0 } else { v })
}

/// A percentage: 1 decimal.
fn p(x: f64) -> Value {
    let v = round_n(x, 1);
    num(if v == 0.0 { 0.0 } else { v })
}

fn opt(map: &mut Map<String, Value>, k: &str, v: Option<Value>) {
    if let Some(v) = v {
        map.insert(k.into(), v);
    }
}

/// `given` against `options`: exactly (any case), else the one option that
/// contains it. The error lists what is valid.
fn pick(what: &str, given: &str, options: &[String]) -> Result<String> {
    let g = given.trim().to_lowercase();
    if let Some(o) = options.iter().find(|o| o.to_lowercase() == g) {
        return Ok(o.clone());
    }
    let near: Vec<&String> = options.iter().filter(|o| o.to_lowercase().contains(&g)).collect();
    if let [one] = near.as_slice() {
        return Ok((*one).clone());
    }
    let shown: Vec<&str> = options.iter().map(String::as_str).take(40).collect();
    bail!("Unknown {what} '{given}'. Known: {}{}", shown.join(", "), if options.len() > 40 { ", ..." } else { "" })
}

/// The period of a month-range tool: the arguments, else the 12 complete
/// months to `to_default`, clipped to the months the data covers.
fn period(env: &Env, a: &Args, to_default: Option<String>) -> Result<(String, String)> {
    let (min, max) = env.data_range();
    let explicit_to = a.month("to_month")?;
    let default_to = to_default.unwrap_or_else(|| env.last_complete());
    let to = explicit_to.clone().unwrap_or_else(|| default_to.clone().min(max.clone().max(min.clone())));
    let from = match a.month("from_month")? {
        Some(f) => f,
        None => shift(&to, -11).max(min.clone()).min(to.clone()),
    };
    if from > to {
        bail!("from_month {from} is after to_month {to}");
    }
    Ok((from, to))
}

// ---------------------------------------------------------------------------
// income and spending: the one definition
// ---------------------------------------------------------------------------

/// One bank row that counts as income or spending: not a transfer between
/// own accounts, the owner's share, at the month's rate (base currency,
/// signed: + in, - out).
struct Flow {
    ym: String,
    amount: f64,
    cat: String,
    desc: String,
    ccy: String,
    account: String,
}

fn flows(env: &Env) -> Arc<Vec<Flow>> {
    cache::shared(&env.ctx, "mcp_flows", "", || {
        let (bank, flags, rates) = (env.ctx.bank(), env.ctx.flags(), env.ctx.rates());
        bank.iter()
            .filter_map(|b| {
                let cat = b.cat();
                if flags.is_transfer(cat) {
                    return None;
                }
                let ym = b.ym();
                Some(Flow {
                    ym: ym.to_string(),
                    amount: b.amount? * rates.rate(&b.currency, ym) * b.share1(),
                    cat: cat.to_string(),
                    desc: b.description.clone(),
                    ccy: b.currency.clone(),
                    account: source_label(&b.source, &b.account),
                })
            })
            .collect()
    })
}

/// (income, spending) per month of [from, to], every month present.
fn monthly(flows: &[Flow], from: &str, to: &str) -> BTreeMap<String, (f64, f64)> {
    let mut out: BTreeMap<String, (f64, f64)> = months_of(from, to).into_iter().map(|m| (m, (0.0, 0.0))).collect();
    for f in flows {
        if let Some(e) = out.get_mut(&f.ym) {
            if f.amount > 0.0 {
                e.0 += f.amount;
            } else {
                e.1 += -f.amount;
            }
        }
    }
    out
}

/// income, spending, savings and the rate, for a total.
fn flow_totals(income: f64, spending: f64) -> Map<String, Value> {
    let mut o = Map::new();
    o.insert("income".into(), m(income));
    o.insert("spending".into(), m(spending));
    o.insert("savings".into(), m(income - spending));
    if income > 0.0 {
        o.insert("savings_rate".into(), p((income - spending) / income * 100.0));
    }
    o
}

// ---------------------------------------------------------------------------
// summary
// ---------------------------------------------------------------------------

fn summary(env: &Env) -> Result<Value> {
    let nw = env.src.net_worth();
    let f = |k: &str| nw.get(k).and_then(Value::as_f64);
    let mut net = Map::new();
    net.insert("total".into(), m(f("total_hkd").unwrap_or(0.0)));
    for (k, key) in [("cash", "banks_hkd"), ("investments", "investments_hkd"), ("crypto", "crypto_hkd"), ("owed_to_you", "receivables_hkd")] {
        net.insert(k.into(), m(f(key).unwrap_or(0.0)));
    }
    for (k, key) in [("property", "property_hkd"), ("liabilities", "liabilities_hkd")] {
        opt(&mut net, k, f(key).map(m));
    }

    let today = env.ctx.today;
    let mut freshness = Map::new();
    freshness.insert("prices_stale".into(), json!(nw.get("stale").and_then(Value::as_bool).unwrap_or(true)));
    let caps = capabilities::capabilities(&env.ctx);
    let stale: Vec<Value> = caps["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["has_data"] == true)
        .filter_map(|s| {
            let last = s["last"].as_str()?;
            let d = NaiveDate::parse_from_str(last.get(..10)?, "%Y-%m-%d").ok()?;
            ((today - d).num_days() > 45).then(|| json!({"name": s["label"], "last_data": last}))
        })
        .collect();
    if !stale.is_empty() {
        freshness.insert("stale_sources".into(), Value::Array(stale));
    }

    let all = flows(env);
    let (min, max) = env.data_range();
    let this = env.ctx.ym_now();
    let (mi, ms) = monthly(&all, &this, &this).get(&this).copied().unwrap_or((0.0, 0.0));
    let mut this_month = Map::new();
    this_month.insert("month".into(), json!(this));
    this_month.extend(flow_totals(mi, ms));

    let to = env.last_complete();
    let from = shift(&to, -11).max(min.clone());
    let series = monthly(&all, &from, &to);
    let (ti, ts) = series.values().fold((0.0, 0.0), |a, v| (a.0 + v.0, a.1 + v.1));
    let n = series.len().max(1);
    let mut trailing = Map::new();
    trailing.insert("from".into(), json!(from));
    trailing.insert("to".into(), json!(to));
    trailing.extend(flow_totals(ti, ts));
    trailing.insert("avg_monthly_spending".into(), m(ts / n as f64));

    // the names the filters take
    let bank = env.ctx.bank();
    let flags = env.ctx.flags();
    let accounts: BTreeSet<String> = bank.iter().map(|b| source_label(&b.source, &b.account)).collect();
    let mut by_cat: HashMap<&str, f64> = HashMap::new();
    for b in bank.iter() {
        *by_cat.entry(b.cat()).or_default() += b.amount.unwrap_or(0.0).abs();
    }
    let mut cats: Vec<(&str, f64)> = by_cat.into_iter().collect();
    cats.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
    let categories: Vec<&str> = cats.iter().filter(|(c, _)| !flags.is_transfer(c)).map(|(c, _)| *c).collect();
    let transfer_categories: Vec<&str> = cats.iter().filter(|(c, _)| flags.is_transfer(c)).map(|(c, _)| *c).collect();
    let mut investment_accounts: Vec<String> = nw["investments_holdings"].as_array().into_iter().flatten().filter_map(|h| h["holding"].as_str()).map(String::from).collect();
    if f("crypto_hkd").unwrap_or(0.0) > 0.0 {
        investment_accounts.push(CRYPTO.into());
    }

    let mut out = Map::new();
    out.insert("currency".into(), json!(env.base));
    opt(&mut out, "as_of", nw["as_of"].as_str().map(|s| json!(crate::views::take_chars(s, 16))));
    out.insert("freshness".into(), Value::Object(freshness));
    out.insert("net_worth".into(), Value::Object(net));
    out.insert("this_month".into(), Value::Object(this_month));
    out.insert("trailing_12_months".into(), Value::Object(trailing));
    out.insert("accounts".into(), json!(accounts));
    out.insert("categories".into(), json!(categories));
    if !transfer_categories.is_empty() {
        out.insert("transfer_categories".into(), json!(transfer_categories));
    }
    if !investment_accounts.is_empty() {
        out.insert("investment_accounts".into(), json!(investment_accounts));
    }
    out.insert("months".into(), json!({"from": min, "to": max}));
    Ok(Value::Object(out))
}

/// The crypto class's name in the investments views.
const CRYPTO: &str = crate::views::investments::CRYPTO;

// ---------------------------------------------------------------------------
// trend
// ---------------------------------------------------------------------------

fn trend(env: &Env, a: &Args) -> Result<Value> {
    let metric = a.str("metric").ok_or_else(|| anyhow!("metric is required"))?;
    let (kind, name) = match metric.split_once(':') {
        Some((k, n)) => (k.trim().to_lowercase(), Some(n.trim())),
        None => (metric.to_lowercase(), None),
    };
    let point_in_time = kind == "net_worth" || kind == "account";
    let (from, to) = period(env, a, point_in_time.then(|| env.ctx.ym_now()))?;
    let months = months_of(&from, &to);
    let mut percent = false;
    let values: Vec<Option<f64>> = match (kind.as_str(), name) {
        ("net_worth", None) => sheet_series(env, "total_hkd", &months)?,
        ("account", Some(n)) => {
            let ov = cache::overview(&env.ctx, None, None)?;
            let cols: Vec<(String, String)> =
                ov["sheet"]["columns"].as_array().into_iter().flatten().filter_map(|c| Some((c["key"].as_str()?.to_string(), c["label"].as_str()?.to_string()))).collect();
            let labels: Vec<String> = cols.iter().map(|c| c.1.clone()).collect();
            let label = pick("account", n, &labels)?;
            let key = &cols.iter().find(|c| c.1 == label).expect("picked from the list").0;
            sheet_series(env, key, &months)?
        }
        ("income" | "spending" | "savings" | "savings_rate", None) => {
            let mo = monthly(&flows(env), &from, &to);
            percent = kind == "savings_rate";
            months
                .iter()
                .map(|ym| {
                    let (i, s) = mo[ym];
                    match kind.as_str() {
                        "income" => Some(i),
                        "spending" => Some(s),
                        "savings" => Some(i - s),
                        _ => (i > 0.0).then(|| (i - s) / i * 100.0),
                    }
                })
                .collect()
        }
        ("category", Some(n)) => {
            let bank = env.ctx.bank();
            let flags = env.ctx.flags();
            let cats: BTreeSet<String> = bank.iter().map(|b| b.cat().to_string()).collect();
            let cat = pick("category", n, &cats.into_iter().collect::<Vec<_>>())?;
            if flags.is_transfer(&cat) {
                bail!("'{cat}' is a transfer category: moves between own accounts count as neither income nor spending");
            }
            let income = flags.is_income(&cat);
            let mut by: BTreeMap<&str, f64> = months.iter().map(|m| (m.as_str(), 0.0)).collect();
            for f in flows(env).iter().filter(|f| f.cat == cat) {
                if let Some(e) = by.get_mut(f.ym.as_str()) {
                    if income && f.amount > 0.0 {
                        *e += f.amount;
                    } else if !income && f.amount < 0.0 {
                        *e += -f.amount;
                    }
                }
            }
            months.iter().map(|ym| Some(by[ym.as_str()])).collect()
        }
        _ => bail!("metric must be net_worth, income, spending, savings, savings_rate, 'category:<name>' or 'account:<name>', got '{metric}'"),
    };
    let fmt = |v: f64| if percent { p(v) } else { m(v) };
    let series: Vec<Value> = months.iter().zip(&values).filter_map(|(ym, v)| Some(json!({"month": ym, "value": fmt((*v)?)}))).collect();
    let pts: Vec<(&String, f64)> = months.iter().zip(&values).filter_map(|(ym, v)| Some((ym, (*v)?))).collect();
    let mut out = Map::new();
    if !percent {
        out.insert("currency".into(), json!(env.base));
    }
    out.insert("metric".into(), json!(metric));
    if percent {
        out.insert("unit".into(), json!("percent"));
    }
    out.insert("from".into(), json!(from));
    out.insert("to".into(), json!(to));
    out.insert("series".into(), Value::Array(series));
    if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
        let at = |(ym, v): &(&String, f64)| json!({"month": ym, "value": fmt(*v)});
        let min = pts.iter().min_by(|a, b| a.1.total_cmp(&b.1)).expect("non-empty");
        let max = pts.iter().max_by(|a, b| a.1.total_cmp(&b.1)).expect("non-empty");
        out.insert("min".into(), at(min));
        out.insert("max".into(), at(max));
        out.insert("avg".into(), fmt(pts.iter().map(|x| x.1).sum::<f64>() / pts.len() as f64));
        let mut change = Map::new();
        change.insert("abs".into(), fmt(last.1 - first.1));
        if !percent && first.1 != 0.0 {
            change.insert("pct".into(), p((last.1 - first.1) / first.1.abs() * 100.0));
        }
        out.insert("change".into(), Value::Object(change));
    }
    Ok(Value::Object(out))
}

/// One column of the monthly balance sheet over `months` (None where the
/// sheet has no row).
fn sheet_series(env: &Env, key: &str, months: &[String]) -> Result<Vec<Option<f64>>> {
    let ov = cache::overview(&env.ctx, None, None)?;
    let by: HashMap<&str, f64> = ov["sheet"]["rows"].as_array().into_iter().flatten().filter_map(|r| Some((r["month"].as_str()?, r[key].as_f64()?))).collect();
    Ok(months.iter().map(|ym| by.get(ym.as_str()).copied()).collect())
}

// ---------------------------------------------------------------------------
// breakdown
// ---------------------------------------------------------------------------

/// name -> (total, rows, display name) of the flows of `kind` in [from, to].
fn grouped(all: &[Flow], by: &str, income: bool, from: &str, to: &str) -> HashMap<String, (f64, usize, String)> {
    let mut keys: HashMap<&str, String> = HashMap::new();
    let mut out: HashMap<String, (f64, usize, String)> = HashMap::new();
    for f in all {
        if f.ym.as_str() < from || f.ym.as_str() > to || (income && f.amount <= 0.0) || (!income && f.amount >= 0.0) {
            continue;
        }
        let (key, shown) = match by {
            "category" => (f.cat.clone(), f.cat.clone()),
            "account" => (f.account.clone(), f.account.clone()),
            "currency" => (f.ccy.clone(), f.ccy.clone()),
            _ => {
                let k = keys.entry(&f.desc).or_insert_with(|| an::normalize_merchant(&f.desc)).clone();
                let k = if k.is_empty() { "(blank)".to_string() } else { k };
                (k, an::display_merchant(&f.desc, 40))
            }
        };
        let e = out.entry(key).or_insert((0.0, 0, shown));
        e.0 += f.amount.abs();
        e.1 += 1;
    }
    out
}

fn breakdown(env: &Env, a: &Args) -> Result<Value> {
    let by = a.str("by").ok_or_else(|| anyhow!("by is required"))?;
    let income = a.str("kind") == Some("income");
    let top = a.int("top").unwrap_or(15).clamp(1, 200) as usize;
    let (from, to) = period(env, a, None)?;
    let n = months_of(&from, &to).len() as i32;
    let (pfrom, pto) = (shift(&from, -n), shift(&from, -1));
    let all = flows(env);
    let cur = grouped(&all, by, income, &from, &to);
    let prev = grouped(&all, by, income, &pfrom, &pto);
    let total: f64 = cur.values().map(|v| v.0).sum();
    let prev_total: f64 = prev.values().map(|v| v.0).sum();
    let change = |now: f64, before: f64| (before > 0.0).then(|| p((now - before) / before * 100.0));
    let mut rows: Vec<(&String, &(f64, usize, String))> = cur.iter().collect();
    rows.sort_by(|x, y| y.1 .0.total_cmp(&x.1 .0).then(x.0.cmp(y.0)));
    let share = |v: f64| if total > 0.0 { p(v / total * 100.0) } else { p(0.0) };
    let mut list: Vec<Value> = Vec::new();
    for (key, (t, count, shown)) in rows.iter().take(top) {
        let mut r = Map::new();
        r.insert("name".into(), json!(shown));
        r.insert("total".into(), m(*t));
        r.insert("share_pct".into(), share(*t));
        r.insert("count".into(), json!(count));
        opt(&mut r, "change_pct", change(*t, prev.get(*key).map_or(0.0, |v| v.0)));
        list.push(Value::Object(r));
    }
    if rows.len() > top {
        let rest = &rows[top..];
        let (t, c) = rest.iter().fold((0.0, 0), |a, (_, v)| (a.0 + v.0, a.1 + v.1));
        list.push(json!({"name": "Other", "total": m(t), "share_pct": share(t), "count": c}));
    }
    let mut out = Map::new();
    out.insert("currency".into(), json!(env.base));
    out.insert("kind".into(), json!(if income { "income" } else { "spending" }));
    out.insert("by".into(), json!(by));
    out.insert("from".into(), json!(from));
    out.insert("to".into(), json!(to));
    out.insert("total".into(), m(total));
    out.insert("previous_total".into(), m(prev_total));
    opt(&mut out, "change_pct", change(total, prev_total));
    out.insert("rows".into(), Value::Array(list));
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// transactions
// ---------------------------------------------------------------------------

fn transactions(env: &Env, a: &Args) -> Result<Value> {
    let bank = env.ctx.bank();
    let (rates, flags) = (env.ctx.rates(), env.ctx.flags());
    let text = a.str("text").map(str::to_lowercase);
    let account = match a.str("account") {
        Some(n) => {
            let labels: BTreeSet<String> = bank.iter().map(|b| source_label(&b.source, &b.account)).collect();
            let all: Vec<String> = labels.into_iter().collect();
            // a source's key or a ledger's column key names it too
            let hit = bank.iter().find(|b| bank_key(&b.source, &b.account).eq_ignore_ascii_case(n)).map(|b| source_label(&b.source, &b.account));
            Some(match hit {
                Some(l) => l,
                None => pick("account", n, &all)?,
            })
        }
        None => None,
    };
    let category = match a.str("category") {
        Some(n) => {
            let cats: BTreeSet<String> = bank.iter().map(|b| b.cat().to_string()).collect();
            Some(pick("category", n, &cats.into_iter().collect::<Vec<_>>())?)
        }
        None => None,
    };
    let (from, to) = (a.date("from_date")?, a.date("to_date")?);
    let (min, max) = (a.f64("min_amount"), a.f64("max_amount"));
    let direction = a.str("direction");
    let limit = a.int("limit").unwrap_or(50).clamp(1, 500) as usize;
    let offset = match a.str("cursor") {
        Some(c) => c.strip_prefix('c').and_then(|n| n.parse::<usize>().ok()).ok_or_else(|| anyhow!("cursor is not one this server returned"))?,
        None => 0,
    };
    let mut hits: Vec<(&crate::views::store::BankRow, f64)> = Vec::new();
    for b in bank.iter() {
        let amount = b.amount.unwrap_or(0.0);
        if text.as_ref().is_some_and(|t| !b.description.to_lowercase().contains(t))
            || category.as_deref().is_some_and(|c| b.cat() != c)
            || from.as_deref().is_some_and(|f| b.txn_date.as_str() < f)
            || to.as_deref().is_some_and(|t| crate::views::take_chars(&b.txn_date, 10) > t)
            || min.is_some_and(|v| amount.abs() < v)
            || max.is_some_and(|v| amount.abs() > v)
            || (direction == Some("in") && amount <= 0.0)
            || (direction == Some("out") && amount >= 0.0)
        {
            continue;
        }
        if let Some(acc) = &account {
            if source_label(&b.source, &b.account) != *acc {
                continue;
            }
        }
        hits.push((b, amount * rates.rate(&b.currency, b.ym()) * b.share1()));
    }
    // newest first (later rows of a day first), or the largest first
    if a.str("sort") == Some("amount") {
        hits.sort_by(|x, y| y.1.abs().total_cmp(&x.1.abs()).then(y.0.id.cmp(&x.0.id)));
    } else {
        hits.sort_by(|x, y| y.0.txn_date.cmp(&x.0.txn_date).then(y.0.id.cmp(&x.0.id)));
    }
    let total = hits.len();
    // transfers between own accounts are not part of the owner's total
    let net: f64 = hits.iter().filter(|(b, _)| !flags.is_transfer(b.cat())).map(|h| h.1).sum();
    let rows: Vec<Value> = hits
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(b, _)| {
            let mut r = Map::new();
            r.insert("date".into(), json!(crate::views::take_chars(&b.txn_date, 10)));
            r.insert("account".into(), json!(source_label(&b.source, &b.account)));
            r.insert("description".into(), json!(b.description));
            r.insert("amount".into(), m(b.amount.unwrap_or(0.0)));
            r.insert("currency".into(), json!(b.currency));
            if let Some(c) = b.category.as_deref().filter(|c| !c.is_empty()) {
                r.insert("category".into(), json!(c));
            }
            Value::Object(r)
        })
        .collect();
    let mut out = Map::new();
    out.insert("matches".into(), json!(total));
    out.insert("currency".into(), json!(env.base));
    out.insert("net_total".into(), m(net));
    out.insert("rows".into(), Value::Array(rows));
    if offset + limit < total {
        out.insert("next_cursor".into(), json!(format!("c{}", offset + limit)));
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// recurring
// ---------------------------------------------------------------------------

fn recurring(env: &Env) -> Result<Value> {
    let v = cache::view(&env.ctx, "mcp_recurring", "", || {
        use finance_core::recurring::{detect, Flow as Kind};
        let ledger = env.src.ledger()?;
        let series = detect(&ledger);
        let mut items = Vec::new();
        let mut monthly = 0.0;
        for s in series.iter().filter(|s| s.active && s.flow == Kind::Charge) {
            let mut r = Map::new();
            r.insert("name".into(), json!(s.name));
            r.insert("cadence".into(), json!(s.cadence.label()));
            r.insert("amount".into(), m(s.typical));
            r.insert("currency".into(), json!(s.currency));
            r.insert("monthly".into(), m(s.monthly_hkd()));
            r.insert("last".into(), json!(s.last));
            r.insert("next".into(), json!(s.next));
            if !s.category.is_empty() {
                r.insert("category".into(), json!(s.category));
            }
            r.insert("account".into(), json!(source_label(&s.source, &s.account)));
            if s.varies {
                r.insert("varies".into(), json!(true));
            }
            if let Some(c) = &s.change {
                r.insert("price_change".into(), json!({"date": c.date, "from": m(c.from), "to": m(c.to), "pct": p(c.pct())}));
            }
            monthly += s.monthly_hkd();
            items.push(Value::Object(r));
        }
        Ok(json!({"currency": env.base, "monthly_total": m(monthly), "count": items.len(), "items": items}))
    })?;
    Ok((*v).clone())
}

// ---------------------------------------------------------------------------
// holdings
// ---------------------------------------------------------------------------

struct Pos {
    symbol: String,
    name: String,
    account: String,
    class: &'static str,
    qty: Option<f64>,
    price: Option<f64>,
    ccy: String,
    value: f64,
    cost: Option<f64>,
}

/// An asset class as the tools name it.
fn class_key(c: finance_core::exposure::class::AssetClass) -> &'static str {
    use finance_core::exposure::class::AssetClass::*;
    match c {
        Equity => "equity",
        Bond => "bond",
        Cash => "cash",
        Crypto => "crypto",
        Commodity => "commodity",
        Unclassified => "other",
    }
}

struct Held {
    rows: Vec<Pos>,
    stale: bool,
    as_of: Option<String>,
}

fn held(env: &Env) -> Result<Held> {
    use finance_core::exposure::class::{classify, classify_coin};
    let nw = env.src.net_worth();
    let setup = env.ctx.invest();
    let rates = env.ctx.rates();
    let ym = env.ctx.ym_now();
    let f = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64);
    let mut rows: Vec<Pos> = Vec::new();
    let mut stale = nw.get("stale").and_then(Value::as_bool).unwrap_or(false);
    let mut as_of = None;

    // the broker: positions at the snapshot's marks and cash
    let broker = setup.ibkr_name();
    let ibkr = cache::ibkr(&env.ctx)?;
    let snap = &ibkr["snapshot"];
    let mut broker_seen = false;
    let has_snapshot = snap["positions"].as_array().is_some_and(|p| !p.is_empty()) || snap["cash"].as_array().is_some_and(|c| !c.is_empty());
    if has_snapshot {
        let positions = snap["positions"].as_array().cloned().unwrap_or_default();
        broker_seen = true;
        as_of = snap["as_of"].as_str().map(String::from);
        let snap_fx = rates.rate(snap["base_ccy"].as_str().filter(|s| !s.is_empty()).unwrap_or(&env.base), &ym);
        let hist = &ibkr["history"];
        let buys: Vec<&Value> = hist["purchases"].as_array().into_iter().flatten().collect();
        for pos in &positions {
            let sym = pos["symbol"].as_str().unwrap_or("");
            let value = f(pos, "value_base").unwrap_or(0.0) * snap_fx;
            let value_ccy = f(pos, "value_ccy").unwrap_or(0.0);
            let lot = hist["symbols"].as_array().into_iter().flatten().find(|s| s["symbol"] == sym);
            // cost: what the buys cost at their own rates, scaled to what is still held
            let cost = lot.and_then(|l| {
                let (pc, ph) = buys.iter().filter(|b| b["symbol"] == sym).fold((0.0, 0.0), |a, b| (a.0 + f(b, "cost_ccy").unwrap_or(0.0), a.1 + f(b, "hkd").unwrap_or(0.0)));
                let held = f(l, "cost_ccy")?;
                (pc > 0.0).then(|| ph * held / pc)
            });
            let cost = cost.or_else(|| f(pos, "cost_ccy").filter(|_| value_ccy > 0.0).map(|c| c * value / value_ccy));
            let desc = pos["description"].as_str().unwrap_or(sym);
            rows.push(Pos {
                symbol: sym.to_string(),
                name: desc.to_string(),
                account: broker.clone(),
                class: class_key(classify(sym, desc).class),
                qty: f(pos, "qty"),
                price: f(pos, "mark_price"),
                ccy: pos["currency"].as_str().unwrap_or("").to_string(),
                value,
                cost,
            });
        }
        for c in snap["cash"].as_array().into_iter().flatten() {
            let ccy = c["currency"].as_str().unwrap_or("");
            rows.push(Pos {
                symbol: ccy.to_string(),
                name: format!("Cash ({ccy})"),
                account: broker.clone(),
                class: "cash",
                qty: f(c, "amount"),
                price: None,
                ccy: ccy.to_string(),
                value: f(c, "amount_base").unwrap_or(0.0) * rates.rate(c["base_ccy"].as_str().filter(|s| !s.is_empty()).unwrap_or(&env.base), &ym),
                cost: None,
            });
        }
    }
    // the other investment accounts (a fund): one position each
    for h in nw["investments_holdings"].as_array().into_iter().flatten() {
        let name = h["holding"].as_str().unwrap_or("");
        if setup.is_ibkr(name) && broker_seen {
            continue;
        }
        let value = f(h, "market").unwrap_or(0.0);
        if value <= 0.0 {
            continue;
        }
        rows.push(Pos { symbol: String::new(), name: name.to_string(), account: name.to_string(), class: "other", qty: None, price: None, ccy: env.base.clone(), value, cost: f(h, "cost") });
    }
    // crypto, per asset across every exchange and wallet
    let mut coins: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for s in nw["by_source"].as_array().into_iter().flatten().filter(|s| s["kind"] == "crypto") {
        let e = coins.entry(s["currency"].as_str().unwrap_or("").to_string()).or_default();
        e.0 += f(s, "native_amount").unwrap_or(0.0);
        e.1 += f(s, "hkd_value").unwrap_or(0.0);
    }
    if !coins.is_empty() {
        let page = cache::crypto_page(&env.ctx)?;
        stale |= page["stale"].as_bool().unwrap_or(false);
        let usd = rates.rate("USD", &ym);
        for (asset, (qty, value)) in coins {
            let profit = &page["result"]["profit"][&asset];
            let cost = f(profit, "avg_allin").filter(|a| *a > 0.0).map(|a| a * qty * usd);
            rows.push(Pos {
                symbol: asset.clone(),
                name: asset.clone(),
                account: CRYPTO.to_string(),
                class: class_key(classify_coin(&asset).class),
                qty: Some(qty),
                price: f(&page["prices"], &asset),
                ccy: "USD".into(),
                value,
                cost,
            });
        }
    }
    rows.sort_by(|a, b| b.value.total_cmp(&a.value).then(a.name.cmp(&b.name)));
    Ok(Held { rows, stale, as_of })
}

fn holdings(env: &Env, a: &Args) -> Result<Value> {
    let mut h = held(env)?;
    let accounts: Vec<String> = {
        let mut v: Vec<String> = env.ctx.invest().accounts.iter().map(|a| a.name.clone()).collect();
        v.extend(h.rows.iter().map(|r| r.account.clone()));
        v.sort();
        v.dedup();
        v
    };
    if let Some(n) = a.str("account") {
        let acc = pick("investment account", n, &accounts)?;
        h.rows.retain(|r| r.account == acc);
    }
    if let Some(c) = a.str("class") {
        h.rows.retain(|r| r.class == c);
    }
    let total: f64 = h.rows.iter().map(|r| r.value).sum();
    let weight = |v: f64| if total > 0.0 { p(v / total * 100.0) } else { p(0.0) };
    let (cv, cc) = h.rows.iter().filter_map(|r| Some((r.value, r.cost?))).fold((0.0, 0.0), |a, x| (a.0 + x.0, a.1 + x.1));
    let mut out = Map::new();
    out.insert("currency".into(), json!(env.base));
    out.insert("total".into(), m(total));
    if cc > 0.0 {
        out.insert("cost".into(), m(cc));
        out.insert("gain".into(), m(cv - cc));
        out.insert("gain_pct".into(), p((cv - cc) / cc * 100.0));
    }
    out.insert("prices_stale".into(), json!(h.stale));
    opt(&mut out, "broker_as_of", h.as_of.map(Value::from));
    match a.str("group_by") {
        Some(by) => {
            let mut groups: Vec<(String, f64, usize)> = Vec::new();
            for r in &h.rows {
                let key = match by {
                    "account" => r.account.clone(),
                    "asset_class" => r.class.to_string(),
                    _ => r.ccy.clone(),
                };
                match groups.iter_mut().find(|g| g.0 == key) {
                    Some(g) => {
                        g.1 += r.value;
                        g.2 += 1;
                    }
                    None => groups.push((key, r.value, 1)),
                }
            }
            groups.sort_by(|x, y| y.1.total_cmp(&x.1));
            out.insert(
                "groups".into(),
                groups.iter().map(|(k, v, n)| json!({"name": k, "value": m(*v), "weight_pct": weight(*v), "positions": n})).collect(),
            );
        }
        None => {
            let rows: Vec<Value> = h
                .rows
                .iter()
                .map(|r| {
                    let mut o = Map::new();
                    if !r.symbol.is_empty() {
                        o.insert("symbol".into(), json!(r.symbol));
                    }
                    if r.name != r.symbol {
                        o.insert("name".into(), json!(r.name));
                    }
                    o.insert("account".into(), json!(r.account));
                    o.insert("class".into(), json!(r.class));
                    opt(&mut o, "quantity", r.qty.map(|q| num(round_n(q, 6))));
                    opt(&mut o, "price", r.price.map(|x| num(round_n(x, 4))));
                    if r.price.is_some() {
                        o.insert("price_currency".into(), json!(r.ccy));
                    }
                    o.insert("value".into(), m(r.value));
                    if let Some(c) = r.cost.filter(|c| *c > 0.0) {
                        o.insert("cost".into(), m(c));
                        o.insert("gain".into(), m(r.value - c));
                        o.insert("gain_pct".into(), p((r.value - c) / c * 100.0));
                    }
                    o.insert("weight_pct".into(), weight(r.value));
                    Value::Object(o)
                })
                .collect();
            out.insert("positions".into(), Value::Array(rows));
        }
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// performance
// ---------------------------------------------------------------------------

/// Income the broker paid in [from, to] (months), by kind, in the account's
/// base currency. Empty without the table.
fn income_received(env: &Env, from: &str, to: &str) -> Map<String, Value> {
    let mut out: BTreeMap<&str, f64> = BTreeMap::new();
    let sql = "SELECT kind, SUM(amount_base) FROM ibkr_income WHERE substr(date, 1, 7) >= ?1 AND substr(date, 1, 7) <= ?2 GROUP BY kind";
    if let Ok(mut st) = env.ctx.conn.prepare_cached(sql) {
        if let Ok(rows) = st.query_map([from, to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))) {
            for (kind, sum) in rows.flatten() {
                let k = kind.to_lowercase();
                let key = if k.contains("withholding") || k.contains("tax") {
                    "withholding_tax"
                } else if k.contains("dividend") || k.contains("lieu") {
                    "dividends"
                } else if k.contains("interest") {
                    "interest"
                } else if k.contains("fee") {
                    "fees"
                } else {
                    "other"
                };
                *out.entry(key).or_default() += sum;
            }
        }
    }
    out.into_iter().filter(|(_, v)| v.abs() >= 0.005).map(|(k, v)| (k.to_string(), m(v))).collect()
}

/// Net money put into one investment account per month (the views' own flows:
/// the broker's dated deposits, bank rows into the others, crypto buys), base
/// currency.
fn account_flows(env: &Env, account: &str) -> BTreeMap<String, f64> {
    let rates = env.ctx.rates();
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    if account == CRYPTO {
        for (ym, v) in crate::views::investments::crypto_monthly_flows_hkd(&rates, &env.ctx.export()) {
            *out.entry(ym).or_default() += v;
        }
    } else {
        for c in crate::views::holdings::investment_flows(&env.ctx, &rates, None, false).into_iter().filter(|c| c.holding == account) {
            *out.entry(c.ym).or_default() += c.hkd;
        }
    }
    out
}

fn performance(env: &Env, a: &Args) -> Result<Value> {
    let inv = cache::investments(&env.ctx, true)?;
    let mut out = Map::new();
    out.insert("currency".into(), json!(env.base));
    let rows: Vec<&Value> = inv["rows"].as_array().into_iter().flatten().collect();
    if inv["empty"] == true || rows.is_empty() {
        out.insert("note".into(), json!("no investments yet"));
        return Ok(Value::Object(out));
    }
    let first = rows[0]["month"].as_str().unwrap_or("").to_string();
    let last = rows[rows.len() - 1]["month"].as_str().unwrap_or("").to_string();
    let from = a.month("from_month")?.unwrap_or_else(|| first.clone()).max(first.clone());
    let to = a.month("to_month")?.unwrap_or_else(|| last.clone()).min(last.clone());
    if from > to {
        bail!("from_month {from} is after to_month {to}");
    }
    let setup = env.ctx.invest();
    let classes: Vec<String> = inv["classes"].as_array().into_iter().flatten().filter_map(|c| c["key"].as_str()).map(String::from).collect();
    let account = match a.str("account") {
        Some(n) => Some(pick("investment account", n, &classes)?),
        None => None,
    };
    let mut notes: Vec<&str> = Vec::new();

    // (month, value, contribution that month, time-weighted % cumulative if known)
    let pts: Vec<(String, f64, f64, Option<f64>)>;
    let mut exact = None;
    match &account {
        None => {
            pts = rows.iter().map(|r| (r["month"].as_str().unwrap_or("").to_string(), r["value"].as_f64().unwrap_or(0.0), r["flow"].as_f64().unwrap_or(0.0), r["twr_pct"].as_f64())).collect();
            notes.push("time_weighted_pct chains monthly returns: exact for the broker (from its daily NAV), an approximation (modified Dietz on month-end values) for the other accounts");
        }
        Some(acc) => {
            let flows = account_flows(env, acc);
            pts = rows
                .iter()
                .filter_map(|r| {
                    let month = r["month"].as_str()?;
                    Some((month.to_string(), r["classes"][acc.as_str()].as_f64()?, flows.get(month).copied().unwrap_or(0.0), None))
                })
                .collect();
            if setup.is_ibkr(acc) {
                let ib = cache::ibkr(&env.ctx)?;
                if let Some(perf) = ib["performance"].as_object() {
                    exact = Some(json!({"since": perf["first_day"], "net_deposits": perf["net_deposits_base"].as_f64().map(m), "time_weighted_pct": perf["twr_pct"].as_f64().map(p),
                                        "money_weighted_pct": perf["mwr_pct"].as_f64().map(p), "note": "from the broker's daily NAV and dated cash flows"}));
                }
            } else {
                notes.push("a time-weighted return is only available for the whole portfolio and for the broker");
            }
        }
    }
    let idx: Vec<usize> = (0..pts.len()).filter(|i| pts[*i].0 >= from && pts[*i].0 <= to).collect();
    // money in up to the month before the period, then running through it
    let mut cumulative: f64 = pts.iter().filter(|p| p.0 < from).map(|p| p.2).sum();
    let series: Vec<Value> = idx
        .iter()
        .map(|i| {
            let (month, value, flow, _) = &pts[*i];
            cumulative += flow;
            let mut r = Map::new();
            r.insert("month".into(), json!(month));
            r.insert("value".into(), m(*value));
            r.insert("contributed".into(), m(*flow));
            r.insert("gain".into(), m(value - cumulative));
            Value::Object(r)
        })
        .collect();
    if let (Some(&i0), Some(&i1)) = (idx.first(), idx.last()) {
        let start = if i0 > 0 { pts[i0 - 1].1 } else { 0.0 };
        let end = pts[i1].1;
        let mut period = Map::new();
        period.insert("start_value".into(), m(start));
        period.insert("end_value".into(), m(end));
        let contrib: f64 = idx.iter().map(|i| pts[*i].2).sum();
        period.insert("contributions".into(), m(contrib));
        period.insert("gain".into(), m(end - start - contrib));
        // time-weighted: the cumulative series from the month before to the last
        if let (Some(c1), true) = (pts[i1].3, account.is_none()) {
            let c0 = if i0 > 0 { pts[i0 - 1].3.unwrap_or(0.0) } else { 0.0 };
            period.insert("time_weighted_pct".into(), p(((1.0 + c1 / 100.0) / (1.0 + c0 / 100.0) - 1.0) * 100.0));
        }
        // money-weighted: the same flows through the views' monthly XIRR (the opening value is the first flow)
        let mut flows: Vec<f64> = idx.iter().map(|i| pts[*i].2).collect();
        flows[0] += start;
        if let Some(irr) = irr_annual_pct(&flows, end) {
            period.insert("money_weighted_annual_pct".into(), p(irr));
            notes.push("money_weighted_annual_pct is an XIRR approximated on monthly flows, annualised");
        }
        if account.is_none() {
            // the same flows bought into the benchmark (the view's own series)
            let spx = |ym: &str| rows.iter().find(|r| r["month"] == ym).and_then(|r| r["spx_twr_pct"].as_f64());
            if let Some(b1) = spx(&pts[i1].0) {
                let b0 = if i0 > 0 { spx(&pts[i0 - 1].0).unwrap_or(0.0) } else { 0.0 };
                period.insert(
                    "benchmark".into(),
                    json!({"symbol": crate::invest_setup::benchmark(env.ctx.conn), "time_weighted_pct": p(((1.0 + b1 / 100.0) / (1.0 + b0 / 100.0) - 1.0) * 100.0)}),
                );
            }
        }
        out.insert("from".into(), json!(from));
        out.insert("to".into(), json!(to));
        out.insert("period".into(), Value::Object(period));
    }
    out.insert("series".into(), Value::Array(series));
    if account.is_none() {
        let t = &inv["totals"];
        out.insert(
            "since_start".into(),
            json!({"from": t["first_month"], "invested": t["invested"].as_f64().map(m), "value": t["value"].as_f64().map(m), "gain": t["gain"].as_f64().map(m),
                   "return_pct": t["roi_pct"].as_f64().map(p), "money_weighted_annual_pct": t["irr_pct"].as_f64().map(p),
                   "time_weighted_pct": rows[rows.len() - 1]["twr_pct"].as_f64().map(p)}),
        );
    }
    opt(&mut out, "broker_exact", exact);
    if account.as_ref().is_none_or(|acc| setup.is_ibkr(acc)) {
        let inc = income_received(env, &from, &to);
        if !inc.is_empty() {
            out.insert("income_received".into(), Value::Object(inc));
        }
        // fees and cash drag: the Costs section's own figures
        let c = cache::costs(&env.ctx)?;
        let mut fees = Map::new();
        opt(&mut fees, "commissions_paid", c["paid"]["ibkr_commissions_hkd"].as_f64().filter(|v| *v != 0.0).map(m));
        opt(&mut fees, "platform_fees_paid", c["paid"]["flexinvest_platform_hkd"].as_f64().filter(|v| *v != 0.0).map(m));
        opt(&mut fees, "running_cost_per_year", c["running"]["total_yr_hkd"].as_f64().filter(|v| *v != 0.0).map(m));
        opt(&mut fees, "running_cost_pct", c["running"]["pct"].as_f64().map(p));
        if !fees.is_empty() {
            if fees.contains_key("running_cost_per_year") {
                notes.push("running_cost_per_year is an estimate: each holding's yearly fund fee on today's value");
            }
            out.insert("fees".into(), Value::Object(fees));
        }
        if let Some(d) = c["cash_drag"].as_object() {
            out.insert(
                "cash_drag".into(),
                json!({"idle_cash": d["cash_hkd"].as_f64().map(m), "tbill_yield_pct": d["pct"].as_f64().map(p), "forgone_per_year": d["yr_hkd"].as_f64().map(m)}),
            );
            notes.push("cash_drag.forgone_per_year approximates what idle bank cash would have earned in T-bills");
        }
    }
    opt(&mut out, "notes", (!notes.is_empty()).then(|| json!(notes)));
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// activity
// ---------------------------------------------------------------------------

struct Event {
    date: String,
    kind: &'static str,
    account: String,
    symbol: String,
    qty: Option<f64>,
    price: Option<f64>,
    ccy: String,
    /// base currency, magnitude
    amount: f64,
    fee: Option<f64>,
}

/// The kind of a broker cash row or income row, by its words.
fn cash_kind(words: &str, signed: f64) -> &'static str {
    let w = words.to_lowercase();
    if w.contains("withholding") || w.contains("tax") {
        "tax"
    } else if w.contains("dividend") || w.contains("lieu") {
        "dividend"
    } else if w.contains("interest") {
        "interest"
    } else if w.contains("fee") || w.contains("commission") {
        "fee"
    } else if w.contains("deposit") || w.contains("withdraw") {
        if signed >= 0.0 { "deposit" } else { "withdrawal" }
    } else {
        "other"
    }
}

fn events(env: &Env) -> Vec<Event> {
    let conn = env.ctx.conn;
    let (setup, rates) = (env.ctx.invest(), env.ctx.rates());
    let broker = setup.ibkr_name();
    let mut out = Vec::new();
    let f = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    for t in env.ctx.ibkr_trades().iter() {
        let (qty, fxr) = (f(t, "qty"), f(t, "fx_to_base"));
        let fxr = if fxr == 0.0 { 1.0 } else { fxr };
        out.push(Event {
            date: crate::views::ms_to_date(t["ts"].as_i64().unwrap_or(0)).to_string(),
            kind: if qty >= 0.0 { "buy" } else { "sell" },
            account: broker.clone(),
            symbol: t["symbol"].as_str().unwrap_or("").to_string(),
            qty: Some(qty.abs()),
            price: Some(f(t, "price")),
            ccy: t["currency"].as_str().unwrap_or("").to_string(),
            amount: f(t, "amount").abs().max(qty.abs() * f(t, "price")) * fxr,
            fee: Some(f(t, "commission").abs() * fxr).filter(|v| *v > 0.0),
        });
    }
    let mut rows = |sql: &str, per: &mut dyn FnMut(&rusqlite::Row) -> rusqlite::Result<Event>| {
        if let Ok(mut st) = conn.prepare_cached(sql) {
            if let Ok(it) = st.query_map([], |r| per(r)) {
                out.extend(it.flatten());
            }
        }
    };
    rows("SELECT date, type, description, amount_base FROM ibkr_cash_flows", &mut |r| {
        let (date, ty, desc, amt): (String, String, String, f64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
        Ok(Event { date, kind: cash_kind(&format!("{ty} {desc}"), amt), account: broker.clone(), symbol: String::new(), qty: None, price: None, ccy: String::new(), amount: amt.abs(), fee: None })
    });
    rows("SELECT date, symbol, kind, description, amount_base FROM ibkr_income", &mut |r| {
        let (date, symbol, kind, desc, amt): (String, String, String, String, f64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
        Ok(Event { date, kind: cash_kind(&format!("{kind} {desc}"), amt), account: broker.clone(), symbol, qty: None, price: None, ccy: String::new(), amount: amt.abs(), fee: None })
    });
    // fund executions (HSBC SMS confirmations: real units at the NAV, HKD)
    let fund = |code: &str| setup.accounts.iter().find(|a| a.fund_code.as_deref() == Some(code)).or(setup.fund_account()).map(|a| a.name.clone()).unwrap_or_else(|| code.to_string());
    rows("SELECT trade_date, fund, units, px FROM fund_subscriptions", &mut |r| {
        let (date, code, units, px): (String, String, f64, f64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
        let rate = rates.rate("HKD", crate::views::take_chars(&date, 7));
        Ok(Event { kind: if units >= 0.0 { "buy" } else { "sell" }, account: fund(&code), symbol: code, qty: Some(units.abs()), price: Some(px), ccy: "HKD".into(), amount: units.abs() * px * rate, fee: None, date })
    });
    let usd = |ym: &str| rates.rate("USD", ym);
    for c in env.ctx.export().events.iter() {
        let date = crate::views::ms_to_date(c.ts).to_string();
        let kind = match c.kind.as_str() {
            "reward" => "reward",
            "deposit" => "deposit",
            "withdraw" | "withdrawal" => "withdrawal",
            "convert-out" | "spot-sell" | "p2p-sell" | "fiat-sell" => "sell",
            k if crate::views::crypto::is_buy(k) => "buy",
            "revolut-btc" => if c.delta >= 0.0 { "buy" } else { "sell" },
            _ => "other",
        };
        let amount = c.usd.unwrap_or(0.0).abs() * usd(crate::views::take_chars(&date, 7));
        out.push(Event { date, kind, account: CRYPTO.to_string(), symbol: c.asset.clone(), qty: Some(c.delta.abs()), price: None, ccy: String::new(), amount, fee: None });
    }
    // newest first; stable, so a day keeps its ledger order
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

fn activity(env: &Env, a: &Args) -> Result<Value> {
    let all = events(env);
    let accounts: Vec<String> = {
        let mut v: Vec<String> = env.ctx.invest().accounts.iter().map(|a| a.name.clone()).collect();
        v.extend(all.iter().map(|e| e.account.clone()));
        v.sort();
        v.dedup();
        v
    };
    let account = match a.str("account") {
        Some(n) => Some(pick("investment account", n, &accounts)?),
        None => None,
    };
    let symbol = a.str("symbol").map(str::to_uppercase);
    let kind = a.str("type");
    let (from, to) = (a.date("from_date")?, a.date("to_date")?);
    let limit = a.int("limit").unwrap_or(50).clamp(1, 500) as usize;
    let hits: Vec<&Event> = all
        .iter()
        .filter(|e| account.as_deref().is_none_or(|x| e.account == x))
        .filter(|e| symbol.as_deref().is_none_or(|s| e.symbol.to_uppercase() == s))
        .filter(|e| kind.is_none_or(|k| e.kind == k))
        .filter(|e| from.as_deref().is_none_or(|f| e.date.as_str() >= f) && to.as_deref().is_none_or(|t| e.date.as_str() <= t))
        .collect();
    let mut totals: BTreeMap<&str, f64> = BTreeMap::new();
    for e in &hits {
        *totals.entry(e.kind).or_default() += e.amount;
    }
    let rows: Vec<Value> = hits
        .iter()
        .take(limit)
        .map(|e| {
            let mut r = Map::new();
            r.insert("date".into(), json!(e.date));
            r.insert("type".into(), json!(e.kind));
            r.insert("account".into(), json!(e.account));
            if !e.symbol.is_empty() {
                r.insert("symbol".into(), json!(e.symbol));
            }
            opt(&mut r, "quantity", e.qty.map(|q| num(round_n(q, 6))));
            if let Some(px) = e.price {
                r.insert("price".into(), num(round_n(px, 4)));
                r.insert("price_currency".into(), json!(e.ccy));
            }
            r.insert("amount".into(), m(e.amount));
            opt(&mut r, "fee", e.fee.map(m));
            Value::Object(r)
        })
        .collect();
    let mut out = Map::new();
    out.insert("currency".into(), json!(env.base));
    out.insert("matches".into(), json!(hits.len()));
    out.insert("totals".into(), Value::Object(totals.into_iter().map(|(k, v)| (k.to_string(), m(v))).collect()));
    out.insert("rows".into(), Value::Array(rows));
    if hits.len() > limit {
        out.insert("truncated".into(), json!(true));
    }
    if let Some(sym) = &symbol {
        opt(&mut out, "dca", dca_summary(env, sym)?);
    }
    Ok(Value::Object(out))
}

/// The repeated buys of one symbol as a DCA plan (finance_core::dca): the
/// broker's through its holding plan, BTC through the coin plan. In the
/// instrument's own currency.
fn dca_summary(env: &Env, symbol: &str) -> Result<Option<Value>> {
    use finance_core::dca;
    let ledger = env.src.ledger()?;
    let today = env.ctx.today.to_string();
    let plan: Option<(dca::Dca, String)> = if ledger.trades.iter().any(|t| t.symbol.to_uppercase() == symbol) {
        let ib = cache::ibkr(&env.ctx)?;
        let marks: HashMap<String, f64> = ib["snapshot"]["positions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p["symbol"].as_str()?.to_string(), p["mark_price"].as_f64()?)))
            .collect();
        finance_core::invest::holding_plans(&ledger, &finance_core::invest::Fx::default(), &today, &marks)
            .into_iter()
            .find(|h| h.symbol.to_uppercase() == symbol)
            .map(|h| (h.dca, h.currency))
    } else if symbol == "BTC" {
        let page = cache::crypto_page(&env.ctx)?;
        dca::analyze(&ledger, page["prices"]["BTC"].as_f64().filter(|p| *p > 0.0), dca::Scope::All).map(|d| (d, "USD".to_string()))
    } else {
        None
    };
    Ok(plan.filter(|(d, _)| d.buys.len() >= 2).map(|(d, ccy)| {
        json!({"symbol": symbol, "currency": ccy, "buys": d.buys.len(), "first_buy": d.buys.first().map(|b| b.date.clone()),
               "invested": m(d.bought), "avg_cost": num(round_n(d.avg_cost(), 4)), "price_now": num(round_n(d.price, 4)),
               "gain": m(d.gain()), "gain_pct": p(d.gain_pct())})
    }))
}
