//! The monthly HKD balance sheet: per-ledger monthly balances, the crypto
//! column and the investments column, one Vec per column, aligned on
//! `months`.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::fx::monthly_rates;
use super::{bank_key, balance_method, get_str, month_date, month_range, month_start, ms_to_date, num, r2,
            source_label, spec, ym_of, Ctx};

/// One bank ledger column of the sheet.
#[derive(Clone, Debug)]
pub struct Column {
    pub key: String,
    pub label: String,
    pub currency: String,
    /// no FX pair for the currency: kept out of every HKD aggregate
    pub no_fx: bool,
}

impl Column {
    pub fn json(&self) -> Value {
        let mut m = Map::new();
        m.insert("key".into(), json!(self.key));
        m.insert("label".into(), json!(self.label));
        m.insert("currency".into(), json!(self.currency));
        if self.no_fx {
            m.insert("no_fx".into(), json!(true));
        }
        Value::Object(m)
    }
}

/// The sheet: every column aligned on `months`.
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub months: Vec<NaiveDate>,
    pub columns: Vec<Column>,
    /// (key, HKD series) of every priced bank column, in column order
    pub banks: Vec<(String, Vec<f64>)>,
    pub crypto_hkd: Vec<f64>,
    pub investments_hkd: Vec<f64>,
    pub receivables_hkd: Vec<f64>,
    pub total_hkd: Vec<f64>,
    pub total_eur: Vec<f64>,
}

impl Sheet {
    pub fn len(&self) -> usize {
        self.months.len()
    }

    pub fn is_empty(&self) -> bool {
        self.months.is_empty()
    }

    /// A named column: a priced bank key or one of the fixed ones.
    pub fn col(&self, key: &str) -> Option<&Vec<f64>> {
        match key {
            "crypto_hkd" => Some(&self.crypto_hkd),
            "investments_hkd" => Some(&self.investments_hkd),
            "receivables_hkd" => Some(&self.receivables_hkd),
            "total_hkd" => Some(&self.total_hkd),
            "total_eur" => Some(&self.total_eur),
            _ => self.banks.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        }
    }

    /// One JSON object per month, month as 'YYYY-MM'.
    pub fn rows_json(&self) -> Value {
        let rows: Vec<Value> = (0..self.len())
            .map(|i| {
                let mut m = Map::new();
                m.insert("month".into(), json!(ym_of(self.months[i])));
                for (k, v) in &self.banks {
                    m.insert(k.clone(), num(v[i]));
                }
                m.insert("crypto_hkd".into(), num(self.crypto_hkd[i]));
                m.insert("investments_hkd".into(), num(self.investments_hkd[i]));
                m.insert("receivables_hkd".into(), num(self.receivables_hkd[i]));
                m.insert("total_hkd".into(), num(self.total_hkd[i]));
                m.insert("total_eur".into(), num(self.total_eur[i]));
                Value::Object(m)
            })
            .collect();
        Value::Array(rows)
    }

    pub fn columns_json(&self) -> Value {
        Value::Array(self.columns.iter().map(Column::json).collect())
    }

    /// Keep months in [from, to] ('YYYY-MM' or 'YYYY-MM-DD').
    fn filter(&mut self, from: Option<&str>, to: Option<&str>) {
        let lo = from.map(month_date);
        let hi = to.map(month_date);
        let keep: Vec<bool> =
            self.months.iter().map(|m| lo.is_none_or(|l| *m >= l) && hi.is_none_or(|h| *m <= h)).collect();
        if keep.iter().all(|k| *k) {
            return;
        }
        let f = |v: &mut Vec<f64>| {
            let mut i = 0;
            v.retain(|_| {
                i += 1;
                keep[i - 1]
            });
        };
        let mut i = 0;
        self.months.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        for (_, v) in &mut self.banks {
            f(v);
        }
        f(&mut self.crypto_hkd);
        f(&mut self.investments_hkd);
        f(&mut self.receivables_hkd);
        f(&mut self.total_hkd);
        f(&mut self.total_eur);
    }
}

