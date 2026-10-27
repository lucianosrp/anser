//! How a dollar-cost averaging plan is doing: any asset's buys against its
//! own price bars. Everything is in the asset's quote currency (USD for BTC
//! and the US listings, GBP for EQQQ, ...); converting to HKD is the
//! caller's business (see `invest`).
//!
//! * **Buys** are purchases at a cost (fees included) for a quantity.
//!   **Sells** take quantity out for proceeds; **rewards** add free units.
//! * **The edge**: the average price paid (fees aside) against the average
//!   close over the plan's bars, first buy to last. Buying a fixed amount buys
//!   more units when the price is low, so a steady plan pays less than the
//!   average price (the harmonic vs arithmetic mean); positive = it worked.
//!   `edge_after_fees_pct` uses the price with fees, so it shows what the
//!   commissions took back.
//! * **Against a lump sum**: the same total invested in one go at the first
//!   buy's bar, valued today.
//! * Bars can be any period: BTC uses weekly candles (`BTC-1W`), the broker
//!   holdings daily bars. One plan "point" per bar from the first buy on.
//!
//! [`analyze`] is the BTC plan from the crypto ledger: the buys are the
//! purchases themselves (recurring auto-invest, spot and fiat buys, converts
//! into BTC, the old Revolut ledger), deposits and wallet moves only shift
//! coins between the owner's pockets so they are not part of it. **Scope**:
//! the current plan (from the first recurring buy: Binance auto-invest, then
//! OKX) or every BTC buy ever. Mixing eras makes the lump-sum comparison
//! meaningless (all of 2026's money "invested" in 2020), so the plan is the
//! default. [`plan`] is the generic analysis the broker holdings use too.

use std::collections::BTreeMap;

use crate::ledger::{Bar, Ledger};

/// The price cache's symbol for BTC's weekly candles (Binance, USDT ~ USD).
pub const BTC_WEEKLY: &str = "BTC-1W";

const BUY_KINDS: [&str; 5] = ["auto-invest", "spot-buy", "fiat", "convert-in", "revolut-btc"];
const SELL_KINDS: [&str; 3] = ["spot-sell", "convert-out", "revolut-btc"];
/// Recurring buys: the first one starts "the plan".
const PLAN_KINDS: [&str; 1] = ["auto-invest"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// from the first recurring buy on
    #[default]
    Plan,
    /// every BTC buy in the ledger
    All,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Plan => "Current plan",
            Scope::All => "All BTC buys",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Buy {
    pub date: String,
    pub venue: String,
    pub kind: String,
    /// units bought
    pub qty: f64,
    /// what the buy cost, fees included (quote currency)
    pub cost: f64,
    /// of which fees (quote currency)
    pub fee: f64,
    /// quote currency -> HKD at the trade (1.0 when unknown / not needed)
    pub fx: f64,
}

impl Buy {
    /// price per unit, fees included
    pub fn price(&self) -> f64 {
        self.cost / self.qty
    }

    /// price per unit the market charged (fees aside)
    pub fn paid_price(&self) -> f64 {
        (self.cost - self.fee) / self.qty
    }

    /// fees as % of the money put in
    pub fn fee_pct(&self) -> f64 {
        if self.cost > 0. { self.fee / self.cost * 100. } else { 0. }
    }
}

/// One bar of the plan (from the first buy's bar on).
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    /// the bar's date
    pub date: String,
    pub close: f64,
    /// money put in so far, net of sells
    pub invested: f64,
    /// units held from the plan (bought - sold + rewards)
    pub qty: f64,
    /// average price paid so far (fees included)
    pub avg_cost: f64,
}

