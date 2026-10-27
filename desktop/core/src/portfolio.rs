//! Everything invested, in one place: the FlexInvest fund, the broker
//! holdings, the coins and the Revolut pot as one list of holdings with
//! their returns over a week, a month and the year; the accounts; the
//! time-weighted returns against the benchmarks; money in per account per
//! month; and the income the investments paid (crypto rewards, broker
//! interest and dividends).
//!
//! Values and gains come from the bundle (the server's valuation model):
//! an account's total is always the server's. Inside an account the split
//! is ours: broker holdings at the snapshot's marks and cost at each buy's
//! rate (`invest::holding_plans`); coins scaled so they add up to the
//! server's crypto class. Returns over a period are price returns of each
//! holding's own bars (the FlexInvest fund through its proxy, URTH, when
//! the replica has its bars, else its monthly proxy closes).
//!
//! "Gain this year" is the market result inside the calendar year: the
//! units held going into each bar times the bar's move, so money put in
//! during the year never counts as gain.

use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike as _, Duration, NaiveDate};
use serde_json::Value;

use crate::dca::{self, bar_of, BTC_WEEKLY};
use crate::invest::{Extra, HoldingPlan};
use crate::jkb;
use crate::ledger::{Bar, Ledger};
use crate::Bundle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Account {
    FlexInvest,
    Ibkr,
    Crypto,
    Revolut,
}

impl Account {
    pub const ALL: [Account; 4] = [Account::FlexInvest, Account::Ibkr, Account::Crypto, Account::Revolut];

    pub fn label(self) -> &'static str {
        match self {
            Account::FlexInvest => "FlexInvest",
            Account::Ibkr => "IBKR",
            Account::Crypto => "Crypto",
            Account::Revolut => "Revolut",
        }
    }

    /// the bundle's `investments.classes[].key` (the fund account's name is
    /// the server's: crate::base, from the bundle's flexinvest section)
    pub fn class_key(self) -> String {
        match self {
            Account::FlexInvest => crate::base::get().fund_name(),
            Account::Ibkr => "IBKR".into(),
            Account::Crypto => "Crypto".into(),
            Account::Revolut => "Revolut".into(),
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// Coins that track the dollar: rewards in them are worth their quantity,
/// and buying them isn't investing.
pub const STABLE: [&str; 5] = ["USDT", "USDC", "BUSD", "FDUSD", "DAI"];
/// Crypto events that are purchases (the same set as the BTC plan's).
const BUY_KINDS: [&str; 5] = ["auto-invest", "spot-buy", "fiat", "convert-in", "revolut-btc"];

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Holding {
    pub name: String,
    pub symbol: String,
    pub account: Option<Account>,
    pub value_hkd: f64,
    pub cost_hkd: f64,
    pub first_buy: Option<String>,
    /// price returns, %
    pub ret_1w: Option<f64>,
    pub ret_1m: Option<f64>,
    pub ret_ytd: Option<f64>,
    /// recent closes, oldest first (a sparkline)
    pub spark: Vec<f64>,
    /// the market result this calendar year, HKD
    pub gain_ytd_hkd: Option<f64>,
    /// what the returns are measured on when it isn't the holding itself
    pub proxy: Option<String>,
}

impl Holding {
    pub fn gain_hkd(&self) -> f64 {
        self.value_hkd - self.cost_hkd
    }
    pub fn gain_pct(&self) -> Option<f64> {
        (self.cost_hkd > 0.).then(|| self.gain_hkd() / self.cost_hkd * 100.)
    }
    /// HKD the holding moved over a period with return `pct` (to today's value)
    pub fn moved_hkd(&self, pct: f64) -> f64 {
        self.value_hkd - self.value_hkd / (1. + pct / 100.)
    }
    /// What it is: coins by their symbol (stablecoins are cash), the
    /// Revolut pot unclassified (its contents aren't in the data), and the
    /// rest by symbol and name (`exposure::class`).
    pub fn classification(&self) -> crate::exposure::class::Classification {
        use crate::exposure::class::{classify, classify_coin, Classification};
        match self.account {
            Some(Account::Crypto) => classify_coin(&self.symbol),
            Some(Account::Revolut) => Classification::UNKNOWN,
            _ => classify(&self.symbol, &self.name),
        }
    }
}

/// One account: the server's totals, a value series, the year's result.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountSum {
    pub account: Account,
    pub value: f64,
    pub invested: f64,
    pub gain_ytd: Option<f64>,
    /// value over time, oldest first (a sparkline)
    pub spark: Vec<f64>,
}

impl AccountSum {
    pub fn gain(&self) -> f64 {
        self.value - self.invested
    }
    pub fn gain_pct(&self) -> Option<f64> {
        (self.invested > 0.).then(|| self.gain() / self.invested * 100.)
    }
}

/// Returns over this month, this year and since the start, %.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Periods {
    pub month: Option<f64>,
    pub ytd: Option<f64>,
    pub since: Option<f64>,
}

