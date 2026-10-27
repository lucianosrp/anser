//! Recurring money found in the ledger: subscriptions, bills, loan
//! repayments, the card bill and salary. Nothing is tagged by hand: a series
//! is one merchant charging on a steady cadence (weekly, monthly, quarterly,
//! yearly) at a steady amount, with the category as a hint for how much
//! evidence it needs. Also: price changes, the flags of the subscription
//! audit, and the projection the cash-flow calendar draws. UI-free.
//!
//! The thresholds are named constants below; each says what it protects.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::drill;
use crate::ledger::{Entry, Kind, Ledger};

// ---------- thresholds ----------

/// A commitment not seen for this long is ended (the Spending page's
/// fixed-cost list uses the same rule). Longer cadences get 1.5 periods.
pub const ENDED_AFTER_DAYS: i64 = 62;

/// Amounts of one merchant further apart than this ratio are different
/// charges: a coworking chain's HK$12 day passes vs its HK$4,807 office rent, PayPal's
/// €0.29 micro-charges vs a €17.90 plan. A price rise (Spotify +18%, a
/// loan +47%) stays well under it.
const CLUSTER_SPLIT_RATIO: f64 = 1.8;

/// A pause longer than this, and longer than SEGMENT_GAP_PERIODS typical
/// gaps, splits a merchant's history into separate runs (a streaming plan
/// paid in one country, then again years later from another).
const SEGMENT_GAP_DAYS: f64 = 120.;
const SEGMENT_GAP_PERIODS: f64 = 3.;

/// Two consecutive payments further apart than this count as an amount
/// "jitter" step. 10% absorbs card FX noise on a fixed foreign price.
const AMOUNT_TOLERANCE: f64 = 0.10;

/// A price change: the typical amount moved by more than this, and stayed.
pub const PRICE_STEP: f64 = 0.05;

/// Top-ups with no fixed cadence (a prepaid SIM) count only for strongly
/// recurring categories, at least this many times, at most this far apart.
const IRREGULAR_MIN_ROWS: usize = 4;
const IRREGULAR_MAX_GAP: f64 = 120.;

/// "Maybe forgotten": a charge this small a month, in the base currency,
/// is easy to miss (HK$ 120, about US$ 15; a round local figure per
/// currency, US$ 15 worth for the rest).
pub fn small_monthly(base: &str) -> f64 {
    match base {
        "HKD" => 120.,
        "USD" | "EUR" | "CHF" | "CAD" | "AUD" | "NZD" | "SGD" => 15.,
        "GBP" => 12.,
        "JPY" => 2000.,
        "CNY" | "MOP" => 100.,
        "SEK" | "NOK" | "DKK" => 150.,
        _ => 15.,
    }
}

/// Series paying less than this per payment (in the base currency) stay
/// off the calendar: HK$ 5.
fn min_planned(base: &str) -> f64 {
    small_monthly(base) / 24.
}

/// How much evidence a series needs, by how recurring its category is.
#[derive(Clone, Copy, Debug)]
struct Bar {
    /// payments needed for weekly / monthly series
    min_rows: usize,
    /// payments needed for quarterly / yearly series
    min_rows_long: usize,
    /// share of the span's periods with a payment (monthly), or of the gaps
    /// on cadence (other cadences)
    regularity: f64,
    /// share of consecutive payments allowed to differ by > AMOUNT_TOLERANCE
    jitter: f64,
    /// payments per period at most (a daily coffee is "every month" too)
    per_period: f64,
}

/// Subscriptions, utilities, telecom, gym, rent, loans, insurance, salary:
/// a steady cadence is enough; amounts may vary (a power bill, a bonus).
const STRONG: Bar = Bar { min_rows: 3, min_rows_long: 2, regularity: 0.6, jitter: 1.0, per_period: 1.5 };
/// Everything else (fees, office, shopping, other): cadence and amount both steady.
const NEUTRAL: Bar = Bar { min_rows: 4, min_rows_long: 3, regularity: 0.8, jitter: 0.25, per_period: 1.2 };
/// Dining and groceries: ordinary frequent spending, so only a near-perfect
/// pattern counts (a monthly meal plan, not the coffee shop).
const STRICT: Bar = Bar { min_rows: 6, min_rows_long: 4, regularity: 0.9, jitter: 0.1, per_period: 1.1 };

// Which bar a category gets (strong / neutral / strict) and which ones are
// obligations (never "maybe forgotten") are category flags:
// categories::recurring_hint and categories::obligation.

/// Services that do the same job: two active ones are a "duplicate".
/// Matched as whole words of the description. Kept short on purpose.
const SERVICE_GROUPS: [(&str, &[&str]); 5] = [
    ("music", &["spotify", "apple music", "youtube music", "tidal", "deezer", "amazon music"]),
    ("video", &["netflix", "disney", "hbo", "prime video", "youtube premium", "viu", "crunchyroll"]),
    ("VPN", &["nordvpn", "expressvpn", "surfshark", "protonvpn", "mullvad"]),
    ("cloud storage", &["icloud", "google one", "google photos", "dropbox", "onedrive"]),
    ("AI", &["openai", "chatgpt", "anthropic", "claude", "openrouter", "perplexity"]),
];

// ---------- types ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cadence {
    Weekly,
    Monthly,
    Quarterly,
    Yearly,
    /// top-ups with no fixed rhythm; the period is the median gap
    Irregular,
}