impl Point {
    pub fn value(&self) -> f64 {
        self.qty * self.close
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Month {
    /// "YYYY-MM"
    pub month: String,
    pub cost: f64,
    pub qty: f64,
    pub buys: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dca {
    /// every buy, oldest first
    pub buys: Vec<Buy>,
    pub points: Vec<Point>,
    pub months: Vec<Month>,
    /// money put into buys (fees included) and the units they got
    pub bought: f64,
    pub bought_qty: f64,
    /// fees inside `bought`
    pub fees: f64,
    pub sold: f64,
    pub sold_qty: f64,
    pub rewards_qty: f64,
    /// the price everything is valued at now
    pub price: f64,
    /// mean close over the plan's bars (first to last buy)
    pub mean_close: f64,
    /// bars in that mean
    pub span_bars: usize,
    /// what the same money would hold now, all bought at the first buy's bar
    pub lump_qty: f64,
}

/// Up / down day of a buy: the bar's close against the previous bar's.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DaySplit {
    pub up: usize,
    pub down: usize,
    /// average return today of the buys in each group, %
    pub up_return: f64,
    pub down_return: f64,
    /// average move of the day, %
    pub up_move: f64,
    pub down_move: f64,
}

impl Dca {
    /// average price per unit, fees included
    pub fn avg_cost(&self) -> f64 {
        if self.bought_qty > 0. { self.bought / self.bought_qty } else { 0. }
    }

    /// average price per unit the market charged (fees aside)
    pub fn avg_price(&self) -> f64 {
        if self.bought_qty > 0. { (self.bought - self.fees) / self.bought_qty } else { 0. }
    }

    pub fn position_qty(&self) -> f64 {
        self.bought_qty - self.sold_qty + self.rewards_qty
    }

    pub fn value(&self) -> f64 {
        self.position_qty() * self.price
    }

    /// value now + what sells returned - what was paid
    pub fn gain(&self) -> f64 {
        self.value() + self.sold - self.bought
    }

    pub fn gain_pct(&self) -> f64 {
        if self.bought > 0. { self.gain() / self.bought * 100. } else { 0. }
    }

    /// % below the mean close the plan bought at, fees aside (positive = good)
    pub fn edge_pct(&self) -> f64 {
        if self.mean_close > 0. { (1. - self.avg_price() / self.mean_close) * 100. } else { 0. }
    }

    /// the same with the fees in the price
    pub fn edge_after_fees_pct(&self) -> f64 {
        if self.mean_close > 0. { (1. - self.avg_cost() / self.mean_close) * 100. } else { 0. }
    }

    /// fees as % of the money put into buys
    pub fn fee_drag_pct(&self) -> f64 {
        if self.bought > 0. { self.fees / self.bought * 100. } else { 0. }
    }

    pub fn lump_value(&self) -> f64 {
        self.lump_qty * self.price
    }

    /// the plan's value (rewards excluded, sells included) against the lump
    /// sum's, in % (positive = DCA did better)
    pub fn vs_lump_pct(&self) -> f64 {
        let lump = self.lump_value();
        let dca = (self.bought_qty - self.sold_qty) * self.price + self.sold;
        if lump > 0. { (dca / lump - 1.) * 100. } else { 0. }
    }

    /// each buy's return at today's price, fees included, %
    pub fn buy_return_pct(&self, b: &Buy) -> f64 {
        (self.price / b.price() - 1.) * 100.
    }

    pub fn buys_in_profit(&self) -> usize {
        self.buys.iter().filter(|b| b.price() < self.price).count()
    }

    /// share of the plan's bars the position was worth at least what it cost
    pub fn points_in_profit_pct(&self) -> f64 {
        let n = self.points.iter().filter(|w| w.invested > 0.).count();
        if n == 0 {
            return 0.;
        }
        self.points.iter().filter(|w| w.invested > 0. && w.value() >= w.invested).count() as f64 / n as f64 * 100.
    }

    /// the deepest the position ever was under its cost, in % (<= 0)
    pub fn worst_drawdown_pct(&self) -> f64 {
        self.points.iter()
            .filter(|w| w.invested > 0.)
            .map(|w| (w.value() / w.invested - 1.) * 100.)
            .fold(0., f64::min)
    }

    /// months with at least one buy, out of the months from first to last
    pub fn months_active(&self) -> (usize, usize) {
        let span = match (self.buys.first(), self.buys.last()) {
            (Some(a), Some(b)) => month_index(&b.date) - month_index(&a.date) + 1,
            _ => 0,
        };
        (self.months.len(), span.max(0) as usize)
    }

    /// Buys on days the price closed up vs down (against the previous bar).
    /// A buy on a non-trading day counts with the bar before it.
    pub fn day_split(&self, bars: &[Bar]) -> DaySplit {
        let mut s = DaySplit::default();
        let (mut ur, mut dr, mut um, mut dm) = (0., 0., 0., 0.);
        for b in &self.buys {
            let Some(i) = bar_of(bars, &b.date) else { continue };
            let Some(prev) = i.checked_sub(1).map(|j| bars[j].close).filter(|p| *p > 0.) else { continue };
            let mv = (bars[i].close / prev - 1.) * 100.;
            let ret = self.buy_return_pct(b);
            if mv < 0. {
                s.down += 1;
                dr += ret;
                dm += mv;
            } else {
                s.up += 1;
                ur += ret;
                um += mv;
            }
        }
        if s.up > 0 {
            s.up_return = ur / s.up as f64;
            s.up_move = um / s.up as f64;
        }
        if s.down > 0 {
            s.down_return = dr / s.down as f64;
            s.down_move = dm / s.down as f64;
        }
        s
    }
}

pub fn month_index(date: &str) -> i64 {
    let y: i64 = date.get(0..4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let m: i64 = date.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    y * 12 + m - 1
}

/// The bar a date falls in: the last one starting on or before it.
pub fn bar_of(bars: &[Bar], date: &str) -> Option<usize> {
    bars.partition_point(|b| b.date.as_str() <= date).checked_sub(1)
}

/// A sale out of the plan: (date, units, proceeds).
pub type Sell = (String, f64, f64);
/// Free units: (date, units).
pub type Reward = (String, f64);

/// The generic plan: buys (any order), sells, rewards and the asset's bars,
/// oldest first. `price_now` wins over the last close. None without buys or
/// bars.
pub fn plan(mut buys: Vec<Buy>, sells: &[Sell], rewards: &[Reward], bars: &[Bar], price_now: Option<f64>) -> Option<Dca> {
    buys.retain(|b| b.qty > 0. && b.cost > 0.);
    if buys.is_empty() || bars.is_empty() {
        return None;
    }
    buys.sort_by(|a, b| a.date.cmp(&b.date));
    let bought: f64 = buys.iter().map(|b| b.cost).sum();
    let bought_qty: f64 = buys.iter().map(|b| b.qty).sum();
    let fees: f64 = buys.iter().map(|b| b.fee).sum();
    let sold: f64 = sells.iter().map(|s| s.2).sum();
    let sold_qty: f64 = sells.iter().map(|s| s.1).sum();
    let rewards_qty: f64 = rewards.iter().map(|r| r.1).sum();
    let price = price_now.filter(|p| *p > 0.).unwrap_or(bars[bars.len() - 1].close);

    // (date, money delta, units delta, is a buy) of everything that moves the plan
    let mut moves: Vec<(&str, f64, f64, bool)> = buys.iter().map(|b| (b.date.as_str(), b.cost, b.qty, true))
        .chain(sells.iter().map(|s| (s.0.as_str(), -s.2, -s.1, false)))
        .chain(rewards.iter().map(|r| (r.0.as_str(), 0., r.1, false)))
        .collect();
    moves.sort_by(|a, b| a.0.cmp(b.0));

    // the walk, from the first buy's bar to the last bar (a buy before the
    // first bar counts in the first one)
    let start = bar_of(bars, &buys[0].date).unwrap_or(0);
    let mut points = Vec::with_capacity(bars.len() - start);
    let (mut invested, mut qty, mut paid, mut got) = (0., 0., 0., 0.);
    let mut mi = 0;
    for (i, bar) in bars.iter().enumerate().skip(start) {
        let next = bars.get(i + 1).map(|b| b.date.as_str());
        while mi < moves.len() && next.is_none_or(|n| moves[mi].0 < n) {
            let (_, money, units, is_buy) = moves[mi];
            invested += money;
            qty += units;
            if is_buy {
                paid += money;
                got += units;
            }
            mi += 1;
        }
        points.push(Point {
            date: bar.date.clone(),
            close: bar.close,
            invested,
            qty,
            avg_cost: if got > 0. { paid / got } else { 0. },
        });
    }

    // mean close over the plan's own bars
    let last = bar_of(bars, &buys[buys.len() - 1].date).unwrap_or(bars.len() - 1);
    let span = &bars[start..=last.max(start)];
    let mean_close = span.iter().map(|b| b.close).sum::<f64>() / span.len() as f64;
    let lump_qty = bought / bars[start].close;

    let mut by_month: BTreeMap<String, Month> = BTreeMap::new();
    for b in &buys {
        let m = by_month.entry(b.date[..7].to_string()).or_insert_with(|| Month { month: b.date[..7].to_string(), cost: 0., qty: 0., buys: 0 });
        m.cost += b.cost;
        m.qty += b.qty;
        m.buys += 1;
    }

    Some(Dca {
        buys,
        points,
        months: by_month.into_values().collect(),
        bought,
        bought_qty,
        fees,
        sold,
        sold_qty,
        rewards_qty,
        price,
        mean_close,
        span_bars: span.len(),
        lump_qty,
    })
}

/// The BTC plan, in USD, on the weekly candles; None when there are no BTC
/// buys or no candles. `price_now` (e.g. the live price from the bundle)
/// wins over the last close.
pub fn analyze(ledger: &Ledger, price_now: Option<f64>, scope: Scope) -> Option<Dca> {
    let bars = ledger.bars.get(BTC_WEEKLY).filter(|b| !b.is_empty())?;
    let btc = || ledger.coins.iter().filter(|c| c.asset == "BTC");
    let since = match scope {
        Scope::All => String::new(),
        Scope::Plan => btc().find(|c| c.delta > 0. && PLAN_KINDS.contains(&c.kind.as_str())).map(|c| c.date.clone()).unwrap_or_default(),
    };
    let (mut buys, mut sells, mut rewards) = (Vec::new(), Vec::new(), Vec::new());
    for c in btc().filter(|c| c.date >= since) {
        let usd = c.usd.unwrap_or(0.).abs();
        if c.delta > 0. && usd > 0. && BUY_KINDS.contains(&c.kind.as_str()) {
            buys.push(Buy { date: c.date.clone(), venue: c.exchange.clone(), kind: c.kind.clone(), qty: c.delta, cost: usd, fee: 0., fx: 1. });
        } else if c.delta < 0. && SELL_KINDS.contains(&c.kind.as_str()) {
            sells.push((c.date.clone(), -c.delta, usd));
        } else if c.delta > 0. && c.kind == "reward" {
            rewards.push((c.date.clone(), c.delta));
        }
    }
    plan(buys, &sells, &rewards, bars, price_now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::CoinEvent;

    fn bar(date: &str, close: f64) -> Bar {
        Bar { date: date.into(), open: close, high: close, low: close, close }
    }
    fn coin(date: &str, kind: &str, delta: f64, usd: Option<f64>) -> CoinEvent {
        CoinEvent { date: date.into(), exchange: "binance".into(), asset: "BTC".into(), delta, usd, kind: kind.into() }
    }

    /// $100 a week into a price that halves and recovers: 100, 50, 100.
    fn ledger() -> Ledger {
        let mut l = Ledger::default();
        l.bars.insert(BTC_WEEKLY.into(), vec![bar("2026-01-05", 100.), bar("2026-01-12", 50.), bar("2026-01-19", 100.)]);
        l.coins = vec![
            coin("2026-01-06", "auto-invest", 1.0, Some(100.)),
            coin("2026-01-13", "auto-invest", 2.0, Some(100.)),
            coin("2026-01-14", "reward", 0.5, Some(0.)),
            coin("2026-01-15", "withdraw", -1.0, None), // a wallet move, not the plan
            coin("2026-01-20", "auto-invest", 1.0, Some(100.)),
        ];
        l
    }

    #[test]
    fn dca_beats_the_average_price() {
        let d = analyze(&ledger(), None, Scope::All).unwrap();
        assert_eq!(d.buys.len(), 3);
        assert_eq!(d.avg_cost(), 75.); // $300 for 4 coins
        assert!((d.mean_close - 250. / 3.).abs() < 1e-9);
        assert!((d.edge_pct() - 10.).abs() < 1e-9, "paid 75 vs a mean of 83.33: {}", d.edge_pct());
        assert_eq!(d.edge_pct(), d.edge_after_fees_pct()); // no fees on these
        // lump sum at 100 buys 3 coins; the plan got 4
        assert_eq!(d.lump_qty, 3.);
        assert!((d.vs_lump_pct() - (4. / 3. - 1.) * 100.).abs() < 1e-9);
        assert_eq!(d.position_qty(), 4.5);
        assert_eq!(d.gain(), 450. - 300.);
        assert_eq!(d.buys_in_profit(), 1); // the 100s are break-even, only the 50 is up
    }

    #[test]
    fn weekly_walk() {
        let d = analyze(&ledger(), None, Scope::All).unwrap();
        let w: Vec<(f64, f64, f64)> = d.points.iter().map(|w| (w.invested, w.qty, w.avg_cost)).collect();
        assert_eq!(w, vec![(100., 1., 100.), (200., 3.5, 200. / 3.), (300., 4.5, 75.)]);
        assert!((d.worst_drawdown_pct() - (175. / 200. - 1.) * 100.).abs() < 1e-9);
        assert_eq!(d.months_active(), (1, 1));
    }

    #[test]
    fn live_price_wins_and_sells_count() {
        let mut l = ledger();
        l.coins.push(coin("2026-01-21", "spot-sell", -1.0, Some(120.)));
        let d = analyze(&l, Some(200.), Scope::All).unwrap();
        assert_eq!(d.price, 200.);
        assert_eq!(d.position_qty(), 3.5);
        assert_eq!(d.gain(), 3.5 * 200. + 120. - 300.);
    }

    #[test]
    fn nothing_without_buys_or_candles() {
        assert!(analyze(&Ledger::default(), None, Scope::All).is_none());
        let mut l = ledger();
        l.coins.retain(|c| c.kind != "auto-invest");
        assert!(analyze(&l, None, Scope::All).is_none());
    }

    #[test]
    fn the_plan_starts_at_the_first_recurring_buy() {
        let mut l = ledger();
        l.bars.get_mut(BTC_WEEKLY).unwrap().insert(0, bar("2025-06-02", 20.));
        l.coins.insert(0, coin("2025-06-03", "revolut-btc", 5.0, Some(100.)));
        assert_eq!(analyze(&l, None, Scope::All).unwrap().buys.len(), 4);
        let plan = analyze(&l, None, Scope::Plan).unwrap();
        assert_eq!(plan.buys.len(), 3);
        assert_eq!(plan.points[0].date, "2026-01-05");
    }

    /// Daily bars, fees: the edge is on the market price, the fees show
    /// separately, and a buy over a weekend lands on Friday's bar.
    #[test]
    fn daily_bars_with_fees() {
        let bars = vec![bar("2026-03-02", 10.), bar("2026-03-03", 8.), bar("2026-03-04", 12.), bar("2026-03-05", 10.)];
        let buy = |date: &str, qty: f64, px: f64, fee: f64| Buy { date: date.into(), qty, cost: qty * px + fee, fee, fx: 7.8, ..Default::default() };
        // 10 on day 1 at 10 (+1 fee), 10 on day 2 at 8 (+1 fee)
        let d = plan(vec![buy("2026-03-03", 10., 8., 1.), buy("2026-03-02", 10., 10., 1.)], &[], &[], &bars, None).unwrap();
        assert_eq!(d.buys[0].date, "2026-03-02"); // sorted
        assert_eq!(d.span_bars, 2);
        assert!((d.mean_close - 9.).abs() < 1e-12);
        assert!((d.avg_price() - 9.).abs() < 1e-12);
        assert!(d.edge_pct().abs() < 1e-9); // same quantity each time: no averaging gain
        assert!((d.avg_cost() - 9.1).abs() < 1e-12);
        assert!((d.edge_after_fees_pct() - (1. - 9.1 / 9.) * 100.).abs() < 1e-9);
        assert!((d.fee_drag_pct() - 2. / 182. * 100.).abs() < 1e-9);
        assert_eq!(d.price, 10.);
        assert_eq!(d.points.len(), 4);
        // day 2 closed down (8 < 10): one down-day buy; day 1 has no bar before
        let s = d.day_split(&bars);
        assert_eq!((s.up, s.down), (0, 1));
        assert!((s.down_move + 20.).abs() < 1e-9);
        assert!((s.down_return - (10. / 8.1 - 1.) * 100.).abs() < 1e-9);
        // a buy on Saturday counts with Thursday's (the last) bar
        assert_eq!(bar_of(&bars, "2026-03-07"), Some(3));
    }
}
