//! The investing deep dive's data: the broker holdings as DCA plans (each in
//! its own currency, rolled up in HKD), the broker's value split into money
//! in, market gain, currency moves and fees, the same split for all
//! investments, and dividends with their withholding tax.
//!
//! [`Extra`] reads what the ledger doesn't keep: IBKR's cash flows
//! (deposits, dividends, withholding), the monthly exchange rates, and the
//! bars of the FlexInvest fund's proxy (URTH). Exchange rates between the
//! monthly points are interpolated linearly by day, with each trade's own
//! rate and the broker snapshot's rate added as exact points.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Result;
use chrono::NaiveDate;
use serde_json::Value;

use crate::dca::{self, bar_of, Buy, Dca};
use crate::ledger::{Bar, Ledger, Trade};
use crate::Bundle;

/// The fund account's market proxy when nothing says otherwise (MSCI World).
pub const FUND_PROXY: &str = "URTH";

/// The fund account's proxy, as the server prices it (the bundle's
/// flexinvest section, from the investment setup).
pub fn fund_proxy(b: &Bundle) -> String {
    b.str(&["flexinvest", "proxy"]).unwrap_or(FUND_PROXY).to_string()
}

/// One IBKR cash movement (Flex "Cash Transactions"): the deposits and
/// withdrawals of ibkr_cash_flows, and the dividends and withholding of
/// ibkr_income (which the ledger carries, [`Ledger::income`]).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct CashFlow {
    pub date: String,
    /// "Deposits/Withdrawals", "Dividends", "Withholding Tax", ...
    pub kind: String,
    /// income rows: the instrument ("" for flows)
    #[serde(default)]
    pub symbol: String,
    pub currency: String,
    pub amount: f64,
    pub amount_base: f64,
    pub description: String,
}

/// Exchange rates to the base currency: per currency, (day number, rate),
/// sorted.
#[derive(Debug, Clone, Default)]
pub struct Fx(HashMap<String, Vec<(i64, f64)>>);

fn day_num(date: &str) -> Option<i64> {
    NaiveDate::parse_from_str(date.get(..10)?, "%Y-%m-%d").ok().map(|d| d.num_days_from_ce() as i64)
}

use chrono::Datelike as _;

fn is_base(ccy: &str) -> bool {
    crate::base::get().currency == ccy
}

impl Fx {
    /// Add an exact point (a trade's rate, the snapshot's).
    pub fn add(&mut self, ccy: &str, date: &str, rate: f64) {
        let Some(d) = day_num(date) else { return };
        if rate <= 0. || is_base(ccy) {
            return;
        }
        let v = self.0.entry(ccy.to_string()).or_default();
        match v.binary_search_by(|p| p.0.cmp(&d)) {
            Ok(i) => v[i].1 = rate,
            Err(i) => v.insert(i, (d, rate)),
        }
    }

    /// Base currency per unit of `ccy` on `date`: linear between the known
    /// points, flat beyond them. None for a currency with no points.
    pub fn rate(&self, ccy: &str, date: &str) -> Option<f64> {
        if is_base(ccy) {
            return Some(1.);
        }
        let v = self.0.get(ccy).filter(|v| !v.is_empty())?;
        let d = day_num(date)?;
        let i = v.partition_point(|p| p.0 <= d);
        Some(match (i.checked_sub(1).map(|j| v[j]), v.get(i)) {
            (Some(a), Some(b)) if b.0 > a.0 => a.1 + (b.1 - a.1) * (d - a.0) as f64 / (b.0 - a.0) as f64,
            (Some(a), _) => a.1,
            (None, Some(b)) => b.1,
            (None, None) => return None,
        })
    }
}

/// What the deep dive reads from the replica beyond the ledger.
#[derive(Debug, Clone, Default)]
pub struct Extra {
    pub cash_flows: Vec<CashFlow>,
    pub fx: Fx,
    /// daily bars of the FlexInvest proxy
    pub proxy_bars: Vec<Bar>,
}