impl Cadence {
    pub fn label(self) -> &'static str {
        match self {
            Cadence::Weekly => "weekly",
            Cadence::Monthly => "monthly",
            Cadence::Quarterly => "quarterly",
            Cadence::Yearly => "yearly",
            Cadence::Irregular => "top-ups",
        }
    }

    /// Nominal length in days.
    pub fn days(self) -> f64 {
        match self {
            Cadence::Weekly => 7.,
            Cadence::Monthly => 30.44,
            Cadence::Quarterly => 91.31,
            Cadence::Yearly => 365.25,
            Cadence::Irregular => 30.44,
        }
    }

    /// The cadence a typical gap (days) fits, if any.
    fn from_gap(gap: f64) -> Option<Cadence> {
        match gap {
            g if (5. ..=9.).contains(&g) => Some(Cadence::Weekly),
            g if (25. ..=37.).contains(&g) => Some(Cadence::Monthly),
            g if (80. ..=100.).contains(&g) => Some(Cadence::Quarterly),
            g if (340. ..=390.).contains(&g) => Some(Cadence::Yearly),
            _ => None,
        }
    }

    /// Days a gap may be off its period and still be on cadence.
    fn tolerance(self) -> f64 {
        match self {
            Cadence::Weekly => 2.,
            Cadence::Monthly => 6.,
            Cadence::Quarterly => 12.,
            Cadence::Yearly => 25.,
            Cadence::Irregular => f64::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flow {
    /// money out that is spending (subscriptions, bills, loans, rent)
    Charge,
    /// money in: salary, a partner's repayments
    Income,
    /// paying off the credit card from a bank account
    CardBill,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PriceChange {
    /// the first payment at the new price
    pub date: String,
    /// native currency, cash (the full amount)
    pub from: f64,
    pub to: f64,
    /// HKD at the owner's share, at the latest payment's rate
    pub from_hkd: f64,
    pub to_hkd: f64,
}

impl PriceChange {
    pub fn pct(&self) -> f64 {
        if self.from.abs() > 1e-9 { (self.to - self.from) / self.from * 100. } else { 0. }
    }
}

#[derive(Clone, Debug)]
pub struct Series {
    pub flow: Flow,
    /// the merchant key the rows share
    pub key: String,
    /// a readable merchant name, from the latest description
    pub name: String,
    pub category: String,
    pub cadence: Cadence,
    /// days between payments: the cadence's, or the median gap for top-ups
    pub period_days: f64,
    /// the usual day of the month it lands on (not for weekly / top-ups)
    pub day_of_month: Option<u32>,
    pub currency: String,
    /// median of the payments since the last price change (the latest 12
    /// at most), native currency, cash, positive
    pub typical: f64,
    /// `typical` in HKD at the latest payment's rate: the cash that moves
    pub typical_cash_hkd: f64,
    /// ... and the owner's share of it (shared rent counts 60%, say)
    pub typical_hkd: f64,
    /// the latest payment, native currency, cash, positive
    pub last_amount: f64,
    /// the latest row's share
    pub share: f64,
    pub first: String,
    pub last: String,
    /// the last payment plus one period (may be in the past)
    pub next: String,
    /// typical × payments a year, owner's share / cash
    pub yearly_hkd: f64,
    pub yearly_cash_hkd: f64,
    /// where the latest payment came from
    pub source: String,
    pub account: String,
    /// ledger rows, newest first
    pub rows: Vec<usize>,
    pub active: bool,
    /// amounts move around (a bill, salary with bonuses): show as "≈"
    pub varies: bool,
    /// the latest change of more than PRICE_STEP that stuck
    pub change: Option<PriceChange>,
    /// 0..1, how well the payments fit the cadence
    pub regularity: f64,
}

impl Series {
    /// Paid on a credit card: the card bill moves the cash, not this charge.
    pub fn on_card(&self) -> bool {
        self.source.ends_with("_card")
    }

    /// The account it's paid from, for people ("N26 · Main Account", "HSBC card").
    pub fn account_label(&self) -> String {
        account_label(&self.source, &self.account)
    }

    /// The bank balance key (`net_worth.by_source[].key`) it moves.
    pub fn account_key(&self) -> String {
        account_key(&self.source, &self.account)
    }

    /// Monthly equivalent of the owner's share.
    pub fn monthly_hkd(&self) -> f64 {
        self.yearly_hkd / 12.
    }

    /// The payment after `d`.
    pub fn step(&self, d: NaiveDate) -> NaiveDate {
        let dom = self.day_of_month.unwrap_or(d.day());
        match self.cadence {
            Cadence::Weekly => d + Duration::days(7),
            Cadence::Monthly => add_months(d, 1, dom),
            Cadence::Quarterly => add_months(d, 3, dom),
            Cadence::Yearly => add_months(d, 12, dom),
            Cadence::Irregular => d + Duration::days(self.period_days.round().max(1.) as i64),
        }
    }

    /// The next expected payment on or after `today`. Payments that fell due
    /// between the last one seen and today are assumed made (an account's
    /// export can lag), so they're skipped, not reported as missed.
    pub fn next_from(&self, today: NaiveDate) -> Option<NaiveDate> {
        let mut d = self.step(day(&self.last)?);
        while d < today {
            d = self.step(d);
        }
        Some(d)
    }

    /// Expected payment dates in [from, to].
    pub fn occurrences(&self, from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        let mut out = Vec::new();
        let Some(mut d) = self.next_from(from) else { return out };
        while d <= to {
            out.push(d);
            d = self.step(d);
        }
        out
    }
}

// ---------- helpers ----------

pub fn day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1).and_then(|d| d.pred_opt()).map(|d| d.day()).unwrap_or(28)
}

/// `d` moved `n` months on, landing on day `dom` (clamped to the month's length).
pub fn add_months(d: NaiveDate, n: i32, dom: u32) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + n;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    NaiveDate::from_ymd_opt(y, m, dom.clamp(1, days_in_month(y, m))).unwrap_or(d)
}

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2. }
}

fn within(x: f64, of: f64, tol: f64) -> bool {
    of.abs() > 1e-9 && ((x - of) / of).abs() <= tol
}

/// True when a commitment last seen on `last` has stopped, as of `newest`
/// (the newest row in the ledger). The Spending page uses this too.
pub fn is_ended(last: &str, newest: &str, period_days: f64) -> bool {
    match (day(last), day(newest)) {
        (Some(l), Some(n)) => (n - l).num_days() > ended_after(period_days),
        _ => true,
    }
}

/// Days without a payment after which a series with this period is ended.
pub fn ended_after(period_days: f64) -> i64 {
    ENDED_AFTER_DAYS.max((period_days * 1.5).round() as i64)
}

/// "N26 · Main Account", "Revolut · GBP", "Mox · HKD", "HSBC card", "HSBC".
pub fn account_label(source: &str, account: &str) -> String {
    let s = source.to_lowercase();
    let bank = if s.starts_with("hsbc") {
        "HSBC"
    } else if s.starts_with("n26") {
        "N26"
    } else if s.starts_with("revolut") {
        "Revolut"
    } else if s.starts_with("mox") {
        "Mox"
    } else {
        source
    };
    if source.ends_with("_card") {
        format!("{bank} card")
    } else if account.is_empty() {
        bank.to_string()
    } else {
        format!("{bank} · {account}")
    }
}

/// The key the bundle's `net_worth.by_source` uses for a bank account.
pub fn account_key(source: &str, account: &str) -> String {
    if account.is_empty() { source.to_string() } else { format!("{source}_{account}") }
}