/// SELECT DISTINCT source, account FROM bank_transactions (SQLite's order).
pub fn bank_pairs(conn: &Connection) -> Vec<(String, String)> {
    let Ok(mut stmt) = conn.prepare_cached("SELECT DISTINCT source, account FROM bank_transactions") else {
        return vec![];
    };
    stmt.query_map([], |r| Ok((get_str(r, 0).unwrap_or_default(), get_str(r, 1).unwrap_or_default())))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// The currency of a ledger: the spec's, else its first row's, else HKD.
pub fn ledger_currency(conn: &Connection, source: &str, account: &str) -> String {
    if let Some(c) = spec(source).and_then(|s| s.currency) {
        return c.to_string();
    }
    conn.query_row(
        "SELECT currency FROM bank_transactions WHERE source=? AND account=? LIMIT 1",
        [source, account],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_else(|_| super::fx::base_currency(conn))
}

/// (txn_date, amount, balance)
pub type LedgerRow = (String, Option<f64>, Option<f64>);

/// One (source, account) bank ledger: its rows (txn_date, amount,
/// balance), stably sorted by date.
#[derive(Clone, Debug)]
pub struct Ledger {
    pub source: String,
    pub account: String,
    pub rows: Vec<LedgerRow>,
}

/// Every ledger, in SELECT DISTINCT source, account order (SQLite's: it
/// walks an index, so it is not first appearance in the table).
pub fn load_ledgers(conn: &Connection, bank: &[super::store::BankRow]) -> Vec<Ledger> {
    let mut out: Vec<Ledger> =
        bank_pairs(conn).into_iter().map(|(source, account)| Ledger { source, account, rows: vec![] }).collect();
    let idx: HashMap<(&str, &str), usize> =
        out.iter().enumerate().map(|(i, l)| ((l.source.as_str(), l.account.as_str()), i)).collect();
    let mut rows: Vec<Vec<LedgerRow>> = vec![vec![]; out.len()];
    for b in bank {
        if let Some(&i) = idx.get(&(b.source.as_str(), b.account.as_str())) {
            rows[i].push((b.txn_date.clone(), b.amount, b.balance));
        }
    }
    for (l, mut r) in out.iter_mut().zip(rows) {
        // stable by date (ties in rowid order)
        r.sort_by(|a, b| a.0.cmp(&b.0));
        l.rows = r;
    }
    out
}

/// One ledger's monthly balance: one balance per month, None where the
/// month's last row carries no balance (the sheet forward-fills it).
///   cumsum   -> running total of amount, shifted up by the minimal opening
///               balance that keeps it >= 0
///   reported -> the ledger's own balance column
pub fn monthly_balance(ledger: &Ledger, method: &str) -> Vec<(NaiveDate, Option<f64>)> {
    let rows = &ledger.rows;
    if rows.is_empty() {
        return vec![];
    }
    let mut bal: Vec<Option<f64>> = Vec::with_capacity(rows.len());
    if method == "cumsum" {
        let mut run = 0.0;
        for (_, amt, _) in rows {
            bal.push(amt.map(|a| {
                run += a;
                run
            }));
        }
        let floor = bal.iter().flatten().copied().fold(f64::INFINITY, f64::min);
        if floor.is_finite() && floor < 0.0 {
            for b in bal.iter_mut().flatten() {
                *b -= floor;
            }
        }
    } else {
        bal = rows.iter().map(|r| r.2).collect();
    }
    let mut out: Vec<(NaiveDate, Option<f64>)> = Vec::new();
    for ((d, _, _), b) in rows.iter().zip(bal) {
        let m = month_date(d);
        match out.last_mut() {
            Some(last) if last.0 == m => last.1 = b,
            _ => out.push((m, b)),
        }
    }
    out
}

fn crypto_span(conn: &Connection) -> Option<(NaiveDate, NaiveDate)> {
    let (mn, mx): (Option<i64>, Option<i64>) =
        conn.query_row("SELECT MIN(ts) mn, MAX(ts) mx FROM crypto_events", [], |r| Ok((r.get(0)?, r.get(1)?))).ok()?;
    Some((month_start(ms_to_date(mn?)), month_start(ms_to_date(mx?))))
}

/// Per month, every asset's cumulative ledger
/// holdings (clamped at 0) x its month-end close (forward-filled; stables
/// = 1 USD), summed, x that month's USD/HKD.
fn crypto_hkd_column(ctx: &Ctx, months: &[NaiveDate], usd_hkd: &[f64]) -> Vec<f64> {
    let export = ctx.export();
    let events = &export.events;
    if events.is_empty() {
        return vec![0.0; months.len()];
    }
    let idx: HashMap<NaiveDate, usize> = months.iter().enumerate().map(|(i, m)| (*m, i)).collect();
    // per asset (sorted), month -> last cumsum
    let mut holdings: BTreeMap<String, Vec<Option<f64>>> = BTreeMap::new();
    let mut run: HashMap<String, f64> = HashMap::new();
    for e in events {
        let c = run.entry(e.asset.clone()).or_insert(0.0);
        *c += e.delta;
        let m = month_start(ms_to_date(e.ts));
        let series = holdings.entry(e.asset.clone()).or_insert_with(|| vec![None; months.len()]);
        if let Some(&i) = idx.get(&m) {
            series[i] = Some(*c);
        }
    }
    let mut prices: HashMap<(String, NaiveDate), f64> = HashMap::new();
    for (a, by_month) in &export.prices_monthly {
        for (m, c) in by_month {
            prices.insert((a.clone(), month_date(m)), *c);
        }
    }
    let mut usd = vec![0.0; months.len()];
    for (asset, series) in &holdings {
        let stable = super::prices::is_stable(asset);
        let (mut amt, mut close): (Option<f64>, Option<f64>) = (None, None);
        for (i, m) in months.iter().enumerate() {
            if series[i].is_some() {
                amt = series[i];
            }
            if let Some(c) = prices.get(&(asset.clone(), *m)) {
                close = Some(*c);
            }
            let q = amt.unwrap_or(0.0).max(0.0);
            let px = if stable { 1.0 } else { close.unwrap_or(0.0) };
            usd[i] += q * px;
        }
    }
    usd.iter().zip(usd_hkd).map(|(u, r)| r2(u * r)).collect()
}

/// One HKD column per (source, account) bank
/// ledger (forward-filled, at that month's FX), crypto_hkd,
/// investments_hkd, receivables_hkd, total_hkd and total_eur. Columns
/// whose currency has no FX pair are flagged no_fx and left out.
pub fn build_balance_sheet(ctx: &Ctx, from: Option<&str>, to: Option<&str>) -> Sheet {
    let mut sheet = ctx.full_sheet().clone();
    sheet.filter(from, to);
    sheet
}

/// The unfiltered sheet (memoized on the Ctx).
pub(super) fn build_full(ctx: &Ctx) -> Sheet {
    let conn = ctx.conn;
    let mut columns: Vec<Column> = Vec::new();
    let mut frames: HashMap<String, Vec<(NaiveDate, Option<f64>)>> = HashMap::new();
    for l in ctx.ledgers().iter() {
        let key = bank_key(&l.source, &l.account);
        let method = balance_method(&l.source);
        if method == "none" {
            continue; // a liability (credit card): its payments already left a bank account
        }
        let bal = monthly_balance(l, method);
        if !bal.is_empty() {
            frames.insert(key.clone(), bal);
        }
        let currency = ledger_currency(conn, &l.source, &l.account);
        columns.push(Column { label: source_label(&l.source, &l.account), key, currency, no_fx: false });
    }

    let mut candidates: Vec<NaiveDate> = Vec::new();
    for f in frames.values() {
        candidates.push(f[0].0);
        candidates.push(f[f.len() - 1].0);
    }
    if let Some((a, b)) = crypto_span(conn) {
        candidates.push(a);
        candidates.push(b);
    }
    let (min_m, max_m) = match (candidates.iter().min(), candidates.iter().max()) {
        (Some(a), Some(b)) => (*a, *b),
        _ => (month_start(ctx.today), month_start(ctx.today)),
    };
    let months = month_range(min_m, max_m);
    let n = months.len();
    let idx: HashMap<NaiveDate, usize> = months.iter().enumerate().map(|(i, m)| (*m, i)).collect();

    // bank columns: left join, forward fill, fill 0
    let mut native: Vec<Vec<f64>> = Vec::with_capacity(columns.len());
    for c in &columns {
        let mut v: Vec<Option<f64>> = vec![None; n];
        if let Some(f) = frames.get(&c.key) {
            for (m, b) in f {
                if let Some(&i) = idx.get(m) {
                    v[i] = *b;
                }
            }
        }
        native.push(ffill(&v));
    }

    // fx: left join, forward fill, fill 0
    let rates = monthly_rates(ctx, min_m, max_m);
    let rate_cols: Vec<Vec<f64>> = rates.cols.iter().map(|c| ffill(c)).collect();

    let mut banks: Vec<(String, Vec<f64>)> = Vec::new();
    for (c, v) in columns.iter_mut().zip(native) {
        if c.currency == rates.base {
            banks.push((c.key.clone(), v)); // already the target unit
            continue;
        }
        match rates.col(&c.currency) {
            Some(ri) => banks.push((c.key.clone(), v.iter().zip(&rate_cols[ri]).map(|(a, r)| r2(a * r)).collect())),
            None => c.no_fx = true,
        }
    }

    let usd_hkd = &rates.filled("USD");
    let crypto_hkd = crypto_hkd_column(ctx, &months, usd_hkd);

    let by_holding = super::holdings::investments_hkd_by_holding(ctx, &months);
    let investments_hkd: Vec<f64> =
        (0..n).map(|i| r2(by_holding.iter().fold(0.0, |acc, (_, v)| acc + v[i]))).collect();

    // a deposit is cash that left a bank account but is still owned; history
    // must render without it if anything there fails
    let receivables_hkd = super::receivables::monthly_hkd(ctx, &months).unwrap_or_else(|_| vec![0.0; n]);

    let eur_hkd = &rates.filled(crate::config::SECONDARY_CURRENCY);
    let mut total_hkd = Vec::with_capacity(n);
    let mut total_eur = Vec::with_capacity(n);
    for i in 0..n {
        let banks_sum = banks.iter().fold(0.0, |acc, (_, v)| acc + v[i]);
        let t = r2(banks_sum + crypto_hkd[i] + investments_hkd[i] + receivables_hkd[i]);
        total_hkd.push(t);
        total_eur.push(if eur_hkd[i] > 0.0 { r2(t / eur_hkd[i]) } else { 0.0 });
    }

    Sheet { months, columns, banks, crypto_hkd, investments_hkd, receivables_hkd, total_hkd, total_eur }
}

/// forward_fill then fill_null(0.0).
pub fn ffill(v: &[Option<f64>]) -> Vec<f64> {
    let mut last = None;
    v.iter()
        .map(|x| {
            if x.is_some() {
                last = *x;
            }
            last.unwrap_or(0.0)
        })
        .collect()
}