impl Extra {
    /// Web build: no replica in the browser, so none of it (the trades' own
    /// rates and the income still come in through [`Extra::with_ledger`]).
    #[cfg(not(feature = "native"))]
    pub fn load(_db: &Path) -> Result<Self> {
        Ok(Extra::default())
    }

    /// Read the replica (read-only). Tables an older replica lacks are skipped.
    #[cfg(feature = "native")]
    pub fn load(db: &Path) -> Result<Self> {
        let conn = crate::replica::open_db(db)?;
        let mut x = Extra::default();
        if let Ok(mut stmt) = conn.prepare("SELECT date, type, currency, amount, amount_base, description FROM ibkr_cash_flows ORDER BY date") {
            let rows = stmt.query_map([], |r| Ok(CashFlow {
                date: r.get(0)?, kind: r.get(1)?, symbol: String::new(), currency: r.get(2)?, amount: r.get(3)?,
                amount_base: r.get(4)?, description: r.get(5)?,
            }))?;
            x.cash_flows = rows.collect::<Result<_, _>>()?;
        }
        // the rates to the replica's base currency (its profile's)
        let base: String = conn
            .query_row("SELECT base_currency FROM profile WHERE id = 1", [], |r| r.get::<_, String>(0))
            .map(|c| c.trim().to_uppercase())
            .unwrap_or_else(|_| crate::base::currency());
        if let Ok(mut stmt) = conn.prepare("SELECT base, month, rate FROM fx_rates WHERE quote = ?") {
            let rows = stmt.query_map([base], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?)))?;
            for row in rows {
                let (ccy, month, rate) = row?;
                // "2026-09" or "2026-09-01": the rate of the month's first day
                let date = if month.len() == 7 { format!("{month}-01") } else { month };
                x.fx.add(&ccy, &date, rate);
            }
        }
        if let Ok(mut stmt) = conn.prepare("SELECT date, close FROM equity_prices WHERE symbol = ? ORDER BY date") {
            // the replica's investment setup names the proxy (Phase 4)
            let proxy: String = conn
                .query_row(
                    "SELECT proxy FROM investment_accounts WHERE valued_by = 'proxy' AND proxy IS NOT NULL ORDER BY sort, name LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| FUND_PROXY.to_string());
            let rows = stmt.query_map([proxy], |r| {
                let close: f64 = r.get(1)?;
                Ok(Bar { date: r.get(0)?, open: close, high: close, low: close, close })
            })?;
            x.proxy_bars = rows.collect::<Result<_, _>>()?;
        }
        Ok(x)
    }

    /// Add what the ledger and the snapshot know: the exact rates, and the
    /// dividends and withholding (the ledger's, so the web build has them).
    pub fn with_ledger(mut self, ledger: &Ledger, b: &Bundle) -> Self {
        self.cash_flows.extend(ledger.income.iter().cloned());
        self.cash_flows.sort_by(|a, b| a.date.cmp(&b.date));
        for t in &ledger.trades {
            self.fx.add(&t.currency, &t.date, t.fx_to_base);
        }
        for (ccy, date, rate) in snapshot_rates(b) {
            self.fx.add(&ccy, &date, rate);
        }
        self
    }
}

/// The rates the broker snapshot implies: value_base / value_ccy per position.
pub fn snapshot_rates(b: &Bundle) -> Vec<(String, String, f64)> {
    b.arr(&["ibkr", "snapshot", "positions"]).iter().filter_map(|p| {
        let ccy = p.get("currency")?.as_str()?;
        let (vb, vc) = (p.get("value_base")?.as_f64()?, p.get("value_ccy")?.as_f64()?);
        let date = p.get("as_of")?.as_str()?;
        (vc.abs() > 1e-9 && !is_base(ccy)).then(|| (ccy.to_string(), date.to_string(), vb / vc))
    }).collect()
}

// ---------------------------------------------------------------- broker DCA

/// Money a buy actually cost, fees included. IBKR's `amount` is the net cash
/// (commission inside); other sources may leave the commission out.
pub fn buy_cost(t: &Trade) -> f64 {
    (-t.amount).max(t.qty * t.price + t.commission)
}