/// The merchant key, or for descriptions the key can't read (an all-digit
/// word like "N26"), the description itself, lowercased.
fn series_key(desc: &str) -> String {
    let k = drill::merchant_key(desc);
    if !k.is_empty() {
        return k;
    }
    desc.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// A readable merchant name from a bank description: references, card
/// numbers, dates, a trailing foreign amount and country code dropped;
/// ALL-CAPS title-cased (short words like "QZX" stay).
/// "ACME GEO LIMITED HC1000… 29SEP" -> "Acme Geo Limited".
pub fn display_name(desc: &str) -> String {
    // Revolut appends the conversion: "Oculus FX Rate £1 = …"
    let desc = desc.split(" FX Rate").next().unwrap_or(desc);
    let mut words: Vec<&str> = desc.split_whitespace()
        .filter(|w| {
            let digits = w.chars().filter(|c| c.is_ascii_digit()).count();
            let letters = w.chars().any(|c| c.is_alphabetic());
            !(digits >= 5 || (digits > 0 && letters) || w.starts_with('(') || w.parse::<f64>().is_ok())
        })
        .collect();
    let caps = |w: &str| w.len() >= 2 && w.chars().all(|c| c.is_ascii_uppercase());
    // "… NEW YORK US USD 10.80": the amount went with the digits; drop the currency and the country
    if words.len() > 2 && words.last().is_some_and(|w| w.len() == 3 && caps(w)) && desc.split_whitespace().last().is_some_and(|w| w.parse::<f64>().is_ok()) {
        words.pop();
    }
    if words.len() > 2 && words.last().is_some_and(|w| w.len() == 2 && caps(w)) {
        words.pop();
    }
    // the city after the merchant says nothing: "QZX HONG KONG HONG KONG"
    let mut out: Vec<&str> = Vec::new();
    for w in words {
        let city = out.len() >= 1 && (w.eq_ignore_ascii_case("hong") || w.eq_ignore_ascii_case("kong") || w.eq_ignore_ascii_case("hongkong"));
        if !city {
            out.push(w);
        }
    }
    let name = out.join(" ").trim_end_matches(',').to_string();
    let shouting = name.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase());
    let name = if shouting {
        name.split(' ').enumerate().map(|(i, w)| {
            // a short first word is a brand's initials ("QZX")
            if i == 0 && w.chars().filter(|c| c.is_alphabetic()).count() <= 3 {
                w.to_string()
            } else {
                let lower = w.to_lowercase();
                let mut c = lower.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        }).collect::<Vec<_>>().join(" ")
    } else {
        name
    };
    if name.is_empty() { desc.trim().to_string() } else { name }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Hint {
    Strong,
    Neutral,
    Strict,
}

impl Hint {
    fn of(category: &str, flow: Flow) -> Hint {
        if flow == Flow::CardBill {
            return Hint::Strong;
        }
        match crate::categories::recurring_hint(category) {
            "strong" => Hint::Strong,
            "strict" => Hint::Strict,
            _ => Hint::Neutral,
        }
    }

    fn bar(self) -> Bar {
        match self {
            Hint::Strong => STRONG,
            Hint::Neutral => NEUTRAL,
            Hint::Strict => STRICT,
        }
    }
}

fn flow_of(e: &Entry) -> Option<Flow> {
    if !matches!(e.kind, Kind::Bank | Kind::Card) || e.hkd.is_none() || e.amount == 0. {
        return None;
    }
    let cat = e.category.to_lowercase();
    if e.kind == Kind::Bank && e.amount < 0. && cat.contains("credit-card bill") {
        Some(Flow::CardBill)
    } else if drill::is_spending(e) {
        Some(Flow::Charge)
    } else if drill::is_income(e) || (e.amount > 0. && cat.contains("partner")) {
        Some(Flow::Income)
    } else {
        None
    }
}

/// Amounts that legitimately move: the card bill, pay, a partner's transfers.
fn amounts_vary(flow: Flow, category: &str) -> bool {
    let c = category.to_lowercase();
    flow == Flow::CardBill || (flow == Flow::Income && (c.contains("salary") || c.contains("partner")))
}

/// Split rows (chronological) into groups of similar amounts.
fn clusters(ledger: &Ledger, rows: &[usize]) -> Vec<Vec<usize>> {
    let amt = |i: usize| ledger.entries[i].amount.abs();
    let mut by_amt = rows.to_vec();
    by_amt.sort_by(|a, b| amt(*a).total_cmp(&amt(*b)));
    let mut out: Vec<Vec<usize>> = Vec::new();
    for i in by_amt {
        match out.last_mut() {
            Some(c) if amt(i) <= amt(*c.last().unwrap()) * CLUSTER_SPLIT_RATIO => c.push(i),
            _ => out.push(vec![i]),
        }
    }
    for c in &mut out {
        c.sort_by(|a, b| ledger.entries[*a].date.cmp(&ledger.entries[*b].date));
    }
    out
}

/// Split chronological rows at long pauses.
fn segments(ledger: &Ledger, rows: &[usize]) -> Vec<Vec<usize>> {
    let dates: Vec<NaiveDate> = rows.iter().filter_map(|&i| day(&ledger.entries[i].date)).collect();
    if dates.len() != rows.len() || rows.len() < 2 {
        return vec![rows.to_vec()];
    }
    let gaps: Vec<f64> = dates.windows(2).map(|w| (w[1] - w[0]).num_days() as f64).collect();
    let limit = SEGMENT_GAP_DAYS.max(SEGMENT_GAP_PERIODS * median(&gaps));
    let mut out = vec![vec![rows[0]]];
    for (k, g) in gaps.iter().enumerate() {
        if *g > limit {
            out.push(Vec::new());
        }
        out.last_mut().unwrap().push(rows[k + 1]);
    }
    out
}

/// (regularity, payments per period) for chronological dates at `cadence`.
fn fit(dates: &[NaiveDate], cadence: Cadence) -> (f64, f64) {
    let n = dates.len();
    if n < 2 {
        return (0., 1.);
    }
    if cadence == Cadence::Monthly {
        let idx = |d: &NaiveDate| d.year() * 12 + d.month0() as i32;
        let months: HashSet<i32> = dates.iter().map(idx).collect();
        let span = (idx(&dates[n - 1]) - idx(&dates[0]) + 1) as f64;
        return (months.len() as f64 / span, n as f64 / months.len() as f64);
    }
    let p = cadence.days();
    let tol = cadence.tolerance();
    let gaps: Vec<f64> = dates.windows(2).map(|w| (w[1] - w[0]).num_days() as f64).collect();
    let on = gaps.iter().filter(|g| (*g - p).abs() <= tol).count();
    let skip = gaps.iter().filter(|g| (*g - 2. * p).abs() <= 2. * tol).count();
    let good = on + skip.min(on);
    let span = (dates[n - 1] - dates[0]).num_days() as f64;
    (good as f64 / gaps.len() as f64, n as f64 / (span / p + 1.))
}

/// The latest price change that stuck: (index of the first new payment,
/// old level, new level). Both sides must be steady; `need` payments each.
fn price_change(amts: &[f64], need: usize) -> Option<(usize, f64, f64)> {
    let n = amts.len();
    if n < 2 * need {
        return None;
    }
    for j in (need..=n - need).rev() {
        let after = &amts[j..];
        let to = median(after);
        if !after.iter().all(|x| within(*x, to, PRICE_STEP)) {
            continue;
        }
        let mut k = j;
        while k > 0 && j - k < 3 && within(amts[k - 1], amts[j - 1], PRICE_STEP) {
            k -= 1;
        }
        let before = &amts[k..j];
        if before.len() < need {
            continue;
        }
        let from = median(before);
        if !within(to, from, PRICE_STEP) {
            return Some((j, from, to));
        }
    }
    None
}

/// Build a series from chronological rows, or None if they aren't one.
fn series(ledger: &Ledger, rows: &[usize], flow: Flow, hint: Hint, newest: &str) -> Option<Series> {
    let es: Vec<&Entry> = rows.iter().map(|&i| &ledger.entries[i]).collect();
    let dates: Vec<NaiveDate> = es.iter().filter_map(|e| day(&e.date)).collect();
    if dates.len() != es.len() || es.len() < 2 {
        return None;
    }
    let bar = hint.bar();
    let category = {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in &es {
            *counts.entry(e.category.as_str()).or_default() += 1;
        }
        counts.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0))).map(|(c, _)| c.to_string()).unwrap_or_default()
    };
    let varies_by_nature = amounts_vary(flow, &category);
    let gaps: Vec<f64> = dates.windows(2).map(|w| (w[1] - w[0]).num_days() as f64).collect();
    let gap = median(&gaps);
    let amts: Vec<f64> = es.iter().map(|e| e.amount.abs()).collect();
    let jitter = amts.windows(2).filter(|w| !within(w[1], w[0], AMOUNT_TOLERANCE)).count() as f64 / (amts.len() - 1) as f64;

    let (cadence, regularity) = match Cadence::from_gap(gap) {
        Some(c) => {
            let long = matches!(c, Cadence::Quarterly | Cadence::Yearly);
            let need = if long { bar.min_rows_long } else { bar.min_rows };
            let (reg, per) = fit(&dates, c);
            if es.len() < need || reg < bar.regularity || per > bar.per_period {
                return None;
            }
            if !varies_by_nature && jitter > bar.jitter {
                return None;
            }
            (c, reg)
        }
        None => {
            let span = (dates[dates.len() - 1] - dates[0]).num_days() as f64;
            if hint != Hint::Strong || es.len() < IRREGULAR_MIN_ROWS || gap > IRREGULAR_MAX_GAP || gap < 1. || span < 90. {
                return None;
            }
            (Cadence::Irregular, 0.)
        }
    };

    let last = *es.last()?;
    let long = matches!(cadence, Cadence::Quarterly | Cadence::Yearly);
    // a "price" is what you pay: interest moving by a cent is not news
    let steady = flow == Flow::Charge && !varies_by_nature && cadence != Cadence::Irregular;
    let change = if steady { price_change(&amts, if long { 1 } else { 2 }) } else { None };
    let since = change.map(|c| c.0).unwrap_or(0);
    let recent = &amts[since.max(amts.len().saturating_sub(12))..];
    let typical = median(recent);
    let rate = last.hkd.map(|h| (h / last.amount).abs()).filter(|r| r.is_finite()).unwrap_or(1.);
    let share = last.share;
    let period_days = if cadence == Cadence::Irregular { gap } else { cadence.days() };
    let day_of_month = (!matches!(cadence, Cadence::Weekly | Cadence::Irregular)).then(|| {
        let tail = &dates[dates.len().saturating_sub(6)..];
        median(&tail.iter().map(|d| d.day() as f64).collect::<Vec<_>>()).round() as u32
    });
    let per_year = 365.25 / period_days;
    let mut s = Series {
        flow,
        key: series_key(&last.description),
        name: display_name(&last.description),
        category,
        cadence,
        period_days,
        day_of_month,
        currency: last.currency.clone(),
        typical,
        typical_cash_hkd: typical * rate,
        typical_hkd: typical * rate * share,
        last_amount: last.amount.abs(),
        share,
        first: es[0].date.clone(),
        last: last.date.clone(),
        next: String::new(),
        yearly_hkd: typical * rate * share * per_year,
        yearly_cash_hkd: typical * rate * per_year,
        source: last.source.clone(),
        account: last.account.clone(),
        rows: rows.iter().rev().copied().collect(),
        active: !is_ended(&last.date, newest, period_days),
        varies: varies_by_nature || cadence == Cadence::Irregular || jitter > NEUTRAL.jitter,
        change: change.map(|(j, from, to)| PriceChange {
            date: es[j].date.clone(),
            from,
            to,
            from_hkd: from * rate * share,
            to_hkd: to * rate * share,
        }),
        regularity,
    };
    s.next = s.step(*dates.last()?).format("%Y-%m-%d").to_string();
    Some(s)
}