/// Money in during one month, per account (indexed by `Account::index`), HKD.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MonthIn {
    pub month: String,
    pub by: [f64; 4],
}

impl MonthIn {
    pub fn total(&self) -> f64 {
        self.by.iter().sum()
    }
}

/// Crypto rewards in one asset.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reward {
    pub asset: String,
    pub count: usize,
    pub qty: f64,
    /// valued when received where a price exists for that day (BTC's weekly
    /// close, stablecoins at 1), else at today's price
    pub usd: f64,
    pub usd_ytd: f64,
    pub last: String,
}

/// Broker cash income from the replica's IBKR cash flows.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BrokerIncome {
    pub interest_hkd: f64,
    pub dividends_hkd: f64,
    pub withheld_hkd: f64,
}

/// Everything the Investments page shows, computed once per data load.
#[derive(Debug, Clone, Default)]
pub struct Portfolio {
    pub today: String,
    pub usd: f64,
    pub holdings: Vec<Holding>,
    pub accounts: Vec<AccountSum>,
    /// broker holdings as DCA plans (cost, buys, daily points)
    pub plans: Vec<HoldingPlan>,
    /// the BTC plan over every buy, USD
    pub btc: Option<dca::Dca>,
    pub months_in: Vec<MonthIn>,
    /// months with a buy per account
    pub buy_months: BTreeMap<Account, Vec<String>>,
    pub rewards: Vec<Reward>,
    pub broker_income: BrokerIncome,
    pub twr: Periods,
    pub spx: Periods,
    /// MSCI World (URTH) price return, when the replica has its bars
    pub msci: Option<Periods>,
}

// ------------------------------------------------------------- helpers

fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn days_before(today: &str, n: i64) -> String {
    date(today).map(|d| (d - Duration::days(n)).format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// "2025-12-31" for a today in 2026
fn last_year_end(today: &str) -> String {
    format!("{}-12-31", date(today).map(|d| d.year() - 1).unwrap_or(0))
}

/// The last day of the month before today's.
fn last_month_end(today: &str) -> String {
    date(today).and_then(|d| d.with_day(1)).map(|d| (d - Duration::days(1)).format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// The price return from the close on (or before) `since` to `now`, %.
pub fn ret_since(bars: &[Bar], since: &str, now: f64) -> Option<f64> {
    let i = bar_of(bars, since)?;
    let c = bars[i].close;
    (c > 0. && now > 0.).then(|| (now / c - 1.) * 100.)
}

fn tail_closes(bars: &[Bar], n: usize, now: Option<f64>) -> Vec<f64> {
    let mut v: Vec<f64> = bars[bars.len().saturating_sub(n)..].iter().map(|b| b.close).collect();
    if let Some(p) = now.filter(|p| *p > 0.) {
        v.push(p);
    }
    v
}

fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

/// The market result of a plan's points inside the year of `today`, in the
/// plan's currency, scaled by `k` (e.g. units the plan doesn't see).
fn ytd_market(points: &[(String, f64, f64)], price_now: Option<f64>, today: &str, k: f64) -> f64 {
    let start = format!("{}-01-01", today.get(..4).unwrap_or(""));
    jkb::period_pnl(points, price_now).iter()
        .filter(|(d, _)| d == "now" || d.as_str() >= start.as_str())
        .map(|(_, v)| v * k)
        .sum()
}

pub fn plan_points(d: &dca::Dca) -> Vec<(String, f64, f64)> {
    d.points.iter().map(|p| (p.date.clone(), p.qty, p.close)).collect()
}

/// Cumulative %-since-start series -> this month, the year, since the start.
fn periods_of(rows: &[&Value], key: &str, today: &str) -> Periods {
    let cum: Vec<(String, f64)> = rows.iter().filter_map(|r| Some((r.get("month")?.as_str()?.to_string(), f(r, key)?))).collect();
    let Some((last_m, last)) = cum.last().cloned() else { return Periods::default() };
    let g = |c: f64| 1. + c / 100.;
    let rel = |base: f64| (g(base) > 0.).then(|| (g(last) / g(base) - 1.) * 100.);
    let prev_dec = format!("{}-12", last_year_end(today).get(..4).unwrap_or(""));
    Periods {
        month: cum.len().checked_sub(2).and_then(|i| rel(cum[i].1)).filter(|_| last_m.as_str() >= &today[..7.min(today.len())]),
        ytd: cum.iter().find(|(m, _)| *m == prev_dec).and_then(|(_, c)| rel(*c)),
        since: Some(last),
    }
}

/// The same periods as a price return on daily bars, from the end of
/// `first_month` for "since".
fn price_periods(bars: &[Bar], first_month: &str, today: &str) -> Option<Periods> {
    let now = bars.last()?.close;
    let first_end = date(&format!("{first_month}-01")).and_then(|d| {
        let next = if d.month() == 12 { NaiveDate::from_ymd_opt(d.year() + 1, 1, 1) } else { NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1) };
        next.map(|n| (n - Duration::days(1)).format("%Y-%m-%d").to_string())
    })?;
    Some(Periods {
        month: ret_since(bars, &last_month_end(today), now),
        ytd: ret_since(bars, &last_year_end(today), now),
        since: ret_since(bars, &first_end, now),
    })
}

// --------------------------------------------------------------- build

impl Portfolio {
    /// `today` is "YYYY-MM-DD". `x` brings the FX points, the IBKR cash
    /// flows and the proxy's bars (empty in the web build).
    pub fn build(b: &Bundle, ledger: &Ledger, x: &Extra, today: &str) -> Portfolio {
        let usd = b.f64(&["fx", "USD"]).filter(|r| *r > 0.).unwrap_or(7.8);
        let classes = b.arr(&["investments", "classes"]);
        let class = |a: Account| classes.iter().find(|c| c.get("key").and_then(Value::as_str) == Some(a.class_key().as_str()));
        let class_num = |a: Account, k: &str| class(a).and_then(|c| f(c, k)).unwrap_or(0.);
        let rows: Vec<&Value> = b.arr(&["investments", "rows"]).iter().collect();
        let year = today.get(..4).unwrap_or("").to_string();
        let (w1, m1, y0) = (days_before(today, 7), days_before(today, 30), last_year_end(today));

        // ---- broker
        let marks: HashMap<String, f64> = b.arr(&["ibkr", "snapshot", "positions"]).iter()
            .filter_map(|p| Some((p.get("symbol")?.as_str()?.to_string(), f(p, "mark_price")?))).collect();
        let plans = crate::invest::holding_plans(ledger, &x.fx, today, &marks);
        let mut holdings = Vec::new();
        for p in b.arr(&["ibkr", "snapshot", "positions"]) {
            let Some(sym) = p.get("symbol").and_then(Value::as_str) else { continue };
            let value = f(p, "value_base").unwrap_or(0.);
            let plan = plans.iter().find(|h| h.symbol == sym);
            let bars = ledger.bars.get(sym).map(Vec::as_slice).unwrap_or(&[]);
            let now = marks.get(sym).copied().or(bars.last().map(|b| b.close)).unwrap_or(0.);
            holdings.push(Holding {
                name: p.get("description").and_then(Value::as_str).unwrap_or(sym).to_string(),
                symbol: sym.to_string(),
                account: Some(Account::Ibkr),
                value_hkd: value,
                cost_hkd: plan.map(|h| h.cost_hkd() - h.dca.sold * h.fx_now).unwrap_or(value),
                first_buy: plan.and_then(|h| h.dca.buys.first()).map(|b| b.date.clone()),
                ret_1w: ret_since(bars, &w1, now),
                ret_1m: ret_since(bars, &m1, now),
                ret_ytd: ret_since(bars, &y0, now),
                spark: tail_closes(bars, 63, Some(now)),
                gain_ytd_hkd: plan.map(|h| ytd_market(&plan_points(&h.dca), Some(now), today, h.fx_now)),
                proxy: None,
            });
        }

        // ---- FlexInvest: one fund, returns through its proxy
        let flex_rows = b.arr(&["flexinvest", "rows"]);
        let flex_value = class_num(Account::FlexInvest, "value");
        if flex_value > 0. {
            let closes: Vec<f64> = flex_rows.iter().filter_map(|r| f(r, "close")).collect();
            let (r1w, r1m, rytd, spark) = if !x.proxy_bars.is_empty() {
                let pb = &x.proxy_bars;
                let now = pb[pb.len() - 1].close;
                (ret_since(pb, &w1, now), ret_since(pb, &m1, now), ret_since(pb, &y0, now), tail_closes(pb, 63, None))
            } else {
                let m = closes.len().checked_sub(2).map(|i| (closes[i + 1] / closes[i] - 1.) * 100.);
                (None, m, None, closes.clone())
            };
            holdings.push(Holding {
                name: b.str(&["flexinvest", "holding"]).unwrap_or("FlexInvest").to_string(),
                symbol: b.str(&["flexinvest", "fund"]).unwrap_or("FlexInvest").to_string(),
                account: Some(Account::FlexInvest),
                value_hkd: flex_value,
                cost_hkd: class_num(Account::FlexInvest, "invested"),
                first_buy: b.arr(&["flexinvest", "purchases"]).first().and_then(|p| p.get("date")?.as_str()).map(str::to_string),
                ret_1w: r1w,
                ret_1m: r1m,
                ret_ytd: rytd,
                spark,
                gain_ytd_hkd: Some(flex_rows.iter()
                    .filter(|r| r.get("month").and_then(Value::as_str).is_some_and(|m| m.starts_with(&year)))
                    .filter_map(|r| f(r, "market")).sum()),
                proxy: Some(crate::invest::fund_proxy(b)),
            });
        }

        // ---- coins: scaled to add up to the server's crypto class
        let live_btc = b.f64(&["crypto", "assets", "BTC", "price"]).or_else(|| b.f64(&["crypto", "price", "BTC"]));
        let btc = dca::analyze(ledger, live_btc, dca::Scope::All);
        let assets: Vec<(String, &Value)> = b.at(&["crypto", "assets"]).as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v)).collect()).unwrap_or_default();
        let sum_v: f64 = assets.iter().filter_map(|(_, a)| f(a, "value")).sum();
        let sum_c: f64 = assets.iter().filter_map(|(_, a)| f(a, "invested_allin").or(f(a, "invested"))).sum();
        let (cv, ci) = (class_num(Account::Crypto, "value"), class_num(Account::Crypto, "invested"));
        let (kv, kc) = (if sum_v > 0. { cv / sum_v } else { usd }, if sum_c > 0. { ci / sum_c } else { usd });
        let first_coin_buy = |asset: &str| ledger.coins.iter()
            .find(|c| c.asset == asset && c.delta > 0. && BUY_KINDS.contains(&c.kind.as_str()))
            .map(|c| c.date.clone());
        for (asset, a) in &assets {
            let value = f(a, "value").unwrap_or(0.) * kv;
            if value < 1. {
                continue;
            }
            let mut h = Holding {
                name: asset.clone(),
                symbol: asset.clone(),
                account: Some(Account::Crypto),
                value_hkd: value,
                cost_hkd: f(a, "invested_allin").or(f(a, "invested")).unwrap_or(0.) * kc,
                first_buy: first_coin_buy(asset),
                ..Default::default()
            };
            if asset == "BTC" {
                if let (Some(bars), Some(now)) = (ledger.bars.get(BTC_WEEKLY).filter(|v| !v.is_empty()), f(a, "price")) {
                    h.ret_1w = ret_since(bars, &w1, now);
                    h.ret_1m = ret_since(bars, &m1, now);
                    h.ret_ytd = ret_since(bars, &y0, now);
                    h.spark = tail_closes(bars, 26, Some(now));
                    if let Some(d) = &btc {
                        // the plan doesn't see deposited coins: scale to what's held
                        let held = f(a, "qty").unwrap_or(0.);
                        let k = if d.position_qty() > 0. { held / d.position_qty() } else { 1. };
                        h.gain_ytd_hkd = Some(ytd_market(&plan_points(d), Some(now), today, k * kv));
                    }
                }
            }
            holdings.push(h);
        }

        // ---- Revolut's pot, held at cost
        let rev = class_num(Account::Revolut, "value");
        if rev > 0. {
            holdings.push(Holding {
                name: "Revolut invest pot".into(),
                symbol: "Revolut".into(),
                account: Some(Account::Revolut),
                value_hkd: rev,
                cost_hkd: class_num(Account::Revolut, "invested"),
                gain_ytd_hkd: Some(0.),
                proxy: Some("held at cost".into()),
                ..Default::default()
            });
        }
        holdings.sort_by(|a, b| b.value_hkd.total_cmp(&a.value_hkd));

        // ---- accounts
        let series = |a: Account| -> Vec<f64> {
            rows.iter().rev().take(24).rev()
                .map(|r| r.get("classes").and_then(|c| f(c, &a.class_key())).unwrap_or(0.)).collect()
        };
        let nav = crate::invest::nav_days(b);
        // only accounts the server has a source for (absent capabilities: all)
        let caps = crate::capabilities::Capabilities::of(b);
        let has_source = |a: Account| match a {
            Account::FlexInvest => caps.has("fund_proxy"),
            Account::Ibkr => caps.has("broker"),
            Account::Crypto => caps.has("crypto"),
            Account::Revolut => true,
        };
        let accounts = Account::ALL.into_iter().filter(|a| class(*a).is_some() && has_source(*a)).map(|a| {
            let (value, invested) = (class_num(a, "value"), class_num(a, "invested"));
            let (spark, gain_ytd) = match a {
                Account::Ibkr if !nav.is_empty() => {
                    let start = format!("{year}-01-01");
                    let before = nav.iter().rev().find(|d| d.date < start).map(|d| d.nav).unwrap_or(0.);
                    let flows: f64 = nav.iter().filter(|d| d.date >= start).map(|d| d.flow).sum();
                    let step = (nav.len() / 40).max(1);
                    (nav.iter().step_by(step).map(|d| d.nav).collect(), Some(nav[nav.len() - 1].nav - before - flows))
                }
                Account::Revolut => (series(a), Some(0.)),
                _ => {
                    // the holdings that can be measured (ETH has no bars: left out)
                    let known: Vec<f64> = holdings.iter().filter(|h| h.account == Some(a)).filter_map(|h| h.gain_ytd_hkd).collect();
                    let ytd = (!known.is_empty()).then(|| known.iter().sum());
                    (series(a), ytd)
                }
            };
            AccountSum { account: a, value, invested, gain_ytd, spark }
        }).collect();

        // ---- money in per month
        let mut by_month: BTreeMap<String, [f64; 4]> = BTreeMap::new();
        let mut add = |m: &str, a: Account, v: f64| {
            if v > 0. && m.len() >= 7 {
                by_month.entry(m[..7].to_string()).or_default()[a.index()] += v;
            }
        };
        for r in flex_rows {
            if let (Some(m), Some(c)) = (r.get("month").and_then(Value::as_str), f(r, "contrib")) {
                add(m, Account::FlexInvest, c);
            }
        }
        for d in &nav {
            add(&d.date, Account::Ibkr, d.flow);
        }
        for c in &ledger.coins {
            if c.delta > 0. && BUY_KINDS.contains(&c.kind.as_str()) && !STABLE.contains(&c.asset.as_str()) {
                add(&c.date, Account::Crypto, c.usd.unwrap_or(0.).abs() * usd);
            }
        }
        let mut months_in = Vec::new();
        if let Some(first) = by_month.keys().next().cloned() {
            let (a, z) = (dca::month_index(&first), dca::month_index(today));
            for i in a..=z {
                let m = jkb::ym(i);
                months_in.push(MonthIn { by: by_month.get(&m).copied().unwrap_or_default(), month: m });
            }
        }

        // ---- months with a buy (the broker's trades, not its deposits)
        let mut buy_months: BTreeMap<Account, Vec<String>> = BTreeMap::new();
        buy_months.insert(Account::FlexInvest, flex_rows.iter()
            .filter(|r| f(r, "contrib").unwrap_or(0.) > 0.)
            .filter_map(|r| r.get("month")?.as_str().map(str::to_string)).collect());
        buy_months.insert(Account::Ibkr, ledger.trades.iter().filter(|t| t.qty > 0.).map(|t| t.date[..7].to_string()).collect());
        buy_months.insert(Account::Crypto, ledger.coins.iter()
            .filter(|c| c.delta > 0. && c.usd.unwrap_or(0.).abs() > 0. && BUY_KINDS.contains(&c.kind.as_str()) && !STABLE.contains(&c.asset.as_str()))
            .map(|c| c.date[..7].to_string()).collect());
        for v in buy_months.values_mut() {
            v.sort();
            v.dedup();
        }

        // ---- income
        let rewards = rewards(b, ledger, &year);
        let mut broker_income = BrokerIncome::default();
        for c in &x.cash_flows {
            let k = c.kind.to_lowercase();
            if k.contains("interest") {
                broker_income.interest_hkd += c.amount_base;
            } else if k.contains("withholding") {
                broker_income.withheld_hkd -= c.amount_base;
            } else if k.contains("dividend") || k.contains("in lieu") {
                broker_income.dividends_hkd += c.amount_base;
            }
        }

        // ---- returns against the benchmarks
        let first_month = rows.first().and_then(|r| r.get("month")?.as_str()).unwrap_or("").to_string();
        let msci = (!x.proxy_bars.is_empty()).then(|| price_periods(&x.proxy_bars, &first_month, today)).flatten();

        Portfolio {
            today: today.to_string(),
            usd,
            holdings,
            accounts,
            plans,
            btc,
            months_in,
            buy_months,
            rewards,
            broker_income,
            twr: periods_of(&rows, "twr_pct", today),
            spx: periods_of(&rows, "spx_twr_pct", today),
            msci,
        }
    }

    pub fn value(&self) -> f64 {
        self.accounts.iter().map(|a| a.value).sum()
    }
    pub fn invested(&self) -> f64 {
        self.accounts.iter().map(|a| a.invested).sum()
    }

    /// The broker's market result per trading day, HKD at today's rates
    /// (every held day of every holding, summed by date).
    pub fn broker_daily_pnl(&self) -> Vec<(String, f64)> {
        let mut by: BTreeMap<String, f64> = BTreeMap::new();
        for h in &self.plans {
            for (d, v) in jkb::period_pnl(&plan_points(&h.dca), None) {
                *by.entry(d).or_default() += v * h.fx_now;
            }
        }
        by.into_iter().collect()
    }

    /// BTC's market result per week, HKD.
    pub fn btc_weekly_pnl(&self) -> Vec<(String, f64)> {
        let Some(d) = &self.btc else { return Vec::new() };
        jkb::period_pnl(&plan_points(d), Some(d.price)).into_iter().map(|(w, v)| (w, v * self.usd)).collect()
    }
}