/// One broker holding as a DCA plan, in its trading currency.
#[derive(Debug, Clone, PartialEq)]
pub struct HoldingPlan {
    pub symbol: String,
    pub currency: String,
    pub dca: Dca,
    /// trading currency -> HKD now
    pub fx_now: f64,
}

impl HoldingPlan {
    pub fn value_hkd(&self) -> f64 {
        self.dca.value() * self.fx_now
    }
    /// what went in, HKD at each buy's rate
    pub fn cost_hkd(&self) -> f64 {
        self.dca.buys.iter().map(|b| b.cost * b.fx).sum()
    }
    pub fn fees_hkd(&self) -> f64 {
        self.dca.buys.iter().map(|b| b.fee * b.fx).sum()
    }
    pub fn lump_hkd(&self) -> f64 {
        self.dca.lump_value() * self.fx_now
    }
}

/// Every symbol the broker bought, with its daily bars; in first-buy order.
/// `price_now` per symbol (e.g. the broker's mark) wins over the last close.
pub fn holding_plans(ledger: &Ledger, fx: &Fx, today: &str, price_now: &HashMap<String, f64>) -> Vec<HoldingPlan> {
    let mut order: Vec<&str> = Vec::new();
    for t in &ledger.trades {
        if !order.contains(&t.symbol.as_str()) {
            order.push(&t.symbol);
        }
    }
    order.into_iter().filter_map(|sym| {
        let trades: Vec<&Trade> = ledger.trades.iter().filter(|t| t.symbol == sym).collect();
        let buys: Vec<Buy> = trades.iter().filter(|t| t.qty > 0.).map(|t| Buy {
            date: t.date.clone(), venue: "ibkr".into(), kind: "buy".into(), qty: t.qty,
            cost: buy_cost(t), fee: t.commission, fx: t.fx_to_base,
        }).collect();
        let sells: Vec<dca::Sell> = trades.iter().filter(|t| t.qty < 0.)
            .map(|t| (t.date.clone(), -t.qty, t.amount.min(-t.qty * t.price - t.commission).max(0.))).collect();
        let bars = ledger.bars.get(sym)?;
        let d = dca::plan(buys, &sells, &[], bars, price_now.get(sym).copied())?;
        let ccy = trades[0].currency.clone();
        let fx_now = fx.rate(&ccy, today).unwrap_or(trades.last().map(|t| t.fx_to_base).unwrap_or(1.));
        Some(HoldingPlan { symbol: sym.to_string(), currency: ccy, dca: d, fx_now })
    }).collect()
}

/// The broker's plans rolled up in HKD.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Portfolio {
    pub buys: usize,
    pub cost_hkd: f64,
    pub value_hkd: f64,
    pub fees_hkd: f64,
    /// the holdings' edges weighted by what went into each, %
    pub edge_pct: f64,
    pub edge_after_fees_pct: f64,
    /// each holding's money at its own first buy, valued today
    pub lump_hkd: f64,
    pub months_active: usize,
    pub months_span: usize,
    pub in_profit: usize,
    pub up_days: usize,
    pub down_days: usize,
}

impl Portfolio {
    pub fn gain_hkd(&self) -> f64 {
        self.value_hkd - self.cost_hkd
    }
    pub fn vs_lump_pct(&self) -> f64 {
        if self.lump_hkd > 0. { (self.value_hkd / self.lump_hkd - 1.) * 100. } else { 0. }
    }
    pub fn fee_drag_pct(&self) -> f64 {
        if self.cost_hkd > 0. { self.fees_hkd / self.cost_hkd * 100. } else { 0. }
    }
}

