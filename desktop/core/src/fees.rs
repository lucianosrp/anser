//! Broker commissions: what each buy paid, what set the fee (the per-share
//! rate, the per-order minimum, or the 1% cap), the order size that brings
//! the fee under a target share, the yearly projection at the current
//! pattern, and the whole trade history re-priced under IBKR's two plans.
//!
//! **The schedules are approximate.** [`SCHEDULES`] holds the commission
//! components IBKR publishes for its Fixed and Tiered plans, for the venues
//! the owner trades, as compiled on [`SCHEDULES_AS_OF`] from IBKR's public
//! pricing pages. They leave out the small print: monthly volume tiers above
//! the first, exchange rebates and maker/taker fees per venue, regulatory
//! fees on sells (SEC, FINRA TAF), stamp duty (UK ETFs are exempt), and FX
//! conversion costs. Tiered's third-party fees are a rough flat allowance.
//! Check IBKR's current schedule before acting on a difference.
//!
//! Fixed matches this account's history to the cent (US $1.00 minimum,
//! capped at 1% of the trade; London $4 / £3 minimums), which is how the
//! account's plan is identified.

use crate::exposure::{venue_of, Venue};
use crate::ledger::Trade;

/// When [`SCHEDULES`] was compiled.
pub const SCHEDULES_AS_OF: &str = "2026-10-03";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Plan {
    Fixed,
    Tiered,
}

impl Plan {
    pub fn label(self) -> &'static str {
        match self {
            Plan::Fixed => "Fixed",
            Plan::Tiered => "Tiered",
        }
    }
}

/// One plan's fee components for one venue and trading currency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Schedule {
    pub plan: Plan,
    pub venue: Venue,
    /// the trading currency these amounts are in
    pub currency: &'static str,
    /// per share
    pub per_share: f64,
    /// % of the trade value
    pub pct: f64,
    /// per-order minimum
    pub min: f64,
    /// per-order maximum, % of the trade value
    pub max_pct: Option<f64>,
    /// third-party fees on top (exchange, clearing), approximate: per order
    /// and per share; never capped
    pub extra_per_order: f64,
    pub extra_per_share: f64,
    pub note: &'static str,
}

/// IBKR commission components, approximate (see the module docs), first
/// volume tier, as of [`SCHEDULES_AS_OF`].
pub const SCHEDULES: &[Schedule] = &[
    Schedule { plan: Plan::Fixed, venue: Venue::Us, currency: "USD", per_share: 0.005, pct: 0., min: 1.00, max_pct: Some(1.0),
        extra_per_order: 0., extra_per_share: 0.,
        note: "USD 0.005/share, min USD 1.00, max 1% of trade value; exchange and clearing fees included" },
    Schedule { plan: Plan::Tiered, venue: Venue::Us, currency: "USD", per_share: 0.0035, pct: 0., min: 0.35, max_pct: Some(1.0),
        extra_per_order: 0., extra_per_share: 0.0035,
        note: "USD 0.0035/share (up to 300k shares a month), min USD 0.35, max 1%; plus exchange, clearing and pass-through fees (allowed for as ~USD 0.0035/share)" },
    Schedule { plan: Plan::Fixed, venue: Venue::Lse, currency: "GBP", per_share: 0., pct: 0.05, min: 3.00, max_pct: None,
        extra_per_order: 0., extra_per_share: 0.,
        note: "0.05% of trade value, min GBP 3.00; exchange and clearing fees included" },
    Schedule { plan: Plan::Tiered, venue: Venue::Lse, currency: "GBP", per_share: 0., pct: 0.05, min: 1.00, max_pct: None,
        extra_per_order: 0.20, extra_per_share: 0.,
        note: "0.05% of trade value, min GBP 1.00; plus LSE and clearing fees (allowed for as ~GBP 0.20 an order)" },
    Schedule { plan: Plan::Fixed, venue: Venue::Lse, currency: "USD", per_share: 0., pct: 0.05, min: 4.00, max_pct: None,
        extra_per_order: 0., extra_per_share: 0.,
        note: "0.05% of trade value, min USD 4.00 for USD lines in London; exchange and clearing fees included" },
    Schedule { plan: Plan::Tiered, venue: Venue::Lse, currency: "USD", per_share: 0., pct: 0.05, min: 1.25, max_pct: None,
        extra_per_order: 0.25, extra_per_share: 0.,
        note: "0.05% of trade value, min USD 1.25 (approx.); plus LSE and clearing fees (allowed for as ~USD 0.25 an order)" },
];

