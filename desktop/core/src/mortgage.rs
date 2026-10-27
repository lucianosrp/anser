//! Mortgage maths: the annuity payment, the schedule with overpayments and
//! rate changes, balances and interest as of a date, equity, and the "what
//! if I paid more" comparison. Pure and cent-rounded per payment; the Home
//! tab (home setup, mortgage mode) presents it.
//!
//! Dates: payment k (1, 2, ...) falls on `payment_day` (default: the start's
//! day, at most 28) of the month k months after the start's month. An
//! overpayment or a rate change dated `d` takes effect on the first payment
//! on or after `d`: an overpayment is extra principal paid with it, a rate
//! change applies from it.

use chrono::{Datelike, Months, NaiveDate};

/// Longest schedule built, a guard against a runaway loop.
const MAX_PAYMENTS: usize = 1200;

#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub principal: f64,
    pub annual_rate_pct: f64,
    pub term_months: u32,
    pub start: NaiveDate,
    pub payment_day: Option<u32>,
    pub overpayments: Vec<(NaiveDate, f64)>,
    pub rate_changes: Vec<(NaiveDate, f64)>,
}

/// One payment of the schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub date: NaiveDate,
    /// the regular payment, plus any overpayment made with it
    pub payment: f64,
    pub interest: f64,
    /// principal repaid by this payment (overpayment included)
    pub principal: f64,
    /// of which an overpayment made with it
    pub extra: f64,
    /// what is owed after it
    pub balance: f64,
    /// the yearly rate this payment's interest used
    pub rate_pct: f64,
}

fn cents(x: f64) -> f64 {
    (x * 100.).round() / 100.
}

/// The level monthly payment that repays `principal` over `months` at a
/// yearly rate of `annual_rate_pct` percent, rounded to the cent.
pub fn payment(principal: f64, annual_rate_pct: f64, months: u32) -> f64 {
    if months == 0 || principal <= 0. {
        return 0.;
    }
    let r = annual_rate_pct / 1200.;
    if r.abs() < 1e-12 {
        return cents(principal / months as f64);
    }
    let f = (1. + r).powi(months as i32);
    cents(principal * r * f / (f - 1.))
}

/// The one of principal, rate and term the other two and the monthly
/// payment pin down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Third {
    Principal(f64),
    Rate(f64),
    Term(u32),
}

/// Given exactly two of principal, yearly rate and term (months) and the
/// payment, the third: the term that repays the principal, the principal the
/// payment supports, or the rate (found by bisection) that makes the
/// payment. None unless exactly two are given, or when no such value exists
/// (a payment that doesn't cover the interest, a rate outside 0 to 30).
pub fn third(principal: Option<f64>, rate_pct: Option<f64>, term: Option<u32>, pay: f64) -> Option<Third> {
    if pay <= 0. {
        return None;
    }
    match (principal, rate_pct, term) {
        (Some(p), Some(r), None) if p > 0. => months_to_clear(p, r, pay).filter(|n| (1..=600).contains(n)).map(Third::Term),
        (None, Some(r), Some(n)) if n > 0 => {
            let m = r / 1200.;
            let p = if m.abs() < 1e-12 { pay * n as f64 } else { pay * (1. - (1. + m).powi(-(n as i32))) / m };
            Some(Third::Principal(cents(p)))
        }
        (Some(p), None, Some(n)) if p > 0. && n > 0 => {
            // payment(p, r, n) rises with r: bisect on [0, 30]
            let at = |r: f64| payment(p, r, n);
            if pay < at(0.) - 0.01 || pay > at(30.) + 0.01 {
                return None;
            }
            let (mut lo, mut hi) = (0., 30.);
            for _ in 0..60 {
                let mid = (lo + hi) / 2.;
                if at(mid) < pay { lo = mid } else { hi = mid }
            }
            Some(Third::Rate(((lo + hi) / 2. * 100.).round() / 100.))
        }
        _ => None,
    }
}

/// How many months paying `pay` a month takes to clear `balance` at the
/// rate, rounded up; None when the payment doesn't cover the interest.
fn months_to_clear(balance: f64, annual_rate_pct: f64, pay: f64) -> Option<u32> {
    let r = annual_rate_pct / 1200.;
    if pay <= 0. {
        return None;
    }
    if r.abs() < 1e-12 {
        return Some((balance / pay).ceil() as u32);
    }
    let x = 1. - balance * r / pay;
    (x > 0.).then(|| (-x.ln() / (1. + r).ln()).ceil() as u32)
}

