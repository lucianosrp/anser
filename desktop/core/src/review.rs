//! Year in review: one calendar year (or the running one, to date) told from
//! the bundle (income, spending, categories, net worth, investing: the
//! server's numbers, as on the Spending page) and the ledger (merchants,
//! purchases, trips, BTC buys, fun facts). UI-free.

use std::collections::{BTreeSet, HashMap};

use chrono::NaiveDate;
use serde_json::Value;

use crate::bundle::Bundle;
use crate::dca::{self, Scope};
use crate::drill;
use crate::ledger::{Kind, Ledger};
use crate::travel::Trip;

const TOP_CATEGORIES: usize = 8;
const TOP_MERCHANTS: usize = 8;
const BIGGEST: usize = 6;
const COFFEE: [&str; 9] = ["coffee", "starbucks", "arabica", "espresso", "caffe", "caffè", "cafe", "café", "cafè"];
/// "café" that isn't coffee (a Hong Kong fast-food chain)
const NOT_COFFEE: [&str; 2] = ["de coral", "decoral"];

#[derive(Debug, Clone, PartialEq)]
pub struct MonthRow {
    /// "YYYY-MM"
    pub month: String,
    pub income: f64,
    pub spend: f64,
    pub invested: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CategoryRow {
    pub category: String,
    pub hkd: f64,
    /// the same months of the year before
    pub prev: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MerchantRow {
    pub name: String,
    /// the merchant key, for a search
    pub key: String,
    pub hkd: f64,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BtcYear {
    pub buys: usize,
    pub usd: f64,
    pub btc: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Streak {
    pub days: i64,
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Review {
    pub year: i32,
    /// the running year: everything is "to date"
    pub ytd: bool,
    /// the months covered, oldest first (the running month included when ytd)
    pub months: Vec<MonthRow>,
    pub income: f64,
    pub spending: f64,
    /// spending over the same months of the year before
    pub prev_spending: Option<f64>,
    pub prev_income: Option<f64>,
    /// month-end net worth: the end of the year before, then each month of this one
    pub net_worth: Vec<(String, f64)>,
    pub categories: Vec<CategoryRow>,
    pub merchants: Vec<MerchantRow>,
    /// ledger indices of the biggest single spending rows
    pub biggest: Vec<usize>,
    pub trips: Vec<usize>,
    pub trip_days: i64,
    pub travel_hkd: f64,
    pub funds_hkd: f64,
    pub broker_hkd: f64,
    pub crypto_hkd: f64,
    pub btc: BtcYear,
    pub coffee_count: usize,
    pub coffee_hkd: f64,
    /// bank and card rows
    pub txn_count: usize,
    pub spending_count: usize,
    /// (name, key, visits)
    pub most_visited: Option<(String, String, usize)>,
    pub no_spend: Option<Streak>,
    /// the last date the streak and counts look at
    pub through: String,
}

impl Review {
    pub fn saved(&self) -> f64 {
        self.income - self.spending
    }
    pub fn savings_rate(&self) -> Option<f64> {
        (self.income > 0.).then(|| self.saved() / self.income * 100.)
    }
    pub fn invested(&self) -> f64 {
        self.funds_hkd + self.broker_hkd + self.crypto_hkd
    }
    /// (start, end) net worth
    pub fn net_worth_change(&self) -> Option<(f64, f64)> {
        Some((self.net_worth.first()?.1, self.net_worth.last()?.1))
    }
    /// completed months only: a running month would always be the cheapest
    fn complete_months(&self) -> impl Iterator<Item = &MonthRow> {
        let running = self.ytd.then(|| self.months.last().map(|m| m.month.clone())).flatten();
        self.months.iter().filter(move |m| Some(&m.month) != running.as_ref())
    }
    pub fn priciest_month(&self) -> Option<&MonthRow> {
        self.complete_months().max_by(|a, b| a.spend.total_cmp(&b.spend))
    }
    pub fn cheapest_month(&self) -> Option<&MonthRow> {
        self.complete_months().filter(|m| m.spend > 0.).min_by(|a, b| a.spend.total_cmp(&b.spend))
    }
    pub fn avg_month_spend(&self) -> f64 {
        let (n, s) = self.complete_months().fold((0, 0.), |(n, s), m| (n + 1, s + m.spend));
        if n > 0 { s / n as f64 } else { 0. }
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn n(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.)
}

fn year_of(date: &str) -> i32 {
    date.get(..4).and_then(|y| y.parse().ok()).unwrap_or(0)
}

/// Years with spending data, newest first.
pub fn years(b: &Bundle) -> Vec<i32> {
    let ys: BTreeSet<i32> = b.arr(&["spending", "trend"]).iter().map(|t| year_of(s(t, "month"))).filter(|y| *y > 0).collect();
    ys.into_iter().rev().collect()
}

/// The year the page opens on: the last complete one (else the running one).
pub fn default_year(b: &Bundle, today: NaiveDate) -> Option<i32> {
    let this = year_of(&today.format("%Y").to_string());
    let ys = years(b);
    ys.iter().copied().find(|y| *y < this).or_else(|| ys.first().copied())
}

/// "sushiro" -> "Sushiro", "city super" -> "City Super"
pub fn title(key: &str) -> String {
    key.split(' ')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_coffee(desc: &str) -> bool {
    let d = desc.to_lowercase();
    COFFEE.iter().any(|k| d.contains(k)) && !NOT_COFFEE.iter().any(|k| d.contains(k))
}

/// Longest run of whole days in `[from, to]` with no spending row.
fn no_spend_streak(days: &BTreeSet<NaiveDate>, from: NaiveDate, to: NaiveDate) -> Option<Streak> {
    let mut best: Option<(i64, NaiveDate, NaiveDate)> = None;
    let mut prev = from.pred_opt()?;
    for d in days.iter().copied().filter(|d| *d >= from && *d <= to).chain(to.succ_opt()) {
        let gap = (d - prev).num_days() - 1;
        if gap > 0 && best.is_none_or(|b| gap > b.0) {
            best = Some((gap, prev.succ_opt()?, d.pred_opt()?));
        }
        prev = d;
    }
    best.map(|(days, a, b)| Streak { days, start: a.format("%Y-%m-%d").to_string(), end: b.format("%Y-%m-%d").to_string() })
}

pub fn build(b: &Bundle, ledger: &Ledger, trips: &[Trip], year: i32, today: NaiveDate) -> Review {
    let ytd = year_of(&today.format("%Y").to_string()) == year;
    let y = format!("{year:04}");
    let py = format!("{:04}", year - 1);

    // ---- income, spending, invested: the bundle's months ----
    let trend = b.arr(&["spending", "trend"]);
    let months: Vec<MonthRow> = trend.iter().filter(|t| s(t, "month").starts_with(&y)).map(|t| MonthRow {
        month: s(t, "month").to_string(), income: n(t, "income"), spend: n(t, "spend"),
        invested: n(t, "invested") + n(t, "invested_crypto"),
    }).collect();
    // the year before, over the same months (all of it for a complete year)
    let last_mm = if ytd { months.last().and_then(|m| m.month.get(5..7)).unwrap_or("12") } else { "12" }.to_string();
    let in_prev = |m: &str| m.starts_with(&py) && m.get(5..7).is_some_and(|mm| mm <= last_mm.as_str());
    let prev: Vec<&Value> = trend.iter().filter(|t| in_prev(s(t, "month"))).collect();
    // a year before with fewer months (the ledger starting mid-year) isn't comparable
    let (prev_spending, prev_income) = if prev.is_empty() || prev.len() < months.len() {
        (None, None)
    } else {
        (Some(prev.iter().map(|t| n(t, "spend")).sum()), Some(prev.iter().map(|t| n(t, "income")).sum()))
    };
    let income = months.iter().map(|m| m.income).sum();
    let spending = months.iter().map(|m| m.spend).sum();
    let crypto_hkd = trend.iter().filter(|t| s(t, "month").starts_with(&y)).map(|t| n(t, "invested_crypto")).sum();

    // ---- categories against the year before ----
    let mut cur: HashMap<String, f64> = HashMap::new();
    let mut before: HashMap<String, f64> = HashMap::new();
    for c in b.arr(&["spending", "by_category"]) {
        let m = s(c, "month");
        if m.starts_with(&y) {
            *cur.entry(s(c, "category").to_string()).or_default() += n(c, "hkd");
        } else if in_prev(m) && prev_spending.is_some() {
            *before.entry(s(c, "category").to_string()).or_default() += n(c, "hkd");
        }
    }
    let mut categories: Vec<CategoryRow> = cur.into_iter().filter(|(_, v)| *v > 0.5)
        .map(|(c, v)| CategoryRow { prev: before.get(&c).copied().or(prev_spending.map(|_| 0.)), category: c, hkd: v })
        .collect();
    categories.sort_by(|a, b| b.hkd.total_cmp(&a.hkd));
    categories.truncate(TOP_CATEGORIES);

    // ---- net worth: end of the year before, then each month-end ----
    let rows = b.arr(&["overview", "sheet", "rows"]);
    let mut net_worth: Vec<(String, f64)> = Vec::new();
    let start_key = format!("{py}-12");
    if let Some(r) = rows.iter().find(|r| s(r, "month") == start_key) {
        net_worth.push((start_key, n(r, "total_hkd")));
    }
    net_worth.extend(rows.iter().filter(|r| s(r, "month").starts_with(&y)).map(|r| (s(r, "month").to_string(), n(r, "total_hkd"))));

    // ---- the ledger's year ----
    let through = if ytd { today.format("%Y-%m-%d").to_string() } else { format!("{y}-12-31") };
    let in_year = |d: &str| d.starts_with(&y) && d <= through.as_str();
    let mut merchants: HashMap<String, (f64, usize)> = HashMap::new();
    let mut spend_days: BTreeSet<NaiveDate> = BTreeSet::new();
    let (mut coffee_count, mut coffee_hkd, mut txn_count, mut spending_count) = (0, 0., 0, 0);
    let mut broker_hkd = 0.;
    let mut spend_rows: Vec<(f64, usize)> = Vec::new();
    for (i, e) in ledger.entries.iter().enumerate() {
        if !in_year(&e.date) {
            continue;
        }
        match e.kind {
            Kind::Broker => broker_hkd += -e.hkd.unwrap_or(0.),
            Kind::Bank | Kind::Card => txn_count += 1,
            Kind::Crypto => {}
        }
        if !drill::is_spending(e) {
            continue;
        }
        let v = -drill::my_hkd(e).unwrap_or(0.);
        spending_count += 1;
        spend_rows.push((v, i));
        if let Ok(d) = NaiveDate::parse_from_str(&e.date, "%Y-%m-%d") {
            spend_days.insert(d);
        }
        if is_coffee(&e.description) {
            coffee_count += 1;
            coffee_hkd += v;
        }
        let key = drill::merchant_key(&e.description);
        if !key.is_empty() {
            let m = merchants.entry(key).or_default();
            m.0 += v;
            m.1 += 1;
        }
    }
    spend_rows.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let biggest = spend_rows.iter().take(BIGGEST).map(|(_, i)| *i).collect();
    let most_visited = merchants.iter()
        .max_by(|a, b| a.1.1.cmp(&b.1.1).then(b.0.cmp(a.0)))
        .map(|(k, (_, c))| (title(k), k.clone(), *c));
    let mut merchant_rows: Vec<MerchantRow> = merchants.into_iter()
        .map(|(k, (v, c))| MerchantRow { name: title(&k), key: k, hkd: v, count: c })
        .collect();
    merchant_rows.sort_by(|a, b| b.hkd.total_cmp(&a.hkd).then(a.key.cmp(&b.key)));
    merchant_rows.truncate(TOP_MERCHANTS);
    let from = NaiveDate::from_ymd_opt(year, 1, 1);
    let to = NaiveDate::parse_from_str(&through, "%Y-%m-%d").ok();
    // only over the span the ledger covers (no rows before the first ingest isn't a streak)
    let first_row = ledger.entries.iter().filter(|e| matches!(e.kind, Kind::Bank | Kind::Card)).map(|e| e.date.as_str()).min()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let last_row = ledger.entries.iter().filter(|e| matches!(e.kind, Kind::Bank | Kind::Card)).map(|e| e.date.as_str()).max()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let no_spend = match (from, to) {
        (Some(f), Some(t)) => {
            let f = first_row.map_or(f, |r| f.max(r));
            let t = last_row.map_or(t, |r| t.min(r));
            (f <= t).then(|| no_spend_streak(&spend_days, f, t)).flatten()
        }
        _ => None,
    };

    // ---- trips that started this year ----
    let trip_ix: Vec<usize> = trips.iter().enumerate().filter(|(_, t)| in_year(&t.start)).map(|(i, _)| i).collect();
    let trip_days = trip_ix.iter().map(|&i| trips[i].days()).sum();
    let travel_hkd = trip_ix.iter().map(|&i| trips[i].total_hkd()).sum();

    // ---- investing: fund purchases (bundle), broker trades (ledger), crypto (bundle) ----
    let funds_hkd = b.arr(&["flexinvest", "purchases"]).iter().filter(|p| in_year(s(p, "date"))).map(|p| n(p, "hkd")).sum();
    let btc = dca::analyze(ledger, None, Scope::All)
        .map(|d| d.buys.iter().filter(|x| in_year(&x.date)).fold(BtcYear::default(), |mut a, x| {
            a.buys += 1;
            a.usd += x.cost;
            a.btc += x.qty;
            a
        }))
        .unwrap_or_default();

    Review {
        year, ytd, months, income, spending, prev_spending, prev_income, net_worth, categories,
        merchants: merchant_rows, biggest, trips: trip_ix, trip_days, travel_hkd, funds_hkd, broker_hkd,
        crypto_hkd, btc, coffee_count, coffee_hkd, txn_count, spending_count, most_visited, no_spend, through,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Entry;

    fn e(kind: Kind, date: &str, desc: &str, cat: &str, amount: f64) -> Entry {
        Entry {
            kind, date: date.into(), source: "card".into(), account: String::new(), description: desc.into(),
            category: cat.into(), amount, currency: "HKD".into(), hkd: Some(amount), share: 1., haystack: String::new(), raw_hash: String::new(),
        }
    }

    fn bundle() -> Bundle {
        Bundle(serde_json::json!({
            "spending": {
                "trend": [
                    {"month": "2024-11", "income": 100.0, "spend": 50.0},
                    {"month": "2024-12", "income": 100.0, "spend": 80.0},
                    {"month": "2025-01", "income": 200.0, "spend": 120.0, "invested": 10.0, "invested_crypto": 5.0},
                    {"month": "2025-02", "income": 200.0, "spend": 60.0, "invested_crypto": 7.0},
                ],
                "by_category": [
                    {"month": "2024-12", "category": "Dining", "hkd": 40.0},
                    {"month": "2025-01", "category": "Dining", "hkd": 70.0},
                    {"month": "2025-02", "category": "Travel", "hkd": 30.0},
                ]
            },
            "overview": {"sheet": {"rows": [
                {"month": "2024-12", "total_hkd": 1000.0}, {"month": "2025-01", "total_hkd": 1100.0},
                {"month": "2025-02", "total_hkd": 1250.0},
            ]}},
            "flexinvest": {"purchases": [{"date": "2025-01-10", "hkd": 10.0}, {"date": "2024-05-01", "hkd": 99.0}]}
        }))
    }

    fn ledger() -> Ledger {
        Ledger { categories: Vec::new(), rules: Vec::new(), data_rev: 0, income: Vec::new(), trades: Vec::new(), bars: Default::default(), coins: Vec::new(), entries: vec![
            e(Kind::Card, "2025-02-20", "STARBUCKS COFFEE (1234 SHA TIN HK", "Dining", -40.),
            e(Kind::Card, "2025-02-10", "Cafe de Coral", "Dining", -50.),
            e(Kind::Broker, "2025-02-05", "BUY VWRA", "Trade", -500.),
            e(Kind::Card, "2025-01-30", "KPay*Noodle Bar Central HK", "Dining", -100.),
            e(Kind::Bank, "2025-01-20", "To savings", "Internal transfer", -9000.),
            e(Kind::Card, "2025-01-05", "KPay*Noodle Bar Kowloon", "Dining", -60.),
            e(Kind::Bank, "2025-01-02", "Salary", "Salary", 3000.),
            e(Kind::Card, "2024-12-30", "Old", "Dining", -10.),
        ]}
    }

    #[test]
    fn a_complete_year() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        let b = bundle();
        assert_eq!(years(&b), vec![2025, 2024]);
        assert_eq!(default_year(&b, today), Some(2025));
        let r = build(&b, &ledger(), &[], 2025, today);
        assert!(!r.ytd);
        assert_eq!((r.income, r.spending, r.saved()), (400., 180., 220.));
        assert_eq!(r.prev_spending, Some(130.));
        assert_eq!(r.net_worth_change(), Some((1000., 1250.)));
        assert_eq!(r.categories[0], CategoryRow { category: "Dining".into(), hkd: 70., prev: Some(40.) });
        assert_eq!(r.categories[1].prev, Some(0.), "new this year");
        assert_eq!(r.priciest_month().map(|m| m.month.as_str()), Some("2025-01"));
        assert_eq!(r.cheapest_month().map(|m| m.month.as_str()), Some("2025-02"));
        assert_eq!((r.funds_hkd, r.broker_hkd, r.crypto_hkd), (10., 500., 12.));
        assert_eq!((r.coffee_count, r.coffee_hkd), (1, 40.));
        assert_eq!(r.txn_count, 6);
        assert_eq!(r.spending_count, 4);
        assert_eq!(r.merchants[0], MerchantRow { name: "Noodle".into(), key: "noodle".into(), hkd: 160., count: 2 });
        assert_eq!(r.most_visited.as_ref().map(|m| m.2), Some(2));
        assert_eq!(r.biggest.len(), 4);
        // spending on Jan 5, Jan 30, Feb 10, Feb 20; the ledger ends Feb 20
        assert_eq!(r.no_spend, Some(Streak { days: 24, start: "2025-01-06".into(), end: "2025-01-29".into() }));
    }

    #[test]
    fn the_running_year_is_to_date() {
        let today = NaiveDate::from_ymd_opt(2025, 2, 15).unwrap();
        let r = build(&bundle(), &ledger(), &[], 2025, today);
        assert!(r.ytd);
        assert_eq!(r.prev_spending, None, "no Jan–Feb in the year before");
        // the running month is never the cheapest
        assert_eq!(r.cheapest_month().map(|m| m.month.as_str()), Some("2025-01"));
        assert_eq!(r.coffee_count, 0, "after today");
    }

    #[test]
    fn coffee_and_titles() {
        assert!(is_coffee("BBMSL*% Arabica Sha Tin Hong Kong HK"));
        assert!(is_coffee("Caffè Stella"));
        assert!(!is_coffee("CAFE DE CORAL (123) HK"));
        assert_eq!(title("city super"), "City Super");
    }
}
