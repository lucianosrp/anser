//! Analytics over the balance sheet and raw transactions: month-over-month
//! PnL, growth, allocation,
//! cashflows, rolling averages, and the spending side (category breakdown,
//! top merchants, recurring payments, fixed vs variable, one month's
//! drill-down). Amounts are native currency, converted with the fx_rates
//! cache (fx::Rates).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::fx::{MonthlyRates, Rates};
use super::sheet::Sheet;
use super::{get_f64, get_str, num, r2, take_chars, ym_of, Ctx};

// ---------------------------------------------------------------------------
// sheet-derived series
// ---------------------------------------------------------------------------

/// compute_pnl: month-over-month change of each column and total_hkd,
/// rounded; the first month (no previous) is dropped.
pub fn compute_pnl(sheet: &Sheet, cols: &[String]) -> Vec<Value> {
    let mut keys: Vec<&str> = cols.iter().map(String::as_str).collect();
    keys.push("total_hkd");
    let series: Vec<(&str, &Vec<f64>)> = keys.iter().filter_map(|k| sheet.col(k).map(|v| (*k, v))).collect();
    (1..sheet.len())
        .map(|i| {
            let mut m = Map::new();
            m.insert("month".into(), json!(ym_of(sheet.months[i])));
            for (k, v) in &series {
                m.insert(k.to_string(), num(r2(v[i] - v[i - 1])));
            }
            Value::Object(m)
        })
        .collect()
}

