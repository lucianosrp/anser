//! The filter model every page shares, and its text form: the search query.
//!
//! A [`Filter`] is the parsed query, term by term, in the order typed. Chips
//! and pickers edit the terms and write the query back ([`Filter::query`]),
//! so typing and clicking stay in sync and the user's own spelling and order
//! survive an edit elsewhere.
//!
//! Query language (space-separated, every term must hold):
//!
//! | term | means |
//! |---|---|
//! | `coffee` | fuzzy text (the search ranks with it; [`Compiled`] ignores it) |
//! | `cat:dining` | category contains "dining" |
//! | `cat:dining,groceries` | any of them (a comma is "or") |
//! | `cat:"office (reimbursed)"` | quotes keep spaces |
//! | `src:card` | source or kind contains "card" (`bank`, `crypto`, `hsbc`, …) |
//! | `acct:hsbc_card`, `acct:"n26/savings pocket"` | one account: `source/account`, or the account name |
//! | `merchant:sushiro` (`m:`) | the merchant words start with it ([`drill::merchant_text`]) |
//! | `ccy:eur` | native currency |
//! | `trip:2026-03-14` | the rows of the trip that started that day |
//! | `-cat:rent`, `-acct:…` | a leading `-` excludes |
//! | `>500` `<20` | \|HKD\| above / below |
//! | `amt:100..500` | \|HKD\| between, inclusive (`amt:100..` and `amt:..500` too) |
//! | `2026-09` | a year, month or day |
//! | `2026-01..2026-06` | a date range, inclusive (`2026-03..` and `..2025` too) |
//! | `in` `out` | money in / money out only |
//!
//! Repeated tokens of one kind must all hold (`cat:dining cat:coffee` is
//! "Dining: coffee"), as they always did.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Datelike, NaiveDate};

use crate::drill;
use crate::ledger::{Entry, Kind, Ledger};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dim {
    Category,
    Source,
    Account,
    Merchant,
    Currency,
    Trip,
}

impl Dim {
    pub const ALL: [Dim; 6] = [Dim::Source, Dim::Account, Dim::Category, Dim::Merchant, Dim::Currency, Dim::Trip];

    /// The query prefix.
    pub fn key(self) -> &'static str {
        match self {
            Dim::Category => "cat",
            Dim::Source => "src",
            Dim::Account => "acct",
            Dim::Merchant => "merchant",
            Dim::Currency => "ccy",
            Dim::Trip => "trip",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Dim::Category => "Category",
            Dim::Source => "Source",
            Dim::Account => "Account",
            Dim::Merchant => "Merchant",
            Dim::Currency => "Currency",
            Dim::Trip => "Trip",
        }
    }

    fn from_key(k: &str) -> Option<Dim> {
        Some(match k {
            "cat" | "category" => Dim::Category,
            "src" | "source" => Dim::Source,
            "acct" | "account" => Dim::Account,
            "merchant" | "m" => Dim::Merchant,
            "ccy" | "cur" | "currency" => Dim::Currency,
            "trip" => Dim::Trip,
            _ => return None,
        })
    }
}

/// `cat:a,b` / `-acct:x`: any of `values` (lowercase); `negate` excludes them.
#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    pub dim: Dim,
    pub values: Vec<String>,
    pub negate: bool,
}