/// Every recurring series in the ledger: active ones first, then by yearly
/// cost (owner's share), biggest first.
pub fn detect(ledger: &Ledger) -> Vec<Series> {
    let newest = ledger.entries.iter().map(|e| e.date.as_str()).max().unwrap_or("").to_string();
    // (flow, merchant key, currency, hint) -> rows
    let mut groups: HashMap<(Flow, String, String, Hint), Vec<usize>> = HashMap::new();
    for (i, e) in ledger.entries.iter().enumerate() {
        let Some(flow) = flow_of(e) else { continue };
        let key = if flow == Flow::CardBill { "card bill".to_string() } else { series_key(&e.description) };
        if key.is_empty() {
            continue;
        }
        groups.entry((flow, key, e.currency.clone(), Hint::of(&e.category, flow))).or_default().push(i);
    }
    let mut out = Vec::new();
    for ((flow, _, _, hint), mut rows) in groups {
        rows.sort_by(|a, b| ledger.entries[*a].date.cmp(&ledger.entries[*b].date));
        let category = ledger.entries[rows[0]].category.clone();
        // steady-price merchants split by amount; strong categories and
        // pay/bills keep their rows together (a power bill, a pro-rata first month)
        let parts = if hint == Hint::Strong || amounts_vary(flow, &category) { vec![rows] } else { clusters(ledger, &rows) };
        for part in parts {
            for seg in segments(ledger, &part) {
                if let Some(s) = series(ledger, &seg, flow, hint, &newest) {
                    out.push(s);
                }
            }
        }
    }
    out.sort_by(|a, b| b.active.cmp(&a.active).then(b.yearly_hkd.total_cmp(&a.yearly_hkd)).then(a.key.cmp(&b.key)));
    out
}

// ---------- the audit's flags ----------

#[derive(Clone, Debug, PartialEq)]
pub enum Flag {
    /// the price rose by more than PRICE_STEP in the last year
    PriceUp,
    /// still charging, and easy to lose track of (the reason)
    MaybeForgotten(String),
    /// stopped charging
    Ended,
    /// another active charge does the same job (the group: "music", or the merchant)
    Duplicate(String),
}