/// compute_portfolio_pct, reduced to month + the _pct columns (all
/// get_banks keeps): % allocation by original currency. Kept quirks (the
/// owner's output must not change): a USD-denominated column is labelled
/// "BTC", and investments / receivables are not part of the allocation.
pub fn compute_portfolio_pct(sheet: &Sheet, currency_map: &[(String, String)]) -> anyhow::Result<Vec<Value>> {
    let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
    for (col, ccy) in currency_map {
        let label = if ccy == "USD" { "BTC".to_string() } else { ccy.clone() };
        match groups.iter_mut().find(|(l, _)| *l == label) {
            Some((_, v)) => v.push(col),
            None => groups.push((label, vec![col])),
        }
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    // every mapped column is selected; a no_fx column is not in the sheet,
    // which fails the whole banks view (an error, by contract)
    let mut cols: Vec<(String, Vec<&Vec<f64>>)> = Vec::new();
    for (label, keys) in &groups {
        let mut v = Vec::new();
        for k in keys {
            v.push(sheet.col(k).ok_or_else(|| anyhow::anyhow!("ColumnNotFoundError: {k}"))?);
        }
        cols.push((label.clone(), v));
    }
    Ok((0..sheet.len())
        .map(|i| {
            let vals: Vec<f64> = cols.iter().map(|(_, cs)| cs.iter().fold(0.0, |a, c| a + c[i])).collect();
            let total = vals.iter().fold(0.0, |a, v| a + v);
            let mut m = Map::new();
            m.insert("month".into(), json!(ym_of(sheet.months[i])));
            for ((label, _), v) in cols.iter().zip(&vals) {
                let pct = if total > 0.0 { r2(v / total * 100.0) } else { 0.0 };
                m.insert(format!("{label}_pct"), num(pct));
            }
            Value::Object(m)
        })
        .collect())
}

/// compute_growth_rate: % change of total_hkd, only where the previous
/// month held >= 50k (tiny early balances make meaningless spikes).
pub fn compute_growth_rate(sheet: &Sheet) -> Vec<Value> {
    let t = &sheet.total_hkd;
    (1..sheet.len())
        .filter(|&i| t[i - 1] >= 50_000.0)
        .map(|i| json!({"month": ym_of(sheet.months[i]), "growth_pct": num(r2((t[i] - t[i - 1]) / t[i - 1] * 100.0))}))
        .collect()
}

/// compute_rolling_avg: 3-month rolling mean of total_hkd (null for the
/// first two months).
pub fn compute_rolling_avg(sheet: &Sheet) -> Vec<Value> {
    let t = &sheet.total_hkd;
    (0..sheet.len())
        .map(|i| {
            let roll = if i >= 2 { num(r2((t[i - 2] + t[i - 1] + t[i]) / 3.0)) } else { Value::Null };
            json!({"month": ym_of(sheet.months[i]), "total_hkd": num(t[i]), "rolling_3m": roll})
        })
        .collect()
}

/// (month, inflow, outflow) per month of one source, sorted by month.
fn monthly_flows(bank: &[super::store::BankRow], source: &str) -> Vec<(String, f64, f64)> {
    let mut by: std::collections::BTreeMap<&str, (f64, f64)> = std::collections::BTreeMap::new();
    for b in bank.iter().filter(|b| b.source == source) {
        let e = by.entry(b.ym()).or_insert((0.0, 0.0));
        match b.amount {
            Some(a) if a > 0.0 => e.0 += a,
            Some(a) if a < 0.0 => e.1 += a,
            _ => {}
        }
    }
    by.into_iter().map(|(m, (i, o))| (m.to_string(), i, o)).collect()
}

/// compute_cashflows: monthly inflows / outflows in HKD over every bank
/// source marked `cashflow` (with a balance method and one currency; the
/// owner's: N26 and HSBC, once hardcoded). A foreign
/// currency converts at the month's rate, forward-filled across that
/// source's months (0 before the first known rate); HKD as is. Sources sum
/// in registry order.
pub fn compute_cashflows(ctx: &Ctx, rates: Option<&MonthlyRates>) -> Vec<Value> {
    let bank = ctx.bank();
    let mut per_source: Vec<std::collections::BTreeMap<String, (f64, f64)>> = Vec::new();
    for spec in crate::sources::file_sources().iter().map(|s| s.spec().clone()) {
        if spec.kind != crate::sources::Kind::Bank || spec.balance_method == crate::sources::BalanceMethod::None || !spec.cashflow {
            continue;
        }
        let Some(ccy) = spec.currency else { continue };
        let flows = monthly_flows(&bank, spec.key);
        let base = rates.map(|r| r.base.clone()).unwrap_or_else(|| super::fx::base_currency(ctx.conn));
        let converted = if ccy == base {
            flows.into_iter().map(|(m, i, o)| (m, (i, o))).collect()
        } else {
            let col: HashMap<String, Option<f64>> = match rates.and_then(|r| r.col(ccy).map(|c| (r, c))) {
                Some((r, c)) => r.months.iter().zip(&r.cols[c]).map(|(m, v)| (ym_of(*m), *v)).collect(),
                None => HashMap::new(),
            };
            let mut last: Option<f64> = None;
            flows.into_iter().map(|(m, i, o)| {
                if let Some(Some(r)) = col.get(&m) {
                    last = Some(*r);
                }
                let r = last.unwrap_or(0.0);
                (m, (i * r, o * r))
            }).collect()
        };
        per_source.push(converted);
    }
    let months: std::collections::BTreeSet<&String> = per_source.iter().flat_map(|s| s.keys()).collect();
    months.into_iter()
        .map(|m| {
            let (mut inflow, mut outflow) = (None::<f64>, None::<f64>);
            for s in &per_source {
                let (i, o) = s.get(m).copied().unwrap_or((0.0, 0.0));
                inflow = Some(inflow.map_or(i, |x| x + i));
                outflow = Some(outflow.map_or(o, |x| x + o));
            }
            let (inflow, outflow) = (r2(inflow.unwrap_or(0.0)), r2(outflow.unwrap_or(0.0)));
            json!({"month": m, "inflow": num(inflow), "outflow": num(outflow), "net": num(r2(inflow + outflow))})
        })
        .collect()
}

// ---------------------------------------------------------------------------
// spending
// ---------------------------------------------------------------------------

/// The transfer categories (seeds the default rules into an empty
/// table first); unreadable rules = no transfers.
pub fn transfer_categories(conn: &Connection) -> HashSet<String> {
    crate::categorize::transfer_categories(conn).unwrap_or_default()
}

/// One spending outflow: (ym, category, description, hkd > 0), borrowed
/// from the bank rows.
pub struct Spend<'a> {
    pub ym: &'a str,
    pub category: &'a str,
    pub description: &'a str,
    pub hkd: f64,
    /// normalize_merchant(description)
    pub key: Arc<str>,
}