/// A run of months without a BTC buy, and what buying through it would hold.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Pause {
    pub from: String,
    pub to: String,
    pub months: usize,
    /// the median month of BTC buying, USD (months with a buy)
    pub monthly_usd: f64,
    /// buying that amount at the first weekly close of each missed month
    pub money_usd: f64,
    pub units: f64,
    pub value_usd: f64,
}

impl Pause {
    /// value now minus the money: what the pause cost (> 0) or saved (< 0)
    pub fn cost_usd(&self) -> f64 {
        self.value_usd - self.money_usd
    }
}

/// One "buy the dip vs DCA" case: an asset, its own currency.
#[derive(Debug, Clone, PartialEq)]
pub struct DipCase {
    pub symbol: String,
    pub currency: String,
    /// the bars' period, for the text ("weekly", "daily")
    pub period: &'static str,
    /// currency -> HKD now
    pub fx: f64,
    pub buys: usize,
    pub first: String,
    pub r: jkb::DipVsDca,
}

/// Enough history for the dip test: bars from a year before the first buy.
const DIP_HISTORY_DAYS: i64 = 365;

impl Portfolio {
    /// The longest BTC pause, simulated at the median month on the actual
    /// weekly closes.
    pub fn btc_pause(&self, ledger: &Ledger) -> Option<Pause> {
        let d = self.btc.as_ref()?;
        let bars = ledger.bars.get(BTC_WEEKLY)?;
        let months: Vec<String> = d.months.iter().map(|m| m.month.clone()).collect();
        let s = jkb::streak(&months, &self.today);
        let (from, to, n) = s.longest_gap?;
        let missed: Vec<String> = (dca::month_index(&from)..=dca::month_index(&to)).map(jkb::ym).collect();
        let monthly_usd = jkb::median(&d.months.iter().map(|m| m.cost).collect::<Vec<_>>());
        let (money_usd, units) = jkb::buy_months(&missed, monthly_usd, bars);
        Some(Pause { from, to, months: n, monthly_usd, money_usd, units, value_usd: units * d.price })
    }