fn words(text: &str) -> String {
    let cleaned: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    format!(" {} ", cleaned.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The service group of a merchant name, if it's in SERVICE_GROUPS.
pub fn service_group(name: &str) -> Option<&'static str> {
    let w = words(name);
    SERVICE_GROUPS.iter().find(|(_, names)| names.iter().any(|n| w.contains(&words(n)))).map(|(g, _)| *g)
}

/// The currency the owner lives in: the biggest active salary's, else the
/// base currency.
pub fn home_currency(series: &[Series]) -> String {
    series.iter()
        .filter(|s| s.active && s.flow == Flow::Income && s.category.to_lowercase().contains("salary"))
        .max_by(|a, b| a.yearly_cash_hkd.total_cmp(&b.yearly_cash_hkd))
        .map(|s| s.currency.clone())
        .unwrap_or_else(crate::base::currency)
}

/// Flags for each series (same order). Only charges get flags. `newest` is
/// the newest ledger date (a price rise counts if it's within a year of it).
pub fn flags(series: &[Series], newest: &str) -> Vec<Vec<Flag>> {
    let home = home_currency(series);
    let small = small_monthly(&crate::base::currency());
    let year_ago = day(newest).map(|d| d - Duration::days(365));
    // who does the same job: by service group, and the same merchant twice
    let mut by_group: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in series.iter().enumerate().filter(|(_, s)| s.active && s.flow == Flow::Charge) {
        if let Some(g) = service_group(&s.name) {
            by_group.entry(g.to_string()).or_default().push(i);
        }
        if s.category.to_lowercase().contains("gym") {
            by_group.entry("gym".into()).or_default().push(i);
        }
        by_group.entry(format!("={}", s.key)).or_default().push(i);
    }
    let mut dup: HashMap<usize, String> = HashMap::new();
    for (g, ix) in &by_group {
        if ix.len() >= 2 {
            for &i in ix {
                dup.entry(i).or_insert_with(|| g.strip_prefix('=').map(|_| "same merchant twice".to_string()).unwrap_or_else(|| g.clone()));
            }
        }
    }
    series.iter().enumerate().map(|(i, s)| {
        let mut f = Vec::new();
        if s.flow != Flow::Charge {
            return f;
        }
        if !s.active {
            f.push(Flag::Ended);
            return f;
        }
        if let Some(c) = &s.change {
            if c.to > c.from && day(&c.date).zip(year_ago).is_some_and(|(d, y)| d >= y) {
                f.push(Flag::PriceUp);
            }
        }
        if !crate::categories::obligation(&s.category) {
            if s.currency != home {
                f.push(Flag::MaybeForgotten(format!("paid in {} from {}", s.currency, s.account_label())));
            } else if s.monthly_hkd() < small {
                f.push(Flag::MaybeForgotten("small enough to miss".into()));
            }
        }
        if let Some(g) = dup.get(&i) {
            f.push(Flag::Duplicate(g.clone()));
        }
        f
    }).collect()
}

/// HKD (owner's share) of the charge series' rows per month ("YYYY-MM"),
/// in the order given.
pub fn monthly_totals(ledger: &Ledger, series: &[Series], months: &[String]) -> Vec<f64> {
    let mut by: HashMap<&str, f64> = HashMap::new();
    for s in series.iter().filter(|s| s.flow == Flow::Charge) {
        for &i in &s.rows {
            let e = &ledger.entries[i];
            if let Some(m) = e.date.get(..7) {
                *by.entry(m).or_default() += -drill::my_hkd(e).unwrap_or(0.);
            }
        }
    }
    months.iter().map(|m| by.get(m.as_str()).copied().unwrap_or(0.)).collect()
}

// ---------- the cash-flow calendar ----------

/// A commitment the ledger can't see yet (rent before the first payment),
/// from the calendar config.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manual {
    pub label: String,
    /// HKD: negative = money out, positive = money in
    pub amount: f64,
    /// day of the month (clamped to the month's length)
    #[serde(default = "first_day")]
    pub day: u32,
    /// "YYYY-MM": the first month (for `once`, the month it happens)
    #[serde(default)]
    pub from: Option<String>,
    /// "YYYY-MM": the last month, inclusive
    #[serde(default)]
    pub until: Option<String>,
    /// "monthly" (default) or "once"
    #[serde(default)]
    pub every: Option<String>,
    /// once an active detected series of this category (same direction)
    /// exists, the entry steps aside: the data has caught up
    #[serde(default)]
    pub category: Option<String>,
    /// the bank account it moves (`by_source` key); none = the main account
    #[serde(default)]
    pub account: Option<String>,
}

fn first_day() -> u32 {
    1
}

impl Manual {
    pub fn once(&self) -> bool {
        self.every.as_deref().is_some_and(|e| e.eq_ignore_ascii_case("once"))
    }

    /// An active detected series now covers it.
    pub fn superseded(&self, series: &[Series]) -> bool {
        let Some(cat) = self.category.as_deref().filter(|c| !c.is_empty()) else { return false };
        series.iter().any(|s| {
            s.active && s.category.eq_ignore_ascii_case(cat)
                && (s.flow == Flow::Income) == (self.amount > 0.)
        })
    }

