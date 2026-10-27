//! The made-up household behind "Just try it" (the first-run wizard): Alex
//! Demo, in Berlin, paid in euros, with about two years of everything the
//! pages show. Nothing in it is anyone's: the employer, the landlord and
//! the shops are invented (a few are generic words the seed rules already
//! know: a pizzeria, a supermarket), the amounts come from a fixed seed.
//!
//! It is built the way real data is, so every page computes from the same
//! code paths: a finance.db with the server's schema (db, profile,
//! categories, invest_setup), the rows the parsers and fetchers would
//! write, [`crate::categorize::categorize_all`] over them, then the export
//! bundle from [`crate::export::bundle`] on the cache-only feed. Market
//! prices are synthetic random walks baked into the db: no network, ever.
//!
//! * banks: an N26 current account (salary, rent, bills, groceries,
//!   dining, transport, subscriptions, transfers out) and its savings
//!   account, a Revolut card with a pocket per trip currency;
//! * trips: Japan, London, New York, Thailand, paid on the Revolut pockets
//!   in local money, flights booked from home;
//! * receivables: a rental deposit, work expenses (one paid back, two open);
//! * a broker (IBKR rows): a monthly deposit, three ETFs bought every
//!   month, dividends, daily NAV;
//! * crypto (Binance rows): a weekly BTC auto-invest and monthly ETH buys,
//!   monthly closes, weekly candles and a balance snapshot;
//! * budgets, the calendar, the profile (onboarding done).
//!
//! Everything is relative to `today` (the last [`MONTHS`] months up to it)
//! and deterministic for a given `today`. The build runs as a throwaway
//! tenant rooted in its own work dir, so no path of the process's real
//! tree (secrets, fetch status, formats) is read or written.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::db::{self, IbkrCash, IbkrFlow, IbkrIncome, IbkrNav, IbkrPosition, IbkrTrade};

/// Whose data it is.
pub const NAME: &str = "Alex Demo";
pub const BASE: &str = "EUR";
/// How far back the data goes.
pub const MONTHS: i32 = 22;
const SEED: u64 = 0x0DE3_0F1A_7A11_CE5D;
/// The tenant uid the build runs as: never a real account (users start at 1).
const UID: i64 = 0;

// ---------------------------------------------------------------------------
// a small deterministic RNG (splitmix64)
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn f(&mut self) -> f64 {
        (self.u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f()
    }
    fn int(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.u64() % ((hi - lo + 1) as u64)) as i64
    }
    fn chance(&mut self, p: f64) -> bool {
        self.f() < p
    }
    fn normal(&mut self) -> f64 {
        let u = self.f().max(1e-12);
        let v = self.f();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[(self.u64() % v.len() as u64) as usize]
    }
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn first_of(d: NaiveDate) -> NaiveDate {
    d.with_day(1).expect("day 1")
}

/// `months` months after the first of `d`'s month (negative: before).
fn add_months(d: NaiveDate, months: i32) -> NaiveDate {
    let n = d.year() * 12 + d.month0() as i32 + months;
    NaiveDate::from_ymd_opt(n.div_euclid(12), n.rem_euclid(12) as u32 + 1, 1).expect("a month")
}

/// Day `day` of `m`'s month, clamped to its last day.
fn day(m: NaiveDate, day: u32) -> NaiveDate {
    (1..=day).rev().find_map(|d| NaiveDate::from_ymd_opt(m.year(), m.month(), d)).expect("a day")
}