    /// BTC on weekly closes, then each broker holding with 3+ buys and a
    /// year of daily bars before its first buy.
    pub fn dip_cases(&self, ledger: &Ledger) -> Vec<DipCase> {
        let mut out = Vec::new();
        if let (Some(d), Some(bars)) = (&self.btc, ledger.bars.get(BTC_WEEKLY)) {
            let c: Vec<(String, f64)> = d.buys.iter().map(|b| (b.date.clone(), b.cost)).collect();
            if let Some(r) = jkb::dip_vs_dca(&c, bars, Some(d.price)) {
                out.push(DipCase { symbol: "BTC".into(), currency: "USD".into(), period: "weekly", fx: self.usd,
                    buys: d.buys.len(), first: d.buys[0].date.clone(), r });
            }
        }
        for h in &self.plans {
            let Some(bars) = ledger.bars.get(&h.symbol).filter(|b| !b.is_empty()) else { continue };
            let Some(first) = h.dca.buys.first() else { continue };
            let enough = date(&bars[0].date).zip(date(&first.date)).is_some_and(|(a, b)| (b - a).num_days() >= DIP_HISTORY_DAYS);
            if h.dca.buys.len() < 3 || !enough {
                continue;
            }
            let c: Vec<(String, f64)> = h.dca.buys.iter().map(|b| (b.date.clone(), b.cost)).collect();
            if let Some(r) = jkb::dip_vs_dca(&c, bars, Some(h.dca.price)) {
                out.push(DipCase { symbol: h.symbol.clone(), currency: h.currency.clone(), period: "daily", fx: h.fx_now,
                    buys: h.dca.buys.len(), first: first.date.clone(), r });
            }
        }
        out
    }
}