    pub fn occurrences(&self, from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        let ym = |s: &Option<String>| s.as_deref().and_then(|m| NaiveDate::parse_from_str(&format!("{}-01", m.get(..7)?), "%Y-%m-%d").ok());
        let start = ym(&self.from).unwrap_or_else(|| from.with_day(1).unwrap_or(from));
        let end_month = ym(&self.until);
        let mut out = Vec::new();
        let mut m = start;
        loop {
            let d = add_months(m, 0, self.day);
            if d > to || end_month.is_some_and(|e| m > e) {
                break;
            }
            if d >= from {
                out.push(d);
            }
            if self.once() {
                break;
            }
            m = add_months(m, 1, 1);
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Origin {
    /// index into the detected series
    Series(usize),
    /// index into the manual entries
    Manual(usize),
}

/// One expected payment.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub date: NaiveDate,
    pub label: String,
    pub category: String,
    /// signed HKD that moves through the accounts (the full amount)
    pub cash_hkd: f64,
    /// signed HKD at the owner's share
    pub mine_hkd: f64,
    /// false for a credit-card charge: the card bill moves the cash
    pub affects_cash: bool,
    /// the amount is an estimate (a bill, pay, top-ups)
    pub estimate: bool,
    /// `by_source` key of the account it moves ("" = unknown / main)
    pub account: String,
    pub origin: Origin,
}

/// Everything expected in [from, to], by date (then biggest first).
pub fn plan(series: &[Series], manual: &[Manual], from: NaiveDate, to: NaiveDate) -> Vec<Planned> {
    let mut out = Vec::new();
    // a few cents of interest a month is noise on a calendar
    let min = min_planned(&crate::base::currency());
    for (i, s) in series.iter().enumerate().filter(|(_, s)| s.active && s.typical_cash_hkd >= min) {
        let sign = if s.flow == Flow::Income { 1. } else { -1. };
        for d in s.occurrences(from, to) {
            out.push(Planned {
                date: d,
                label: s.name.clone(),
                category: s.category.clone(),
                cash_hkd: sign * s.typical_cash_hkd,
                mine_hkd: sign * s.typical_hkd,
                affects_cash: !(s.flow == Flow::Charge && s.on_card()),
                estimate: s.varies,
                account: s.account_key(),
                origin: Origin::Series(i),
            });
        }
    }
    for (i, m) in manual.iter().enumerate() {
        if m.superseded(series) || m.amount == 0. {
            continue;
        }
        for d in m.occurrences(from, to) {
            out.push(Planned {
                date: d,
                label: m.label.clone(),
                category: m.category.clone().unwrap_or_default(),
                cash_hkd: m.amount,
                mine_hkd: m.amount,
                affects_cash: true,
                estimate: false,
                account: m.account.clone().unwrap_or_default(),
                origin: Origin::Manual(i),
            });
        }
    }
    out.sort_by(|a, b| a.date.cmp(&b.date).then(a.cash_hkd.abs().total_cmp(&b.cash_hkd.abs()).reverse()));
    out
}

/// End-of-day balance for each day in [from, to], starting from `start`
/// (the cash before `from`), moved by the cash-affecting items in `plan`.
pub fn balance_path(start: f64, plan: &[Planned], from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, f64)> {
    let mut out = Vec::new();
    let mut bal = start;
    let mut d = from;
    while d <= to {
        bal += plan.iter().filter(|p| p.affects_cash && p.date == d).map(|p| p.cash_hkd).sum::<f64>();
        out.push((d, bal));
        d = d + Duration::days(1);
    }
    out
}

/// The next weeks at a glance (the Overview tile).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outlook {
    /// cash out / in, positive
    pub out_hkd: f64,
    pub in_hkd: f64,
    /// the first payment of at least `big` HKD, else the biggest one
    pub next_big: Option<Planned>,
}

pub fn outlook(plan: &[Planned], big: f64) -> Outlook {
    let cash: Vec<&Planned> = plan.iter().filter(|p| p.affects_cash).collect();
    let out_hkd = cash.iter().filter(|p| p.cash_hkd < 0.).map(|p| -p.cash_hkd).sum();
    let in_hkd = cash.iter().filter(|p| p.cash_hkd > 0.).map(|p| p.cash_hkd).sum();
    let next_big = cash.iter().find(|p| p.cash_hkd <= -big)
        .or_else(|| cash.iter().filter(|p| p.cash_hkd < 0.).min_by(|a, b| a.cash_hkd.total_cmp(&b.cash_hkd)))
        .map(|p| (*p).clone());
    Outlook { out_hkd, in_hkd, next_big }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(date: &str, desc: &str, cat: &str, amount: f64, ccy: &str, rate: f64, source: &str, account: &str) -> Entry {
        let kind = if source.ends_with("_card") { Kind::Card } else { Kind::Bank };
        Entry {
            kind, date: date.into(), source: source.into(), account: account.into(), description: desc.into(),
            category: cat.into(), amount, currency: ccy.into(), hkd: Some(amount * rate), share: 1.0,
            haystack: String::new(), raw_hash: String::new(),
        }
    }

    fn d(s: &str) -> NaiveDate {
        day(s).unwrap()
    }

    /// `n` monthly payments on `dom` starting at (y, m).
    fn monthly(out: &mut Vec<Entry>, y: i32, m: u32, n: i32, dom: u32, desc: &str, cat: &str, amt: impl Fn(i32) -> f64, ccy: &str, rate: f64, source: &str, account: &str) {
        let start = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
        for k in 0..n {
            let dt = add_months(start, k, dom).format("%Y-%m-%d").to_string();
            out.push(entry(&dt, desc, cat, amt(k), ccy, rate, source, account));
        }
    }

    fn ledger(mut entries: Vec<Entry>) -> Ledger {
        entries.sort_by(|a, b| b.date.cmp(&a.date));
        Ledger { entries, ..Default::default() }
    }

    /// A realistic year: subscriptions, a loan with a step, gym, salary, the
    /// card bill, office rent with day passes, and lots of everyday noise.
    fn sample() -> Ledger {
        let mut v = Vec::new();
        // Spotify on N26 (EUR): 10.99 then 12.99 from the 9th payment
        monthly(&mut v, 2025, 3, 18, 25, "Spotify", "Subscriptions", |k| if k < 8 { -10.99 } else { -12.99 }, "EUR", 8.9, "n26", "Main Account");
        // a GBP loan from Revolut: 150 then 220
        monthly(&mut v, 2025, 4, 17, 24, "SWIFT transfer to Example Loans Ltd", "Student loan", |k| if k < 8 { -150. } else { -220. }, "GBP", 10.4, "revolut_new", "GBP");
        // gym on the card: a pro-rata first month, then 550
        monthly(&mut v, 2026, 1, 10, 1, "ZENITH EXAMPLE FITNESS HONG KONG HK", "Gym", |k| if k == 0 { -53. } else { -550. }, "HKD", 1., "hsbc_card", "");
        // salary with bonuses
        monthly(&mut v, 2025, 6, 16, 28, "ACME GEO LIMITED HC1000001 27JUN", "Salary", |k| if k == 9 { 98000. } else { 52000. }, "HKD", 1., "hsbc", "");
        // card bill, all over the place
        monthly(&mut v, 2025, 6, 16, 18, "CREDIT CARD PAYMENT 5555555555554444", "Credit-card bill", |k| -(1000. + 2700. * k as f64), "HKD", 1., "hsbc", "");
        // office rent (a slid payment: day 6, 2, 4, 15, 8, 4, 3) + HK$12 day passes almost daily
        for (i, dt) in ["2026-03-06", "2026-04-02", "2026-05-04", "2026-06-15", "2026-07-08", "2026-08-04", "2026-09-03"].iter().enumerate() {
            v.push(entry(dt, "QZX HONG KONG HONG KONG HK", "Office (reimbursed)", if i == 0 { -4700. } else { -4800. }, "HKD", 1., "hsbc_card", ""));
        }
        for k in 0..40 {
            let dt = (d("2026-03-02") + Duration::days(k * 5)).format("%Y-%m-%d").to_string();
            v.push(entry(&dt, "QZX HONG KONG LIMITED HONG KONG HK", "Office (reimbursed)", -12., "HKD", 1., "hsbc_card", ""));
        }
        // coffee two or three times a week, steady price: must not be a subscription
        for k in 0..120 {
            let dt = (d("2025-06-01") + Duration::days(k * 3)).format("%Y-%m-%d").to_string();
            v.push(entry(&dt, "PACIFIC COFFEE CO LTD", "Dining", -38., "HKD", 1., "hsbc_card", ""));
        }
        // groceries about weekly, varying amounts
        for k in 0..60 {
            let dt = (d("2025-06-02") + Duration::days(k * 7 + (k % 3))).format("%Y-%m-%d").to_string();
            v.push(entry(&dt, "WELLCOME 123 SHA TIN", "Groceries", -(120. + (k * 37 % 90) as f64), "HKD", 1., "hsbc_card", ""));
        }
        // a monthly lunch at the same place for 4 months (ordinary, not recurring enough)
        monthly(&mut v, 2026, 3, 4, 12, "BAMBOO KITCHEN", "Dining", |_| -280., "HKD", 1., "hsbc_card", "");
        // Uber rides at random-ish times
        for k in 0..30 {
            let dt = (d("2025-11-02") + Duration::days(k * 11 % 300)).format("%Y-%m-%d").to_string();
            v.push(entry(&dt, "UBER *TRIP", "Transport", -(60. + (k * 13 % 50) as f64), "HKD", 1., "hsbc_card", ""));
        }
        // prepaid SIM top-ups, irregular
        for (dt, a) in [("2025-02-21", -10.), ("2025-04-14", -15.), ("2025-07-01", -20.), ("2025-09-19", -20.), ("2025-12-18", -15.), ("2026-01-23", -10.), ("2026-04-02", -20.), ("2026-06-11", -20.), ("2026-07-09", -15.)] {
            v.push(entry(dt, "Vodafone Italia S.p.A", "Utilities / Telecom", a, "EUR", 8.9, "n26", "Main Account"));
        }
        // N26's own monthly fee: the description is just "N26"
        monthly(&mut v, 2025, 2, 19, 19, "N26", "Fees", |_| -4.9, "EUR", 8.9, "n26", "Main Account");
        // a yearly VPN, twice
        v.push(entry("2024-11-03", "A.NordVPN", "Subscriptions", -80.33, "GBP", 10.4, "revolut_new", "GBP"));
        v.push(entry("2025-11-04", "A.NordVPN", "Subscriptions", -80.33, "GBP", 10.4, "revolut_new", "GBP"));
        // an old rent that stopped
        monthly(&mut v, 2022, 5, 30, 4, "FlatcoLiving", "Rent", |_| -650., "EUR", 8.5, "n26", "House");
        // internal transfers never count
        monthly(&mut v, 2025, 1, 20, 2, "To savings", "Internal transfer", |_| -500., "HKD", 1., "hsbc", "");
        // the newest row: 2026-10-02
        v.push(entry("2026-10-02", "PACIFIC COFFEE CO LTD", "Dining", -38., "HKD", 1., "hsbc_card", ""));
        ledger(v)
    }

    fn find<'a>(s: &'a [Series], key: &str) -> Option<&'a Series> {
        s.iter().find(|x| x.key == key)
    }

    #[test]
    fn finds_the_real_commitments() {
        let s = detect(&sample());
        let spotify = find(&s, "spotify").expect("spotify");
        assert_eq!(spotify.cadence, Cadence::Monthly);
        assert_eq!(spotify.flow, Flow::Charge);
        assert!(spotify.active);
        assert_eq!(spotify.typical, 12.99);
        assert_eq!(spotify.day_of_month, Some(25));
        assert_eq!(spotify.account_label(), "N26 · Main Account");
        assert!((spotify.yearly_hkd - 12.99 * 8.9 * 12.).abs() < 1.);

        let loan = find(&s, "swift").expect("loan");
        assert_eq!(loan.typical, 220.);
        assert_eq!(loan.account_label(), "Revolut · GBP");

        let gym = find(&s, "fitness hong").or_else(|| s.iter().find(|x| x.category == "Gym")).expect("gym");
        assert_eq!(gym.typical, 550.);
        assert!(gym.change.is_none(), "a pro-rata first month is not a price change");
        assert!(gym.on_card());

        let pay = s.iter().find(|x| x.flow == Flow::Income).expect("salary");
        assert_eq!(pay.cadence, Cadence::Monthly);
        assert_eq!(pay.typical, 52000.);
        assert_eq!(pay.name, "Acme Geo Limited");

        let bill = s.iter().find(|x| x.flow == Flow::CardBill).expect("card bill");
        assert_eq!(bill.day_of_month, Some(18));
        assert!(bill.varies);

        let office = s.iter().find(|x| x.category.starts_with("Office") && x.typical > 1000.).expect("office rent");
        assert_eq!(office.cadence, Cadence::Monthly);
        assert_eq!(office.typical, 4800.);
        assert!(!s.iter().any(|x| x.category.starts_with("Office") && x.typical < 100.), "day passes are not a subscription");

        let sim = find(&s, "vodafone").expect("prepaid SIM");
        assert_eq!(sim.cadence, Cadence::Irregular);

        let fee = find(&s, "n26").expect("the N26 fee, whose description has no letters-only word");
        assert_eq!(fee.cadence, Cadence::Monthly);

        let vpn = s.iter().find(|x| x.key.contains("nordvpn")).expect("yearly VPN");
        assert_eq!(vpn.cadence, Cadence::Yearly);
        assert!(vpn.active, "11 months since the last payment is within 1.5 years");
    }

    #[test]
    fn everyday_spending_is_not_recurring() {
        let s = detect(&sample());
        for noise in ["pacific", "wellcome", "uber trip", "ho hung"] {
            assert!(find(&s, noise).is_none(), "{noise} flagged as recurring");
        }
        assert!(!s.iter().any(|x| x.category.contains("transfer")));
    }

    #[test]
    fn price_changes_are_dated() {
        let s = detect(&sample());
        let spotify = find(&s, "spotify").unwrap();
        let c = spotify.change.as_ref().expect("spotify rose");
        assert_eq!((c.from, c.to), (10.99, 12.99));
        assert_eq!(c.date, "2025-11-25");
        assert!((c.pct() - 18.2).abs() < 0.1);
        let loan = find(&s, "swift").unwrap();
        assert_eq!(loan.change.as_ref().unwrap().date, "2025-12-24");
        // salary bonuses are not a "price"
        assert!(s.iter().find(|x| x.flow == Flow::Income).unwrap().change.is_none());
    }

    #[test]
    fn ended_uses_the_spending_rule() {
        let s = detect(&sample());
        let rent = find(&s, "flatcoliving").expect("old rent");
        assert!(!rent.active);
        assert!(is_ended("2026-07-01", "2026-10-02", 30.44));
        assert!(!is_ended("2026-08-01", "2026-10-02", 30.44));
        assert_eq!(ended_after(30.44), ENDED_AFTER_DAYS);
        assert_eq!(ended_after(365.25), 548);
        assert!(is_ended("", "2026-10-02", 30.));
    }

    #[test]
    fn flags_for_the_audit() {
        let s = detect(&sample());
        let f = flags(&s, "2026-10-02");
        let of = |key: &str| &f[s.iter().position(|x| x.key == key).unwrap()];
        assert!(of("spotify").contains(&Flag::PriceUp));
        assert!(of("spotify").iter().any(|x| matches!(x, Flag::MaybeForgotten(_))), "EUR while living on HKD");
        assert!(!of("swift").iter().any(|x| matches!(x, Flag::MaybeForgotten(_))), "a loan is not forgotten");
        assert!(of("flatcoliving").contains(&Flag::Ended));
        let gym = s.iter().position(|x| x.category == "Gym").unwrap();
        assert!(f[gym].is_empty(), "{:?}", f[gym]);
        assert_eq!(home_currency(&s), "HKD");
    }

    #[test]
    fn duplicates_are_conservative() {
        let mut v = Vec::new();
        monthly(&mut v, 2026, 1, 9, 3, "Spotify", "Subscriptions", |_| -12.99, "EUR", 8.9, "n26", "Main Account");
        monthly(&mut v, 2026, 1, 9, 9, "APPLE MUSIC", "Subscriptions", |_| -68., "HKD", 1., "hsbc_card", "");
        monthly(&mut v, 2026, 1, 9, 12, "NETFLIX.COM", "Subscriptions", |_| -93., "HKD", 1., "hsbc_card", "");
        let s = detect(&ledger(v));
        let f = flags(&s, "2026-09-12");
        let dups: Vec<&str> = s.iter().zip(&f).filter(|(_, f)| f.iter().any(|x| matches!(x, Flag::Duplicate(g) if g == "music"))).map(|(s, _)| s.key.as_str()).collect();
        assert_eq!(dups.len(), 2, "{dups:?}");
        assert!(!dups.contains(&"netflix"));
    }

    #[test]
    fn names_read_well() {
        assert_eq!(display_name("ACME GEO LIMITED     HC10000000000001   29SEP"), "Acme Geo Limited");
        assert_eq!(display_name("ZENITH EXAMPLE FITNESS HONG KONG HK"), "Zenith Example Fitness");
        assert_eq!(display_name("QZX HONG KONG HONG KONG HK"), "QZX");
        assert_eq!(display_name("CLOUDAPI, INC NEW YORK US USD 20.00"), "Cloudapi, Inc New York");
        assert_eq!(display_name("Spotify"), "Spotify");
        assert_eq!(display_name("SWIFT transfer to Example Loans Ltd"), "SWIFT transfer to Example Loans Ltd");
        assert_eq!(display_name("CREDIT CARD PAYMENT      5555555555554444"), "Credit Card Payment");
        assert_eq!(account_label("hsbc_card", ""), "HSBC card");
        assert_eq!(account_label("mox", "HKD"), "Mox · HKD");
        assert_eq!(account_key("n26", "Main Account"), "n26_Main Account");
    }

    #[test]
    fn dates_step_by_calendar_month() {
        assert_eq!(add_months(d("2026-01-31"), 1, 31), d("2026-02-28"));
        assert_eq!(add_months(d("2026-11-15"), 3, 30), d("2027-02-28"));
        assert_eq!(add_months(d("2026-12-01"), 0, 18), d("2026-12-18"));
        let s = detect(&sample());
        let spotify = find(&s, "spotify").unwrap();
        // last seen 2026-08-25; the September one is assumed paid (export lag)
        assert_eq!(spotify.next_from(d("2026-10-03")), Some(d("2026-10-25")));
        assert_eq!(spotify.occurrences(d("2026-10-03"), d("2026-12-31")), vec![d("2026-10-25"), d("2026-11-25"), d("2026-12-25")]);
    }

    #[test]
    fn plan_balance_and_outlook() {
        let s = detect(&sample());
        let rent = Manual { label: "Rent".into(), amount: -18000., day: 1, from: Some("2026-12".into()), until: Some("2028-11".into()), category: Some("Rent".into()), ..Default::default() };
        let back = Manual { label: "Partner's share".into(), amount: 6000., day: 5, from: Some("2026-12".into()), ..Default::default() };
        let manual = vec![rent.clone(), back];
        let p = plan(&s, &manual, d("2026-12-01"), d("2026-12-31"));
        assert_eq!(p.first().map(|x| (x.date, x.cash_hkd)), Some((d("2026-12-01"), -18000.)));
        // the gym rides on the card: listed, but the cash moves with the card bill
        let gym = p.iter().find(|x| x.category == "Gym").unwrap();
        assert!(!gym.affects_cash);
        assert!(p.iter().any(|x| x.origin == Origin::Manual(1) && x.cash_hkd == 6000.));
        let path = balance_path(100_000., &p, d("2026-12-01"), d("2026-12-31"));
        assert_eq!(path.len(), 31);
        assert_eq!(path[0].1, 100_000. - 18000.);
        let low = path.iter().map(|x| x.1).fold(f64::MAX, f64::min);
        assert!(low < 100_000.);
        let o = outlook(&p, 2000.);
        assert_eq!(o.next_big.as_ref().map(|x| x.label.as_str()), Some("Rent"));
        assert!(o.in_hkd >= 52000. + 6000.);
        // before December the rent isn't due
        assert!(rent.occurrences(d("2026-10-01"), d("2026-11-30")).is_empty());
        assert_eq!(rent.occurrences(d("2028-11-01"), d("2029-02-01")), vec![d("2028-11-01")]);
        // once a Rent series is active, the manual entry steps aside
        let mut v = Vec::new();
        monthly(&mut v, 2026, 7, 3, 1, "CR TO 123-456789-001", "Rent", |_| -18000., "HKD", 1., "hsbc", "");
        assert!(rent.superseded(&detect(&ledger(v))));
        let once = Manual { label: "Stamp duty".into(), amount: -500., day: 20, from: Some("2026-11".into()), every: Some("once".into()), ..Default::default() };
        assert_eq!(once.occurrences(d("2026-10-01"), d("2027-12-31")), vec![d("2026-11-20")]);
    }

    #[test]
    fn monthly_totals_sum_charges() {
        let l = sample();
        let s = detect(&l);
        let t = monthly_totals(&l, &s, &["2026-05".to_string(), "1999-01".to_string()]);
        assert!(t[0] > 550. + 4800.);
        assert_eq!(t[1], 0.);
    }
}