fn payment_date(start: NaiveDate, day: u32, k: u32) -> Option<NaiveDate> {
    let first = start.with_day(1)?.checked_add_months(Months::new(k))?;
    first.with_day(day.clamp(1, 28))
}

/// The whole schedule, from the first payment to the one that clears the
/// balance (or the term's last payment, which clears what is left).
pub fn schedule(p: &Params) -> Vec<Row> {
    let day = p.payment_day.unwrap_or_else(|| p.start.day().min(28));
    let mut rate = p.annual_rate_pct;
    let mut pay = payment(p.principal, rate, p.term_months);
    let mut balance = cents(p.principal);
    let mut rows = Vec::new();
    let mut over: Vec<(NaiveDate, f64)> = p.overpayments.clone();
    over.sort_by_key(|o| o.0);
    let mut changes = p.rate_changes.clone();
    changes.sort_by_key(|c| c.0);
    let (mut next_over, mut next_change) = (0, 0);
    for k in 1..=MAX_PAYMENTS as u32 {
        if balance <= 0.004 {
            break;
        }
        let Some(date) = payment_date(p.start, day, k) else { break };
        // a rate change from this payment on: the payment is recomputed for
        // the months the old one would still have needed
        let (old_rate, mut changed) = (rate, false);
        while next_change < changes.len() && changes[next_change].0 <= date {
            rate = changes[next_change].1;
            next_change += 1;
            changed = true;
        }
        if changed {
            let left = months_to_clear(balance, old_rate, pay).unwrap_or(p.term_months.saturating_sub(k - 1)).max(1);
            pay = payment(balance, rate, left);
        }
        let interest = cents(balance * rate / 1200.);
        let last = k >= p.term_months || balance + interest <= pay;
        let mut regular = if last { cents(balance + interest) } else { pay };
        let mut principal = cents(regular - interest);
        if principal > balance {
            principal = balance;
            regular = cents(principal + interest);
        }
        balance = cents(balance - principal);
        // overpayments dated up to this payment come with it
        let mut extra = 0.;
        while next_over < over.len() && over[next_over].0 <= date {
            extra += over[next_over].1;
            next_over += 1;
        }
        let extra = cents(extra.min(balance));
        balance = cents(balance - extra);
        rows.push(Row { date, payment: cents(regular + extra), interest, principal: cents(principal + extra), extra, balance, rate_pct: rate });
        if last && balance > 0. {
            // the term is over: whatever rounding left is the last payment's
            let r = rows.last_mut().expect("a row");
            r.payment = cents(r.payment + balance);
            r.principal = cents(r.principal + balance);
            r.balance = 0.;
            break;
        }
    }
    rows
}

/// What is owed on `date`: the balance after the last payment on or before
/// it; nothing before the start.
pub fn balance_on(p: &Params, rows: &[Row], date: NaiveDate) -> f64 {
    if date < p.start {
        return 0.;
    }
    rows.iter().take_while(|r| r.date <= date).last().map(|r| r.balance).unwrap_or(cents(p.principal))
}

/// Interest paid by the payments dated from `from` to `to`, both included.
pub fn interest_between(rows: &[Row], from: NaiveDate, to: NaiveDate) -> f64 {
    cents(rows.iter().filter(|r| r.date >= from && r.date <= to).map(|r| r.interest).sum())
}

/// Interest paid up to and including `date`.
pub fn interest_to(rows: &[Row], date: NaiveDate) -> f64 {
    cents(rows.iter().filter(|r| r.date <= date).map(|r| r.interest).sum())
}

pub fn total_interest(rows: &[Row]) -> f64 {
    cents(rows.iter().map(|r| r.interest).sum())
}

/// The date of the last payment.
pub fn payoff_date(rows: &[Row]) -> Option<NaiveDate> {
    rows.last().map(|r| r.date)
}

/// Payments still to come after `date`.
pub fn payments_left(rows: &[Row], date: NaiveDate) -> usize {
    rows.iter().filter(|r| r.date > date).count()
}

/// The regular payment now: the next payment's, else the last one's.
pub fn payment_now(rows: &[Row], date: NaiveDate) -> Option<f64> {
    rows.iter().find(|r| r.date > date).or(rows.last()).map(|r| r.payment)
}