pub fn schedule(plan: Plan, venue: Venue, currency: &str) -> Option<&'static Schedule> {
    SCHEDULES.iter().find(|s| s.plan == plan && s.venue == venue && s.currency == currency)
}

/// What set a commission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// the per-share / % rate
    Rate,
    /// the per-order minimum
    Minimum,
    /// the % maximum, below the minimum (a tiny order)
    Cap,
}

impl Binding {
    pub fn label(self) -> &'static str {
        match self {
            Binding::Rate => "rate",
            Binding::Minimum => "minimum",
            Binding::Cap => "1% cap",
        }
    }
    /// the rate didn't set it: the order was too small for the schedule
    pub fn is_floor(self) -> bool {
        self != Binding::Rate
    }
}

/// The commission for an order of `qty` at `price` (trading currency).
pub fn commission(s: &Schedule, qty: f64, price: f64) -> (f64, Binding) {
    let value = qty.abs() * price;
    let rate = s.per_share * qty.abs() + s.pct / 100. * value;
    let (mut fee, mut bind) = if rate < s.min { (s.min, Binding::Minimum) } else { (rate, Binding::Rate) };
    if let Some(cap) = s.max_pct.map(|m| m / 100. * value) {
        // a cent of tolerance: a $100 order capped at $1.00 is the minimum
        if fee > cap + 0.005 {
            (fee, bind) = (cap, Binding::Cap);
        } else if fee > cap {
            fee = cap;
        }
    }
    (fee + s.extra_per_order + s.extra_per_share * qty.abs(), bind)
}

/// The smallest order (trading currency) whose fee is at most `target_pct`
/// of it, at `price` per share. None when no order size gets there.
pub fn order_size_for(s: &Schedule, price: f64, target_pct: f64) -> Option<f64> {
    let ok = |v: f64| commission(s, v / price, price).0 / v * 100. <= target_pct;
    let (mut lo, mut hi) = (1.0_f64, 1e8_f64);
    if !ok(hi) {
        return None;
    }
    if ok(lo) {
        return Some(lo);
    }
    for _ in 0..100 {
        let mid = (lo * hi).sqrt();
        if ok(mid) { hi = mid } else { lo = mid }
    }
    Some(hi)
}

/// One trade, with the commission it paid and what each plan would charge.
#[derive(Debug, Clone, PartialEq)]
pub struct Repriced {
    pub date: String,
    pub symbol: String,
    pub currency: String,
    pub venue: Venue,
    pub buy: bool,
    pub qty: f64,
    pub price: f64,
    /// trade value, trading currency
    pub value: f64,
    /// trading currency -> HKD at the trade
    pub fx: f64,
    pub actual: f64,
    /// None when no schedule covers the venue
    pub fixed: Option<(f64, Binding)>,
    pub tiered: Option<(f64, Binding)>,
}

impl Repriced {
    pub fn actual_pct(&self) -> f64 {
        if self.value > 0. { self.actual / self.value * 100. } else { 0. }
    }
    pub fn actual_hkd(&self) -> f64 {
        self.actual * self.fx
    }
    pub fn fixed_hkd(&self) -> Option<f64> {
        self.fixed.map(|f| f.0 * self.fx)
    }
    pub fn tiered_hkd(&self) -> Option<f64> {
        self.tiered.map(|f| f.0 * self.fx)
    }
    /// the minimum (or the cap under it) set the fee, on the Fixed schedule
    pub fn floor_set_it(&self) -> bool {
        self.fixed.is_some_and(|f| f.1.is_floor())
    }
}

pub fn reprice(trades: &[Trade]) -> Vec<Repriced> {
    trades.iter().filter(|t| t.qty != 0.).map(|t| {
        let venue = venue_of(&t.symbol, &t.currency);
        let at = |p: Plan| schedule(p, venue, &t.currency).map(|s| commission(s, t.qty, t.price));
        Repriced {
            date: t.date.clone(),
            symbol: t.symbol.clone(),
            currency: t.currency.clone(),
            venue,
            buy: t.qty > 0.,
            qty: t.qty.abs(),
            price: t.price,
            value: t.qty.abs() * t.price,
            fx: t.fx_to_base,
            actual: t.commission,
            fixed: at(Plan::Fixed),
            tiered: at(Plan::Tiered),
        }
    }).collect()
}

/// Totals over a set of re-priced trades, HKD.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Totals {
    pub trades: usize,
    /// trade value
    pub value_hkd: f64,
    pub actual_hkd: f64,
    pub fixed_hkd: f64,
    pub tiered_hkd: f64,
    /// trades whose fee the Fixed minimum (or the cap under it) set
    pub floor_count: usize,
    /// trades no schedule covers (priced at what they paid on both plans)
    pub unpriced: usize,
}