pub fn portfolio(plans: &[HoldingPlan], ledger: &Ledger) -> Portfolio {
    let mut p = Portfolio::default();
    let mut months: Vec<i64> = Vec::new();
    let (mut first, mut last) = (i64::MAX, i64::MIN);
    for h in plans {
        let cost = h.cost_hkd();
        p.buys += h.dca.buys.len();
        p.cost_hkd += cost;
        p.value_hkd += h.value_hkd() + h.dca.sold * h.fx_now;
        p.fees_hkd += h.fees_hkd();
        p.edge_pct += h.dca.edge_pct() * cost;
        p.edge_after_fees_pct += h.dca.edge_after_fees_pct() * cost;
        p.lump_hkd += h.lump_hkd();
        p.in_profit += h.dca.buys_in_profit();
        for b in &h.dca.buys {
            let m = dca::month_index(&b.date);
            first = first.min(m);
            last = last.max(m);
            if !months.contains(&m) {
                months.push(m);
            }
        }
        if let Some(bars) = ledger.bars.get(&h.symbol) {
            let s = h.dca.day_split(bars);
            p.up_days += s.up;
            p.down_days += s.down;
        }
    }
    if p.cost_hkd > 0. {
        p.edge_pct /= p.cost_hkd;
        p.edge_after_fees_pct /= p.cost_hkd;
    }
    p.months_active = months.len();
    p.months_span = if last >= first { (last - first + 1) as usize } else { 0 };
    p
}

// ---------------------------------------------------------- returns split

/// One broker statement day (the bundle's `ibkr.performance.nav`).
#[derive(Debug, Clone, PartialEq)]
pub struct NavDay {
    pub date: String,
    pub nav: f64,
    pub cash: f64,
    /// deposits minus withdrawals that day
    pub flow: f64,
}

pub fn nav_days(b: &Bundle) -> Vec<NavDay> {
    b.arr(&["ibkr", "performance", "nav"]).iter().filter_map(|r| Some(NavDay {
        date: r.get("date")?.as_str()?.to_string(),
        nav: r.get("nav")?.as_f64()?,
        cash: r.get("cash").and_then(Value::as_f64).unwrap_or(0.),
        flow: r.get("flow").and_then(Value::as_f64).unwrap_or(0.),
    })).collect()
}

/// The value on one day, split: value = money_in + market + fx - fees + other.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Split {
    pub date: String,
    pub value: f64,
    /// deposits minus withdrawals so far
    pub money_in: f64,
    /// price moves in each holding's own currency, at the rate of each buy
    pub market: f64,
    /// what the trading currencies did against HKD since each buy
    pub fx: f64,
    /// commissions paid so far (positive)
    pub fees: f64,
    /// what's left: cash interest, conversion spreads, the broker's marks
    /// against the closes used here
    pub other: f64,
}

impl Split {
    pub fn gain(&self) -> f64 {
        self.value - self.money_in
    }
}

/// The broker's daily value split. Holdings are valued at each day's close
/// (the last one on or before it) and that day's exchange rate; sells
/// release cost at the average.
pub fn broker_split(ledger: &Ledger, fx: &Fx, days: &[NavDay]) -> Vec<Split> {
    // per symbol: units, cost in currency (fees aside) and the same at the buys' rates
    struct Lot { qty: f64, cost_ccy: f64, cost_hkd: f64 }
    let mut lots: BTreeMap<&str, Lot> = BTreeMap::new();
    let (mut ti, mut money_in, mut fees, mut realized) = (0usize, 0., 0., 0.);
    let mut out = Vec::with_capacity(days.len());
    for d in days {
        money_in += d.flow;
        while ti < ledger.trades.len() && ledger.trades[ti].date <= d.date {
            let t = &ledger.trades[ti];
            ti += 1;
            fees += t.commission * t.fx_to_base;
            let l = lots.entry(t.symbol.as_str()).or_insert(Lot { qty: 0., cost_ccy: 0., cost_hkd: 0. });
            if t.qty > 0. {
                let px = t.qty * t.price;
                l.qty += t.qty;
                l.cost_ccy += px;
                l.cost_hkd += px * t.fx_to_base;
            } else if t.qty < 0. && l.qty > 0. {
                let frac = (-t.qty).min(l.qty) / l.qty;
                let gross = -t.qty * t.price * t.fx_to_base;
                realized += gross - l.cost_hkd * frac;
                l.qty -= l.qty * frac;
                l.cost_ccy -= l.cost_ccy * frac;
                l.cost_hkd -= l.cost_hkd * frac;
            }
        }
        let (mut market, mut fx_eff) = (realized, 0.);
        for (sym, l) in &lots {
            if l.qty <= 1e-12 {
                continue;
            }
            let Some(bars) = ledger.bars.get(*sym) else { continue };
            let Some(close) = bar_of(bars, &d.date).map(|i| bars[i].close) else { continue };
            let base = crate::base::currency();
            let ccy = ledger.trades.iter().find(|t| t.symbol == *sym).map(|t| t.currency.as_str()).unwrap_or(&base);
            let avg_fx = if l.cost_ccy > 0. { l.cost_hkd / l.cost_ccy } else { 1. };
            let rate = fx.rate(ccy, &d.date).unwrap_or(avg_fx);
            let value_ccy = l.qty * close;
            market += (value_ccy - l.cost_ccy) * avg_fx;
            fx_eff += value_ccy * (rate - avg_fx);
        }
        let other = d.nav - money_in - market - fx_eff + fees;
        out.push(Split { date: d.date.clone(), value: d.nav, money_in, market, fx: fx_eff, fees, other });
    }
    out
}