/// The rate now: the one the last paid payment (else the first) used.
pub fn rate_now(rows: &[Row], date: NaiveDate) -> Option<f64> {
    rows.iter().take_while(|r| r.date <= date).last().or(rows.first()).map(|r| r.rate_pct)
}

/// Loan to value in percent, and the equity, when the home's value is known.
pub fn equity(balance: f64, home_value: f64) -> Option<(f64, f64)> {
    (home_value > 0.).then(|| (balance / home_value * 100., cents(home_value - balance)))
}

/// An extra amount paid every month from `today` on, against the schedule
/// as it is.
#[derive(Debug, Clone, PartialEq)]
pub struct WhatIf {
    pub months_saved: i64,
    pub interest_saved: f64,
    pub payoff: Option<NaiveDate>,
}

pub fn what_if(p: &Params, extra_monthly: f64, today: NaiveDate) -> WhatIf {
    let base = schedule(p);
    let mut q = p.clone();
    q.overpayments.extend(base.iter().filter(|r| r.date > today).map(|r| (r.date, extra_monthly)));
    let alt = schedule(&q);
    WhatIf {
        months_saved: base.len() as i64 - alt.len() as i64,
        interest_saved: cents(total_interest(&base) - total_interest(&alt)),
        payoff: payoff_date(&alt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn loan() -> Params {
        Params { principal: 1_000_000., annual_rate_pct: 3.5, term_months: 360, start: d("2026-01-15"), payment_day: None, overpayments: vec![], rate_changes: vec![] }
    }

    #[test]
    fn the_annuity_payment() {
        assert_eq!(payment(1_000_000., 3.5, 360), 4490.45);
        assert_eq!(payment(120_000., 0., 120), 1000., "no interest: even slices");
        assert_eq!(payment(0., 3., 12), 0.);
        assert_eq!(payment(1000., 3., 0), 0.);
    }

    #[test]
    fn the_schedule_runs_the_term_and_clears() {
        let rows = schedule(&loan());
        assert_eq!(rows.len(), 360);
        assert_eq!(rows[0].date, d("2026-02-15"), "the first payment a month after the start");
        assert_eq!(rows[0].interest, 2916.67, "1,000,000 * 3.5% / 12");
        assert_eq!(rows[0].principal, 1573.78);
        assert_eq!(rows[0].balance, 998_426.22);
        assert_eq!(rows.last().unwrap().balance, 0.);
        assert_eq!(payoff_date(&rows), Some(d("2056-01-15")));
        // every row adds up
        for r in &rows {
            assert!((r.payment - r.interest - r.principal).abs() < 0.011, "{r:?}");
        }
        // the final payment is within a few cents of the level one
        assert!((rows[359].payment - 4490.45).abs() < 3.0, "{}", rows[359].payment);
        let total = total_interest(&rows);
        assert!((total - (4490.45 * 360. - 1_000_000.)).abs() < 5., "{total}");
    }

    #[test]
    fn zero_rate_is_even_slices_and_a_short_final_payment() {
        let p = Params { principal: 1000., annual_rate_pct: 0., term_months: 3, ..loan() };
        let rows = schedule(&p);
        assert_eq!(rows.iter().map(|r| r.payment).collect::<Vec<_>>(), [333.33, 333.33, 333.34], "the last takes the cents");
        assert_eq!(total_interest(&rows), 0.);
        assert_eq!(rows[2].balance, 0.);
    }

    #[test]
    fn a_payment_day_and_month_ends() {
        let p = Params { start: d("2026-01-31"), payment_day: Some(1), term_months: 2, principal: 100., ..loan() };
        assert_eq!(schedule(&p).iter().map(|r| r.date).collect::<Vec<_>>(), [d("2026-02-01"), d("2026-03-01")]);
        let q = Params { start: d("2026-01-31"), term_months: 2, principal: 100., ..loan() };
        assert_eq!(schedule(&q)[0].date, d("2026-02-28"), "the day is at most 28");
    }

    #[test]
    fn an_overpayment_shortens_the_term_and_keeps_the_payment() {
        let base = schedule(&loan());
        let p = Params { overpayments: vec![(d("2027-01-01"), 100_000.)], ..loan() };
        let rows = schedule(&p);
        assert!(rows.len() < base.len() - 20, "{}", rows.len());
        let before = rows.iter().find(|r| r.date == d("2027-01-15")).unwrap();
        let reg = rows[1].payment;
        assert_eq!(reg, 4490.45, "the payment stays");
        assert!(before.payment > 100_000., "the lump comes with the January 2027 payment");
        assert_eq!(before.extra, 100_000.);
        assert_eq!(rows[1].extra, 0.);
        assert!(total_interest(&rows) < total_interest(&base) - 40_000.);
        assert_eq!(rows.last().unwrap().balance, 0.);
    }

    #[test]
    fn a_rate_change_recomputes_the_payment_for_the_rest() {
        let p = Params { rate_changes: vec![(d("2028-01-01"), 5.0)], ..loan() };
        let rows = schedule(&p);
        let (a, b) = (&rows[10], &rows[23]);
        assert_eq!((a.rate_pct, b.rate_pct), (3.5, 5.0));
        assert_eq!(a.payment, 4490.45);
        // 23 payments made before Jan 2028; the new payment repays the balance over the ~337 left
        let after = rows.iter().find(|r| r.date == d("2028-01-15")).unwrap();
        let bal = rows[22].balance;
        assert!([payment(bal, 5.0, 337), payment(bal, 5.0, 338)].contains(&after.payment), "{}", after.payment);
        assert!(rows.len() == 360 || rows.len() == 359, "the same term, a bigger payment: {}", rows.len());
        assert_eq!(rate_now(&rows, d("2027-06-01")), Some(3.5));
        assert_eq!(rate_now(&rows, d("2028-06-01")), Some(5.0));
    }

    #[test]
    fn balances_interest_and_payoff_as_of_a_date() {
        let p = loan();
        let rows = schedule(&p);
        assert_eq!(balance_on(&p, &rows, d("2025-12-01")), 0., "before the loan");
        assert_eq!(balance_on(&p, &rows, d("2026-01-20")), 1_000_000., "after the start, before the first payment");
        assert_eq!(balance_on(&p, &rows, d("2026-02-15")), 998_426.22, "the day of a payment counts");
        assert_eq!(balance_on(&p, &rows, d("2026-03-14")), 998_426.22);
        assert_eq!(balance_on(&p, &rows, d("2099-01-01")), 0.);
        assert_eq!(interest_to(&rows, d("2026-02-15")), 2916.67);
        let year = interest_between(&rows, d("2026-01-01"), d("2026-12-31"));
        assert_eq!(year, cents(rows[..11].iter().map(|r| r.interest).sum()), "Feb to Dec: 11 payments");
        assert_eq!(payments_left(&rows, d("2026-02-15")), 359);
        assert_eq!(payment_now(&rows, d("2026-02-20")), Some(4490.45));
        assert_eq!(payment_now(&rows, d("2099-01-01")), rows.last().map(|r| r.payment), "after the end: the last one");
        assert_eq!(equity(750_000., 1_000_000.), Some((75., 250_000.)));
        assert_eq!(equity(1., 0.), None);
    }

    #[test]
    fn the_third_value_from_the_payment() {
        assert_eq!(third(Some(1_000_000.), Some(3.5), None, 4490.45), Some(Third::Term(360)));
        match third(None, Some(3.5), Some(360), 4490.45) {
            Some(Third::Principal(p)) => assert!((p - 1_000_000.).abs() < 20., "{p}"),
            x => panic!("{x:?}"),
        }
        assert_eq!(third(Some(1_000_000.), None, Some(360), 4490.45), Some(Third::Rate(3.5)));
        assert_eq!(third(Some(120_000.), Some(0.), None, 1000.), Some(Third::Term(120)));
        assert_eq!(third(Some(1_000_000.), None, Some(360), 100.), None, "less than even slices: no such rate");
        assert_eq!(third(Some(1_000_000.), Some(3.5), None, 100.), None, "doesn't cover the interest");
        assert_eq!(third(Some(1.), Some(3.5), Some(360), 4490.45), None, "all three given: nothing to find");
        assert_eq!(third(None, None, Some(360), 4490.45), None);
        assert_eq!(third(Some(1.), Some(3.5), None, 0.), None);
    }

    #[test]
    fn what_if_an_extra_monthly_payment() {
        let p = loan();
        let w = what_if(&p, 500., d("2026-06-01"));
        assert!(w.months_saved > 40 && w.months_saved < 90, "{}", w.months_saved);
        assert!(w.interest_saved > 80_000., "{}", w.interest_saved);
        assert!(w.payoff.unwrap() < d("2056-01-15"));
        let none = what_if(&p, 0., d("2026-06-01"));
        assert_eq!((none.months_saved, none.interest_saved), (0, 0.));
    }
}