impl Totals {
    pub fn add(&mut self, r: &Repriced) {
        self.trades += 1;
        self.value_hkd += r.value * r.fx;
        self.actual_hkd += r.actual_hkd();
        self.fixed_hkd += r.fixed_hkd().unwrap_or(r.actual_hkd());
        self.tiered_hkd += r.tiered_hkd().unwrap_or(r.actual_hkd());
        self.floor_count += r.floor_set_it() as usize;
        self.unpriced += r.fixed.is_none() as usize;
    }
    pub fn of<'a>(rows: impl IntoIterator<Item = &'a Repriced>) -> Self {
        let mut t = Totals::default();
        for r in rows {
            t.add(r);
        }
        t
    }
    pub fn actual_pct(&self) -> f64 {
        if self.value_hkd > 0. { self.actual_hkd / self.value_hkd * 100. } else { 0. }
    }
    /// what Tiered would have saved against Fixed (negative = Tiered costs more)
    pub fn tiered_saving_hkd(&self) -> f64 {
        self.fixed_hkd - self.tiered_hkd
    }
}

/// The next 12 months at the recent pattern: the trades of the last
/// `window_days` up to `today`, scaled to a year.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Projection {
    pub window_days: i64,
    pub trades_in_window: usize,
    /// a year of the window's buying: money in, and fees on each plan, HKD
    pub money_yr_hkd: f64,
    pub actual_yr_hkd: f64,
    pub fixed_yr_hkd: f64,
    pub tiered_yr_hkd: f64,
}

impl Projection {
    pub fn actual_pct(&self) -> f64 {
        if self.money_yr_hkd > 0. { self.actual_yr_hkd / self.money_yr_hkd * 100. } else { 0. }
    }
}

pub fn projection(rows: &[Repriced], today: &str, window_days: i64) -> Projection {
    let Ok(end) = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d") else { return Projection::default() };
    let start = (end - chrono::Duration::days(window_days - 1)).format("%Y-%m-%d").to_string();
    let w: Vec<&Repriced> = rows.iter().filter(|r| r.buy && r.date.as_str() >= start.as_str() && r.date.as_str() <= today).collect();
    let t = Totals::of(w.iter().copied());
    let k = 365. / window_days as f64;
    Projection {
        window_days,
        trades_in_window: w.len(),
        money_yr_hkd: t.value_hkd * k,
        actual_yr_hkd: t.actual_hkd * k,
        fixed_yr_hkd: t.fixed_hkd * k,
        tiered_yr_hkd: t.tiered_hkd * k,
    }
}

/// Which plan the account's commissions fit: Fixed when (nearly) every
/// trade a Fixed schedule covers paid exactly the Fixed fee.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanGuess {
    /// "Fixed" or "Tiered-like"
    pub plan: String,
    /// trades a Fixed schedule covers, and those that paid its fee to the cent
    pub priced: usize,
    pub matched: usize,
    /// a year at the last year's buying: what Tiered would save against
    /// Fixed, HKD (negative = Tiered costs more)
    pub tiered_saving_yr_hkd: f64,
}

/// Commissions within this of the Fixed fee count as Fixed (rounding).
const MATCH_TOLERANCE: f64 = 0.011;

