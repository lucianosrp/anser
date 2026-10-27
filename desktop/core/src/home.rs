//! The home categories ("Mark as" on a transaction, and the Home costs
//! page): a fixed list, each with its icon and flags. A category is created
//! on first use, with these flags; one the owner already has (a "Rent") is
//! used as it is and never touched.

use std::collections::BTreeMap;

use chrono::{Months, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{categories, drill, Kind, Ledger};

/// One home category and the flags it is created with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HomeCategory {
    pub name: &'static str,
    /// a Lucide icon name (embedded by the app's category icons)
    pub icon: &'static str,
    /// "spending", or "deposit" (what "Owed to you" reads)
    pub kind: &'static str,
    /// "always" or "auto"
    pub fixed: &'static str,
    pub obligation: bool,
}

const fn cat(name: &'static str, icon: &'static str, kind: &'static str, fixed: &'static str, obligation: bool) -> HomeCategory {
    HomeCategory { name, icon, kind, fixed, obligation }
}

pub const HOME_CATEGORIES: [HomeCategory; 12] = [
    cat("Rent", "House", "spending", "always", true),
    cat("Mortgage", "House", "spending", "always", true),
    cat("Deposit", "HandCoins", "deposit", "auto", false),
    cat("Agency fee", "Receipt", "spending", "auto", false),
    cat("Electricity", "Zap", "spending", "auto", false),
    cat("Water", "Droplet", "spending", "auto", false),
    cat("Gas", "Flame", "spending", "auto", false),
    cat("Internet", "Wifi", "spending", "always", false),
    cat("Management fee", "Briefcase", "spending", "always", false),
    cat("Rates", "Landmark", "spending", "always", false),
    cat("Home insurance", "Shield", "spending", "always", false),
    cat("Other home cost", "Tag", "spending", "auto", false),
];

impl HomeCategory {
    /// The POST /api/categories body that creates it with its flags.
    pub fn create_body(&self) -> Value {
        let mut b = json!({"name": self.name, "icon": self.icon, "kind": self.kind, "fixed": self.fixed});
        if self.obligation {
            b["obligation"] = json!(true);
        }
        b
    }
}

/// The home category called `name` (any case), if it is one.
pub fn find(name: &str) -> Option<&'static HomeCategory> {
    HOME_CATEGORIES.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

/// What marking as `name` needs first: the body that creates the category,
/// or None when it exists already (then it is used as it is) or is not a
/// home category.
pub fn create_if_missing<'a>(name: &str, existing: impl IntoIterator<Item = &'a str>) -> Option<Value> {
    let home = find(name)?;
    if existing.into_iter().any(|n| n.eq_ignore_ascii_case(home.name)) {
        return None;
    }
    Some(home.create_body())
}


// ---------- the living-costs figures (Spending's Home tab) ----------

/// How many full months the tab looks back at most.
pub const MONTHS_BACK: usize = 12;
const DEPOSIT: &str = "Deposit";
const RENT: &str = "Rent";
pub const MORTGAGE: &str = "Mortgage";
const GROCERIES: &str = "Groceries";

/// The category's name in the home list when it is a home cost (Deposit
/// is not one: it comes back).
fn home_cost(cat: &str) -> Option<&'static str> {
    find(cat).map(|c| c.name).filter(|n| *n != DEPOSIT)
}

/// Does the Home tab have something to show: a bank or card row in a home
/// category (a deposit alone doesn't count).
pub fn has_home_rows(ledger: &Ledger) -> bool {
    ledger.entries.iter().any(|e| matches!(e.kind, Kind::Bank | Kind::Card) && home_cost(&e.category).is_some())
}

/// A category's monthly spending in HKD (the owner's share), one value per
/// month of [`LivingCosts::months`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    pub category: String,
    pub values: Vec<f64>,
}

impl Line {
    fn new(category: &str, months: usize) -> Line {
        Line { category: category.into(), values: vec![0.; months] }
    }

    /// The last month in the window.
    pub fn last(&self) -> f64 {
        self.values.last().copied().unwrap_or(0.)
    }

    /// The average over every month of the window, the ones with no rows
    /// counting as zero (a quarterly bill averages out).
    pub fn avg(&self) -> f64 {
        average(&self.values)
    }
}

fn average(v: &[f64]) -> f64 {
    if v.is_empty() { 0. } else { v.iter().sum::<f64>() / v.len() as f64 }
}

fn sum_lines(lines: &[Line], months: usize) -> Vec<f64> {
    (0..months).map(|i| lines.iter().map(|l| l.values[i]).sum()).collect()
}

/// What living costs: housing (the home categories, so a mortgage can join
/// the list), groceries, the other essentials and everything else.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LivingCosts {
    /// "YYYY-MM", oldest first: the last full months, no earlier than the
    /// ledger's first row
    pub months: Vec<String>,
    pub housing: Vec<Line>,
    pub groceries: Line,
    /// every other spending category that is fixed or an obligation, biggest first
    pub essentials: Vec<Line>,
    pub other: Line,
    /// income over the same months
    pub income: Vec<f64>,
}

impl LivingCosts {
    pub fn housing_total(&self) -> Vec<f64> {
        sum_lines(&self.housing, self.months.len())
    }

    pub fn essentials_total(&self) -> Vec<f64> {
        sum_lines(&self.essentials, self.months.len())
    }

    /// Housing, groceries and the other essentials, per month.
    pub fn living_total(&self) -> Vec<f64> {
        let (h, e) = (self.housing_total(), self.essentials_total());
        (0..self.months.len()).map(|i| h[i] + self.groceries.values[i] + e[i]).collect()
    }

    /// (last month, average) of the cost of living.
    pub fn headline(&self) -> (f64, f64) {
        let t = self.living_total();
        (t.last().copied().unwrap_or(0.), average(&t))
    }

    /// The cost of living's share of income: (last month, average); None
    /// when the income is nothing.
    pub fn income_share(&self) -> (Option<f64>, Option<f64>) {
        let share = |c: f64, i: f64| (i > 0.).then(|| c / i);
        let (last, avg) = self.headline();
        (share(last, self.income.last().copied().unwrap_or(0.)), share(avg, average(&self.income)))
    }
}

/// "2026-09" + `n` months.
pub fn add_months(ym: &str, n: i32) -> Option<String> {
    let d = NaiveDate::parse_from_str(&format!("{ym}-01"), "%Y-%m-%d").ok()?;
    let d = if n >= 0 { d.checked_add_months(Months::new(n as u32))? } else { d.checked_sub_months(Months::new(n.unsigned_abs()))? };
    Some(d.format("%Y-%m").to_string())
}

/// Is the category an essential beyond housing and groceries: fixed by
/// nature, or an obligation (a loan, insurance, tax)? Money that comes
/// back (reimbursable) is not.
pub fn is_essential(cat: &str) -> bool {
    !categories::reimbursable(cat) && (categories::fixed_always(cat) || categories::obligation(cat))
}