/// A memo of a pure function of a merchant description: the same few
/// thousand texts come back on every recompute, and each costs regexes.
struct TextMemo(Mutex<Option<HashMap<String, Arc<str>>>>);

impl TextMemo {
    const fn new() -> Self {
        TextMemo(Mutex::new(None))
    }

    fn get(&self, text: &str, f: impl FnOnce(&str) -> String) -> Arc<str> {
        if let Some(v) = self.0.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(text).cloned())) {
            return v;
        }
        let v: Arc<str> = f(text).into();
        if let Ok(mut m) = self.0.lock() {
            let m = m.get_or_insert_with(HashMap::new);
            if m.len() > 100_000 {
                m.clear(); // bounded, whatever the data
            }
            m.insert(text.to_string(), v.clone());
        }
        v
    }
}

static MERCHANT_KEY: TextMemo = TextMemo::new();
static SHOWN_48: TextMemo = TextMemo::new();
static SHOWN_40: TextMemo = TextMemo::new();

/// normalize_merchant, memoized.
fn merchant_key(desc: &str) -> Arc<str> {
    MERCHANT_KEY.get(desc, normalize_merchant)
}

/// display_merchant at the two widths the spending view uses, memoized.
fn shown(desc: &str, width: usize) -> Arc<str> {
    match width {
        48 => SHOWN_48.get(desc, |d| display_merchant(d, 48)),
        40 => SHOWN_40.get(desc, |d| display_merchant(d, 40)),
        _ => display_merchant(desc, width).into(),
    }
}

/// _spending_rows: outflows that count as spending (no transfer category,
/// no income one: Salary), the owner's share only, in HKD. SQLite row order.
pub fn spending_rows<'a>(
    bank: &'a [super::store::BankRow],
    flags: &crate::categories::Flags,
    rates: &Rates,
    from: Option<&str>,
    to: Option<&str>,
) -> Vec<Spend<'a>> {
    let mut keys: HashMap<&str, Arc<str>> = HashMap::new();
    let mut out = Vec::new();
    for b in bank.iter() {
        let Some(amount) = b.amount.filter(|a| *a < 0.0) else { continue };
        let ym = b.ym();
        if from.is_some_and(|f| ym < f) || to.is_some_and(|t| ym > t) {
            continue;
        }
        let cat = b.cat();
        if flags.is_transfer(cat) || flags.is_income(cat) {
            continue;
        }
        let hkd = amount.abs() * b.share1() * rates.rate(&b.currency, ym);
        if hkd <= 0.0 {
            continue;
        }
        let key = keys.entry(&b.description).or_insert_with(|| merchant_key(&b.description)).clone();
        out.push(Spend { ym, category: cat, description: &b.description, hkd, key });
    }
    out
}

/// spending_by_category: per-month spend per category, sorted (month, -hkd).
pub fn spending_by_category(spend: &[Spend]) -> Vec<Value> {
    let mut agg: Vec<((&str, &str), f64)> = Vec::new();
    let mut idx: HashMap<(&str, &str), usize> = HashMap::new();
    for s in spend {
        let k = (s.ym, s.category);
        match idx.get(&k) {
            Some(&i) => agg[i].1 += s.hkd,
            None => {
                idx.insert(k, agg.len());
                agg.push((k, s.hkd));
            }
        }
    }
    let mut out: Vec<(&str, &str, f64)> = agg.into_iter().map(|((m, c), v)| (m, c, r2(v))).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)));
    out.into_iter().map(|(m, c, h)| json!({"month": m, "category": c, "hkd": num(h)})).collect()
}

