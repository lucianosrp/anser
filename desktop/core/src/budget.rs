//! Monthly budgets per spending category: what each category spent (the
//! Spending page's definition: `drill::is_spending`, the owner's share), a
//! suggestion from history, and how the running month is pacing. UI-free; the
//! budgets themselves live in the app's `budgets.toml`.

use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike, NaiveDate};

use crate::drill;
use crate::ledger::Ledger;

/// Complete months a suggestion looks back over.
pub const SUGGEST_MONTHS: usize = 6;
/// A projection this far over the budget (a share of it) still reads as on
/// track: budgets and projections both come from medians, so a few dollars
/// over is noise.
pub const NEAR_TOLERANCE: f64 = 0.05;

/// "YYYY-MM" -> month -> category -> HKD spent (positive, the owner's share).
pub type ByMonth = BTreeMap<String, HashMap<String, f64>>;

/// Spending per category for every month in the ledger.
pub fn by_month(ledger: &Ledger) -> ByMonth {
    let mut out = ByMonth::new();
    for e in &ledger.entries {
        if !drill::is_spending(e) {
            continue;
        }
        let (Some(v), Some(m)) = (drill::my_hkd(e), e.date.get(..7)) else { continue };
        *out.entry(m.to_string()).or_default().entry(e.category.clone()).or_default() += -v;
    }
    out
}

/// The month `n` months after `month` ("YYYY-MM"; `n` may be negative).
pub fn shift_month(month: &str, n: i32) -> String {
    let y: i32 = month.get(..4).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let m: i32 = month.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    let i = y * 12 + (m - 1) + n;
    format!("{:04}-{:02}", i.div_euclid(12), i.rem_euclid(12) + 1)
}

/// The `n` months before `month`, oldest first.
pub fn months_before(month: &str, n: usize) -> Vec<String> {
    (1..=n as i32).rev().map(|k| shift_month(month, -k)).collect()
}

pub fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2. }
}

/// A budget-shaped number: to the nearest 50 under 1k, 100 under 5k, 500
/// under 20k, 1000 above. Never rounds a real spend down to zero.
pub fn nice_round(v: f64) -> f64 {
    if v <= 0. {
        return 0.;
    }
    let step = match v {
        x if x < 1_000. => 50.,
        x if x < 5_000. => 100.,
        x if x < 20_000. => 500.,
        _ => 1_000.,
    };
    ((v / step).round() * step).max(step)
}