/// One month of all investments, split the same way. Only the broker has
/// FX and fees here: FlexInvest is an HKD fund (its currency moves are in
/// its price, so in "market"), crypto is priced in USD, which the HKD is
/// pegged to, and the FlexInvest platform fee is paid from the bank, not
/// out of the invested money.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MonthSplit {
    pub month: String,
    pub value: f64,
    pub money_in: f64,
    pub market: f64,
    pub fx: f64,
    pub fees: f64,
}

/// The bundle's monthly investment rows, with the broker's month-end value
/// and split from [`broker_split`] (the bundle holds past IBKR months at cost).
pub fn all_split(b: &Bundle, broker: &[Split]) -> Vec<MonthSplit> {
    b.arr(&["investments", "rows"]).iter().filter_map(|r| {
        let month = r.get("month")?.as_str()?.to_string();
        let mut value = r.get("value")?.as_f64()?;
        let money_in = r.get("invested").and_then(Value::as_f64).unwrap_or(0.);
        // the broker at the month's last statement day
        let end = broker.iter().rev().find(|s| s.date.get(..7) == Some(month.as_str()));
        let (mut fx, mut fees) = (0., 0.);
        if let Some(s) = end {
            let ibkr = r.get("classes").and_then(|c| c.get("IBKR")).and_then(Value::as_f64).unwrap_or(0.);
            value += s.value - ibkr;
            fx = s.fx;
            fees = s.fees;
        } else if let Some(prev) = broker.iter().rev().find(|s| s.date.as_str() < month.as_str()) {
            // a month without statement days after the broker started: carry the split
            fx = prev.fx;
            fees = prev.fees;
        }
        let market = value - money_in - fx + fees;
        Some(MonthSplit { month, value, money_in, market, fx, fees })
    }).collect()
}

// ---------------------------------------------------------------- dividends

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Income {
    pub symbol: String,
    pub currency: String,
    pub payments: usize,
    /// in the payment currency
    pub gross: f64,
    /// withheld (positive)
    pub withheld: f64,
    pub gross_hkd: f64,
    pub withheld_hkd: f64,
    pub last: String,
}

impl Income {
    pub fn net_hkd(&self) -> f64 {
        self.gross_hkd - self.withheld_hkd
    }
    pub fn rate_pct(&self) -> f64 {
        if self.gross > 0. { self.withheld / self.gross * 100. } else { 0. }
    }
}

fn is_dividend(kind: &str) -> bool {
    let k = kind.to_lowercase();
    k.contains("dividend") || k.contains("in lieu")
}

fn is_withholding(kind: &str) -> bool {
    kind.to_lowercase().contains("withholding")
}

/// "QQQ(US46090E1038) Cash Dividend USD 0.71 per Share" -> "QQQ"
fn symbol_of(description: &str) -> String {
    description.split(['(', ' ']).next().unwrap_or("").trim().to_string()
}