/// The cost of living over the `months_back` full months before `current`
/// ("YYYY-MM", the running month, left out), from the bank and card rows at
/// the owner's share. Spending is what [`drill::is_spending`] says.
pub fn living_costs(ledger: &Ledger, current: &str, months_back: usize) -> LivingCosts {
    let first = ledger.entries.iter().filter(|e| matches!(e.kind, Kind::Bank | Kind::Card)).filter_map(|e| e.date.get(..7)).min();
    let (Some(first), Some(end)) = (first, add_months(current, -1)) else { return LivingCosts::default() };
    let mut months = Vec::new();
    for back in (0..months_back as i32).rev() {
        match add_months(&end, -back) {
            Some(m) if m.as_str() >= first => months.push(m),
            _ => {}
        }
    }
    let n = months.len();
    let mut out = LivingCosts { groceries: Line::new(GROCERIES, n), other: Line::new("Everything else", n), income: vec![0.; n], months, ..Default::default() };
    let mut housing: BTreeMap<&str, Line> = BTreeMap::new();
    let mut essentials: BTreeMap<String, Line> = BTreeMap::new();
    for e in &ledger.entries {
        let Some(at) = e.date.get(..7).and_then(|m| out.months.iter().position(|x| x == m)) else { continue };
        let Some(v) = drill::my_hkd(e) else { continue };
        if drill::is_income(e) {
            out.income[at] += v;
        } else if drill::is_spending(e) {
            let cost = -v;
            if let Some(h) = home_cost(&e.category) {
                housing.entry(h).or_insert_with(|| Line::new(h, n)).values[at] += cost;
            } else if e.category.eq_ignore_ascii_case(GROCERIES) {
                out.groceries.values[at] += cost;
            } else if is_essential(&e.category) {
                essentials.entry(e.category.clone()).or_insert_with(|| Line::new(&e.category, n)).values[at] += cost;
            } else {
                out.other.values[at] += cost;
            }
        }
    }
    // the home list's order, so a mortgage joining it lands where it is listed
    out.housing = HOME_CATEGORIES.iter().filter_map(|c| housing.remove(c.name)).collect();
    out.essentials = essentials.into_values().collect();
    out.essentials.sort_by(|a, b| b.avg().total_cmp(&a.avg()));
    out
}

// ---------- bills ----------

/// A home bill that comes back: how much, how often, when next, and
/// whether it looks off.
#[derive(Debug, Clone, PartialEq)]
pub struct Bill {
    pub category: String,
    /// the usual amount a time (the earlier months' median)
    pub typical: f64,
    /// months between payments (1 = monthly)
    pub every: u32,
    pub last_date: String,
    pub last_amount: f64,
    /// "YYYY-MM-DD"
    pub next: String,
    /// the expected date and a week of grace have passed without a row
    pub missing: bool,
    /// the last one is over 1.5 times the typical
    pub high: bool,
}

/// Days after the expected date before a bill counts as missing.
pub const GRACE_DAYS: i64 = 7;
/// The last bill over this times the typical one is "unusually high".
pub const HIGH_FACTOR: f64 = 1.5;

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 { 0. } else if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2. }
}

fn month_index(ym: &str) -> Option<i32> {
    let (y, m) = ym.split_once('-')?;
    Some(y.parse::<i32>().ok()? * 12 + m.parse::<i32>().ok()? - 1)
}

/// The bill of one category from its rows (date, amount paid), or None
/// with fewer than two months of them. `today` is "YYYY-MM-DD".
pub fn bill_of(category: &str, rows: &[(String, f64)], today: &str) -> Option<Bill> {
    bill_with(category, rows, today, None)
}

/// [`bill_of`] with the amount the home setup says it should be (the
/// monthly rent): that is the typical one, and a single month of rows is
/// enough to expect the next.
pub fn bill_with(category: &str, rows: &[(String, f64)], today: &str, expected: Option<f64>) -> Option<Bill> {
    let mut by_month: BTreeMap<&str, f64> = BTreeMap::new();
    for (d, v) in rows {
        *by_month.entry(d.get(..7)?).or_default() += v;
    }
    if by_month.len() < if expected.is_some() { 1 } else { 2 } {
        return None;
    }
    let idx: Vec<i32> = by_month.keys().filter_map(|m| month_index(m)).collect();
    let mut gaps: Vec<f64> = idx.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let every = if expected.is_some() { 1 } else { (median(&mut gaps).round() as u32).max(1) };
    let amounts: Vec<f64> = by_month.values().copied().collect();
    let (last_amount, earlier) = amounts.split_last()?;
    let typical = expected.unwrap_or_else(|| median(&mut earlier.to_vec()));
    let last = rows.iter().filter(|(d, _)| d.get(..7) == by_month.keys().last().copied()).map(|(d, _)| d.as_str()).max()?;
    let last_date = NaiveDate::parse_from_str(last, "%Y-%m-%d").ok()?;
    let next = last_date.checked_add_months(Months::new(every))?;
    let today = NaiveDate::parse_from_str(today, "%Y-%m-%d").ok()?;
    Some(Bill {
        category: category.into(),
        typical,
        every,
        last_date: last.into(),
        last_amount: *last_amount,
        next: next.format("%Y-%m-%d").to_string(),
        missing: (today - next).num_days() > GRACE_DAYS,
        high: typical > 0. && *last_amount > typical * HIGH_FACTOR,
    })
}

/// The home bills, Rent first, then the home list's order: every home
/// category but Deposit with at least two months of rows.
pub fn bills(ledger: &Ledger, today: &str) -> Vec<Bill> {
    bills_with(ledger, today, None)
}

/// [`bills`] with the setup's monthly rent (in the base currency), which
/// the Rent bill then expects.
pub fn bills_with(ledger: &Ledger, today: &str, rent: Option<f64>) -> Vec<Bill> {
    bills_expecting(ledger, today, &[(RENT, rent)])
}

/// [`bills`] where some categories have an amount the setup says they
/// should be (the rent, the mortgage payment).
pub fn bills_expecting(ledger: &Ledger, today: &str, expected: &[(&str, Option<f64>)]) -> Vec<Bill> {
    let mut rows: BTreeMap<&str, Vec<(String, f64)>> = BTreeMap::new();
    for e in ledger.entries.iter().filter(|e| drill::is_spending(e)) {
        if let (Some(h), Some(v)) = (home_cost(&e.category), drill::my_hkd(e)) {
            rows.entry(h).or_default().push((e.date.clone(), -v));
        }
    }
    HOME_CATEGORIES.iter().filter_map(|c| bill_with(c.name, rows.get(c.name)?, today, expected.iter().find(|(n, _)| *n == c.name).and_then(|(_, a)| *a))).collect()
}

/// "monthly", "every 3 months", ...
pub fn cadence(every: u32) -> String {
    match every {
        1 => "monthly".into(),
        12 => "yearly".into(),
        n => format!("every {n} months"),
    }
}

// ---------- the home setup (Settings › Home) ----------

/// The per-user setting the setup is kept under.
pub const SETTING_KEY: &str = "home";
/// The kinds of one-off cost a setup lists.
pub const ONEOFF_KINDS: [&str; 4] = ["deposit", "agency_fee", "stamp_duty", "legal_fee"];
/// The renewal reminder shows this many days before the lease ends.
pub const RENEWAL_DAYS: i64 = 60;
/// A deposit and a one-off match when their dates are this close.
pub const MATCH_DAYS: i64 = 31;

/// The `home` setting: how the owner lives, and what it cost to move in.
/// Every field but `mode` may be left out (a mortgage needs its loan).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeSetup {
    /// "rent", "mortgage" or "none"
    pub mode: String,
    pub currency: Option<String>,
    pub rent: Rent,
    pub mortgage: Mortgage,
    pub oneoffs: Vec<OneOff>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rent {
    pub monthly: f64,
    /// text the landlord's payments carry (becomes a rule to Rent)
    pub landlord_match: Option<String>,
    pub lease: Lease,
}

