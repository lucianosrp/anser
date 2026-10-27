//! Deep dives: everything the ledger says about one merchant or one category.
//! UI-free; the app shows them in a dialog.

use std::collections::{BTreeMap, HashMap};

use crate::drill::{self, my_hkd};
use crate::filter::{self, fill_months};
use crate::ledger::{Entry, Kind, Ledger};

fn money(e: &Entry) -> bool {
    matches!(e.kind, Kind::Bank | Kind::Card)
}

fn month(e: &Entry) -> String {
    e.date.get(..7).unwrap_or("").to_string()
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2. }
}

fn days(a: &str, b: &str) -> Option<i64> {
    let p = |d: &str| chrono::NaiveDate::parse_from_str(d.get(..10)?, "%Y-%m-%d").ok();
    Some((p(b)? - p(a)?).num_days())
}

/// "2026-08" -> "2026 Q3"
fn quarter(m: &str) -> String {
    let q = m.get(5..7).and_then(|x| x.parse::<u32>().ok()).map_or(1, |x| (x - 1) / 3 + 1);
    format!("{} Q{q}", m.get(..4).unwrap_or(""))
}

/// A merchant's name from a description, without the card acquirer in
/// front: "BBMSL*Blue Bottle Hong Kong HK" -> "Blue Bottle".
pub fn readable(desc: &str) -> String {
    crate::recurring::display_name(drill::strip_acquirer(desc))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MerchantDive {
    pub key: String,
    /// a readable name (from the newest row)
    pub name: String,
    /// bank and card rows, newest first
    pub rows: Vec<usize>,
    /// HKD at the owner's share, positive
    pub spent: f64,
    pub received: f64,
    pub first: String,
    pub last: String,
    /// spent per month, from the first visit's month to the last's
    pub monthly: Vec<(String, f64)>,
    /// purchases (money out)
    pub visits: usize,
    pub months_active: usize,
    /// purchases per calendar month over the whole span
    pub per_month: f64,
    pub median_gap_days: Option<f64>,
    pub avg_ticket: f64,
    pub median_ticket: f64,
    /// median ticket per quarter, oldest first
    pub price: Vec<(String, f64)>,
    /// median ticket, the last 6 months of visits against the 6 before (%)
    pub price_change: Option<f64>,
    /// (category, rows, spent)
    pub categories: Vec<(String, usize, f64)>,
    /// (account, rows)
    pub accounts: Vec<(String, usize)>,
}

/// Everything about merchant `key` (a [`drill::merchant_key`]).
pub fn merchant(ledger: &Ledger, key: &str) -> MerchantDive {
    let key = key.to_lowercase();
    let rows: Vec<usize> = ledger.entries.iter().enumerate()
        .filter(|(_, e)| money(e) && !key.is_empty() && drill::merchant_text(&e.description).starts_with(&key))
        .map(|(i, _)| i)
        .collect();
    let es: Vec<&Entry> = rows.iter().map(|&i| &ledger.entries[i]).collect();
    let mut d = MerchantDive {
        name: es.first().map(|e| readable(&e.description)).filter(|n| !n.is_empty())
            .unwrap_or_else(|| filter::title(&key)),
        key,
        first: es.iter().map(|e| e.date.clone()).min().unwrap_or_default(),
        last: es.iter().map(|e| e.date.clone()).max().unwrap_or_default(),
        ..Default::default()
    };
    let mut by_month: BTreeMap<String, f64> = BTreeMap::new();
    let mut tickets: Vec<(String, f64)> = Vec::new(); // (date, HKD), oldest last
    let mut cats: HashMap<String, (usize, f64)> = HashMap::new();
    let mut accts: HashMap<String, usize> = HashMap::new();
    for e in &es {
        *accts.entry(filter::account_label(e)).or_default() += 1;
        let c = cats.entry(e.category.clone()).or_default();
        c.0 += 1;
        let Some(v) = my_hkd(e) else { continue };
        if v < 0. {
            d.spent -= v;
            c.1 -= v;
            *by_month.entry(month(e)).or_default() -= v;
            tickets.push((e.date.clone(), -v));
        } else {
            d.received += v;
        }
    }
    d.visits = tickets.len();
    d.months_active = by_month.len();
    d.monthly = fill_months(&by_month);
    d.per_month = if d.monthly.is_empty() { 0. } else { d.visits as f64 / d.monthly.len() as f64 };
    tickets.sort_by(|a, b| a.0.cmp(&b.0));
    let gaps: Vec<f64> = tickets.windows(2).filter_map(|w| days(&w[0].0, &w[1].0)).map(|g| g as f64).collect();
    d.median_gap_days = (!gaps.is_empty()).then(|| median(&mut gaps.clone()));
    let mut amounts: Vec<f64> = tickets.iter().map(|t| t.1).collect();
    d.avg_ticket = if amounts.is_empty() { 0. } else { amounts.iter().sum::<f64>() / amounts.len() as f64 };
    d.median_ticket = median(&mut amounts);
    let mut by_q: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for (date, v) in &tickets {
        by_q.entry(quarter(date)).or_default().push(*v);
    }
    d.price = by_q.into_iter().map(|(q, mut v)| (q, median(&mut v))).collect();
    // the last 6 months of visits against the 6 months before them
    if let Some((last, _)) = tickets.last() {
        let age = |date: &str| days(date, last).unwrap_or(0);
        let mut recent: Vec<f64> = tickets.iter().filter(|t| age(&t.0) < 183).map(|t| t.1).collect();
        let mut before: Vec<f64> = tickets.iter().filter(|t| (183..366).contains(&age(&t.0))).map(|t| t.1).collect();
        if recent.len() >= 3 && before.len() >= 3 {
            let (r, b) = (median(&mut recent), median(&mut before));
            if b > 0. {
                d.price_change = Some((r - b) / b * 100.);
            }
        }
    }
    d.categories = cats.into_iter().map(|(c, (n, v))| (c, n, v)).collect();
    d.categories.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.total_cmp(&a.2)));
    d.accounts = accts.into_iter().collect();
    d.accounts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    d.rows = rows;
    d
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CategoryDive {
    pub category: String,
    /// bank and card rows, newest first
    pub rows: Vec<usize>,
    /// money in dominates (Salary, Interest): the figures are what came in
    pub income: bool,
    /// all time, HKD at the owner's share, positive
    pub total: f64,
    /// the 12 months up to and including `month`
    pub last12: f64,
    pub avg12: f64,
    /// of all spending over those 12 months (a spending category only)
    pub share12: Option<f64>,
    pub this_month: f64,
    /// the 24 months up to `month`
    pub monthly: Vec<(String, f64)>,
    /// (merchant key, name, rows, total), biggest first
    pub merchants: Vec<(String, String, usize, f64)>,
    pub first: String,
    pub last: String,
}