/// Import noise on merchant names: Revolut's " FX Rate £1 = $1.2274" suffix,
/// HSBC's "(13AUG25)" ATM stamp. Stripped for display AND grouping.
fn merchant_noise() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\s+FX Rate\b.*$|\s*\(\d{2}[A-Z]{3}\d{2}\)\s*$").unwrap())
}

/// _display_merchant.
pub fn display_merchant(desc: &str, width: usize) -> String {
    let d = merchant_noise().replace_all(desc.trim(), "");
    let s = take_chars(&d, width).trim();
    if s.is_empty() { "(blank)".into() } else { s.to_string() }
}

/// _normalize_merchant: a stable merchant key (lowercase, ref/id tokens and
/// digits dropped, whitespace collapsed, 32 chars).
pub fn normalize_merchant(desc: &str) -> String {
    static REF: OnceLock<Regex> = OnceLock::new();
    static DIGITS: OnceLock<Regex> = OnceLock::new();
    static WS: OnceLock<Regex> = OnceLock::new();
    let d = desc.to_lowercase();
    let d = REF.get_or_init(|| Regex::new(r"\b[a-z]{0,2}\d[\d a-z]*\d\b").unwrap()).replace_all(&d, " ");
    let d = DIGITS.get_or_init(|| Regex::new(r"\d+").unwrap()).replace_all(&d, " ");
    let d = WS.get_or_init(|| Regex::new(r"\s+").unwrap()).replace_all(&d, " ");
    take_chars(d.trim(), 32).to_string()
}

/// top_spending: top merchants by HKD over the window.
pub fn top_spending(spend: &[Spend], n: usize) -> Vec<Value> {
    let mut agg: Vec<(Arc<str>, &str, f64, i64)> = Vec::new();
    let mut idx: HashMap<Arc<str>, usize> = HashMap::new();
    let mut names: HashMap<&str, Arc<str>> = HashMap::new();
    for s in spend {
        let key = names.entry(s.description).or_insert_with(|| shown(s.description, 48)).clone();
        match idx.get(&key) {
            Some(&i) => {
                agg[i].2 += s.hkd;
                agg[i].3 += 1;
            }
            None => {
                idx.insert(key.clone(), agg.len());
                agg.push((key, s.category, s.hkd, 1));
            }
        }
    }
    agg.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    agg.into_iter()
        .take(n)
        .map(|(m, c, h, k)| json!({"merchant": &*m, "category": c, "hkd": num(r2(h)), "count": k}))
        .collect()
}

pub struct Group {
    merchant: String,
    category: String,
    months: Vec<String>, // distinct, first-seen order
    by_month: Vec<f64>,  // aligned with months
    total: f64,
}

/// Spend grouped by normalized merchant (first-seen order); empty keys
/// dropped. Computed once per spending view, read by the three below.
pub fn merchant_groups(spend: &[Spend]) -> Vec<(String, Group)> {
    let mut out: Vec<(String, Group)> = Vec::new();
    let mut idx: HashMap<&str, usize> = HashMap::new();
    for s in spend {
        if s.key.is_empty() {
            continue;
        }
        let i = *idx.entry(&s.key).or_insert_with(|| {
            out.push((s.key.to_string(), Group {
                merchant: shown(s.description, 40).to_string(),
                category: s.category.to_string(),
                months: vec![],
                by_month: vec![],
                total: 0.0,
            }));
            out.len() - 1
        });
        let g = &mut out[i].1;
        match g.months.iter().position(|m| *m == s.ym) {
            Some(j) => g.by_month[j] += s.hkd,
            None => {
                g.months.push(s.ym.to_string());
                g.by_month.push(s.hkd);
            }
        }
        g.total += s.hkd;
    }
    out
}