fn weekday(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

/// The first weekday on or after `d`.
fn bday(mut d: NaiveDate) -> NaiveDate {
    while !weekday(d) {
        d += Duration::days(1);
    }
    d
}

fn ms(d: NaiveDate, hour: u32) -> i64 {
    d.and_hms_opt(hour, 0, 0).expect("a time").and_utc().timestamp_millis()
}

fn ym(d: NaiveDate) -> String {
    d.format("%Y-%m").to_string()
}

/// The close on `d`, else the last one before it.
fn price_on(series: &BTreeMap<NaiveDate, f64>, d: NaiveDate) -> f64 {
    series.range(..=d).next_back().or_else(|| series.iter().next()).map(|(_, p)| *p).unwrap_or(0.0)
}

/// A geometric random walk over `days` (yearly drift and volatility).
fn walk(rng: &mut Rng, days: &[NaiveDate], start: f64, drift: f64, vol: f64, per_year: f64) -> BTreeMap<NaiveDate, f64> {
    let dt = 1.0 / per_year;
    let mut p = start;
    days.iter()
        .map(|d| {
            p *= ((drift - vol * vol / 2.0) * dt + vol * dt.sqrt() * rng.normal()).exp();
            (*d, r2(p))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// the rules the invented merchants need (user rules, like an owner's own)
// ---------------------------------------------------------------------------

/// (pattern, category, is_transfer)
const RULES: &[(&str, &str, i64)] = &[
    ("previous bank", "Internal transfer", 1),
    ("revolut top-up", "Transfer: self/exchange", 1),
    ("crypto exchange", "Transfer: self/exchange", 1),
    ("rental deposit", "Deposit", 0),
    ("work expense", "Office (reimbursed)", 0),
    ("expense reimbursement", "Office (reimbursed)", 0),
    ("safeharbor insurance", "Insurance", 0),
    ("stadtwerke electricity", "Electricity", 0),
    ("fibernet", "Internet", 0),
    ("skyline mobile", "Utilities / Telecom", 0),
    ("pulse fitness", "Gym", 0),
    ("streamflix", "Subscriptions", 0),
    ("tunewave", "Subscriptions", 0),
    ("cloudbox", "Subscriptions", 0),
    ("meditate+", "Subscriptions", 0),
    ("domainhost", "Subscriptions", 0),
    ("freshmart", "Groceries", 0),
    ("biohof", "Groceries", 0),
    ("konbini", "Groceries", 0),
    ("pho saigon", "Dining", 0),
    ("deli ", "Dining", 0),
    ("rideshare", "Transport", 0),
    ("skyjet airlines", "Travel", 0),
    ("online marketplace", "Shopping", 0),
    ("trailhead outdoor", "Shopping", 0),
    ("bookshop", "Shopping", 0),
    ("night market", "Shopping", 0),
    ("homeware", "Household", 0),
    ("pharmacy", "Health", 0),
    ("barber", "Personal care", 0),
    ("massage", "Personal care", 0),
    ("concert hall", "Entertainment", 0),
];

// ---------------------------------------------------------------------------
// the bank side
// ---------------------------------------------------------------------------

struct Txn {
    source: &'static str,
    account: String,
    currency: String,
    date: NaiveDate,
    amount: f64,
    desc: String,
}

struct Gen {
    rng: Rng,
    start: NaiveDate,
    today: NaiveDate,
    txns: Vec<Txn>,
}

impl Gen {
    fn add(&mut self, source: &'static str, account: &str, ccy: &str, date: NaiveDate, amount: f64, desc: &str) {
        if date < self.start || date > self.today || amount == 0.0 {
            return;
        }
        self.txns.push(Txn { source, account: account.into(), currency: ccy.into(), date, amount: r2(amount), desc: desc.into() });
    }

    /// The N26 current account.
    fn main(&mut self, date: NaiveDate, amount: f64, desc: &str) {
        self.add("n26", "", "EUR", date, amount, desc);
    }

    /// A Revolut pocket.
    fn card(&mut self, ccy: &str, date: NaiveDate, amount: f64, desc: &str) {
        self.add("revolut_new", ccy, ccy, date, amount, desc);
    }
}

/// One stretch of a trip: the city, its days, and (merchant, low, high)
/// in local money.
type Leg = (&'static str, i64, &'static [(&'static str, f64, f64)]);

struct Trip {
    /// months after the start, and the day it begins
    month: i32,
    day: u32,
    ccy: &'static str,
    /// the flight, booked from home
    flight: f64,
    /// a night in a hotel (local money)
    night: (f64, f64),
    legs: &'static [Leg],
    /// between legs: (description, local amount)
    hop: Option<(&'static str, f64)>,
}

const TRIPS: &[Trip] = &[
    Trip {
        month: 3, day: 9, ccy: "JPY", flight: 780.0, night: (11_000.0, 17_000.0),
        legs: &[
            ("TOKYO", 5, &[("SUSHI COUNTER", 2500.0, 6000.0), ("RAMEN SHOP", 900.0, 1600.0), ("KONBINI STORE", 300.0, 1500.0),
                           ("METRO TICKET", 200.0, 900.0), ("CAFE", 500.0, 1200.0)]),
            ("KYOTO", 3, &[("RAMEN SHOP", 900.0, 1500.0), ("MUSEUM", 600.0, 1500.0), ("CAFE", 500.0, 1100.0), ("KONBINI STORE", 300.0, 1200.0)]),
            ("OSAKA", 2, &[("SUSHI COUNTER", 2000.0, 5000.0), ("KONBINI STORE", 300.0, 1200.0), ("METRO TICKET", 200.0, 600.0)]),
        ],
        hop: Some(("SHINKANSEN RAILWAY", 14_170.0)),
    },
    Trip {
        month: 8, day: 14, ccy: "GBP", flight: 160.0, night: (120.0, 180.0),
        legs: &[("LONDON", 4, &[("CAFE", 4.0, 9.0), ("TUBE TFL TICKET", 3.0, 8.0), ("BURGER JOINT", 12.0, 22.0), ("BOOKSHOP", 8.0, 25.0),
                               ("MUSEUM SHOP", 5.0, 20.0)])],
        hop: None,
    },
    Trip {
        month: 13, day: 6, ccy: "USD", flight: 620.0, night: (190.0, 260.0),
        legs: &[("NEW YORK CITY", 6, &[("COFFEE BAR", 4.0, 8.0), ("PIZZA SLICE", 4.0, 12.0), ("DELI", 10.0, 25.0), ("METRO CARD", 3.0, 34.0),
                                 ("MUSEUM", 25.0, 30.0), ("STEAK HOUSE", 60.0, 110.0)])],
        hop: None,
    },
    Trip {
        month: 18, day: 11, ccy: "THB", flight: 720.0, night: (1200.0, 2200.0),
        legs: &[
            ("BANGKOK", 6, &[("STREET NOODLE", 60.0, 150.0), ("TUKTUK TAXI", 100.0, 300.0), ("MASSAGE SPA", 400.0, 900.0),
                             ("NIGHT MARKET", 150.0, 600.0), ("CAFE", 80.0, 160.0)]),
            ("CHIANG MAI", 3, &[("STREET NOODLE", 50.0, 120.0), ("NIGHT MARKET", 150.0, 500.0), ("CAFE", 70.0, 150.0)]),
        ],
        hop: Some(("SKYJET AIRLINES DOMESTIC", 1850.0)),
    },
];

/// The monthly FX cache: (currency, X->EUR around, how much it moves).
const FX: [(&str, f64, f64); 4] = [("USD", 0.92, 0.03), ("GBP", 1.17, 0.02), ("JPY", 0.0062, 0.0003), ("THB", 0.026, 0.001)];

fn fx_table(rng: &mut Rng, months: &[NaiveDate]) -> BTreeMap<(&'static str, String), f64> {
    let mut out = BTreeMap::new();
    for (ccy, mid, amp) in FX {
        let phase = rng.range(0.0, 6.0);
        for (i, m) in months.iter().enumerate() {
            let r = mid + amp * ((i as f64 / 4.0 + phase).sin() + 0.3 * rng.normal());
            out.insert((ccy, ym(*m)), (r * 1e6).round() / 1e6);
        }
    }
    out
}

/// Everything on the bank accounts.
fn bank(g: &mut Gen, months: &[NaiveDate], fx: &BTreeMap<(&'static str, String), f64>) {
    let start = g.start;
    // moving in: the old bank's balance, then the deposit on the flat
    g.main(start, 4200.0, "TRANSFER FROM PREVIOUS BANK");
    g.add("n26", "Savings", "EUR", start, 8000.0, "TRANSFER FROM PREVIOUS BANK");
    g.main(day(start, 2), -2400.0, "RENTAL DEPOSIT LINDEN PROPERTIES");
    let mut savings = 8000.0;
    let groceries = ["GREEN BASKET SUPERMARKET", "FRESHMART BERLIN", "CORNER GROCERY", "BIOHOF MARKET"];
    let dining = ["LUNA PIZZERIA", "HARBOR CAFE", "KOI SUSHI BAR", "THE NOODLE HOUSE", "BURGER BROS", "PHO SAIGON KITCHEN", "CORNER BAKERY"];
    let shops = ["ONLINE MARKETPLACE ORDER", "HOMEWARE STORE", "TRAILHEAD OUTDOOR", "BOOKSHOP BERLIN"];
    for (i, m) in months.iter().enumerate() {
        let i = i as i32;
        let m = *m;
        // income: a raise after a year, a bonus in December
        let salary = if i >= 12 { 4050.0 } else { 3850.0 };
        g.main(bday(day(m, 27)), salary, "ACME ANALYTICS GMBH SALARY DEPOSIT");
        if m.month() == 12 {
            g.main(bday(day(m, 20)), 2000.0, "ACME ANALYTICS GMBH SALARY DEPOSIT BONUS");
        }
        // the fixed costs
        g.main(day(m, 1), -1250.0, "APARTMENT RENT LINDEN PROPERTIES");
        let power = -(55.0 + 25.0 * ((m.month() as f64 - 1.0) / 12.0 * std::f64::consts::TAU).cos().max(0.0));
        g.main(day(m, 5), power, "STADTWERKE ELECTRICITY");
        g.main(day(m, 7), -34.99, "FIBERNET INTERNET");
        g.main(day(m, 12), -19.99, "SKYLINE MOBILE");
        g.main(day(m, 1), -28.40, "SAFEHARBOR INSURANCE");
        g.main(day(m, 3), -39.90, "PULSE FITNESS CLUB");
        g.main(day(m, 8), if i < 9 { -12.99 } else { -13.99 }, "STREAMFLIX.COM");
        g.main(day(m, 15), -10.99, "TUNEWAVE PREMIUM");
        g.main(day(m, 20), -2.99, "CLOUDBOX STORAGE");
        // signed up for, then forgotten
        if i >= 4 {
            g.main(day(m, 23), -6.99, "MEDITATE+ APP");
        }
        if i % 12 == 5 {
            g.main(day(m, 18), -24.0, "DOMAINHOST RENEWAL");
        }
        g.main(day(m, 1), -58.0, "CITY METRO MONTHLY PASS");
        // where the rest goes
        g.main(bday(day(m, 28)), -300.0, "TO SAVINGS POCKET");
        g.add("n26", "Savings", "EUR", bday(day(m, 28)), 300.0, "FROM MAIN ACCOUNT");
        savings += 300.0;
        let interest = r2(savings * 0.02 / 12.0);
        g.add("n26", "Savings", "EUR", day(m, 31), interest, "CREDIT INTEREST");
        savings += interest;
        g.main(bday(day(m, 2)), -500.0, "TO INTERACTIVE BROKERS");
        g.main(bday(day(m, 4)), -150.0, "TO CRYPTO EXCHANGE");
        let atm = day(m, g.rng.int(9, 16) as u32);
        g.main(atm, -100.0, "ATM WITHDRAWAL");
        // everyday spending
        for _ in 0..g.rng.int(6, 9) {
            let d = day(m, g.rng.int(1, 28) as u32);
            let what = *g.rng.pick(&groceries);
            let amt = g.rng.range(14.0, 92.0);
            g.main(d, -amt, what);
        }
        for _ in 0..g.rng.int(5, 9) {
            let d = day(m, g.rng.int(1, 28) as u32);
            let what = *g.rng.pick(&dining);
            let amt = g.rng.range(9.0, 58.0);
            g.main(d, -amt, what);
        }
        for _ in 0..g.rng.int(0, 3) {
            let d = day(m, g.rng.int(1, 28) as u32);
            let (what, amt) = if g.rng.chance(0.6) { ("TAXI BERLIN", g.rng.range(11.0, 26.0)) } else { ("RIDESHARE TRIP", g.rng.range(8.0, 19.0)) };
            g.main(d, -amt, what);
        }
        for _ in 0..g.rng.int(1, 3) {
            let d = day(m, g.rng.int(1, 28) as u32);
            let what = *g.rng.pick(&shops);
            let amt = g.rng.range(15.0, 140.0);
            g.main(d, -amt, what);
        }
        if g.rng.chance(0.5) {
            let d = day(m, g.rng.int(1, 28) as u32);
            let amt = g.rng.range(6.0, 28.0);
            g.main(d, -amt, "CITY PHARMACY");
        }
        if g.rng.chance(0.45) {
            let d = day(m, g.rng.int(1, 28) as u32);
            g.main(d, -24.0, "SHARP CUTS BARBER");
        }
        if g.rng.chance(0.5) {
            let d = day(m, g.rng.int(1, 28) as u32);
            let (what, amt) = *g.rng.pick(&[("STARLIGHT CINEMA", 13.5), ("CITY MUSEUM", 16.0), ("CONCERT HALL TICKETS", 64.0)]);
            g.main(d, -amt, what);
        }
        if i % 9 == 4 {
            g.main(day(m, 14), -95.0, "DENTAL CLINIC");
        }
    }
    // work trips paid ahead: one paid back, two still open
    let m = add_months(start, 6);
    g.main(day(m, 10), -380.0, "WORK EXPENSE CONFERENCE TICKET");
    g.main(day(add_months(m, 1), 27), 380.0, "EXPENSE REIMBURSEMENT ACME ANALYTICS");
    let m = add_months(start, MONTHS - 2);
    g.main(day(m, 16), -145.20, "WORK EXPENSE TRAIN TICKETS");
    g.main(day(m, 17), -89.00, "WORK EXPENSE HOTEL NIGHT");
    for t in TRIPS {
        trip(g, t, fx);
    }
}

/// One trip: the flight booked from home, the pocket topped up, then the
/// days away in local money.
fn trip(g: &mut Gen, t: &Trip, fx: &BTreeMap<(&'static str, String), f64>) {
    let begin = day(add_months(g.start, t.month), t.day);
    if begin > g.today {
        return;
    }
    let rate = fx.get(&(t.ccy, ym(begin))).copied().unwrap_or(1.0);
    let local = |x: f64| if t.ccy == "JPY" || t.ccy == "THB" { x.round() } else { r2(x) };
    let mut spend: Vec<(NaiveDate, f64, String)> = Vec::new();
    let mut d = begin;
    for (li, (city, days, spots)) in t.legs.iter().enumerate() {
        if li > 0 {
            if let Some((hop, amt)) = t.hop {
                spend.push((d, local(amt), format!("{hop} {city}")));
            }
        }
        for _ in 0..*days {
            for _ in 0..g.rng.int(3, 5) {
                let (what, lo, hi) = *g.rng.pick(spots);
                let amt = local(g.rng.range(lo, hi));
                spend.push((d, amt, format!("{what} {city}")));
            }
            d += Duration::days(1);
        }
        let nights = local(g.rng.range(t.night.0, t.night.1) * *days as f64);
        spend.push((d - Duration::days(1), nights, format!("HOTEL {city}")));
    }
    let total: f64 = spend.iter().map(|s| s.1).sum();
    let eur = ((total * 1.08 * rate) / 10.0).ceil() * 10.0;
    let topped = begin - Duration::days(2);
    g.main(begin - Duration::days(45), -t.flight, "SKYJET AIRLINES");
    g.main(topped, -eur, "TO REVOLUT TOP-UP");
    g.card("EUR", topped, eur, "Money added via transfer");
    g.card("EUR", topped, -eur, &format!("Exchanged to {}", t.ccy));
    let got = if t.ccy == "JPY" || t.ccy == "THB" { (eur / rate).floor() } else { r2((eur / rate) - 0.005) };
    g.card(t.ccy, topped, got, &format!("Exchanged to {}", t.ccy));
    for (d, amt, desc) in spend {
        g.card(t.ccy, d, -amt, &desc);
    }
}

// ---------------------------------------------------------------------------
// the build
// ---------------------------------------------------------------------------

/// Fill an empty finance.db (the server's schema already applied) with the
/// household as of `today`.
pub fn build(conn: &Connection, today: NaiveDate) -> Result<()> {
    let start = add_months(first_of(today), -(MONTHS - 1));
    let now = ms(today, 12);
    crate::profile::ensure(conn)?;
    crate::categories::ensure(conn)?;
    crate::invest_setup::ensure(conn)?;
    crate::categorize::ensure_rules(conn)?;
    for (pattern, category, transfer) in RULES {
        conn.execute(
            "INSERT INTO category_rules(priority, pattern, category, is_transfer, origin, created_at) VALUES (15, ?1, ?2, ?3, 'user', ?4)",
            params![pattern, category, transfer, now],
        )?;
    }

    let mut rng = Rng(SEED);
    // a month before the data, for the opening rates
    let months: Vec<NaiveDate> = (0..MONTHS).map(|i| add_months(start, i)).collect();
    let fx_months: Vec<NaiveDate> = (-1..MONTHS).map(|i| add_months(start, i)).collect();
    let fx = fx_table(&mut rng, &fx_months);
    for ((ccy, m), rate) in &fx {
        conn.execute("INSERT INTO fx_rates(base, quote, month, rate, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                     params![ccy, BASE, format!("{m}-01"), rate, now])?;
    }

    let mut g = Gen { rng: Rng(SEED ^ 0xBA4C), start, today, txns: Vec::new() };
    bank(&mut g, &months, &fx);
    // the balances a Revolut export reports, per pocket
    g.txns.sort_by_key(|t| t.date);
    let mut bal: BTreeMap<(&str, String), f64> = BTreeMap::new();
    let mut per_source: BTreeMap<&str, i64> = BTreeMap::new();
    {
        let tx = conn.unchecked_transaction()?;
        let mut st = tx.prepare(
            "INSERT INTO bank_transactions(source, account, currency, txn_date, amount, balance, description, raw_hash, source_file, ingested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for (i, t) in g.txns.iter().enumerate() {
            let balance = (t.source != "n26").then(|| {
                let b = bal.entry((t.source, t.account.clone())).or_insert(0.0);
                *b = r2(*b + t.amount);
                *b
            });
            st.execute(params![t.source, t.account, t.currency, t.date.to_string(), t.amount, balance, t.desc,
                               format!("demo{i:06}"), format!("demo/{}.csv", t.source), now])?;
            *per_source.entry(t.source).or_insert(0) += 1;
        }
        drop(st);
        tx.commit()?;
    }
    for (src, n) in &per_source {
        conn.execute(
            "INSERT INTO ingested_files(path, source, file_hash, row_count, rows_added, ingested_at) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![format!("demo/{src}.csv"), src, format!("demo-{src}"), n, now],
        )?;
    }

    broker(conn, &mut rng, start, today)?;
    crypto(conn, &mut rng, start, today, &fx)?;

    crate::categorize::categorize_all(conn)?;
    crate::categories::sync_categories(conn)?;

    crate::profile::put(conn, &crate::profile::Profile {
        base_currency: BASE.into(),
        home_country: Some("DE".into()),
        home_city: Some("Berlin".into()),
        name: Some(NAME.into()),
        onboarding_step: Some("done".into()),
        avatar_kind: Some("initials".into()),
        avatar_initials: Some("AD".into()),
        avatar_color: Some("chart-2".into()),
        ..Default::default()
    })?;
    crate::profile::put_setting(conn, "budgets", &json!({"monthly": {
        "Groceries": 380.0, "Dining": 260.0, "Transport": 90.0, "Shopping": 150.0, "Subscriptions": 45.0, "Entertainment": 40.0,
        "Personal care": 30.0, "Health": 25.0,
    }}))?;
    crate::profile::put_setting(conn, "calendar", &json!({"low_balance": 800.0, "accounts": [], "entry": []}))?;
    crate::profile::put_setting(conn, "filters", &json!({"filter": []}))?;
    crate::profile::put_setting(conn, "accounts", &json!({}))?;
    // the flat: rent mode, a lease that ends soon (the renewal reminder shows)
    // and the deposit paid when moving in
    // FINANCE_DEMO_HOME=mortgage (screenshots only): the household owns instead,
    // so net worth shows the mortgage and the home
    let owns = std::env::var("FINANCE_DEMO_HOME").is_ok_and(|v| v == "mortgage");
    let loan_start = add_months(first_of(today), -26);
    crate::profile::put_setting(conn, "home", &if owns { json!({
        "mode": "mortgage", "currency": BASE,
        "mortgage": {"principal": 420000.0, "annual_rate_pct": 3.4, "term_months": 300, "start": loan_start.to_string(), "payment_day": 1,
                     "home_value": 560000.0, "home_value_date": day(add_months(first_of(today), -2), 1).to_string()},
    }) } else { json!({
        "mode": "rent", "currency": BASE,
        "rent": {"monthly": 1250.0, "landlord_match": "APARTMENT RENT LINDEN PROPERTIES",
                 "lease": {"start": day(start, 1).to_string(), "end": (today + Duration::days(45)).to_string()}},
        "oneoffs": [{"kind": "deposit", "amount": 2400.0, "date": day(start, 2).to_string(), "returned_on": null}],
    }) })?;
    Ok(())
}

/// (symbol, name, TER %, start price, yearly drift, volatility, dividend per share a quarter)
const ETFS: [(&str, &str, f64, f64, f64, f64, f64); 3] = [
    ("VWCE", "Vanguard FTSE All-World UCITS ETF (Acc)", 0.22, 104.0, 0.09, 0.14, 0.0),
    ("VHYL", "Vanguard FTSE All-World High Dividend Yield UCITS ETF (Dist)", 0.29, 58.0, 0.06, 0.12, 0.42),
    ("SXR8", "iShares Core S&P 500 UCITS ETF (Acc)", 0.07, 470.0, 0.11, 0.16, 0.0),
];

/// The broker: a deposit and three ETF buys a month, dividends, daily NAV.
fn broker(conn: &Connection, rng: &mut Rng, start: NaiveDate, today: NaiveDate) -> Result<()> {
    let bdays: Vec<NaiveDate> = (0..).map(|i| start - Duration::days(40) + Duration::days(i))
        .take_while(|d| *d <= today).filter(|d| weekday(*d)).collect();
    let mut series: Vec<BTreeMap<NaiveDate, f64>> = Vec::new();
    for (sym, _, _, p0, drift, vol, _) in ETFS {
        let s = walk(rng, &bdays, p0, drift, vol, 252.0);
        store_bars(conn, rng, sym, "EUR", &s)?;
        series.push(s);
    }
    // the benchmark the Investments page compares with (S&P 500, USD)
    let spx = walk(rng, &bdays, 5050.0, 0.10, 0.15, 252.0);
    store_bars(conn, rng, crate::invest_setup::DEFAULT_BENCHMARK, "USD", &spx)?;

    crate::invest_setup::put_account(conn, None, &crate::invest_setup::Account {
        name: "Interactive Brokers".into(),
        matches: vec!["interactive brokers".into()],
        sort: 10,
        valued_by: "ibkr".into(),
        ..Default::default()
    })?;
    for (sym, name, ter, ..) in ETFS {
        crate::invest_setup::put_instrument(conn, &crate::invest_setup::Instrument {
            symbol: sym.into(), name: Some(name.into()), kind: "fund".into(), ter_pct: Some(ter),
        })?;
    }

    let mut cash = 0.0;
    let mut qty = [0.0f64; 3];
    let mut cost = [0.0f64; 3];
    let (mut trades, mut flows, mut income) = (Vec::new(), Vec::new(), Vec::new());
    // (day, cash change, symbol index, qty change): what the NAV replays
    let mut moves: Vec<(NaiveDate, f64, usize, f64)> = Vec::new();
    for i in 0..MONTHS {
        let m = add_months(start, i);
        let dep = bday(day(m, 3));
        if dep > today {
            break;
        }
        cash += 500.0;
        moves.push((dep, 500.0, 0, 0.0));
        flows.push(IbkrFlow { flow_id: format!("demo-dep-{}", ym(m)), date: dep.to_string(), kind: "Deposits/Withdrawals".into(),
                              currency: BASE.into(), amount: 500.0, amount_base: 500.0, description: "CASH RECEIPTS / ELECTRONIC FUND TRANSFERS".into() });
        let buy = bday(day(m, 6));
        if buy > today {
            continue;
        }
        let budget = 500.0;
        for (k, share) in [(0usize, 0.62), (1, 0.16), (2, 1.0)] {
            let px = price_on(&series[k], buy);
            let n = if k == 2 { if i % 4 == 3 && cash >= px + 5.0 { 1.0 } else { 0.0 } } else { ((budget * share) / px).floor() };
            if n <= 0.0 || n * px + 1.25 > cash {
                continue;
            }
            let fee = 1.25;
            let gross = r2(n * px);
            cash = r2(cash - gross - fee);
            qty[k] += n;
            cost[k] += gross + fee;
            moves.push((buy, -(gross + fee), k, n));
            trades.push(IbkrTrade {
                trade_id: format!("demo-{}-{}", ETFS[k].0, ym(m)),
                ts: ms(buy, 9) + k as i64 * 60_000,
                symbol: ETFS[k].0.into(),
                description: ETFS[k].1.into(),
                currency: BASE.into(),
                qty: n,
                price: px,
                amount: -(gross + fee),
                commission: -fee,
                fx_to_base: 1.0,
            });
        }
        // the distributing fund pays every quarter
        if m.month().is_multiple_of(3) {
            let paid = bday(day(m, 26));
            let per = ETFS[1].6;
            if paid <= today && qty[1] > 0.0 {
                let amt = r2(qty[1] * per);
                cash = r2(cash + amt);
                moves.push((paid, amt, 0, 0.0));
                income.push(IbkrIncome {
                    income_id: format!("demo-div-{}", ym(m)),
                    date: paid.to_string(),
                    symbol: "VHYL".into(),
                    kind: "Dividends".into(),
                    currency: BASE.into(),
                    amount: amt,
                    amount_base: amt,
                    description: format!("VHYL Cash Dividend EUR {per:.2} per Share"),
                });
            }
        }
    }
    moves.sort_by_key(|m| m.0);
    let mut nav = Vec::new();
    let (mut c, mut q, mut j) = (0.0, [0.0f64; 3], 0);
    for d in bdays.iter().filter(|d| **d >= start) {
        while j < moves.len() && moves[j].0 <= *d {
            c += moves[j].1;
            q[moves[j].2] += moves[j].3;
            j += 1;
        }
        let pos: f64 = (0..3).map(|k| q[k] * price_on(&series[k], *d)).sum();
        if c.abs() < 0.01 && pos.abs() < 0.01 {
            continue;
        }
        nav.push(IbkrNav { date: d.to_string(), cash_base: r2(c), positions_base: r2(pos), nav_base: r2(c + pos), base_ccy: BASE.into() });
    }
    let as_of = bdays.last().copied().unwrap_or(today).to_string();
    let positions: Vec<IbkrPosition> = (0..3).filter(|k| qty[*k] > 0.0).map(|k| {
        let mark = price_on(&series[k], today);
        let value = r2(qty[k] * mark);
        IbkrPosition { symbol: ETFS[k].0.into(), description: ETFS[k].1.into(), currency: BASE.into(), qty: qty[k], mark_price: Some(mark),
                       value_ccy: value, cost_ccy: Some(r2(cost[k])), value_base: value, base_ccy: BASE.into(), as_of: as_of.clone() }
    }).collect();
    let cash_rows = vec![IbkrCash { currency: BASE.into(), amount: r2(cash), amount_base: r2(cash), base_ccy: BASE.into(), as_of }];
    db::upsert_ibkr_trades(conn, &trades)?;
    db::replace_ibkr_state(conn, &positions, &cash_rows)?;
    db::upsert_ibkr_nav(conn, &nav)?;
    db::upsert_ibkr_cash_flows(conn, &flows)?;
    db::upsert_ibkr_income(conn, &income)?;
    Ok(())
}

/// Daily bars around a close series.
fn store_bars(conn: &Connection, rng: &mut Rng, symbol: &str, ccy: &str, s: &BTreeMap<NaiveDate, f64>) -> Result<()> {
    let mut closes = BTreeMap::new();
    let mut bars = BTreeMap::new();
    let mut prev: Option<f64> = None;
    for (d, c) in s {
        let o = prev.unwrap_or(*c);
        let hi = r2(o.max(*c) * (1.0 + rng.range(0.0, 0.006)));
        let lo = r2(o.min(*c) * (1.0 - rng.range(0.0, 0.006)));
        closes.insert(d.to_string(), *c);
        bars.insert(d.to_string(), (o, hi, lo, *c));
        prev = Some(*c);
    }
    db::upsert_equity_prices(conn, symbol, &closes, Some(ccy), Some(&bars))
}

/// Crypto on an exchange: a weekly BTC auto-invest, monthly ETH buys.
fn crypto(conn: &Connection, rng: &mut Rng, start: NaiveDate, today: NaiveDate, fx: &BTreeMap<(&'static str, String), f64>) -> Result<()> {
    let days: Vec<NaiveDate> = (0..).map(|i| start - Duration::days(14) + Duration::days(i)).take_while(|d| *d <= today).collect();
    let btc = walk(rng, &days, 58_000.0, 0.35, 0.50, 365.0);
    let eth = walk(rng, &days, 2_900.0, 0.0, 0.45, 365.0);
    let mut events = Vec::new();
    for d in days.iter().filter(|d| **d >= start) {
        if d.weekday() == Weekday::Mon {
            let px = price_on(&btc, *d);
            events.push(db::Event::new(ms(*d, 8), "BTC", (30.0 / px * 1e8).round() / 1e8, Some(30.0), "auto-invest", "weekly plan"));
        }
        if d.day() == 10 {
            let px = price_on(&eth, *d);
            events.push(db::Event::new(ms(*d, 15), "ETH", (40.0 / px * 1e8).round() / 1e8, Some(40.0), "spot-buy", "ETH/USDT"));
        }
    }
    db::insert_crypto_events(conn, "binance", &events)?;
    // month-end closes, the euro in dollars, weekly candles, today's balances
    let mut monthly: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    for (asset, s) in [("BTC", &btc), ("ETH", &eth)] {
        for (d, p) in s.iter().filter(|(d, _)| **d >= start) {
            monthly.entry(asset.into()).or_default().insert(ym(*d), *p);
        }
    }
    for ((ccy, m), r) in fx {
        if *ccy == "USD" {
            monthly.entry("EUR".into()).or_default().insert(m.clone(), r2(1.0 / r * 1e4) / 1e4);
        }
    }
    db::upsert_prices(conn, &monthly)?;
    let mut weekly = BTreeMap::new();
    let mut wbars = BTreeMap::new();
    for d in days.iter().filter(|d| d.weekday() == Weekday::Mon) {
        let week: Vec<f64> = (0..7).map(|i| *d + Duration::days(i)).filter(|x| *x <= today).map(|x| price_on(&btc, x)).collect();
        let (o, c) = (week[0], *week.last().expect("a day"));
        let hi = week.iter().cloned().fold(f64::MIN, f64::max);
        let lo = week.iter().cloned().fold(f64::MAX, f64::min);
        weekly.insert(d.to_string(), c);
        wbars.insert(d.to_string(), (o, hi, lo, c));
    }
    db::upsert_equity_prices(conn, "BTC-1W", &weekly, Some("USD"), Some(&wbars))?;
    let mut held: BTreeMap<String, f64> = BTreeMap::new();
    for e in &events {
        *held.entry(e.asset.clone()).or_insert(0.0) += e.delta;
    }
    let prices: BTreeMap<String, f64> = [("BTC".to_string(), price_on(&btc, today)), ("ETH".to_string(), price_on(&eth, today))].into();
    db::add_snapshot(conn, ms(today, 12), "binance", &held, &prices)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// the replica
// ---------------------------------------------------------------------------

/// The export bundle of a demo db, as the server would send it: computed on
/// the cache-only feed with the clock at `today`, plus `"demo": true`. The
/// sources read as set up (they have data) and synced today: nothing of
/// the machine's own configuration shows through.
pub fn bundle(conn: &Connection, today: NaiveDate) -> Value {
    let mut ctx = crate::views::Ctx::offline(conn);
    ctx.today = today;
    let mut b = crate::export::bundle(&ctx, None);
    if let Some(list) = b.pointer_mut("/capabilities/sources").and_then(Value::as_array_mut) {
        for s in list.iter_mut() {
            let has = s["has_data"] == true;
            s["configured"] = json!(has);
            s["last_sync"] = if has { json!(ms(today, 12)) } else { Value::Null };
            if let Some(o) = s.as_object_mut() {
                o.remove("configured_via");
            }
        }
    }
    b["demo"] = json!(true);
    b
}

/// Build the demo replica into `dir`: `finance.db` and `export.json`, each
/// written whole and renamed into place. The work happens in a scratch
/// folder inside `dir` (the tenant the build runs as), removed after.
pub fn write(dir: &Path, today: NaiveDate) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let work = dir.join(".demo-build");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("data"))?;
    let tenant = crate::tenant::Tenant::user_at(UID, &work);
    let made = tenant.enter(|| -> Result<Vec<u8>> {
        let db_path = work.join("data").join("finance.db");
        let conn = db::connect(&db_path)?;
        build(&conn, today)?;
        let b = bundle(&conn, today);
        // one plain file, like a synced replica
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        drop(conn);
        Ok(serde_json::to_vec(&b)?)
    });
    let result = made.and_then(|bundle| {
        std::fs::rename(work.join("data").join("finance.db"), dir.join("finance.db")).context("placing the demo finance.db")?;
        let tmp = dir.join("export.json.tmp");
        std::fs::write(&tmp, &bundle)?;
        std::fs::rename(&tmp, dir.join("export.json")).context("placing the demo export.json")?;
        Ok(())
    });
    let _ = std::fs::remove_dir_all(&work);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("finance-demo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    const TODAY: (i32, u32, u32) = (2026, 9, 20);

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(TODAY.0, TODAY.1, TODAY.2).unwrap()
    }

    fn built(tag: &str) -> (std::path::PathBuf, Value) {
        let d = scratch(tag);
        write(&d, today()).unwrap();
        let b: Value = serde_json::from_slice(&std::fs::read(d.join("export.json")).unwrap()).unwrap();
        (d, b)
    }

    #[test]
    fn every_section_computes_and_has_something() {
        let (d, b) = built("sections");
        assert_eq!(b["demo"], true);
        assert_eq!(b["profile"]["name"], NAME);
        assert_eq!(b["fx"]["base"], BASE);
        for (k, v) in b.as_object().unwrap() {
            assert!(v.get("error").is_none(), "section {k} failed: {}", v["error"]);
        }
        let f = &b["capabilities"]["features"];
        for feat in ["banks", "spending", "crypto", "broker", "receivables", "travel", "cashflow"] {
            assert_eq!(f[feat], true, "feature {feat}");
        }
        assert!(b["net_worth"]["total_hkd"].as_f64().unwrap_or(0.0) > 10_000.0, "net worth {}", b["net_worth"]);
        assert!(!b["spending"]["trend"].as_array().unwrap().is_empty());
        assert!(b["overview"]["sheet"]["rows"].as_array().is_some_and(|s| s.len() >= MONTHS as usize - 1), "the net worth history");
        // N26, its savings, a Revolut pocket per currency
        assert_eq!(b["banks"]["sheet"]["columns"].as_array().unwrap().len(), 7);
        assert!(b["banks"]["cashflows"].as_array().is_some_and(|c| c.len() >= MONTHS as usize - 1));
        assert!(b["crypto"].get("empty").is_none());
        assert!(b["crypto"]["assets"]["BTC"]["qty"].as_f64().unwrap() > 0.0);
        assert_eq!(b["ibkr"]["snapshot"]["positions"].as_array().unwrap().len(), 3, "{}", b["ibkr"]["snapshot"]);
        assert!(b["ibkr"]["performance"]["nav"].as_array().unwrap().len() > 300);
        assert!(!b["spending"]["recurring"].as_array().unwrap().is_empty());
        assert!(b["receivables"]["deposits_hkd"].as_f64().unwrap() > 0.0);
        assert!(b["investments"]["classes"].as_array().unwrap().len() >= 2, "the broker and crypto");
        assert!(b["user_settings"]["budgets"]["monthly"]["Groceries"].as_f64().is_some());
        // the home setup: rent, a lease ending in 45 days, the deposit one-off
        let home = &b["user_settings"]["home"];
        let setup = finance_core::home::HomeSetup::parse(home).expect("the demo's home setup is valid");
        assert_eq!((setup.mode.as_str(), setup.rent.monthly), ("rent", 1250.0));
        let end = chrono::NaiveDate::parse_from_str(home["rent"]["lease"]["end"].as_str().unwrap(), "%Y-%m-%d").unwrap();
        assert!(matches!(setup.rent.lease.state(&(end - Duration::days(45)).to_string()),
            Some(finance_core::home::LeaseState::Active { days_left: 45, renew: true })));
        assert_eq!(setup.deposit().map(|o| o.amount), Some(2400.0));
        assert!(b["receivables"]["deposits_hkd"].as_f64().unwrap() > 0.0, "the deposit is still held");
        // a plain SQLite file, nothing left of the build
        assert!(std::fs::read(d.join("finance.db")).unwrap().starts_with(b"SQLite format 3\0"));
        assert!(!d.join(".demo-build").exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_same_day_builds_the_same_data() {
        let rows = |tag: &str| {
            let (d, _) = built(tag);
            let c = Connection::open(d.join("finance.db")).unwrap();
            let v: (i64, f64, i64, i64) = c.query_row(
                "SELECT COUNT(*), ROUND(SUM(amount), 2), (SELECT COUNT(*) FROM ibkr_trades), (SELECT COUNT(*) FROM crypto_events) FROM bank_transactions",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
            let cats: i64 = c.query_row("SELECT COUNT(DISTINCT category) FROM bank_transactions", [], |r| r.get(0)).unwrap();
            let other: i64 = c.query_row("SELECT COUNT(*) FROM bank_transactions WHERE category = 'Other'", [], |r| r.get(0)).unwrap();
            let _ = std::fs::remove_dir_all(d);
            (v, cats, other)
        };
        let a = rows("det-a");
        assert_eq!(a, rows("det-b"));
        let ((n, _, trades, coins), cats, other) = a;
        assert!(n > 800 && trades > 40 && coins > 80, "{a:?}");
        assert!(cats >= 15, "{cats} categories");
        assert_eq!(other, 0, "every invented merchant has a rule");
    }
}