/// Everything about `category` (exact name), seen from `month` ("YYYY-MM",
/// usually the running one).
pub fn category(ledger: &Ledger, category: &str, month_now: &str) -> CategoryDive {
    let rows: Vec<usize> = ledger.entries.iter().enumerate()
        .filter(|(_, e)| money(e) && e.category.eq_ignore_ascii_case(category))
        .map(|(i, _)| i)
        .collect();
    let es: Vec<&Entry> = rows.iter().map(|&i| &ledger.entries[i]).collect();
    let (inflow, outflow) = es.iter().filter_map(|e| my_hkd(e)).fold((0., 0.), |(i, o), v| if v > 0. { (i + v, o) } else { (i, o - v) });
    let income = inflow > outflow;
    // the side of the money this category is about, positive
    let value = |e: &Entry| my_hkd(e).map(|v| if income { v.max(0.) } else { (-v).max(0.) }).unwrap_or(0.);

    let window: Vec<String> = (0..24).rev().map(|k| crate::budget::shift_month(month_now, -k)).collect();
    let last12: Vec<&String> = window.iter().rev().take(12).collect();
    let mut by_month: HashMap<String, f64> = HashMap::new();
    let mut merchants: HashMap<String, (String, usize, f64)> = HashMap::new();
    let mut d = CategoryDive {
        category: es.first().map(|e| e.category.clone()).unwrap_or_else(|| category.to_string()),
        income,
        first: es.iter().map(|e| e.date.clone()).min().unwrap_or_default(),
        last: es.iter().map(|e| e.date.clone()).max().unwrap_or_default(),
        ..Default::default()
    };
    for e in &es {
        let v = value(e);
        d.total += v;
        *by_month.entry(month(e)).or_default() += v;
        let k = drill::merchant_key(&e.description);
        let k = if k.is_empty() { e.description.to_lowercase() } else { k };
        let m = merchants.entry(k).or_insert_with(|| (readable(&e.description), 0, 0.));
        m.1 += 1;
        m.2 += v;
    }
    d.monthly = window.iter().map(|m| (m.clone(), by_month.get(m).copied().unwrap_or(0.))).collect();
    d.last12 = last12.iter().map(|m| by_month.get(*m).copied().unwrap_or(0.)).sum();
    d.avg12 = d.last12 / 12.;
    d.this_month = by_month.get(month_now).copied().unwrap_or(0.);
    let spending = !income && es.iter().any(|e| drill::is_spending(e));
    if spending {
        let all: f64 = ledger.entries.iter()
            .filter(|e| drill::is_spending(e) && last12.iter().any(|m| e.date.starts_with(m.as_str())))
            .filter_map(|e| my_hkd(e))
            .map(|v| -v)
            .sum();
        d.share12 = (all > 0.).then(|| d.last12 / all * 100.);
    }
    d.merchants = merchants.into_iter().map(|(k, (n, c, v))| (k, n, c, v)).collect();
    d.merchants.sort_by(|a, b| b.3.total_cmp(&a.3).then(b.2.cmp(&a.2)));
    d.rows = rows;
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(date: &str, desc: &str, cat: &str, amount: f64) -> Entry {
        Entry {
            kind: Kind::Card, date: date.into(), source: "hsbc_card".into(), account: String::new(), description: desc.into(),
            category: cat.into(), amount, currency: "HKD".into(), hkd: Some(amount), share: 1.0, haystack: String::new(), raw_hash: String::new(),
        }
    }

    fn ledger() -> Ledger {
        // newest first, like the real ledger
        Ledger { entries: vec![
            e("2026-09-20", "BBMSL*SUSHIRO MOS", "Dining", -200.),
            e("2026-09-02", "SUSHIRO MK", "Dining", -180.),
            e("2026-08-15", "SUSHIRO MK", "Dining", -160.),
            e("2026-08-01", "Landlord", "Rent", -9000.),
            e("2026-06-10", "SUSHIRO MK", "Dining", 50.),
            e("2026-03-05", "SUSHIRO TST", "Groceries", -100.),
            e("2026-02-05", "SUSHIRO TST", "Dining", -100.),
            e("2026-01-05", "SUSHIRO TST", "Dining", -100.),
            e("2025-12-31", "Salary", "Salary", 30000.),
        ], ..Default::default() }
    }

    #[test]
    fn merchant_numbers() {
        let d = merchant(&ledger(), "sushiro");
        assert_eq!(d.rows, vec![0, 1, 2, 4, 5, 6, 7]);
        assert_eq!((d.spent, d.received, d.visits), (840., 50., 6));
        assert_eq!((d.first.as_str(), d.last.as_str()), ("2026-01-05", "2026-09-20"));
        assert_eq!(d.monthly.len(), 9); // Jan..Sep, gaps filled
        assert_eq!(d.monthly[0], ("2026-01".to_string(), 100.));
        assert_eq!(d.monthly[3], ("2026-04".to_string(), 0.));
        assert_eq!(d.months_active, 5);
        assert!((d.per_month - 6. / 9.).abs() < 1e-9);
        assert_eq!(d.avg_ticket, 140.);
        assert_eq!(d.median_ticket, 130.);
        assert_eq!(d.price, vec![("2026 Q1".to_string(), 100.), ("2026 Q3".to_string(), 180.)]);
        assert_eq!(d.categories[0], ("Dining".to_string(), 6, 740.));
        assert_eq!(d.accounts, vec![("HSBC card".to_string(), 7)]);
        // 3 visits in the last 6 months (180) vs 3 before (100)
        assert_eq!(d.price_change.map(|p| p.round()), Some(80.));
        assert!(d.median_gap_days.is_some());
        assert!(merchant(&ledger(), "nowhere").rows.is_empty());
        assert!(!d.name.contains('*'), "{}", d.name);
        assert!(!readable("BBMSL*% Blue Bottle Hong Kong HK").starts_with('%'));
    }

    #[test]
    fn category_numbers() {
        let d = category(&ledger(), "dining", "2026-09");
        assert_eq!(d.category, "Dining");
        assert!(!d.income);
        assert_eq!(d.total, 740.);
        assert_eq!(d.this_month, 380.);
        assert_eq!(d.monthly.len(), 24);
        assert_eq!(d.monthly.last(), Some(&("2026-09".to_string(), 380.)));
        assert_eq!(d.last12, 740.);
        // of 840 + 9000 spent in the 12 months
        assert_eq!(d.share12.map(|s| (s * 10.).round() / 10.), Some(7.5));
        assert_eq!(d.merchants[0].0, "sushiro");
        assert_eq!(d.merchants[0].2, 6);
        let s = category(&ledger(), "Salary", "2026-09");
        assert!(s.income);
        assert_eq!(s.total, 30000.);
        assert_eq!(s.share12, None);
    }
}