/// recurring_payments: outflows recurring in >= 3 distinct months, by -avg.
pub fn recurring_payments(groups: &[(String, Group)]) -> Vec<Value> {
    let mut out: Vec<(f64, Value)> = Vec::new();
    for (_, g) in groups {
        let nm = g.months.len();
        if nm < 3 {
            continue;
        }
        let avg = r2(g.total / nm as f64);
        out.push((avg, json!({"merchant": g.merchant, "category": g.category, "months": nm,
                               "avg_hkd": num(avg), "total_hkd": num(r2(g.total))})));
    }
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.into_iter().map(|x| x.1).collect()
}

/// Categories fixed by nature (the fallback when the db has no categories
/// table: categories.fixed = 'always' replaces it).
pub const FIXED_CATEGORIES: [&str; 7] = crate::categories::FIXED_ALWAYS;
const FIXED_MIN_MONTHS: usize = 6;
const FIXED_MAX_CV: f64 = 0.15;

/// fixed_merchants: {merchant_key: {merchant, category, months, avg_hkd,
/// why}} for every outflow stream counted as FIXED spending (a fixed
/// category, or >= 6 months at a steady per-month total, CV <= 0.15). A
/// category fixed 'never' (sums of variable spend, variable habits, money
/// reimbursed) is never fixed however regular.
pub fn fixed_merchants(groups: &[(String, Group)], flags: &crate::categories::Flags) -> Vec<(String, f64, Value)> {
    let mut out = Vec::new();
    for (key, g) in groups {
        if flags.fixed_never(&g.category) {
            continue;
        }
        let n = g.by_month.len();
        let mean = g.by_month.iter().fold(0.0, |a, v| a + v) / n as f64;
        let mut why = None;
        if flags.fixed_always(&g.category) {
            why = Some("category");
        } else if n >= FIXED_MIN_MONTHS && mean > 0.0 {
            let var = g.by_month.iter().fold(0.0, |a, v| a + (v - mean).powi(2)) / n as f64;
            if var.powf(0.5) / mean <= FIXED_MAX_CV {
                why = Some("steady");
            }
        }
        if let Some(why) = why {
            let avg = r2(mean);
            out.push((key.clone(), avg, json!({"merchant": g.merchant, "category": g.category, "months": n,
                                        "avg_hkd": num(avg), "why": why})));
        }
    }
    out
}

/// fixed_variable_by_month: {ym: (fixed, variable)}.
pub fn fixed_variable_by_month(spend: &[Spend], fixed: &[(String, f64, Value)]) -> HashMap<String, (f64, f64)> {
    let fixed: HashSet<&str> = fixed.iter().map(|f| f.0.as_str()).collect();
    let mut out: HashMap<String, (f64, f64)> = HashMap::new();
    for s in spend {
        if !out.contains_key(s.ym) {
            out.insert(s.ym.to_string(), (0.0, 0.0));
        }
        let e = out.get_mut(s.ym).expect("inserted above");
        if fixed.contains(&*s.key) {
            e.0 += s.hkd;
        } else {
            e.1 += s.hkd;
        }
    }
    out
}