/// Dividends and withholding per symbol, from the cash flows (most first).
pub fn income(flows: &[CashFlow]) -> Vec<Income> {
    let mut by: BTreeMap<String, Income> = BTreeMap::new();
    for f in flows.iter().filter(|f| is_dividend(&f.kind) || is_withholding(&f.kind)) {
        let sym = if f.symbol.is_empty() { symbol_of(&f.description) } else { f.symbol.clone() };
        let e = by.entry(sym.clone()).or_insert_with(|| Income { symbol: sym, currency: f.currency.clone(), ..Default::default() });
        if is_withholding(&f.kind) {
            e.withheld += -f.amount;
            e.withheld_hkd += -f.amount_base;
        } else {
            e.payments += 1;
            e.gross += f.amount;
            e.gross_hkd += f.amount_base;
        }
        if f.date > e.last {
            e.last = f.date.clone();
        }
    }
    let mut v: Vec<Income> = by.into_values().collect();
    v.sort_by(|a, b| b.gross_hkd.total_cmp(&a.gross_hkd));
    v
}

/// The kinds of cash flow the replica has, with counts (for the empty state).
pub fn flow_kinds(flows: &[CashFlow]) -> Vec<(String, usize)> {
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    for f in flows {
        *m.entry(f.kind.clone()).or_insert(0) += 1;
    }
    m.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(date: &str, close: f64) -> Bar {
        Bar { date: date.into(), open: close, high: close, low: close, close }
    }
    fn trade(date: &str, sym: &str, ccy: &str, qty: f64, price: f64, comm: f64, fx: f64) -> Trade {
        Trade { date: date.into(), symbol: sym.into(), currency: ccy.into(), qty, price,
                amount: -(qty * price + comm), commission: comm, fx_to_base: fx }
    }

    #[test]
    fn fx_interpolates_and_holds() {
        let mut fx = Fx::default();
        fx.add("GBP", "2026-09-01", 10.);
        fx.add("GBP", "2026-10-01", 11.);
        assert_eq!(fx.rate(&crate::base::currency(), "2026-09-10"), Some(1.));
        assert_eq!(fx.rate("GBP", "2026-08-01"), Some(10.));
        assert_eq!(fx.rate("GBP", "2026-12-01"), Some(11.));
        assert!((fx.rate("GBP", "2026-09-16").unwrap() - 10.5).abs() < 1e-9);
        fx.add("GBP", "2026-09-16", 12.); // an exact point wins
        assert_eq!(fx.rate("GBP", "2026-09-16"), Some(12.));
        assert!(fx.rate("EUR", "2026-09-16").is_none());
    }

    #[test]
    fn the_commission_is_counted_once() {
        // IBKR: amount is the net cash, commission inside
        let t = trade("2026-08-03", "VWRA", "USD", 2., 200., 4., 7.8);
        assert!((buy_cost(&t) - 404.).abs() < 1e-9);
        // a source without the commission in amount
        let mut u = t.clone();
        u.amount = -u.qty * u.price;
        assert!((buy_cost(&u) - 404.).abs() < 1e-9);
    }

    fn ledger() -> Ledger {
        let mut l = Ledger::default();
        // 10 units at 10 GBP (fee 1) at 10 HKD/GBP; price goes to 12, GBP to 11
        l.trades = vec![trade("2026-09-01", "X", "GBP", 10., 10., 1., 10.)];
        l.bars.insert("X".into(), vec![bar("2026-09-01", 10.), bar("2026-09-02", 12.)]);
        l
    }

    #[test]
    fn broker_split_adds_up() {
        let l = ledger();
        let mut fx = Fx::default();
        fx.add("GBP", "2026-09-01", 10.);
        fx.add("GBP", "2026-09-02", 11.);
        let days = vec![
            NavDay { date: "2026-08-31".into(), nav: 1500., cash: 1500., flow: 1500. },
            NavDay { date: "2026-09-01".into(), nav: 1490., cash: 490., flow: 0. },
            // positions 10*12*11 = 1320, cash 490, plus 5 of interest
            NavDay { date: "2026-09-02".into(), nav: 1815., cash: 495., flow: 0. },
        ];
        let s = broker_split(&l, &fx, &days);
        assert_eq!(s[0].market + s[0].fx + s[0].fees + s[0].other, 0.);
        let z = &s[2];
        assert_eq!(z.money_in, 1500.);
        assert!((z.market - 200.).abs() < 1e-9); // (120 - 100) GBP at 10
        assert!((z.fx - 120.).abs() < 1e-9); // 120 GBP * (11 - 10)
        assert!((z.fees - 10.).abs() < 1e-9);
        assert!((z.other - 5.).abs() < 1e-9);
        assert!((z.money_in + z.market + z.fx - z.fees + z.other - z.value).abs() < 1e-9);
    }

    #[test]
    fn plans_and_portfolio() {
        let mut l = ledger();
        l.trades.push(trade("2026-09-02", "X", "GBP", 5., 12., 1., 10.));
        let mut fx = Fx::default();
        fx.add("GBP", "2026-09-02", 11.);
        let plans = holding_plans(&l, &fx, "2026-09-02", &HashMap::new());
        assert_eq!(plans.len(), 1);
        let h = &plans[0];
        assert_eq!(h.fx_now, 11.);
        assert!((h.cost_hkd() - (101. + 61.) * 10.).abs() < 1e-9);
        assert!((h.fees_hkd() - 20.).abs() < 1e-9);
        let p = portfolio(&plans, &l);
        assert_eq!((p.buys, p.months_active, p.months_span), (2, 1, 1));
        assert!((p.value_hkd - 15. * 12. * 11.).abs() < 1e-9);
        assert!((p.edge_pct - h.dca.edge_pct()).abs() < 1e-9);
        assert_eq!((p.up_days, p.down_days), (1, 0)); // the second buy, on the up day
    }

    #[test]
    fn dividends_and_withholding() {
        let f = |date: &str, kind: &str, amount: f64, desc: &str| CashFlow {
            date: date.into(), kind: kind.into(), currency: "USD".into(), amount, amount_base: amount * 7.8, description: desc.into(),
            ..Default::default() };
        let flows = vec![
            f("2026-06-25", "Deposits/Withdrawals", 10000., "CASH RECEIPTS"),
            f("2026-09-30", "Dividends", 1.0, "QQQ(US46090E1038) Cash Dividend USD 0.71 per Share"),
            f("2026-09-30", "Withholding Tax", -0.3, "QQQ(US46090E1038) Cash Dividend USD 0.71 per Share - US Tax"),
        ];
        let inc = income(&flows);
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].symbol, "QQQ");
        assert_eq!(inc[0].payments, 1);
        assert!((inc[0].rate_pct() - 30.).abs() < 1e-9);
        assert!((inc[0].net_hkd() - 0.7 * 7.8).abs() < 1e-9);
        assert!(income(&flows[..1]).is_empty());
        assert_eq!(flow_kinds(&flows).len(), 3);
    }

    #[test]
    fn income_comes_from_the_ledger() {
        let mut ledger = Ledger::default();
        ledger.income = vec![
            CashFlow { date: "2026-09-26".into(), kind: "Payment In Lieu Of Dividends".into(), symbol: "VT".into(), currency: "USD".into(),
                       amount: 2., amount_base: 15.6, description: "something without a ticker".into() },
        ];
        let x = Extra::default().with_ledger(&ledger, &Bundle(serde_json::json!({})));
        let inc = income(&x.cash_flows);
        assert_eq!((inc.len(), inc[0].symbol.as_str(), inc[0].payments), (1, "VT", 1));
        // through the wire format the web build reads
        let back = Ledger::from_json(&ledger.to_json().unwrap()).unwrap();
        assert_eq!(back.income, ledger.income);
        let old: Ledger = Ledger::from_json(br#"{"entries":[],"trades":[],"bars":{},"coins":[]}"#).unwrap();
        assert!(old.income.is_empty(), "a server from before income still parses");
    }
}