/// Inclusive date bounds, each a prefix ("2026", "2026-03", "2026-03-14").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DateRange {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl DateRange {
    pub fn new(from: Option<&str>, to: Option<&str>) -> Self {
        DateRange { from: from.map(str::to_string), to: to.map(str::to_string) }
    }

    /// One year, month or day.
    pub fn prefix(p: &str) -> Self {
        DateRange::new(Some(p), Some(p))
    }

    pub fn contains(&self, date: &str) -> bool {
        if self.from.as_deref().is_some_and(|f| date < f) {
            return false;
        }
        if let Some(t) = self.to.as_deref() {
            if date.get(..t.len()).unwrap_or(date) > t {
                return false;
            }
        }
        true
    }

    /// "2026-09", "2026-01..2026-06", "2026-03..", "..2025"
    pub fn query(&self) -> String {
        match (&self.from, &self.to) {
            (Some(f), Some(t)) if f == t => f.clone(),
            (f, t) => format!("{}..{}", f.as_deref().unwrap_or(""), t.as_deref().unwrap_or("")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Text(String),
    Clause(Clause),
    Date(DateRange),
    /// \|HKD\| > n
    Above(f64),
    /// \|HKD\| < n
    Below(f64),
    /// a <= \|HKD\| <= b
    Between(Option<f64>, Option<f64>),
    /// true = money in
    Direction(bool),
}

/// 500.0 -> "500", 12.5 -> "12.5"
fn num(v: f64) -> String {
    if v.fract() == 0. { format!("{v:.0}") } else { format!("{v}") }
}

fn quote(v: &str) -> String {
    if v.is_empty() || v.chars().any(|c| c.is_whitespace() || c == ',' || c == '"') {
        format!("\"{}\"", v.replace('"', ""))
    } else {
        v.to_string()
    }
}

impl Term {
    pub fn query(&self) -> String {
        match self {
            Term::Text(s) => s.clone(),
            Term::Clause(c) => format!("{}{}:{}", if c.negate { "-" } else { "" }, c.dim.key(),
                c.values.iter().map(|v| quote(v)).collect::<Vec<_>>().join(",")),
            Term::Date(r) => r.query(),
            Term::Above(v) => format!(">{}", num(*v)),
            Term::Below(v) => format!("<{}", num(*v)),
            Term::Between(a, b) => format!("amt:{}..{}", a.map(num).unwrap_or_default(), b.map(num).unwrap_or_default()),
            Term::Direction(true) => "in".into(),
            Term::Direction(false) => "out".into(),
        }
    }

    fn is_amount(&self) -> bool {
        matches!(self, Term::Above(_) | Term::Below(_) | Term::Between(..))
    }
}

/// Split on whitespace, except inside double quotes.
fn tokens(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in input.chars() {
        if c == '"' {
            quoted = !quoted;
            cur.push(c);
        } else if c.is_whitespace() && !quoted {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `a,"b c",d` -> ["a", "b c", "d"], lowercase, empties dropped.
fn values(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in raw.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|v| v.trim().to_lowercase()).filter(|v| !v.is_empty()).collect()
}

fn is_date_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| r.into_iter().all(|i| b.get(i).is_some_and(u8::is_ascii_digit));
    match b.len() {
        4 => digits(0..4) && (s.starts_with("19") || s.starts_with("20")),
        7 => digits(0..4) && b[4] == b'-' && digits(5..7),
        10 => digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10),
        _ => false,
    }
}

fn date_term(s: &str) -> Option<DateRange> {
    if is_date_prefix(s) {
        return Some(DateRange::prefix(s));
    }
    let (a, b) = s.split_once("..")?;
    let ok = |x: &str| x.is_empty() || is_date_prefix(x);
    (ok(a) && ok(b) && !(a.is_empty() && b.is_empty()))
        .then(|| DateRange::new((!a.is_empty()).then_some(a), (!b.is_empty()).then_some(b)))
}

fn amount(s: &str) -> Option<f64> {
    s.replace(',', "").parse().ok().filter(|v: &f64| v.is_finite())
}

fn parse_term(tok: &str) -> Term {
    let lower = tok.to_lowercase();
    let (negate, body) = match lower.strip_prefix('-') {
        Some(rest) if rest.contains(':') => (true, rest),
        _ => (false, lower.as_str()),
    };
    if let Some((k, v)) = body.split_once(':') {
        if let Some(dim) = Dim::from_key(k) {
            return Term::Clause(Clause { dim, values: values(v), negate });
        }
        if k == "amt" && !negate {
            let (a, b) = v.split_once("..").unwrap_or((v, v));
            let (a, b) = (amount(a), amount(b));
            if a.is_some() || b.is_some() {
                return Term::Between(a, b);
            }
        }
    }
    if let Some(v) = lower.strip_prefix('>').and_then(amount) {
        return Term::Above(v);
    }
    if let Some(v) = lower.strip_prefix('<').and_then(amount) {
        return Term::Below(v);
    }
    if let Some(r) = date_term(&lower) {
        return Term::Date(r);
    }
    match lower.as_str() {
        "in" => Term::Direction(true),
        "out" => Term::Direction(false),
        _ => Term::Text(tok.to_string()),
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    pub terms: Vec<Term>,
}

impl Filter {
    pub fn parse(input: &str) -> Self {
        Filter { terms: tokens(input).iter().map(|t| parse_term(t)).collect() }
    }

    /// The query text: every term, in order.
    pub fn query(&self) -> String {
        self.terms.iter().map(Term::query).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// The free-text words.
    pub fn text(&self) -> String {
        self.terms.iter().filter_map(|t| match t {
            Term::Text(s) => Some(s.as_str()),
            _ => None,
        }).collect::<Vec<_>>().join(" ")
    }

    /// Anything besides free text?
    pub fn has_filters(&self) -> bool {
        self.terms.iter().any(|t| match t {
            Term::Text(_) => false,
            Term::Clause(c) => !c.values.is_empty(),
            _ => true,
        })
    }

    /// The values of every `dim` clause with this polarity.
    pub fn values(&self, dim: Dim, negate: bool) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in &self.terms {
            if let Term::Clause(c) = t {
                if c.dim == dim && c.negate == negate {
                    for v in &c.values {
                        if !out.contains(v) {
                            out.push(v.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// Add `value` to the first `dim` clause of that polarity (a new one at
    /// the end when there is none), or take it out again. It leaves the other
    /// polarity: including what was excluded lifts the exclusion.
    pub fn toggle(&mut self, dim: Dim, negate: bool, value: &str) {
        let value = value.to_lowercase();
        let on = self.values(dim, negate).contains(&value);
        for t in self.terms.iter_mut() {
            if let Term::Clause(c) = t {
                if c.dim == dim && (c.negate != negate || on) {
                    c.values.retain(|v| *v != value);
                }
            }
        }
        if !on {
            let first = self.terms.iter_mut().find_map(|t| match t {
                Term::Clause(c) if c.dim == dim && c.negate == negate && !c.values.is_empty() => Some(c),
                _ => None,
            });
            match first {
                Some(c) => c.values.push(value),
                None => self.terms.push(Term::Clause(Clause { dim, values: vec![value], negate })),
            }
        }
        self.terms.retain(|t| !matches!(t, Term::Clause(c) if c.values.is_empty()));
    }

    /// Replace every `dim` clause of that polarity with one holding `values`.
    pub fn set_values(&mut self, dim: Dim, negate: bool, values: &[String]) {
        let at = self.terms.iter().position(|t| matches!(t, Term::Clause(c) if c.dim == dim && c.negate == negate));
        self.terms.retain(|t| !matches!(t, Term::Clause(c) if c.dim == dim && c.negate == negate));
        if !values.is_empty() {
            let term = Term::Clause(Clause { dim, values: values.iter().map(|v| v.to_lowercase()).collect(), negate });
            let at = at.unwrap_or(self.terms.len()).min(self.terms.len());
            self.terms.insert(at, term);
        }
    }

    pub fn remove(&mut self, i: usize) {
        if i < self.terms.len() {
            self.terms.remove(i);
        }
    }

    /// The date range in effect (the last one typed wins).
    pub fn date(&self) -> Option<&DateRange> {
        self.terms.iter().rev().find_map(|t| match t {
            Term::Date(r) => Some(r),
            _ => None,
        })
    }

    pub fn set_date(&mut self, range: Option<DateRange>) {
        self.replace(|t| matches!(t, Term::Date(_)), range.map(Term::Date));
    }

    /// The amount terms (`>`, `<`, `amt:`), replaced by `term`.
    pub fn set_amount(&mut self, term: Option<Term>) {
        self.replace(Term::is_amount, term.filter(Term::is_amount));
    }

    pub fn direction(&self) -> Option<bool> {
        self.terms.iter().rev().find_map(|t| match t {
            Term::Direction(d) => Some(*d),
            _ => None,
        })
    }

    pub fn set_direction(&mut self, inflow: Option<bool>) {
        self.replace(|t| matches!(t, Term::Direction(_)), inflow.map(Term::Direction));
    }

    /// Drop the terms `old` picks; `new` takes the first one's place (or the end).
    fn replace(&mut self, old: impl Fn(&Term) -> bool, new: Option<Term>) {
        let at = self.terms.iter().position(&old);
        self.terms.retain(|t| !old(t));
        if let Some(n) = new {
            let at = at.unwrap_or(self.terms.len()).min(self.terms.len());
            self.terms.insert(at, n);
        }
    }

    /// The filter without `dim`'s including clauses: what a picker for `dim`
    /// counts its options against.
    pub fn without(&self, dim: Dim) -> Filter {
        Filter { terms: self.terms.iter().filter(|t| !matches!(t, Term::Clause(c) if c.dim == dim && !c.negate)).cloned().collect() }
    }

    pub fn compile<'a>(&'a self, trips: &'a Trips) -> Compiled<'a> {
        let merchants = self.terms.iter().any(|t| matches!(t, Term::Clause(c) if c.dim == Dim::Merchant));
        Compiled { f: self, trips, merchants }
    }
}

/// The trips a `trip:` term can name: start date -> (label, its rows).
#[derive(Debug, Clone, Default)]
pub struct Trips(pub HashMap<String, (String, HashSet<usize>)>);

impl Trips {
    pub fn from_trips(trips: &[crate::travel::Trip]) -> Self {
        Trips(trips.iter().map(|t| (t.start.clone(), (t.label(), t.rows().into_iter().collect()))).collect())
    }
}

/// A filter ready to test rows (free text is the search's job).
pub struct Compiled<'a> {
    f: &'a Filter,
    trips: &'a Trips,
    merchants: bool,
}

/// The account key `acct:` matches: "hsbc_card", "n26/savings pocket".
pub fn account_key(e: &Entry) -> String {
    if e.account.is_empty() { e.source.to_lowercase() } else { format!("{}/{}", e.source, e.account).to_lowercase() }
}

impl Compiled<'_> {
    pub fn matches(&self, ix: usize, e: &Entry) -> bool {
        let merchant = if self.merchants { drill::merchant_text(&e.description) } else { String::new() };
        let abs = e.hkd.unwrap_or(e.amount).abs();
        self.f.terms.iter().all(|t| match t {
            Term::Text(_) => true,
            Term::Clause(c) if c.values.is_empty() => true,
            Term::Clause(c) => c.values.iter().any(|v| self.hit(c.dim, v, ix, e, &merchant)) != c.negate,
            Term::Date(r) => r.contains(&e.date),
            Term::Above(m) => abs > *m,
            Term::Below(m) => abs < *m,
            Term::Between(a, b) => a.is_none_or(|a| abs >= a) && b.is_none_or(|b| abs <= b),
            Term::Direction(inflow) => (e.amount > 0.0) == *inflow,
        })
    }

    fn hit(&self, dim: Dim, v: &str, ix: usize, e: &Entry, merchant: &str) -> bool {
        match dim {
            Dim::Category => e.category.to_lowercase().contains(v),
            Dim::Source => format!("{} {}", e.source, e.kind.label()).to_lowercase().contains(v),
            Dim::Account => account_key(e) == v || (!e.account.is_empty() && e.account.to_lowercase() == v),
            Dim::Merchant => merchant.starts_with(v),
            Dim::Currency => e.currency.eq_ignore_ascii_case(v),
            Dim::Trip => self.trips.0.get(v).is_some_and(|(_, rows)| rows.contains(&ix)),
        }
    }

    /// Ledger indices that match, in ledger order.
    pub fn rows(&self, ledger: &Ledger) -> Vec<usize> {
        ledger.entries.iter().enumerate().filter(|(i, e)| self.matches(*i, e)).map(|(i, _)| i).collect()
    }
}

// ---------- the options a picker offers ----------

/// One choice in a picker: what it writes into the query, what it shows,
/// and how many rows (of those the rest of the filter keeps) it has.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub dim: Dim,
    pub value: String,
    pub label: String,
    pub count: usize,
}

/// "HSBC card", "N26 · Savings pocket", "Revolut · GBP", "Revolut (old)".
pub fn account_label(e: &Entry) -> String {
    let base = crate::recurring::account_label(&e.source, &e.account);
    if e.source.contains("_old") { format!("{base} (old)") } else { base }
}

/// "city super" -> "City Super"
pub fn title(s: &str) -> String {
    s.split(' ').map(|w| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" ")
}

/// The choices for `dim`, most rows first, counted over the rows the rest
/// of `filter` keeps (its including `dim` clauses aside). Values already in
/// the filter are always listed, even with no rows.
pub fn choices(ledger: &Ledger, filter: &Filter, trips: &Trips, dim: Dim) -> Vec<Choice> {
    let rest = filter.without(dim);
    let c = rest.compile(trips);
    let money = |e: &Entry| matches!(e.kind, Kind::Bank | Kind::Card);
    let mut counts: HashMap<String, (String, usize)> = HashMap::new();
    let mut add = |value: String, label: String| {
        counts.entry(value).or_insert((label, 0)).1 += 1;
    };
    for (i, e) in ledger.entries.iter().enumerate() {
        if !c.matches(i, e) {
            continue;
        }
        match dim {
            Dim::Category => add(e.category.to_lowercase(), e.category.clone()),
            Dim::Source => {
                add(e.kind.label().to_lowercase(), e.kind.label().to_string());
                if !money(e) {
                    add(e.source.to_lowercase(), e.source.clone());
                }
            }
            Dim::Account if money(e) => add(account_key(e), account_label(e)),
            Dim::Merchant if money(e) => {
                let k = drill::merchant_key(&e.description);
                if !k.is_empty() {
                    add(k.clone(), title(&k));
                }
            }
            Dim::Currency if e.kind != Kind::Crypto => add(e.currency.to_lowercase(), e.currency.clone()),
            Dim::Trip => {
                for (start, (label, rows)) in &trips.0 {
                    if rows.contains(&i) {
                        add(start.clone(), format!("{label} · {start}"));
                    }
                }
            }
            _ => {}
        }
    }
    for v in filter.values(dim, false).into_iter().chain(filter.values(dim, true)) {
        counts.entry(v.clone()).or_insert((v, 0));
    }
    let mut out: Vec<Choice> = counts.into_iter().map(|(value, (label, count))| Choice { dim, value, label, count }).collect();
    match dim {
        // newest trip first
        Dim::Trip => out.sort_by(|a, b| b.value.cmp(&a.value)),
        _ => out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label))),
    }
    out
}

/// A readable name for a value already in a query: the ledger's own
/// spelling ("dining" -> "Dining", "hsbc_card" -> "HSBC card").
pub fn display(ledger: &Ledger, trips: &Trips, dim: Dim, value: &str) -> String {
    let found = match dim {
        Dim::Category => ledger.entries.iter().find(|e| e.category.eq_ignore_ascii_case(value)).map(|e| e.category.clone()),
        Dim::Account => ledger.entries.iter().find(|e| account_key(e) == value).map(account_label),
        Dim::Source => ledger.entries.iter().find(|e| e.kind.label().eq_ignore_ascii_case(value)).map(|e| e.kind.label().to_string()),
        Dim::Merchant => Some(title(value)),
        Dim::Currency => Some(value.to_uppercase()),
        Dim::Trip => trips.0.get(value).map(|(label, _)| format!("{label} · {value}")),
    };
    found.unwrap_or_else(|| value.to_string())
}

// ---------- dates ----------

/// The date presets: (label, range), relative to `today`.
pub fn date_presets(today: NaiveDate) -> Vec<(&'static str, DateRange)> {
    let ym = |d: NaiveDate| d.format("%Y-%m").to_string();
    let back = |n: u32| ym(today.checked_sub_months(chrono::Months::new(n)).unwrap_or(today));
    let year = today.year();
    vec![
        ("This month", DateRange::prefix(&ym(today))),
        ("Last month", DateRange::prefix(&back(1))),
        ("Last 3 months", DateRange::new(Some(&back(2)), None)),
        ("Last 6 months", DateRange::new(Some(&back(5)), None)),
        ("Last 12 months", DateRange::new(Some(&back(11)), None)),
        ("This year", DateRange::prefix(&year.to_string())),
        ("Last year", DateRange::prefix(&(year - 1).to_string())),
    ]
}

/// "Last 3 months" when `r` is a preset, else its query form.
pub fn date_label(r: &DateRange, today: NaiveDate) -> String {
    date_presets(today).into_iter().find(|(_, p)| p == r).map(|(l, _)| l.to_string()).unwrap_or_else(|| match (&r.from, &r.to) {
        (Some(f), Some(t)) if f == t => f.clone(),
        (Some(f), Some(t)) => format!("{f} → {t}"),
        (Some(f), None) => format!("since {f}"),
        (None, Some(t)) => format!("until {t}"),
        (None, None) => "any time".into(),
    })
}

// ---------- what a filtered set adds up to ----------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub count: usize,
    /// HKD at the owner's share, both positive
    pub inflow: f64,
    pub outflow: f64,
    /// every month from the first row's to the last's: (month, in, out)
    pub months: Vec<(String, f64, f64)>,
}

impl Summary {
    pub fn net(&self) -> f64 {
        self.inflow - self.outflow
    }
}

fn month_index(m: &str) -> Option<i32> {
    Some(m.get(0..4)?.parse::<i32>().ok()? * 12 + m.get(5..7)?.parse::<i32>().ok()? - 1)
}

fn month_name(i: i32) -> String {
    format!("{:04}-{:02}", i.div_euclid(12), i.rem_euclid(12) + 1)
}

/// `months` from the first to the last key, gaps filled with the default.
pub fn fill_months<T: Default + Clone>(by: &BTreeMap<String, T>) -> Vec<(String, T)> {
    let (Some(a), Some(b)) = (by.keys().next().and_then(|k| month_index(k)), by.keys().last().and_then(|k| month_index(k))) else {
        return Vec::new();
    };
    (a..=b).map(|i| {
        let m = month_name(i);
        let v = by.get(&m).cloned().unwrap_or_default();
        (m, v)
    }).collect()
}

/// Count, in, out and the months of `rows`, at the owner's share in HKD.
pub fn summarize(ledger: &Ledger, rows: &[usize]) -> Summary {
    let mut s = Summary::default();
    let mut by: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for e in rows.iter().filter_map(|&i| ledger.entries.get(i)) {
        s.count += 1;
        let Some(v) = drill::my_hkd(e) else { continue };
        let m = by.entry(e.date.get(..7).unwrap_or("").to_string()).or_default();
        if v >= 0. {
            s.inflow += v;
            m.0 += v;
        } else {
            s.outflow -= v;
            m.1 -= v;
        }
    }
    s.months = fill_months(&by).into_iter().map(|(m, (i, o))| (m, i, o)).collect();
    s
}

// ---------- the Spending page, filtered ----------

/// Categories fixed by nature (the fallback: the categories table's
/// `fixed = always` decides, [`crate::categories::fixed_always`]).
pub const FIXED_CATEGORIES: [&str; 7] = crate::categories::FIXED_CATEGORIES;

/// One month of Spending's numbers, worked out from the ledger rows a filter keeps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MonthFigures {
    pub income: f64,
    pub spend: f64,
    pub fixed: f64,
    /// bank and card money sent to investments
    pub invested: f64,
    pub by_category: BTreeMap<String, f64>,
}

/// Spending's monthly numbers over the rows `keep` accepts, at the owner's
/// share: income and spending as `drill` defines them; fixed = a fixed
/// category or a merchant the server calls fixed (`fixed_names`, matched as
/// a description prefix, lowercase).
pub fn month_figures(ledger: &Ledger, keep: impl Fn(usize, &Entry) -> bool, fixed_names: &[String]) -> BTreeMap<String, MonthFigures> {
    let mut out: BTreeMap<String, MonthFigures> = BTreeMap::new();
    for (i, e) in ledger.entries.iter().enumerate() {
        if !matches!(e.kind, Kind::Bank | Kind::Card) || !keep(i, e) {
            continue;
        }
        let Some(v) = drill::my_hkd(e) else { continue };
        let m = out.entry(e.date.get(..7).unwrap_or("").to_string()).or_default();
        if drill::is_income(e) {
            m.income += v;
        } else if drill::is_spending(e) {
            m.spend -= v;
            *m.by_category.entry(e.category.clone()).or_default() -= v;
            let desc = e.description.to_lowercase();
            if crate::categories::fixed_always(&e.category) || fixed_names.iter().any(|n| desc.starts_with(n.as_str())) {
                m.fixed -= v;
            }
        } else if e.amount < 0. && crate::categories::investment(&e.category) {
            m.invested -= v;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: Kind, date: &str, source: &str, account: &str, desc: &str, cat: &str, amount: f64, ccy: &str) -> Entry {
        Entry {
            kind, date: date.into(), source: source.into(), account: account.into(), description: desc.into(),
            category: cat.into(), amount, currency: ccy.into(), hkd: Some(amount), share: 1.0, haystack: desc.to_lowercase(), raw_hash: String::new(),
        }
    }

    fn ledger() -> Ledger {
        Ledger { entries: vec![
            e(Kind::Card, "2026-09-20", "hsbc_card", "", "BBMSL*SUSHIRO MOS", "Dining", -180., "HKD"),
            e(Kind::Bank, "2026-09-15", "n26", "Savings pocket", "Landlord", "Rent", -8000., "EUR"),
            e(Kind::Bank, "2026-09-01", "hsbc", "", "Salary", "Salary", 30000., "HKD"),
            e(Kind::Card, "2026-08-11", "hsbc_card", "", "SUSHIRO MK", "Dining", -220., "HKD"),
            e(Kind::Bank, "2026-07-02", "n26", "Main Account", "Office supplies", "Office (reimbursed)", -500., "EUR"),
            e(Kind::Crypto, "2026-06-03", "binance", "", "auto-invest", "auto-invest", 0.01, "BTC"),
        ], ..Default::default() }
    }

    fn rows(q: &str) -> Vec<usize> {
        let l = ledger();
        let f = Filter::parse(q);
        let t = Trips::default();
        f.compile(&t).rows(&l)
    }

    #[test]
    fn old_queries_mean_what_they_did() {
        // the original search language, unchanged
        assert_eq!(rows("cat:dining"), vec![0, 3]);
        assert_eq!(rows("src:card"), vec![0, 3]);
        assert_eq!(rows("src:bank"), vec![1, 2, 4]);
        assert_eq!(rows(">500"), vec![1, 2]);
        assert_eq!(rows("<200"), vec![0, 5]);
        assert_eq!(rows("2026-09"), vec![0, 1, 2]);
        assert_eq!(rows("2026"), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(rows("2026-09-15"), vec![1]);
        assert_eq!(rows("in"), vec![2, 5]);
        assert_eq!(rows("cat:dining 2026-09 out"), vec![0]);
        // repeated tokens of a kind must all hold
        assert_eq!(rows("cat:dining cat:rent"), Vec::<usize>::new());
        let f = Filter::parse("coffee cat:dining src:card >50 2026-09 out");
        assert_eq!(f.text(), "coffee");
        assert_eq!(f.values(Dim::Category, false), vec!["dining"]);
        assert_eq!(f.date(), Some(&DateRange::prefix("2026-09")));
        assert_eq!(f.direction(), Some(false));
        assert_eq!(Filter::parse("12345").text(), "12345");
    }

    #[test]
    fn new_terms() {
        assert_eq!(rows("cat:dining,rent"), vec![0, 1, 3]);
        assert_eq!(rows("-cat:rent -cat:\"office (reimbursed)\" out"), vec![0, 3]);
        assert_eq!(rows("acct:hsbc_card"), vec![0, 3]);
        assert_eq!(rows("acct:hsbc"), vec![2]); // the bank account, not the card
        assert_eq!(rows("acct:\"n26/savings pocket\""), vec![1]);
        assert_eq!(rows("acct:\"main account\""), vec![4]);
        assert_eq!(rows("-acct:\"n26/savings pocket\",\"n26/main account\" src:bank"), vec![2]);
        assert_eq!(rows("merchant:sushiro"), vec![0, 3]);
        assert_eq!(rows("m:sushiro 2026-09"), vec![0]);
        assert_eq!(rows("ccy:eur"), vec![1, 4]);
        assert_eq!(rows("-ccy:eur src:bank"), vec![2]);
        assert_eq!(rows("2026-08..2026-09"), vec![0, 1, 2, 3]);
        assert_eq!(rows("2026-08.."), vec![0, 1, 2, 3]);
        assert_eq!(rows("..2026-07"), vec![4, 5]);
        assert_eq!(rows("2026-08-11..2026-09-01"), vec![2, 3]);
        assert_eq!(rows("amt:180..500"), vec![0, 3, 4]);
        assert_eq!(rows("amt:..200"), vec![0, 5]);
        assert_eq!(rows("amt:8000"), vec![1]);
    }

    #[test]
    fn trips_by_start_day() {
        let l = ledger();
        let mut t = Trips::default();
        t.0.insert("2026-08-10".into(), ("Japan".into(), [3].into_iter().collect()));
        let f = Filter::parse("trip:2026-08-10");
        assert_eq!(f.compile(&t).rows(&l), vec![3]);
        assert_eq!(display(&l, &t, Dim::Trip, "2026-08-10"), "Japan · 2026-08-10");
    }

    #[test]
    fn round_trips_and_edits() {
        let q = "sushi cat:dining,\"office (reimbursed)\" -acct:hsbc_card 2026-01..2026-06 amt:100..500 >20 in";
        let f = Filter::parse(q);
        assert_eq!(f.query(), q);
        assert_eq!(Filter::parse(&f.query()), f);
        assert_eq!(Filter::parse("CAT:Dining").query(), "cat:dining");

        let mut f = Filter::parse("coffee cat:dining 2026");
        f.toggle(Dim::Category, false, "Groceries");
        assert_eq!(f.query(), "coffee cat:dining,groceries 2026");
        f.toggle(Dim::Category, false, "dining");
        assert_eq!(f.query(), "coffee cat:groceries 2026");
        f.toggle(Dim::Category, false, "groceries");
        assert_eq!(f.query(), "coffee 2026");
        f.toggle(Dim::Category, true, "Rent");
        assert_eq!(f.query(), "coffee 2026 -cat:rent");
        // including what was excluded lifts the exclusion
        f.toggle(Dim::Category, false, "rent");
        assert_eq!(f.query(), "coffee 2026 cat:rent");
        f.set_date(Some(DateRange::new(Some("2026-03"), None)));
        assert_eq!(f.query(), "coffee 2026-03.. cat:rent");
        f.set_amount(Some(Term::Above(500.)));
        f.set_direction(Some(false));
        assert_eq!(f.query(), "coffee 2026-03.. cat:rent >500 out");
        f.set_values(Dim::Category, false, &["dining".into(), "city super".into()]);
        assert_eq!(f.query(), "coffee 2026-03.. cat:dining,\"city super\" >500 out");
        f.set_amount(None);
        f.set_date(None);
        f.set_direction(None);
        assert_eq!(f.query(), "coffee cat:dining,\"city super\"");
        assert!(f.has_filters());
        assert!(!Filter::parse("coffee cat:").has_filters());
    }

    #[test]
    fn choices_count_against_the_rest_of_the_filter() {
        let l = ledger();
        let t = Trips::default();
        let f = Filter::parse("cat:dining 2026-09");
        // categories ignore the category clause itself
        let cats = choices(&l, &f, &t, Dim::Category);
        assert_eq!(cats.iter().map(|c| (c.label.as_str(), c.count)).collect::<Vec<_>>(),
            vec![("Dining", 1), ("Rent", 1), ("Salary", 1)]);
        let accts = choices(&l, &Filter::default(), &t, Dim::Account);
        assert_eq!(accts[0].label, "HSBC card");
        assert_eq!(accts[0].value, "hsbc_card");
        assert_eq!(accts[0].count, 2);
        assert!(accts.iter().any(|c| c.value == "n26/savings pocket" && c.label == "N26 · Savings pocket"));
        let m = choices(&l, &Filter::default(), &t, Dim::Merchant);
        assert_eq!((m[0].value.as_str(), m[0].label.as_str(), m[0].count), ("sushiro", "Sushiro", 2));
        assert_eq!(display(&l, &t, Dim::Category, "dining"), "Dining");
        assert_eq!(display(&l, &t, Dim::Account, "hsbc_card"), "HSBC card");
    }

    #[test]
    fn presets_and_labels() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
        let p = date_presets(today);
        assert_eq!(p[0].1.query(), "2026-10");
        assert_eq!(p[1].1.query(), "2026-09");
        assert_eq!(p[2].1.query(), "2026-08..");
        assert_eq!(p[4].1.query(), "2025-11..");
        assert_eq!(p[6].1.query(), "2025");
        assert_eq!(date_label(&DateRange::new(Some("2026-05"), None), today), "Last 6 months");
        assert_eq!(date_label(&DateRange::new(Some("2026-01"), Some("2026-06")), today), "2026-01 → 2026-06");
    }

    #[test]
    fn summary_and_month_figures() {
        let l = ledger();
        let s = summarize(&l, &[0, 3, 2]);
        assert_eq!((s.count, s.inflow, s.outflow, s.net()), (3, 30000., 400., 29600.));
        assert_eq!(s.months, vec![("2026-08".into(), 0., 220.), ("2026-09".into(), 30000., 180.)]);
        let f = Filter::parse("-ccy:eur");
        let t = Trips::default();
        let c = f.compile(&t);
        let m = month_figures(&l, |i, e| c.matches(i, e), &["bbmsl*sushiro".into()]);
        let sep = &m["2026-09"];
        assert_eq!((sep.income, sep.spend, sep.fixed), (30000., 180., 180.));
        assert!(!m.contains_key("2026-07")); // the EUR row was filtered out
        let all = month_figures(&l, |_, _| true, &[]);
        assert_eq!(all["2026-09"].fixed, 8000.); // rent is fixed by category
        assert_eq!(all["2026-07"].by_category["Office (reimbursed)"], 500.);
    }
}