pub fn infer_plan(rows: &[Repriced], today: &str) -> Option<PlanGuess> {
    let priced: Vec<&Repriced> = rows.iter().filter(|r| r.fixed.is_some() && r.actual > 0.).collect();
    if priced.is_empty() {
        return None;
    }
    let matched = priced.iter().filter(|r| r.fixed.is_some_and(|(f, _)| (f - r.actual).abs() <= MATCH_TOLERANCE)).count();
    let p = projection(rows, today, 365);
    Some(PlanGuess {
        plan: if matched * 10 >= priced.len() * 9 { "Fixed" } else { "Tiered-like" }.into(),
        priced: priced.len(),
        matched,
        tiered_saving_yr_hkd: p.fixed_yr_hkd - p.tiered_yr_hkd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(sym: &str, ccy: &str, qty: f64, price: f64, comm: f64, fx: f64) -> Trade {
        Trade { date: "2026-08-03".into(), symbol: sym.into(), currency: ccy.into(), qty, price,
                amount: -(qty * price + comm), commission: comm, fx_to_base: fx }
    }

    #[test]
    fn fixed_matches_the_account_history() {
        let us = schedule(Plan::Fixed, Venue::Us, "USD").unwrap();
        // $100 of a stock: the $1 minimum and the 1% cap meet -> $1, "minimum"
        let (f, b) = commission(us, 0.5, 199.99);
        assert!((f - 0.99995).abs() < 1e-4 && b == Binding::Minimum, "{f} {b:?}");
        // $40 of a stock: 1% = $0.40 is under the minimum -> the cap sets it
        let (f, b) = commission(us, 0.25, 159.96);
        assert!((f - 0.3999).abs() < 1e-4 && b == Binding::Cap);
        // $600 of QQQ: still the minimum
        assert_eq!(commission(us, 0.84, 714.24).1, Binding::Minimum);
        // 1000 shares: the per-share rate
        assert_eq!(commission(us, 1000., 50.), (5., Binding::Rate));
        let gbp = schedule(Plan::Fixed, Venue::Lse, "GBP").unwrap();
        assert_eq!(commission(gbp, 1.2, 500.), (3., Binding::Minimum));
        let usd_lse = schedule(Plan::Fixed, Venue::Lse, "USD").unwrap();
        assert_eq!(commission(usd_lse, 2., 210.), (4., Binding::Minimum));
        // 0.05% beats the minimum from 8,000 up
        assert_eq!(commission(usd_lse, 100., 100.), (5., Binding::Rate));
    }

    #[test]
    fn tiered_is_cheaper_on_small_orders() {
        let us = schedule(Plan::Tiered, Venue::Us, "USD").unwrap();
        let (f, b) = commission(us, 0.5, 200.);
        assert!((f - (0.35 + 0.5 * 0.0035)).abs() < 1e-12 && b == Binding::Minimum);
        let gbp = schedule(Plan::Tiered, Venue::Lse, "GBP").unwrap();
        assert!((commission(gbp, 1., 600.).0 - 1.2).abs() < 1e-12);
    }

    #[test]
    fn order_size_for_a_quarter_percent() {
        // Fixed US: $1 / 0.25% = $400
        let us = schedule(Plan::Fixed, Venue::Us, "USD").unwrap();
        assert!((order_size_for(us, 200., 0.25).unwrap() - 400.).abs() < 0.01);
        // Fixed London GBP: £3 / 0.25% = £1,200
        let gbp = schedule(Plan::Fixed, Venue::Lse, "GBP").unwrap();
        assert!((order_size_for(gbp, 500., 0.25).unwrap() - 1200.).abs() < 0.01);
        // never under 0.04% when the rate alone is 0.05%
        assert!(order_size_for(gbp, 500., 0.04).is_none());
    }

    #[test]
    fn reprice_totals_and_projection() {
        let rows = reprice(&[
            trade("VWRA", "USD", 2., 210., 4., 7.8),
            trade("ACME", "USD", 0.5, 199.99, 1., 7.8),
            trade("XYZ", "EUR", 1., 100., 2., 8.8), // no schedule
        ]);
        assert_eq!(rows[0].venue, Venue::Lse);
        assert!(rows[0].floor_set_it() && rows[1].floor_set_it());
        let t = Totals::of(&rows);
        assert_eq!((t.trades, t.floor_count, t.unpriced), (3, 2, 1));
        assert!((t.actual_hkd - (4. * 7.8 + 7.8 + 17.6)).abs() < 1e-9);
        // Tiered: 1.25 + 0.25 + 0.05% of 420 (0.21) on VWRA; ~0.35 on the stock
        assert!(t.tiered_saving_hkd() > 0.);
        let p = projection(&rows, "2026-08-31", 30);
        assert_eq!(p.trades_in_window, 3);
        assert!((p.actual_yr_hkd - t.actual_hkd * 365. / 30.).abs() < 1e-9);
        assert_eq!(projection(&rows, "2026-10-31", 30).trades_in_window, 0);
    }

    #[test]
    fn infers_the_plan_from_commissions() {
        let us = schedule(Plan::Fixed, Venue::Us, "USD").unwrap();
        let fixed_fee = commission(us, 10., 50.).0;
        let rows = reprice(&[trade("VT", "USD", 10., 50., fixed_fee, 7.8), trade("VT", "USD", 10., 50., fixed_fee, 7.8)]);
        let g = infer_plan(&rows, "2026-09-01").unwrap();
        assert_eq!((g.plan.as_str(), g.priced, g.matched), ("Fixed", 2, 2));
        let rows = reprice(&[trade("VT", "USD", 10., 50., fixed_fee + 0.4, 7.8)]);
        assert_eq!(infer_plan(&rows, "2026-09-01").unwrap().plan, "Tiered-like");
        assert!(infer_plan(&[], "2026-09-01").is_none());
    }
}