/// For each category spent on in the `SUGGEST_MONTHS` complete months before
/// `month`: the median month (a month without spending counts as 0), rounded
/// nicely. Categories whose median is 0 are left out.
pub fn suggest(by: &ByMonth, month: &str) -> BTreeMap<String, f64> {
    let window = months_before(month, SUGGEST_MONTHS);
    let mut cats: Vec<&String> = window.iter().filter_map(|m| by.get(m)).flat_map(|c| c.keys()).collect();
    cats.sort();
    cats.dedup();
    cats.into_iter()
        .filter(|c| !crate::categories::reimbursable(c))
        .filter_map(|c| {
            let vals: Vec<f64> = window.iter().map(|m| by.get(m).and_then(|x| x.get(c)).copied().unwrap_or(0.)).collect();
            let v = nice_round(median(&vals));
            (v > 0.).then(|| (c.clone(), v))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// spent more than the budget already
    Over,
    /// under budget, but the projection ends the month over it (by more than NEAR_TOLERANCE)
    Near,
    OnTrack,
    /// spending in a category without a budget
    Unbudgeted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub category: String,
    pub budget: Option<f64>,
    pub spent: f64,
    /// spent + what the category usually spends in the rest of the month
    pub projected: f64,
}

impl Line {
    pub fn status(&self) -> Status {
        match self.budget {
            None => Status::Unbudgeted,
            Some(b) if self.spent > b + 0.5 => Status::Over,
            Some(b) if self.projected > b * (1. + NEAR_TOLERANCE) + 0.5 => Status::Near,
            Some(_) => Status::OnTrack,
        }
    }
    /// budget - spent (negative when over)
    pub fn remaining(&self) -> f64 {
        self.budget.unwrap_or(0.) - self.spent
    }
}

/// The running month against the budgets.
#[derive(Debug, Clone, PartialEq)]
pub struct MonthStatus {
    /// "YYYY-MM"
    pub month: String,
    /// day of the month today (1-based) and the month's length
    pub day: u32,
    pub days: u32,
    /// budgeted categories first (biggest budget first), then unbudgeted ones by spend
    pub lines: Vec<Line>,
}

impl MonthStatus {
    /// Share of the month gone, counting today as done (0..=1).
    pub fn elapsed(&self) -> f64 {
        self.day as f64 / self.days.max(1) as f64
    }
    pub fn budgeted(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter().filter(|l| l.budget.is_some())
    }
    pub fn unbudgeted(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter().filter(|l| l.budget.is_none())
    }
    pub fn budget_total(&self) -> f64 {
        self.budgeted().filter_map(|l| l.budget).sum()
    }
    /// spent in budgeted categories
    pub fn spent_total(&self) -> f64 {
        self.budgeted().map(|l| l.spent).sum()
    }
    pub fn projected_total(&self) -> f64 {
        self.budgeted().map(|l| l.projected).sum()
    }
    pub fn unbudgeted_total(&self) -> f64 {
        self.unbudgeted().map(|l| l.spent).sum()
    }
    pub fn count(&self, s: Status) -> usize {
        self.lines.iter().filter(|l| l.status() == s).count()
    }
    /// Budget used against month gone: 1.0 = exactly on pace, above = ahead of it.
    pub fn pace(&self) -> Option<f64> {
        let b = self.budget_total();
        (b > 0.).then(|| self.spent_total() / (b * self.elapsed()).max(1e-9))
    }
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(30)
}

/// How `today`'s month is going. The projection adds, per category, the
/// median of what the last `SUGGEST_MONTHS` complete months spent after this
/// day of the month: rent paid on the 1st doesn't project as 30 rents, and a
/// category that usually spends late in the month still counts.
pub fn month_status(ledger: &Ledger, budgets: &BTreeMap<String, f64>, today: NaiveDate) -> MonthStatus {
    let month = format!("{:04}-{:02}", today.year(), today.month());
    let day = today.day();
    let window = months_before(&month, SUGGEST_MONTHS);
    // this month so far, and each window month after `day`
    let mut now: HashMap<String, f64> = HashMap::new();
    let mut rest: HashMap<String, HashMap<String, f64>> = HashMap::new();
    for e in &ledger.entries {
        if !drill::is_spending(e) {
            continue;
        }
        let (Some(v), Some(m)) = (drill::my_hkd(e), e.date.get(..7)) else { continue };
        let d: u32 = e.date.get(8..10).and_then(|s| s.parse().ok()).unwrap_or(1);
        if m == month {
            if d <= day {
                *now.entry(e.category.clone()).or_default() += -v;
            }
        } else if d > day && window.iter().any(|w| w == m) {
            *rest.entry(e.category.clone()).or_default().entry(m.to_string()).or_default() += -v;
        }
    }
    let typical_rest = |c: &str| -> f64 {
        let per = rest.get(c);
        let vals: Vec<f64> = window.iter().map(|m| per.and_then(|p| p.get(m)).copied().unwrap_or(0.)).collect();
        median(&vals)
    };
    let mut lines: Vec<Line> = budgets.iter().filter(|(_, b)| **b > 0.).map(|(c, b)| {
        let spent = now.get(c).copied().unwrap_or(0.);
        Line { category: c.clone(), budget: Some(*b), spent, projected: spent + typical_rest(c) }
    }).collect();
    lines.sort_by(|a, b| b.budget.unwrap_or(0.).total_cmp(&a.budget.unwrap_or(0.)).then(a.category.cmp(&b.category)));
    let mut extra: Vec<Line> = now.iter()
        .filter(|(c, v)| !budgets.get(*c).is_some_and(|b| *b > 0.) && **v > 0.5)
        .map(|(c, v)| Line { category: c.clone(), budget: None, spent: *v, projected: *v + typical_rest(c) })
        .collect();
    extra.sort_by(|a, b| b.spent.total_cmp(&a.spent));
    lines.extend(extra);
    MonthStatus { month, day, days: days_in_month(today.year(), today.month()), lines }
}

/// One category's spend for the 12 months up to and including `month`, oldest first.
pub fn history(by: &ByMonth, category: &str, month: &str) -> Vec<(String, f64)> {
    let mut ms = months_before(month, 11);
    ms.push(month.to_string());
    ms.into_iter().map(|m| {
        let v = by.get(&m).and_then(|c| c.get(category)).copied().unwrap_or(0.);
        (m, v)
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{Entry, Kind};

    fn e(date: &str, cat: &str, amount: f64, share: f64) -> Entry {
        Entry {
            kind: Kind::Card, date: date.into(), source: "card".into(), account: String::new(), description: "x".into(),
            category: cat.into(), amount, currency: "HKD".into(), hkd: Some(amount), share, haystack: String::new(), raw_hash: String::new(),
        }
    }

    fn ledger(entries: Vec<Entry>) -> Ledger {
        Ledger { entries, ..Default::default() }
    }

    #[test]
    fn months_shift_across_years() {
        assert_eq!(shift_month("2026-01", -1), "2025-12");
        assert_eq!(shift_month("2026-11", 3), "2027-02");
        assert_eq!(months_before("2026-02", 3), vec!["2025-11", "2025-12", "2026-01"]);
    }

    #[test]
    fn rounding_and_median() {
        assert_eq!(nice_round(1234.), 1200.);
        assert_eq!(nice_round(12.), 50.);
        assert_eq!(nice_round(7_260.), 7_500.);
        assert_eq!(nice_round(0.), 0.);
        assert_eq!(median(&[3., 1., 2.]), 2.);
        assert_eq!(median(&[4., 1., 2., 3.]), 2.5);
    }

    #[test]
    fn suggests_the_median_with_empty_months_as_zero() {
        let l = ledger(vec![
            e("2026-08-03", "Dining", -1000., 1.), e("2026-07-03", "Dining", -1200., 1.),
            e("2026-06-03", "Dining", -900., 1.), e("2026-05-03", "Dining", -1100., 1.),
            e("2026-04-03", "Dining", -1000., 1.), e("2026-03-03", "Dining", -5000., 1.),
            e("2026-08-01", "Rent", -20000., 0.5), // shared: the owner's half
            e("2026-08-05", "Gym", -600., 1.), // one month of six: median 0
            e("2026-08-06", "Office (reimbursed)", -3000., 1.),
            e("2026-08-07", "Internal transfer", -9000., 1.),
        ]);
        let s = suggest(&by_month(&l), "2026-09");
        assert_eq!(s.get("Dining"), Some(&1100.), "median 1050, to the nearest 100");
        assert_eq!(s.get("Rent"), None, "one month of six: the median is 0");
        assert!(!s.contains_key("Gym") && !s.contains_key("Office (reimbursed)") && !s.contains_key("Internal transfer"));
    }

    #[test]
    fn status_projects_with_the_usual_rest_of_month() {
        let mut entries = vec![e("2026-09-01", "Rent", -10000., 1.), e("2026-09-05", "Dining", -800., 1.),
                               e("2026-09-06", "Shopping", -300., 1.)];
        for m in ["2026-03", "2026-04", "2026-05", "2026-06", "2026-07", "2026-08"] {
            entries.push(e(&format!("{m}-01"), "Rent", -10000., 1.));
            entries.push(e(&format!("{m}-20"), "Dining", -1500., 1.));
        }
        let l = ledger(entries);
        let budgets: BTreeMap<String, f64> = [("Rent".to_string(), 10000.), ("Dining".to_string(), 2000.)].into();
        let st = month_status(&l, &budgets, NaiveDate::from_ymd_opt(2026, 9, 10).unwrap());
        assert_eq!((st.day, st.days), (10, 30));
        let rent = &st.lines[0];
        assert_eq!((rent.category.as_str(), rent.spent, rent.projected), ("Rent", 10000., 10000.));
        assert_eq!(rent.status(), Status::OnTrack);
        let dining = &st.lines[1];
        assert_eq!((dining.spent, dining.projected), (800., 2300.));
        assert_eq!(dining.status(), Status::Near);
        assert_eq!(st.lines[2].status(), Status::Unbudgeted);
        assert_eq!(st.unbudgeted_total(), 300.);
        assert_eq!(st.count(Status::Near), 1);
        assert!((st.pace().unwrap() - 10800. / (12000. / 3.)).abs() < 1e-9);
        let h = history(&by_month(&l), "Dining", "2026-09");
        assert_eq!(h.len(), 12);
        assert_eq!(h.last(), Some(&("2026-09".to_string(), 800.)));
    }
}