/// The loan of a home bought with a mortgage (see [`crate::mortgage`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mortgage {
    pub principal: f64,
    pub annual_rate_pct: f64,
    pub term_months: u32,
    /// "YYYY-MM-DD"
    pub start: Option<String>,
    /// 1-28; the start's day when left out
    pub payment_day: Option<u32>,
    /// text the lender's payments carry (becomes a rule to Mortgage)
    pub lender_match: Option<String>,
    pub home_value: Option<f64>,
    pub home_value_date: Option<String>,
    pub overpayments: Vec<Overpayment>,
    pub rate_changes: Vec<RateChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overpayment {
    pub date: Option<String>,
    pub amount: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RateChange {
    pub from: Option<String>,
    pub annual_rate_pct: f64,
}

impl Mortgage {
    /// The loan for the maths; None without a principal, a term or a start.
    pub fn params(&self) -> Option<crate::mortgage::Params> {
        let start = date_of(filled(&self.start)?)?;
        if self.principal <= 0. || self.term_months == 0 {
            return None;
        }
        Some(crate::mortgage::Params {
            principal: self.principal,
            annual_rate_pct: self.annual_rate_pct,
            term_months: self.term_months,
            start,
            payment_day: self.payment_day,
            overpayments: self.overpayments.iter().filter_map(|o| Some((date_of(filled(&o.date)?)?, o.amount))).collect(),
            rate_changes: self.rate_changes.iter().filter_map(|c| Some((date_of(filled(&c.from)?)?, c.annual_rate_pct))).collect(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lease {
    /// "YYYY-MM-DD"
    pub start: Option<String>,
    pub end: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OneOff {
    /// one of [`ONEOFF_KINDS`]
    pub kind: String,
    pub amount: f64,
    pub date: Option<String>,
    pub returned_on: Option<String>,
}

fn date_of(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

fn filled(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

impl HomeSetup {
    /// Check a JSON body and parse it: the one validator the server's
    /// setting write and the form share.
    pub fn check(v: &Value) -> Result<HomeSetup, String> {
        let mode = v.get("mode").and_then(Value::as_str).ok_or("home needs a mode: rent or none")?;
        match mode {
            "rent" | "none" => {}
            "mortgage" => {}
            m => return Err(format!("{m:?} isn't a home mode (rent, mortgage or none)")),
        }
        let s: HomeSetup = serde_json::from_value(v.clone()).map_err(|e| format!("not a home setup: {e}"))?;
        if let Some(c) = filled(&s.currency) {
            if c.len() != 3 || !c.chars().all(|ch| ch.is_ascii_alphabetic()) {
                return Err("the currency is a 3-letter code like USD".into());
            }
        }
        let money = |x: f64| x.is_finite() && (0. ..1e12).contains(&x);
        if !money(s.rent.monthly) {
            return Err("the monthly rent is a positive amount".into());
        }
        if s.rent.landlord_match.as_deref().is_some_and(|m| m.chars().count() > 80) {
            return Err("the landlord match is at most 80 characters".into());
        }
        let day = |what: &str, d: &Option<String>| match filled(d) {
            Some(x) => date_of(x).map(Some).ok_or(format!("{what} is a date like 2026-12-31")),
            None => Ok(None),
        };
        let (start, end) = (day("the lease start", &s.rent.lease.start)?, day("the lease end", &s.rent.lease.end)?);
        if start.zip(end).is_some_and(|(a, b)| b < a) {
            return Err("the lease ends before it starts".into());
        }
        if mode == "mortgage" {
            check_mortgage(&s.mortgage, &day)?;
        }
        if s.oneoffs.len() > 50 {
            return Err("at most 50 one-off costs".into());
        }
        for o in &s.oneoffs {
            if !ONEOFF_KINDS.contains(&o.kind.as_str()) {
                return Err(format!("{:?} isn't a one-off kind ({})", o.kind, ONEOFF_KINDS.join(", ")));
            }
            if !money(o.amount) {
                return Err("a one-off amount is a positive number".into());
            }
            let (on, back) = (day("a one-off's date", &o.date)?, day("a one-off's returned date", &o.returned_on)?);
            if on.zip(back).is_some_and(|(a, b)| b < a) {
                return Err("a cost can't be returned before it was paid".into());
            }
        }
        Ok(s)
    }

    /// The setting, parsed; None when absent or invalid.
    pub fn parse(v: &Value) -> Option<HomeSetup> {
        HomeSetup::check(v).ok()
    }

    /// The monthly rent when it is set and in the base currency (a ledger
    /// amount is in it, so only then can they be compared).
    pub fn rent_in(&self, base: &str) -> Option<f64> {
        let same = filled(&self.currency).is_none_or(|c| c.eq_ignore_ascii_case(base));
        (self.mode == "rent" && self.rent.monthly > 0. && same).then_some(self.rent.monthly)
    }

    /// The deposit one-off, if there is one.
    pub fn deposit(&self) -> Option<&OneOff> {
        self.oneoffs.iter().find(|o| o.kind == "deposit")
    }

    /// Has something to show on the Home tab beyond the ledger.
    pub fn is_set(&self) -> bool {
        self.mode == "rent" || self.mode == "mortgage" || self.deposit().is_some()
    }

    /// The mortgage's loan, in mortgage mode.
    pub fn loan(&self) -> Option<crate::mortgage::Params> {
        (self.mode == "mortgage").then(|| self.mortgage.params()).flatten()
    }

    /// The monthly mortgage payment now, when the loan is in the base
    /// currency (a ledger amount is in it, so only then can they be compared).
    pub fn mortgage_payment_in(&self, base: &str, today: NaiveDate) -> Option<f64> {
        let same = filled(&self.currency).is_none_or(|c| c.eq_ignore_ascii_case(base));
        let rows = crate::mortgage::schedule(&self.loan()?);
        same.then(|| crate::mortgage::payment_now(&rows, today)).flatten()
    }
}

/// The mortgage's own checks: principal above 0, a rate of 0 to 30 percent,
/// a term of 1 to 600 months, valid dates and lists.
fn check_mortgage(m: &Mortgage, day: &dyn Fn(&str, &Option<String>) -> Result<Option<NaiveDate>, String>) -> Result<(), String> {
    if !(m.principal.is_finite() && m.principal > 0. && m.principal < 1e12) {
        return Err("the mortgage principal is a positive amount".into());
    }
    let rate = |r: f64| r.is_finite() && (0. ..=30.).contains(&r);
    if !rate(m.annual_rate_pct) {
        return Err("the mortgage rate is between 0 and 30 percent a year".into());
    }
    if !(1..=600).contains(&m.term_months) {
        return Err("the mortgage term is 1 to 600 months".into());
    }
    if day("the mortgage start", &m.start)?.is_none() {
        return Err("the mortgage needs a start date like 2026-01-15".into());
    }
    if m.payment_day.is_some_and(|d| !(1..=28).contains(&d)) {
        return Err("the payment day is 1 to 28".into());
    }
    if let Some(v) = m.home_value {
        if !(v.is_finite() && (0. ..1e13).contains(&v)) {
            return Err("the home value is a positive amount".into());
        }
    }
    day("the home value date", &m.home_value_date)?;
    if m.overpayments.len() > 200 || m.rate_changes.len() > 100 {
        return Err("too many overpayments or rate changes".into());
    }
    for o in &m.overpayments {
        if !(o.amount.is_finite() && o.amount > 0. && o.amount < 1e12) {
            return Err("an overpayment is a positive amount".into());
        }
        if day("an overpayment's date", &o.date)?.is_none() {
            return Err("an overpayment needs a date".into());
        }
    }
    for c in &m.rate_changes {
        if !rate(c.annual_rate_pct) {
            return Err("a rate change is between 0 and 30 percent a year".into());
        }
        if day("a rate change's date", &c.from)?.is_none() {
            return Err("a rate change needs a date".into());
        }
    }
    Ok(())
}

/// Are two "YYYY-MM-DD" dates within [`MATCH_DAYS`] of each other?
pub fn near(a: &str, b: &str) -> bool {
    matches!((date_of(a), date_of(b)), (Some(a), Some(b)) if (a - b).num_days().abs() <= MATCH_DAYS)
}

/// Where the lease stands today.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LeaseState {
    /// `renew`: inside the reminder window
    Active { days_left: i64, renew: bool },
    Ended { days_ago: i64 },
}

impl Lease {
    /// None without an end date. `today` is "YYYY-MM-DD".
    pub fn state(&self, today: &str) -> Option<LeaseState> {
        let end = date_of(filled(&self.end)?)?;
        let left = (end - date_of(today)?).num_days();
        Some(if left < 0 { LeaseState::Ended { days_ago: -left } } else { LeaseState::Active { days_left: left, renew: left <= RENEWAL_DAYS } })
    }
}

/// A month the rent moved: `from` to `to` (the first month at the new
/// amount).
#[derive(Debug, Clone, PartialEq)]
pub struct RentChange {
    pub month: String,
    pub from: f64,
    pub to: f64,
}

/// The rent's changes from its monthly line: between two months that were
/// paid, an amount that differs by more than a unit. Months with no rent
/// row are skipped, not read as a change.
pub fn rent_changes(rent: &Line, months: &[String]) -> Vec<RentChange> {
    let mut out = Vec::new();
    let mut prev: Option<f64> = None;
    for (m, v) in months.iter().zip(&rent.values) {
        if *v < 0.005 {
            continue;
        }
        if let Some(p) = prev.filter(|p| (v - p).abs() > 1.) {
            out.push(RentChange { month: m.clone(), from: p, to: *v });
        }
        prev = Some(*v);
    }
    out
}

/// The last month with rent paid, and how much.
pub fn last_rent(rent: &Line, months: &[String]) -> Option<(String, f64)> {
    months.iter().zip(&rent.values).rev().find(|(_, v)| **v >= 0.005).map(|(m, v)| (m.clone(), *v))
}

/// Where a deposit stands, and which evidence says so.
#[derive(Debug, Clone, PartialEq)]
pub enum DepositStatus {
    Held,
    /// `on`: the date, when known; `by_setting`: you said so (returned on)
    /// rather than the bank rows showing the refund
    Returned { on: Option<String>, by_setting: bool },
}

/// The deposit one-off's status. The setting's `returned_on` wins; else the
/// receivables view: still held when it lists an open deposit in that
/// currency opened near the date (`held` = (since, currency)), refunded when
/// the bank rows have the deposit paid but it is no longer open. A deposit
/// the bank rows never saw can't be read: held, as entered.
pub fn deposit_status(one: &OneOff, currency: &str, held: &[(String, String)], ledger: &Ledger) -> DepositStatus {
    if let Some(on) = filled(&one.returned_on) {
        return DepositStatus::Returned { on: Some(on.to_string()), by_setting: true };
    }
    let Some(date) = filled(&one.date) else { return DepositStatus::Held };
    if held.iter().any(|(since, c)| c.eq_ignore_ascii_case(currency) && near(since, date)) {
        return DepositStatus::Held;
    }
    let paid = ledger.entries.iter().any(|e| e.amount < 0. && e.category.eq_ignore_ascii_case(DEPOSIT) && e.currency.eq_ignore_ascii_case(currency) && near(&e.date, date));
    if paid { DepositStatus::Returned { on: None, by_setting: false } } else { DepositStatus::Held }
}

// ---------- suggested values for the setup form ----------

/// A suggested value and the reason to show with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggested<T> {
    pub value: T,
    pub why: String,
}

fn sug<T>(value: T, why: impl Into<String>) -> Option<Suggested<T>> {
    Some(Suggested { value, why: why.into() })
}

/// What the ledger says about the home setup form's fields. Each is
/// optional: only what the rows support.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Suggestion {
    /// "rent" or "mortgage"
    pub mode: Option<Suggested<&'static str>>,
    pub currency: Option<Suggested<String>>,
    pub rent_monthly: Option<Suggested<f64>>,
    pub landlord: Option<Suggested<String>>,
    pub lease_start: Option<Suggested<String>>,
    /// a guess: the contract says
    pub lease_end: Option<Suggested<String>>,
    /// deposit, agency fee, stamp duty and legal fee rows (the form offers the
    /// ones its mode has)
    pub oneoffs: Vec<Suggested<OneOff>>,
    pub mortgage_payment: Option<Suggested<f64>>,
    pub mortgage_start: Option<Suggested<String>>,
    pub lender: Option<Suggested<String>>,
}

/// How far back "recent" and the typical payment look.
const RECENT_MONTHS: u32 = 6;
/// The merchant must cover this share of the rows to be suggested.
const MATCH_SHARE: f64 = 0.6;
/// The shortest common prefix worth suggesting as a rule.
const MIN_PREFIX: usize = 8;
/// A deposit this close before the first rent starts the lease.
const DEPOSIT_LEAD_DAYS: i64 = 60;

fn category_rows<'a>(ledger: &'a Ledger, cat: &str, outflow: bool) -> Vec<&'a crate::Entry> {
    ledger.entries.iter()
        .filter(|e| matches!(e.kind, Kind::Bank | Kind::Card) && e.category.eq_ignore_ascii_case(cat) && (e.amount < 0.) == outflow && e.amount != 0.)
        .collect()
}

/// What was paid per month, at the owner's share and in the rows' own
/// currency, for the months from `since` on.
fn monthly_paid(rows: &[&crate::Entry], since: &str) -> BTreeMap<String, f64> {
    let mut by: BTreeMap<String, f64> = BTreeMap::new();
    for e in rows.iter().filter(|e| e.date.as_str() >= since) {
        if let Some(m) = e.date.get(..7) {
            *by.entry(m.to_string()).or_default() += -e.amount * e.share;
        }
    }
    by
}

/// The commonest currency among the rows.
fn main_currency(rows: &[&crate::Entry]) -> Option<String> {
    let mut n: BTreeMap<&str, usize> = BTreeMap::new();
    for e in rows {
        *n.entry(e.currency.as_str()).or_default() += 1;
    }
    n.into_iter().max_by_key(|(_, c)| *c).map(|(c, _)| c.to_string()).filter(|c| !c.is_empty())
}

/// A description as a rule would match it: uppercase, words with digits
/// (dates, reference numbers) dropped, one space between words.
fn normalised(desc: &str) -> String {
    desc.to_uppercase().split_whitespace().filter(|w| !w.chars().any(|c| c.is_ascii_digit())).filter(|w| w.chars().any(|c| c.is_alphabetic())).collect::<Vec<_>>().join(" ")
}

/// Shortest text a rule for the rows could use: the normalised description
/// most rows share when it covers [`MATCH_SHARE`] of them; else the longest
/// prefix all the descriptions share (at least [`MIN_PREFIX`] characters,
/// cut at a word end); else nothing. Returns the text and a reason.
fn main_description(rows: &[&crate::Entry]) -> Option<(String, String)> {
    let all: Vec<String> = rows.iter().map(|e| normalised(&e.description)).filter(|d| !d.is_empty()).collect();
    if all.is_empty() {
        return None;
    }
    let mut n: BTreeMap<&str, usize> = BTreeMap::new();
    for d in &all {
        *n.entry(d.as_str()).or_default() += 1;
    }
    let (best, hits) = n.into_iter().max_by_key(|(_, c)| *c)?;
    if hits as f64 >= rows.len() as f64 * MATCH_SHARE {
        return Some((best.to_string(), format!("\u{201c}{best}\u{201d} is on {hits} of {} recent payments", rows.len())));
    }
    let first = &all[0];
    let mut len = first.len();
    for d in &all[1..] {
        len = len.min(first.chars().zip(d.chars()).take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum());
    }
    // back to a word end
    let prefix = first[..len].to_string();
    let cut = if all.iter().all(|d| d.len() == len || d[len..].starts_with(' ')) { prefix.trim_end().to_string() } else { prefix.rsplit_once(' ').map(|(h, _)| h.to_string()).unwrap_or_default() };
    (cut.chars().count() >= MIN_PREFIX && all.len() == rows.len()).then(|| {
        let why = format!("all {} recent payments start with \u{201c}{cut}\u{201d}", rows.len());
        (cut, why)
    })
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The suggestions for the setup form, from the rows of the categories Rent,
/// Mortgage, Deposit, Agency fee, Stamp duty and Legal fee. `today` is
/// "YYYY-MM-DD".
pub fn suggest(ledger: &Ledger, today: &str) -> Suggestion {
    let mut out = Suggestion::default();
    let Some(now) = date_of(today) else { return out };
    let since = now.checked_sub_months(Months::new(RECENT_MONTHS)).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
    let rent = category_rows(ledger, RENT, true);
    let mortgage = category_rows(ledger, MORTGAGE, true);
    let latest = |rows: &[&crate::Entry]| rows.iter().map(|e| e.date.as_str()).filter(|d| *d >= since.as_str()).max().map(str::to_string);
    let (l_rent, l_mort) = (latest(&rent), latest(&mortgage));
    // the most recent payment decides when both are recent
    let mode = match (&l_rent, &l_mort) {
        (Some(r), Some(m)) if m > r => Some(("mortgage", m.clone(), true)),
        (Some(r), m) => Some(("rent", r.clone(), m.is_some())),
        (None, Some(m)) => Some(("mortgage", m.clone(), false)),
        (None, None) => None,
    };
    let Some((mode, last, both)) = mode else { return out };
    out.mode = sug(mode, format!("{} paid {}{}", if mode == "rent" { "Rent" } else { "Mortgage" }, long_date(&last),
        if both { " (both are recent: the latest wins)" } else { "" }));

    // rent
    if !rent.is_empty() {
        let by = monthly_paid(&rent, &since);
        if !by.is_empty() {
            let mut v: Vec<f64> = by.values().copied().collect();
            let m = (median(&mut v) * 100.).round() / 100.;
            out.rent_monthly = sug(m, format!("the median of your last {} of Rent", plural(by.len(), "month")));
        }
        let recent: Vec<&crate::Entry> = rent.iter().copied().filter(|e| e.date.as_str() >= since.as_str()).collect();
        if let Some((text, why)) = main_description(&recent) {
            out.landlord = sug(text, why);
        }
        // the lease: from the first rent (or the deposit just before it)
        let first = rent.iter().map(|e| e.date.as_str()).min().unwrap_or_default().to_string();
        let deposit_before = category_rows(ledger, DEPOSIT, true).into_iter().map(|e| e.date.clone())
            .filter(|d| d.as_str() < first.as_str() && date_of(&first).zip(date_of(d)).is_some_and(|(f, d)| (f - d).num_days() <= DEPOSIT_LEAD_DAYS))
            .min();
        let (start, why) = match deposit_before {
            Some(d) => (d, "the deposit paid just before your first rent".to_string()),
            None => (first, "your first Rent payment".to_string()),
        };
        if let Some(s) = date_of(&start) {
            out.lease_start = sug(start.clone(), why);
            let paid_months = rent.iter().filter_map(|e| e.date.get(..7)).collect::<std::collections::BTreeSet<_>>().len();
            let years = if paid_months < 12 { 1 } else { 2 };
            let mut end = s.checked_add_months(Months::new(12 * years));
            // a lease that ran out is assumed renewed for a year at a time
            while let Some(e) = end.filter(|e| *e < now) {
                end = e.checked_add_months(Months::new(12));
            }
            if let Some(e) = end {
                out.lease_end = sug(e.format("%Y-%m-%d").to_string(),
                    format!("a guess from {} of rent: {} year{} on, check your contract", plural(paid_months, "month"), years, if years == 1 { "" } else { "s" }));
            }
        }
        out.currency = main_currency(&rent).and_then(|c| (mode == "rent").then(|| sug(c, "the currency of your Rent payments")).flatten());
    }

    // mortgage
    if !mortgage.is_empty() {
        let by = monthly_paid(&mortgage, &since);
        if !by.is_empty() {
            let mut v: Vec<f64> = by.values().copied().collect();
            let m = (median(&mut v) * 100.).round() / 100.;
            out.mortgage_payment = sug(m, format!("the median of your last {} of Mortgage", plural(by.len(), "month")));
        }
        let first = mortgage.iter().map(|e| e.date.as_str()).min().unwrap_or_default().to_string();
        out.mortgage_start = sug(first, "your first Mortgage payment (the loan itself began earlier: check)");
        let recent: Vec<&crate::Entry> = mortgage.iter().copied().filter(|e| e.date.as_str() >= since.as_str()).collect();
        if let Some((text, why)) = main_description(&recent) {
            out.lender = sug(text, why);
        }
        if mode == "mortgage" {
            out.currency = main_currency(&mortgage).and_then(|c| sug(c, "the currency of your Mortgage payments"));
        }
    }

    // one-off costs
    let outflows = category_rows(ledger, DEPOSIT, true);
    let mut refunds: Vec<&crate::Entry> = category_rows(ledger, DEPOSIT, false);
    refunds.sort_by(|a, b| a.date.cmp(&b.date));
    let mut used = vec![false; refunds.len()];
    let mut deposits = outflows;
    deposits.sort_by(|a, b| a.date.cmp(&b.date));
    for d in deposits {
        let back = refunds.iter().enumerate().find(|(i, r)| !used[*i] && r.date >= d.date && r.currency == d.currency).map(|(i, r)| (i, r.date.clone()));
        if let Some((i, _)) = &back {
            used[*i] = true;
        }
        out.oneoffs.push(Suggested {
            value: OneOff { kind: "deposit".into(), amount: ((-d.amount * d.share) * 100.).round() / 100., date: Some(d.date.clone()), returned_on: back.map(|b| b.1) },
            why: format!("a Deposit payment on {}", long_date(&d.date)),
        });
    }
    for (cat, kind) in [("Agency fee", "agency_fee"), ("Stamp duty", "stamp_duty"), ("Legal fee", "legal_fee")] {
        for e in category_rows(ledger, cat, true) {
            out.oneoffs.push(Suggested {
                value: OneOff { kind: kind.into(), amount: ((-e.amount * e.share) * 100.).round() / 100., date: Some(e.date.clone()), returned_on: None },
                why: format!("a {cat} payment on {}", long_date(&e.date)),
            });
        }
    }
    out
}

fn long_date(d: &str) -> String {
    date_of(d).map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_else(|| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Entry;

    fn row(date: &str, cat: &str, amount: f64) -> Entry {
        Entry {
            kind: Kind::Bank, date: date.into(), source: "n26".into(), account: String::new(), description: cat.to_lowercase(), category: cat.into(),
            amount, currency: "HKD".into(), hkd: Some(amount), share: 1.0, haystack: String::new(), raw_hash: String::new(),
        }
    }

    fn ledger(rows: Vec<Entry>) -> Ledger {
        Ledger { entries: rows, ..Default::default() }
    }

    fn sample() -> Ledger {
        ledger(vec![
            row("2026-09-01", "Rent", -1000.),
            row("2026-09-03", "Electricity", -80.),
            row("2026-09-10", "Groceries", -300.),
            row("2026-09-12", "Insurance", -40.),
            row("2026-09-14", "Dining", -200.),
            row("2026-09-27", "Salary", 4000.),
            row("2026-08-01", "Rent", -1000.),
            row("2026-08-20", "Groceries", -250.),
            row("2026-08-21", "Dining", -100.),
            row("2026-08-27", "Salary", 4000.),
            // the running month and a deposit stay out
            row("2026-10-01", "Rent", -1000.),
            row("2026-08-02", "Deposit", -2000.),
        ])
    }

    #[test]
    fn the_tab_shows_only_with_home_rows() {
        assert!(has_home_rows(&sample()));
        assert!(!has_home_rows(&ledger(vec![row("2026-09-01", "Deposit", -2000.), row("2026-09-02", "Dining", -10.)])));
        assert!(!has_home_rows(&ledger(vec![])));
    }

    #[test]
    fn months_group_and_average_over_empty_months() {
        let c = living_costs(&sample(), "2026-10", 12);
        // no earlier than the first row
        assert_eq!(c.months, ["2026-08", "2026-09"]);
        assert_eq!(c.housing.iter().map(|l| l.category.as_str()).collect::<Vec<_>>(), ["Rent", "Electricity"]);
        let elec = &c.housing[1];
        assert_eq!(elec.values, [0., 80.]);
        assert_eq!((elec.last(), elec.avg()), (80., 40.), "the empty month counts");
        assert_eq!(c.groceries.values, [250., 300.]);
        assert_eq!(c.other.values, [100., 200.]);
        assert_eq!(c.income, [4000., 4000.]);
        let (last, avg) = c.headline();
        assert_eq!((last, avg), (1000. + 80. + 300. + 40., (1000. + 250. + 1000. + 80. + 300. + 40.) / 2.));
        assert!((c.income_share().0.unwrap() - 1420. / 4000.).abs() < 1e-9);
    }

    #[test]
    fn a_short_window_and_no_rows() {
        assert_eq!(living_costs(&sample(), "2026-10", 1).months, ["2026-09"]);
        assert!(living_costs(&ledger(vec![]), "2026-10", 12).months.is_empty());
        assert_eq!(living_costs(&ledger(vec![]), "2026-10", 12).headline(), (0., 0.));
    }

    #[test]
    fn essentials_follow_the_flags() {
        let c = living_costs(&sample(), "2026-10", 12);
        assert_eq!(c.essentials.iter().map(|l| l.category.as_str()).collect::<Vec<_>>(), ["Insurance"]);
        assert!(is_essential("Student loan") && !is_essential("Dining"));
        assert!(!is_essential("Office (reimbursed)"), "the money comes back");
    }

    fn rows(v: &[(&str, f64)]) -> Vec<(String, f64)> {
        v.iter().map(|(d, a)| (d.to_string(), *a)).collect()
    }

    #[test]
    fn bills_flag_missing_and_high() {
        let monthly = rows(&[("2026-07-05", 80.), ("2026-08-05", 90.), ("2026-09-05", 85.)]);
        let b = bill_of("Electricity", &monthly, "2026-10-08").unwrap();
        assert_eq!((b.every, b.typical, b.next.as_str()), (1, 85., "2026-10-05"));
        assert!(!b.missing && !b.high, "three days late is inside the grace");
        assert!(bill_of("Electricity", &monthly, "2026-10-13").unwrap().missing);
        let high = rows(&[("2026-07-05", 80.), ("2026-08-05", 90.), ("2026-09-05", 200.)]);
        let b = bill_of("Electricity", &high, "2026-09-20").unwrap();
        assert!(b.high && !b.missing);
        let quarterly = rows(&[("2026-03-10", 300.), ("2026-06-10", 300.), ("2026-09-10", 300.)]);
        let b = bill_of("Rates", &quarterly, "2026-10-01").unwrap();
        assert_eq!((b.every, b.next.as_str(), b.missing), (3, "2026-12-10", false));
        assert_eq!(cadence(b.every), "every 3 months");
        assert!(bill_of("Gas", &rows(&[("2026-09-05", 30.), ("2026-09-20", 30.)]), "2026-10-01").is_none(), "one month is not a bill");
    }

    #[test]
    fn bills_come_from_the_ledger() {
        let b = bills(&sample(), "2026-10-02");
        assert_eq!(b.iter().map(|b| b.category.as_str()).collect::<Vec<_>>(), ["Rent"], "Electricity has a single month, Deposit is none");
    }

    #[test]
    fn the_table_is_unique_and_flagged() {
        let mut names: Vec<_> = HOME_CATEGORIES.iter().map(|c| c.name.to_lowercase()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), HOME_CATEGORIES.len());
        for c in &HOME_CATEGORIES {
            assert!(crate::categories::KINDS.contains(&c.kind) && crate::categories::FIXED.contains(&c.fixed), "{}", c.name);
            assert!(!c.icon.is_empty() && c.icon.chars().all(|ch| ch.is_ascii_alphanumeric()));
        }
        assert_eq!(find("deposit").unwrap().kind, "deposit");
        assert_eq!(HOME_CATEGORIES.iter().filter(|c| c.kind == "deposit").count(), 1);
        let always: Vec<_> = HOME_CATEGORIES.iter().filter(|c| c.fixed == "always").map(|c| c.name).collect();
        assert_eq!(always, ["Rent", "Mortgage", "Internet", "Management fee", "Rates", "Home insurance"]);
        assert_eq!(HOME_CATEGORIES.iter().filter(|c| c.obligation).map(|c| c.name).collect::<Vec<_>>(), ["Rent", "Mortgage"]);
    }

    #[test]
    fn create_or_reuse() {
        let have = ["Dining", "rent"];
        assert_eq!(create_if_missing("Rent", have), None, "the owner's own Rent is used as is");
        let b = create_if_missing("Electricity", have).unwrap();
        assert_eq!(b["name"], "Electricity");
        assert_eq!(b["kind"], "spending");
        assert!(b.get("obligation").is_none());
        assert_eq!(create_if_missing("Deposit", have).unwrap()["kind"], "deposit");
        assert_eq!(create_if_missing("Rent", ["Dining"]).unwrap()["obligation"], true);
        assert_eq!(create_if_missing("Dining", have), None, "not a home category");
    }

    // ---------- the setup ----------

    fn setup(v: serde_json::Value) -> Result<HomeSetup, String> {
        HomeSetup::check(&v)
    }

    #[test]
    fn the_setting_parses_and_validates() {
        let ok = setup(json!({"mode": "rent", "currency": "HKD", "rent": {"monthly": 18000, "landlord_match": "linden",
            "lease": {"start": "2026-01-01", "end": "2026-12-31"}},
            "oneoffs": [{"kind": "deposit", "amount": 36000, "date": "2026-01-02", "returned_on": null}]})).unwrap();
        assert_eq!((ok.mode.as_str(), ok.rent.monthly, ok.oneoffs.len()), ("rent", 18000., 1));
        assert_eq!(ok.deposit().unwrap().amount, 36000.);
        assert!(setup(json!({"mode": "none"})).is_ok(), "everything but the mode is optional");
        assert!(setup(json!({"mode": "rent", "rent": {"lease": {"start": "", "end": null}}})).is_ok(), "blank dates are none");
        assert!(setup(json!({})).is_err(), "no mode");
        assert!(setup(json!({"mode": "mortgage"})).unwrap_err().contains("principal"), "a mortgage needs its loan");
        assert!(setup(json!({"mode": "castle"})).is_err());
        assert!(setup(json!({"mode": "rent", "currency": "dollars"})).is_err());
        assert!(setup(json!({"mode": "rent", "rent": {"monthly": -5}})).is_err());
        assert!(setup(json!({"mode": "rent", "rent": {"lease": {"end": "2026-13-40"}}})).is_err());
        assert!(setup(json!({"mode": "rent", "rent": {"lease": {"start": "2026-06-01", "end": "2026-01-01"}}})).is_err());
        assert!(setup(json!({"mode": "rent", "oneoffs": [{"kind": "bribe", "amount": 1}]})).is_err());
        assert!(setup(json!({"mode": "rent", "oneoffs": [{"kind": "deposit", "amount": 1, "date": "2026-03-01", "returned_on": "2026-02-01"}]})).is_err());
        assert!(setup(json!({"mode": "rent", "rent": "cheap"})).is_err());
    }

    fn mortgage_body() -> serde_json::Value {
        json!({"mode": "mortgage", "currency": "HKD", "mortgage": {"principal": 1000000, "annual_rate_pct": 3.5, "term_months": 360, "start": "2026-01-15",
            "payment_day": 15, "lender_match": "bank of x", "home_value": 1400000, "home_value_date": "2026-01-01",
            "overpayments": [{"date": "2027-01-01", "amount": 5000}], "rate_changes": [{"from": "2028-01-01", "annual_rate_pct": 4.0}]},
            "oneoffs": [{"kind": "legal_fee", "amount": 3000, "date": "2026-01-10"}, {"kind": "stamp_duty", "amount": 9000, "date": "2026-01-15"}]})
    }

    #[test]
    fn a_mortgage_setup_parses_and_validates() {
        let s = setup(mortgage_body()).unwrap();
        assert_eq!((s.mode.as_str(), s.mortgage.principal, s.mortgage.term_months), ("mortgage", 1_000_000., 360));
        assert!(s.is_set() && s.loan().is_some());
        assert_eq!(s.mortgage_payment_in("HKD", "2026-03-01".parse().unwrap()), Some(4490.45));
        assert_eq!(s.mortgage_payment_in("EUR", "2026-03-01".parse().unwrap()), None, "another currency");
        assert_eq!(setup(json!({"mode": "rent"})).unwrap().loan(), None);
        let bad = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut v = mortgage_body();
            f(&mut v);
            setup(v).is_err()
        };
        assert!(bad(&|v| v["mortgage"]["principal"] = json!(0)));
        assert!(bad(&|v| v["mortgage"]["annual_rate_pct"] = json!(31)));
        assert!(bad(&|v| v["mortgage"]["annual_rate_pct"] = json!(-1)));
        assert!(bad(&|v| v["mortgage"]["term_months"] = json!(0)));
        assert!(bad(&|v| v["mortgage"]["term_months"] = json!(601)));
        assert!(bad(&|v| v["mortgage"]["start"] = json!("soon")));
        assert!(bad(&|v| v["mortgage"]["start"] = json!(null)));
        assert!(bad(&|v| v["mortgage"]["payment_day"] = json!(29)));
        assert!(bad(&|v| v["mortgage"]["home_value_date"] = json!("x")));
        assert!(bad(&|v| v["mortgage"]["overpayments"] = json!([{"date": "2027-01-01", "amount": 0}])));
        assert!(bad(&|v| v["mortgage"]["overpayments"] = json!([{"amount": 5}])));
        assert!(bad(&|v| v["mortgage"]["rate_changes"] = json!([{"from": "2028-01-01", "annual_rate_pct": 99}])));
        assert!(setup(json!({"mode": "rent", "mortgage": {"principal": 0}})).is_ok(), "the loan is only checked in mortgage mode");
        assert!(setup(json!({"mode": "none"})).is_ok());
    }

    #[test]
    fn the_mortgage_bill_expects_the_schedules_payment() {
        let l = ledger(vec![row("2026-08-15", "Mortgage", -4400.), row("2026-09-15", "Mortgage", -4400.)]);
        let b = bills_expecting(&l, "2026-09-20", &[("Mortgage", Some(4490.45))]);
        assert_eq!((b[0].category.as_str(), b[0].typical), ("Mortgage", 4490.45));
        assert!(has_home_rows(&l), "Mortgage is a home cost");
        assert_eq!(living_costs(&l, "2026-10", 12).housing[0].category, "Mortgage");
    }

    #[test]
    fn rent_in_the_base_currency_only() {
        let s = setup(json!({"mode": "rent", "currency": "EUR", "rent": {"monthly": 1250}})).unwrap();
        assert_eq!((s.rent_in("EUR"), s.rent_in("HKD")), (Some(1250.), None));
        assert_eq!(setup(json!({"mode": "rent", "rent": {"monthly": 5}})).unwrap().rent_in("HKD"), Some(5.), "no currency: the base");
        assert_eq!(setup(json!({"mode": "none", "rent": {"monthly": 5}})).unwrap().rent_in("HKD"), None);
    }

    #[test]
    fn the_lease_counts_down_reminds_and_ends() {
        let l = Lease { start: Some("2026-01-01".into()), end: Some("2026-12-01".into()) };
        assert_eq!(l.state("2026-06-01"), Some(LeaseState::Active { days_left: 183, renew: false }));
        assert_eq!(l.state("2026-10-02"), Some(LeaseState::Active { days_left: 60, renew: true }), "the reminder starts 60 days before");
        assert_eq!(l.state("2026-10-01"), Some(LeaseState::Active { days_left: 61, renew: false }));
        assert_eq!(l.state("2026-12-01"), Some(LeaseState::Active { days_left: 0, renew: true }), "the last day");
        assert_eq!(l.state("2026-12-04"), Some(LeaseState::Ended { days_ago: 3 }));
        assert_eq!(Lease::default().state("2026-06-01"), None);
    }

    #[test]
    fn rent_changes_come_from_the_paid_months() {
        let months: Vec<String> = ["2026-05", "2026-06", "2026-07", "2026-08", "2026-09"].iter().map(|m| m.to_string()).collect();
        let line = Line { category: "Rent".into(), values: vec![1000., 1000., 0., 1100., 1100.] };
        assert_eq!(rent_changes(&line, &months), [RentChange { month: "2026-08".into(), from: 1000., to: 1100. }], "a gap isn't a change");
        assert_eq!(last_rent(&line, &months), Some(("2026-09".into(), 1100.)));
        let flat = Line { category: "Rent".into(), values: vec![1000.; 5] };
        assert!(rent_changes(&flat, &months).is_empty());
        assert_eq!(last_rent(&Line::default(), &[]), None);
    }

    #[test]
    fn the_setups_rent_is_the_bills_typical() {
        let one_month = rows(&[("2026-09-01", 1250.)]);
        assert!(bill_of("Rent", &one_month, "2026-09-20").is_none(), "one month alone is no bill");
        let b = bill_with("Rent", &one_month, "2026-10-20", Some(1200.)).unwrap();
        assert_eq!((b.typical, b.every, b.next.as_str(), b.missing, b.high), (1200., 1, "2026-10-01", true, false), "the missing check runs on the setup");
        let up = rows(&[("2026-08-01", 1000.), ("2026-09-01", 1900.)]);
        assert!(bill_with("Rent", &up, "2026-09-20", Some(1200.)).unwrap().high, "1900 against the 1200 the setup expects");
        let l = ledger(vec![row("2026-08-01", "Rent", -1000.), row("2026-09-01", "Rent", -1000.)]);
        assert_eq!(bills_with(&l, "2026-09-05", Some(1100.))[0].typical, 1100.);
        assert_eq!(bills_with(&l, "2026-09-05", None)[0].typical, 1000.);
    }

    #[test]
    fn a_deposits_status_and_its_evidence() {
        let one = OneOff { kind: "deposit".into(), amount: 2000., date: Some("2026-08-02".into()), returned_on: None };
        let paid = ledger(vec![row("2026-08-02", "Deposit", -2000.)]);
        let held = vec![("2026-08-02".to_string(), "HKD".to_string())];
        assert_eq!(deposit_status(&one, "HKD", &held, &paid), DepositStatus::Held, "the receivables view still has it");
        assert_eq!(deposit_status(&one, "HKD", &[], &paid), DepositStatus::Returned { on: None, by_setting: false }, "paid, and no longer open: refunded");
        assert_eq!(deposit_status(&one, "HKD", &[], &ledger(vec![])), DepositStatus::Held, "never in the bank rows: as entered");
        let back = OneOff { returned_on: Some("2026-09-30".into()), ..one.clone() };
        assert_eq!(deposit_status(&back, "HKD", &held, &paid), DepositStatus::Returned { on: Some("2026-09-30".into()), by_setting: true });
        assert!(near("2026-08-02", "2026-09-01") && !near("2026-08-02", "2026-09-04"), "31 days");
        assert!(!near("nope", "2026-09-01"));
    }

    // ---------- suggestions ----------

    fn paid(date: &str, cat: &str, amount: f64, desc: &str) -> Entry {
        Entry { description: desc.into(), ..row(date, cat, amount) }
    }

    fn steady_rent() -> Vec<Entry> {
        (3..=9).map(|m| paid(&format!("2026-{m:02}-01"), "Rent", -1000., "APARTMENT RENT LINDEN PROPERTIES")).collect()
    }

    #[test]
    fn rent_is_suggested_from_steady_payments() {
        let s = suggest(&ledger(steady_rent()), "2026-10-05");
        assert_eq!(s.mode.as_ref().map(|m| m.value), Some("rent"));
        assert_eq!(s.rent_monthly.as_ref().map(|m| m.value), Some(1000.));
        assert_eq!(s.currency.as_ref().map(|c| c.value.as_str()), Some("HKD"));
        assert_eq!(s.landlord.as_ref().map(|c| c.value.as_str()), Some("APARTMENT RENT LINDEN PROPERTIES"));
        assert_eq!(s.lease_start.as_ref().map(|c| c.value.as_str()), Some("2026-03-01"));
        // 7 months of rent: one year on from the first payment, flagged as a guess
        let end = s.lease_end.unwrap();
        assert_eq!(end.value, "2027-03-01");
        assert!(end.why.contains("check your contract"), "{}", end.why);
        assert!(s.mortgage_payment.is_none() && s.oneoffs.is_empty());
    }

    #[test]
    fn a_long_lease_is_two_years_and_rolls_on_when_over() {
        let rows: Vec<Entry> = (0..30).map(|i| {
            let (y, m) = (2024 + (9 + i) / 12, (9 + i) % 12 + 1);
            paid(&format!("{y}-{m:02}-01"), "Rent", -1000., "rent linden")
        }).collect();
        let s = suggest(&ledger(rows), "2027-03-15");
        assert_eq!(s.lease_start.as_ref().map(|c| c.value.as_str()), Some("2024-10-01"));
        // two years from the start is 2026-10-01 (past), so renewed to 2027-10-01
        assert_eq!(s.lease_end.as_ref().map(|c| c.value.as_str()), Some("2027-10-01"));
    }

    #[test]
    fn a_rent_change_gives_the_median_of_the_last_six_months() {
        let mut rows = Vec::new();
        for (i, m) in (4..=9).enumerate() {
            rows.push(paid(&format!("2026-{m:02}-01"), "Rent", if i < 2 { -1000. } else { -1200. }, "landlord co"));
        }
        let s = suggest(&ledger(rows), "2026-10-05");
        assert_eq!(s.rent_monthly.unwrap().value, 1200., "four of six months at the new rent");
    }

    #[test]
    fn shared_rent_is_the_owners_share() {
        let rows: Vec<Entry> = steady_rent().into_iter().map(|e| Entry { share: 0.5, ..e }).collect();
        assert_eq!(suggest(&ledger(rows), "2026-10-05").rent_monthly.unwrap().value, 500.);
    }

    #[test]
    fn nothing_is_suggested_without_rows_or_from_old_ones() {
        assert_eq!(suggest(&ledger(vec![]), "2026-10-05"), Suggestion::default());
        let old = vec![paid("2025-01-01", "Rent", -1000., "landlord co")];
        assert!(suggest(&ledger(old), "2026-10-05").mode.is_none(), "no Rent row in the last 6 months");
        assert_eq!(suggest(&ledger(steady_rent()), "not a date"), Suggestion::default());
    }

    #[test]
    fn a_landlord_needs_most_of_the_payments() {
        let mut rows = steady_rent();
        // the six recent months (Apr to Sep): three from another payee is half
        for e in rows.iter_mut().skip(1).take(3) {
            e.description = "FPS TRANSFER JANE ROE".into();
        }
        assert!(suggest(&ledger(rows.clone()), "2026-10-01").landlord.is_none(), "no description on 60%, no common start");
        rows[2].description = "APARTMENT RENT LINDEN PROPERTIES".into();
        assert_eq!(suggest(&ledger(rows), "2026-10-01").landlord.unwrap().value, "APARTMENT RENT LINDEN PROPERTIES", "4 of 6 is enough");
    }

    #[test]
    fn references_and_dates_do_not_split_a_description() {
        let rows: Vec<Entry> = (4..=9).map(|m| paid(&format!("2026-{m:02}-01"), "Rent", -1000.,
            &format!("apartment  rent linden properties REF{m}0042 {m:02}/2026"))).collect();
        let s = suggest(&ledger(rows), "2026-10-01");
        assert_eq!(s.landlord.unwrap().value, "APARTMENT RENT LINDEN PROPERTIES");
    }

    #[test]
    fn a_common_start_stands_in_when_no_description_repeats() {
        let names = ["LINDEN PROPERTIES RENT APR", "LINDEN PROPERTIES RENT MAY", "LINDEN PROPERTIES RENT JUN", "LINDEN PROPERTIES RENT JUL", "LINDEN PROPERTIES RENT AUG", "LINDEN PROPERTIES RENT SEP"];
        let rows: Vec<Entry> = names.iter().enumerate().map(|(i, n)| paid(&format!("2026-{:02}-01", i + 4), "Rent", -1000., n)).collect();
        let s = suggest(&ledger(rows), "2026-10-01");
        let l = s.landlord.unwrap();
        assert_eq!(l.value, "LINDEN PROPERTIES RENT", "cut at a word end");
        assert!(l.why.contains("start with"), "{}", l.why);
        // too short a start is nothing: 'LIND' alone is not a rule
        let short: Vec<Entry> = ["LINDEN A", "LINDEN B", "LINDEN C", "LINDEN D", "LINDEN E", "LINDEN F"].iter().enumerate()
            .map(|(i, n)| paid(&format!("2026-{:02}-01", i + 4), "Rent", -1000., &format!("{n}x"))).collect();
        assert!(suggest(&ledger(short), "2026-10-01").landlord.is_none(), "'LINDEN' is under 8 characters");
    }

    #[test]
    fn deposits_and_costs_with_their_refund() {
        let mut rows = steady_rent();
        rows.push(paid("2026-02-10", "Deposit", -2000., "RENTAL DEPOSIT LINDEN"));
        rows.push(paid("2026-02-12", "Agency fee", -500., "AGENT LTD"));
        rows.push(paid("2026-09-20", "Deposit", 2000., "RENTAL DEPOSIT REFUND"));
        let s = suggest(&ledger(rows.clone()), "2026-10-05");
        let dep = s.oneoffs.iter().find(|o| o.value.kind == "deposit").unwrap();
        assert_eq!((dep.value.amount, dep.value.date.as_deref(), dep.value.returned_on.as_deref()), (2000., Some("2026-02-10"), Some("2026-09-20")));
        assert!(s.oneoffs.iter().any(|o| o.value.kind == "agency_fee" && o.value.amount == 500.));
        // the deposit, 19 days before the first rent, starts the lease
        assert_eq!(s.lease_start.unwrap().value, "2026-02-10");
        // without the refund it is still held
        rows.retain(|e| e.amount < 0.);
        let s = suggest(&ledger(rows), "2026-10-05");
        assert_eq!(s.oneoffs.iter().find(|o| o.value.kind == "deposit").unwrap().value.returned_on, None);
        // a deposit long before the rent is not the lease start
        let far = vec![paid("2025-01-10", "Deposit", -2000., "x"), paid("2026-09-01", "Rent", -1000., "landlord co")];
        assert_eq!(suggest(&ledger(far), "2026-10-05").lease_start.unwrap().value, "2026-09-01");
    }

    #[test]
    fn a_mortgage_is_suggested_and_the_latest_payment_wins() {
        let mort: Vec<Entry> = (5..=9).map(|m| paid(&format!("2026-{m:02}-15"), "Mortgage", -4490.45, "HOMELOAN SERVICING")).collect();
        let s = suggest(&ledger(mort.clone()), "2026-10-05");
        assert_eq!(s.mode.as_ref().map(|m| m.value), Some("mortgage"));
        assert_eq!(s.mortgage_payment.as_ref().map(|m| m.value), Some(4490.45));
        assert_eq!(s.mortgage_start.as_ref().map(|m| m.value.as_str()), Some("2026-05-15"));
        assert_eq!(s.lender.as_ref().map(|m| m.value.as_str()), Some("HOMELOAN SERVICING"));
        assert_eq!(s.currency.as_ref().map(|c| c.value.as_str()), Some("HKD"));
        // both recent: the later payment decides, and it is said
        let mut both = mort.clone();
        both.extend(steady_rent().into_iter().filter(|e| e.date.as_str() < "2026-08-01"));
        let s = suggest(&ledger(both), "2026-10-05");
        assert_eq!(s.mode.as_ref().map(|m| m.value), Some("mortgage"));
        assert!(s.mode.unwrap().why.contains("latest wins"));
        let mut both = steady_rent();
        both.extend(mort.into_iter().filter(|e| e.date.as_str() < "2026-07-01"));
        assert_eq!(suggest(&ledger(both), "2026-10-05").mode.unwrap().value, "rent");
    }
}