/// month_breakdown: everything that moved in one month, HKD, split into
/// income / spending / transfers, by category and the largest rows.
pub fn month_breakdown(conn: &Connection, month: &str) -> Value {
    let transfers = transfer_categories(conn);
    let rates = Rates::load(conn);
    let rows: Vec<(String, f64, String, String, String, f64)> = conn
        .prepare_cached(
            "SELECT currency, amount, COALESCE(category,'Other') category, description, txn_date, \
             COALESCE(share,1) share FROM bank_transactions WHERE substr(txn_date,1,7)=?",
        )
        .and_then(|mut s| {
            s.query_map([month], |r| {
                Ok((
                    get_str(r, 0).unwrap_or_default(),
                    get_f64(r, 1).unwrap_or(0.0),
                    get_str(r, 2).unwrap_or_default(),
                    get_str(r, 3).unwrap_or_default(),
                    get_str(r, 4).unwrap_or_default(),
                    get_f64(r, 5).unwrap_or(1.0),
                ))
            })?
            .collect()
        })
        .unwrap_or_default();

    // 0 = in, 1 = out, 2 = transfer
    let mut cat_tot: [Vec<(String, f64)>; 3] = Default::default();
    let mut txns: [Vec<(f64, Value)>; 3] = Default::default();
    for (ccy, amount, cat, desc, date, share) in rows {
        let mut hkd = amount * rates.rate(&ccy, month);
        if hkd.abs() < 0.005 {
            continue;
        }
        let ft = if transfers.contains(&cat) { 2 } else if hkd > 0.0 { 0 } else { 1 };
        if ft != 2 {
            hkd *= share; // only the owner's part of a shared bill
        }
        match cat_tot[ft].iter_mut().find(|(c, _)| *c == cat) {
            Some((_, v)) => *v += hkd,
            None => cat_tot[ft].push((cat.clone(), hkd)),
        }
        let h = r2(hkd);
        txns[ft].push((h, json!({"merchant": display_merchant(&desc, 44), "category": cat, "hkd": num(h),
                                 "date": take_chars(&date, 10)})));
    }
    let by_abs = |a: &f64, b: &f64| b.abs().partial_cmp(&a.abs()).unwrap_or(std::cmp::Ordering::Equal);
    let cats = |ft: usize| -> Vec<Value> {
        let mut out: Vec<(String, f64)> = cat_tot[ft].iter().map(|(c, v)| (c.clone(), r2(*v))).collect();
        out.sort_by(|a, b| by_abs(&a.1, &b.1));
        out.into_iter().map(|(c, h)| json!({"category": c, "hkd": num(h)})).collect()
    };
    let top = |ft: usize| -> Vec<Value> {
        let mut t: Vec<&(f64, Value)> = txns[ft].iter().collect();
        t.sort_by(|a, b| by_abs(&a.0, &b.0));
        t.into_iter().take(8).map(|x| x.1.clone()).collect()
    };
    // a sum of nothing is the JSON integer 0 (not 0.0): the bundle's bytes
    let sum_or_zero = |vals: Vec<f64>| -> Value {
        if vals.is_empty() { json!(0) } else { num(r2(vals.iter().fold(0.0, |a, v| a + v))) }
    };
    let income: Vec<f64> = cat_tot[0].iter().map(|x| x.1).collect();
    let spend: Vec<f64> = cat_tot[1].iter().map(|x| x.1).collect();
    let tr_out: Vec<f64> = cat_tot[2].iter().map(|x| x.1).filter(|v| *v < 0.0).collect();
    let tr_in: Vec<f64> = cat_tot[2].iter().map(|x| x.1).filter(|v| *v > 0.0).collect();
    let inc = income.iter().fold(0.0, |a, v| a + v);
    let sp = -spend.iter().fold(0.0, |a, v| a + v);
    let mut m = Map::new();
    m.insert("month".into(), json!(month));
    m.insert("income_hkd".into(), sum_or_zero(income.clone()));
    m.insert("spend_hkd".into(), if spend.is_empty() { json!(0) } else { num(r2(sp)) });
    m.insert("net_hkd".into(), if income.is_empty() && spend.is_empty() { json!(0) } else { num(r2(inc - sp)) });
    m.insert("transfer_out_hkd".into(),
             if tr_out.is_empty() { json!(0) } else { num(r2(-tr_out.iter().fold(0.0, |a, v| a + v))) });
    m.insert("transfer_in_hkd".into(), sum_or_zero(tr_in));
    m.insert("inflows".into(), Value::Array(cats(0)));
    m.insert("outflows".into(), Value::Array(cats(1)));
    m.insert("transfers".into(), Value::Array(cats(2)));
    m.insert("top_in".into(), Value::Array(top(0)));
    m.insert("top_out".into(), Value::Array(top(1)));
    m.insert("top_transfers".into(), Value::Array(top(2)));
    Value::Object(m)
}