/// Crypto rewards per asset, biggest first.
pub fn rewards(b: &Bundle, ledger: &Ledger, year: &str) -> Vec<Reward> {
    let weekly = ledger.bars.get(BTC_WEEKLY).map(Vec::as_slice).unwrap_or(&[]);
    let mut by: BTreeMap<&str, Reward> = BTreeMap::new();
    for c in ledger.coins.iter().filter(|c| c.kind == "reward" && c.delta > 0.) {
        let px = if STABLE.contains(&c.asset.as_str()) {
            1.
        } else if c.asset == "BTC" && !weekly.is_empty() {
            bar_of(weekly, &c.date).map(|i| weekly[i].close).unwrap_or(weekly[0].close)
        } else {
            b.f64(&["crypto", "price", &c.asset]).or_else(|| b.f64(&["crypto", "assets", &c.asset, "price"])).unwrap_or(0.)
        };
        let r = by.entry(c.asset.as_str()).or_insert_with(|| Reward { asset: c.asset.clone(), ..Default::default() });
        r.count += 1;
        r.qty += c.delta;
        r.usd += c.delta * px;
        if c.date.starts_with(year) {
            r.usd_ytd += c.delta * px;
        }
        r.last = r.last.clone().max(c.date.clone());
    }
    let mut v: Vec<Reward> = by.into_values().collect();
    v.sort_by(|a, b| b.usd.total_cmp(&a.usd));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::CoinEvent;

    fn bar(date: &str, close: f64) -> Bar {
        Bar { date: date.into(), open: close, high: close, low: close, close }
    }

    #[test]
    fn price_returns_and_dates() {
        let bars = vec![bar("2025-12-31", 100.), bar("2026-09-02", 110.), bar("2026-09-26", 120.), bar("2026-10-02", 132.)];
        assert_eq!(last_year_end("2026-10-03"), "2025-12-31");
        assert_eq!(last_month_end("2026-10-03"), "2026-09-30");
        assert_eq!(days_before("2026-10-03", 7), "2026-09-26");
        assert!((ret_since(&bars, "2025-12-31", 132.).unwrap() - 32.).abs() < 1e-9);
        assert!((ret_since(&bars, "2026-09-26", 132.).unwrap() - 10.).abs() < 1e-9);
        // before the first bar: unknown
        assert!(ret_since(&bars, "2025-01-01", 132.).is_none());
    }

    #[test]
    fn cumulative_periods() {
        let rows: Vec<Value> = [("2025-11", 10.), ("2025-12", 21.), ("2026-09", 32.1), ("2026-10", 45.31)]
            .iter().map(|(m, c)| serde_json::json!({ "month": m, "twr_pct": c })).collect();
        let r: Vec<&Value> = rows.iter().collect();
        let p = periods_of(&r, "twr_pct", "2026-10-03");
        assert!((p.month.unwrap() - 10.).abs() < 1e-9);
        assert!((p.ytd.unwrap() - (145.31 / 121. - 1.) * 100.).abs() < 1e-9);
        assert_eq!(p.since, Some(45.31));
    }

    #[test]
    fn ytd_market_skips_last_year() {
        // 1 unit through Dec (no count), 2 units from Jan: +2 x 10
        let pts = vec![("2025-12-01".to_string(), 1., 90.), ("2025-12-31".into(), 2., 100.), ("2026-01-31".into(), 2., 110.)];
        assert_eq!(ytd_market(&pts, None, "2026-10-03", 1.), 20.);
        // and a last move to the live price, scaled
        assert_eq!(ytd_market(&pts, Some(115.), "2026-10-03", 2.), 60.);
    }

    #[test]
    fn rewards_are_valued() {
        let mut l = Ledger::default();
        l.bars.insert(BTC_WEEKLY.into(), vec![bar("2026-01-05", 100_000.)]);
        let ev = |asset: &str, date: &str, delta: f64| CoinEvent {
            date: date.into(), exchange: "okx".into(), asset: asset.into(), delta, usd: Some(0.), kind: "reward".into(),
        };
        l.coins = vec![ev("BTC", "2026-01-06", 0.0001), ev("USDC", "2025-06-01", 2.), ev("USDC", "2026-02-01", 1.)];
        let b = Bundle(serde_json::json!({}));
        let r = rewards(&b, &l, "2026");
        let usdc = r.iter().find(|r| r.asset == "USDC").unwrap();
        assert_eq!((usdc.count, usdc.usd, usdc.usd_ytd), (2, 3., 1.));
        let btc = r.iter().find(|r| r.asset == "BTC").unwrap();
        assert!((btc.usd - 10.).abs() < 1e-9);
    }
}
