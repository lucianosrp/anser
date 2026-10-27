//! The Investments page's Buying habits: the ideas of Nick Maggiulli's
//! *Just Keep Buying* as simulations on the user's own data. Pure maths, no bundle: the Investments page feeds it buys, bars
//! and amounts. Facts and what-ifs, never advice.
//!
//! * [`grow`]: the invested pool month by month at an annual rate, the same
//!   compounding the Overview projection uses (it calls this too).
//! * [`crossover`]: when the expected monthly market gain overtakes the
//!   monthly contributions, at the current pace.
//! * [`streak`]: months in a row with a buy, the longest run, the gaps.
//! * [`buy_months`]: what buying a fixed amount in given months would hold
//!   (the cost of a pause, on the actual prices).
//! * [`dip_vs_dca`]: the book's "even God can't beat DCA" test. The same
//!   money, arriving on the same dates, either buys at once (DCA) or waits
//!   in cash for the lowest close between two all-time highs (a perfect dip
//!   buyer who knows the future). Both at the bars' closes, fees aside.
//! * [`period_pnl`] and [`best_share`]: how much of a position's market
//!   result came from its few best days or weeks.

use crate::dca::{bar_of, month_index};
use crate::ledger::Bar;

/// The monthly rate of an annual one, compounded monthly.
pub fn monthly_rate(annual_pct: f64) -> f64 {
    (1. + annual_pct / 100.).powf(1. / 12.) - 1.
}

/// The pool after each month (index 0 = today): it grows at the annual
/// rate, then the month's addition goes in.
pub fn grow(start: f64, monthly_add: f64, annual_pct: f64, months: usize) -> Vec<f64> {
    let r = monthly_rate(annual_pct);
    let mut p = start;
    let mut out = Vec::with_capacity(months + 1);
    out.push(p);
    for _ in 0..months {
        p = p * (1. + r) + monthly_add;
        out.push(p);
    }
    out
}

/// "YYYY-MM" of a month index (`dca::month_index`).
pub fn ym(i: i64) -> String {
    format!("{:04}-{:02}", i.div_euclid(12), i.rem_euclid(12) + 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Crossover {
    /// expected market gain this month at the rate: value x monthly rate
    pub gain_now: f64,
    /// gain_now / monthly contributions (1.0 = they're even)
    pub ratio: f64,
    /// months from now until the expected gain reaches the contributions,
    /// at the same pace (None past the horizon, or with no contributions)
    pub months: Option<usize>,
    /// the pool's value then
    pub value_then: f64,
}

/// Where savings stop being the bigger engine: the first month the pool's
/// expected monthly gain is at least the monthly contribution.
pub fn crossover(value: f64, monthly_in: f64, annual_pct: f64, horizon: usize) -> Crossover {
    let r = monthly_rate(annual_pct);
    let gain_now = value * r;
    let ratio = if monthly_in > 0. { gain_now / monthly_in } else { f64::INFINITY };
    if monthly_in <= 0. || r <= 0. {
        return Crossover { gain_now, ratio, months: (monthly_in <= 0.).then_some(0), value_then: value };
    }
    let path = grow(value, monthly_in, annual_pct, horizon);
    let m = path.iter().position(|v| v * r >= monthly_in);
    Crossover { gain_now, ratio, months: m, value_then: m.map(|m| path[m]).unwrap_or(f64::NAN) }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Streak {
    /// months with a buy, and the months from the first buy to `through`
    pub active: usize,
    pub span: usize,
    /// months in a row with a buy, ending at `through` (or the month before
    /// it, while `through` is still running without a buy yet)
    pub current: usize,
    pub longest: usize,
    /// the longest run without a buy: first and last missed month, length
    pub longest_gap: Option<(String, String, usize)>,
    /// every month without a buy since the first one
    pub missed: Vec<String>,
}

/// Streaks of monthly buying. `months` are "YYYY-MM" with a buy (any order,
/// duplicates fine); `through` is the current month.
pub fn streak(months: &[String], through: &str) -> Streak {
    let mut idx: Vec<i64> = months.iter().map(|m| month_index(m)).collect();
    idx.sort_unstable();
    idx.dedup();
    let Some(&first) = idx.first() else { return Streak::default() };
    let end = month_index(through).max(*idx.last().unwrap());
    let has = |m: i64| idx.binary_search(&m).is_ok();
    let mut s = Streak { active: idx.len(), span: (end - first + 1) as usize, ..Default::default() };
    let (mut run, mut gap, mut gap_start) = (0usize, 0usize, 0i64);
    for m in first..=end {
        if has(m) {
            run += 1;
            s.longest = s.longest.max(run);
            gap = 0;
        } else {
            // the running month doesn't count as missed yet
            if m == end {
                break;
            }
            run = 0;
            if gap == 0 {
                gap_start = m;
            }
            gap += 1;
            s.missed.push(ym(m));
            if s.longest_gap.as_ref().is_none_or(|g| gap > g.2) {
                s.longest_gap = Some((ym(gap_start), ym(m), gap));
            }
        }
    }
    let mut m = if has(end) { end } else { end - 1 };
    while m >= first && has(m) {
        s.current += 1;
        m -= 1;
    }
    s
}

/// Buying `amount` at the first bar of each month in `months`: (money, units).
/// Months before the first bar or after the last are skipped.
pub fn buy_months(months: &[String], amount: f64, bars: &[Bar]) -> (f64, f64) {
    let (mut money, mut units) = (0., 0.);
    for m in months {
        let i = bars.partition_point(|b| b.date.as_str() < m.as_str());
        if let Some(b) = bars.get(i).filter(|b| b.date.starts_with(m.as_str()) && b.close > 0.) {
            money += amount;
            units += amount / b.close;
        }
    }
    (money, units)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DipVsDca {
    pub money_in: f64,
    pub price: f64,
    /// buying each contribution at its bar's close
    pub dca_units: f64,
    /// the dip buyer's units and the cash still waiting for a dip
    pub dip_units: f64,
    pub dip_cash: f64,
    /// dips the dip buyer bought at
    pub dips: Vec<(String, f64)>,
}

impl DipVsDca {
    pub fn dca_value(&self) -> f64 {
        self.dca_units * self.price
    }
    pub fn dip_value(&self) -> f64 {
        self.dip_units * self.price + self.dip_cash
    }
    /// how far DCA is ahead of the dip buyer, % of the dip buyer's value
    pub fn dca_ahead_pct(&self) -> f64 {
        let d = self.dip_value();
        if d > 0. { (self.dca_value() / d - 1.) * 100. } else { 0. }
    }
}

/// The bars that are the lowest close between two all-time highs (over the
/// whole bar history, not just the buys').
pub fn dips(bars: &[Bar]) -> Vec<usize> {
    let mut highs = Vec::new();
    let mut max = f64::MIN;
    for (i, b) in bars.iter().enumerate() {
        if b.close > max {
            max = b.close;
            highs.push(i);
        }
    }
    highs.windows(2).filter(|w| w[1] > w[0] + 1).filter_map(|w| {
        (w[0] + 1..w[1]).min_by(|a, b| bars[*a].close.total_cmp(&bars[*b].close))
    }).collect()
}

/// DCA against the perfect dip buyer, for contributions `(date, money)` in
/// the bars' currency. `price_now` wins over the last close.
pub fn dip_vs_dca(contribs: &[(String, f64)], bars: &[Bar], price_now: Option<f64>) -> Option<DipVsDca> {
    let last = bars.last()?;
    let price = price_now.filter(|p| *p > 0.).unwrap_or(last.close);
    let dip_bars = dips(bars);
    let mut r = DipVsDca { price, ..Default::default() };
    let mut waiting: Vec<(usize, f64)> = Vec::new();
    for (date, money) in contribs {
        if *money <= 0. {
            continue;
        }
        let t = bar_of(bars, date).unwrap_or(0);
        if bars[t].close <= 0. {
            continue;
        }
        r.money_in += money;
        r.dca_units += money / bars[t].close;
        waiting.push((t, *money));
    }
    if r.money_in <= 0. {
        return None;
    }
    // each contribution waits for the first dip at or after its bar
    let mut spent = vec![0.; dip_bars.len()];
    for (t, money) in waiting {
        match dip_bars.iter().position(|d| *d >= t) {
            Some(k) => spent[k] += money,
            None => r.dip_cash += money,
        }
    }
    for (k, money) in spent.into_iter().enumerate().filter(|(_, m)| *m > 0.) {
        let b = &bars[dip_bars[k]];
        r.dip_units += money / b.close;
        r.dips.push((b.date.clone(), money));
    }
    Some(r)
}

/// A position's market result per bar: the units held going into the bar
/// times the bar's move. `points` are (date, units held after that bar's
/// moves, close), oldest first; the first point has no move. `price_now`
/// adds a last move from the last close to it.
pub fn period_pnl(points: &[(String, f64, f64)], price_now: Option<f64>) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = points.windows(2).map(|w| (w[1].0.clone(), w[0].1 * (w[1].2 - w[0].2))).collect();
    if let (Some(p), Some(l)) = (price_now, points.last()) {
        if p > 0. && (p - l.2).abs() > 1e-12 {
            out.push(("now".into(), l.1 * (p - l.2)));
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BestShare {
    pub periods: usize,
    /// the whole market result
    pub total: f64,
    /// the best `n` periods' result together
    pub best: f64,
    pub n: usize,
}

impl BestShare {
    /// the result without the best periods
    pub fn without(&self) -> f64 {
        self.total - self.best
    }
}

pub fn best_share(pnl: &[f64], n: usize) -> BestShare {
    let mut v: Vec<f64> = pnl.to_vec();
    v.sort_by(|a, b| b.total_cmp(a));
    let n = n.min(v.len());
    BestShare { periods: pnl.len(), total: pnl.iter().sum(), best: v[..n].iter().sum(), n }
}

/// The median of some amounts (0 for none).
pub fn median(v: &[f64]) -> f64 {
    let mut v: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return 0.;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2. }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes.iter().enumerate().map(|(i, c)| Bar {
            date: format!("2026-{:02}-01", i + 1), open: *c, high: *c, low: *c, close: *c,
        }).collect()
    }

    #[test]
    fn grow_matches_the_projection_formula() {
        let p = grow(1000., 100., 12., 2);
        let r = monthly_rate(12.);
        assert!((p[1] - (1000. * (1. + r) + 100.)).abs() < 1e-9);
        assert!((p[2] - (p[1] * (1. + r) + 100.)).abs() < 1e-9);
        assert!((monthly_rate(12.) - 0.009488792934583).abs() < 1e-12);
    }

    #[test]
    fn crossover_month() {
        // 6%/yr ~ 0.4868%/mo: 100k makes ~487 a month; 1,000 a month in
        let c = crossover(100_000., 1_000., 6., 600);
        assert!(c.ratio < 1. && c.ratio > 0.48);
        let m = c.months.unwrap();
        let path = grow(100_000., 1_000., 6., m);
        assert!(path[m] * monthly_rate(6.) >= 1_000.);
        assert!(path[m - 1] * monthly_rate(6.) < 1_000.);
        // already past it
        assert_eq!(crossover(1e7, 1_000., 6., 10).months, Some(0));
        // never, within the horizon
        assert_eq!(crossover(0., 1_000., 6., 12).months, None);
    }

    #[test]
    fn streaks_and_gaps() {
        let m = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let s = streak(&m(&["2025-01", "2025-02", "2025-05", "2025-06", "2025-07"]), "2025-08");
        assert_eq!((s.active, s.span, s.longest), (5, 8, 3));
        // August is running without a buy: the streak still counts to July
        assert_eq!(s.current, 3);
        assert_eq!(s.longest_gap, Some(("2025-03".into(), "2025-04".into(), 2)));
        assert_eq!(s.missed, vec!["2025-03", "2025-04"]);
        // a missed past month breaks it
        let s = streak(&m(&["2025-01", "2025-02"]), "2025-04");
        assert_eq!(s.current, 0);
        assert_eq!(s.missed, vec!["2025-03"]);
        assert_eq!(streak(&[], "2025-01"), Streak::default());
    }

    #[test]
    fn buying_missed_months() {
        let b = bars(&[100., 50., 200.]);
        let (money, units) = buy_months(&["2026-01".into(), "2026-02".into(), "2027-01".into()], 100., &b);
        assert_eq!(money, 200.);
        assert!((units - 3.).abs() < 1e-12);
    }

    #[test]
    fn dips_between_highs() {
        // highs at 0 (100), 3 (120), 6 (150); lowest between: bar 1 (80), bar 5 (90)
        let b = bars(&[100., 80., 90., 120., 110., 90., 150., 140.]);
        assert_eq!(dips(&b), vec![1, 5]);
    }

    #[test]
    fn dca_against_the_dip_buyer() {
        let b = bars(&[100., 80., 90., 120., 110., 90., 150., 140.]);
        let c: Vec<(String, f64)> = b.iter().map(|x| (x.date.clone(), 100.)).collect();
        let r = dip_vs_dca(&c, &b, None).unwrap();
        assert_eq!(r.money_in, 800.);
        // DCA: 100 at every close
        let dca: f64 = [100., 80., 90., 120., 110., 90., 150., 140.].iter().map(|p| 100. / p).sum();
        assert!((r.dca_units - dca).abs() < 1e-12);
        // dip buyer: 200 at 80 (bars 0-1), 400 at 90 (bars 2-5), 200 waits in cash
        assert!((r.dip_units - (200. / 80. + 400. / 90.)).abs() < 1e-12);
        assert_eq!(r.dip_cash, 200.);
        assert_eq!(r.dips.len(), 2);
        assert!((r.dca_value() - dca * 140.).abs() < 1e-9);
        // here the dip buyer wins: 972 + 200 cash vs DCA 1063
        assert!(r.dca_ahead_pct() < 0. && (r.dip_value() - (r.dip_units * 140. + 200.)).abs() < 1e-9);
        assert!(dip_vs_dca(&[], &b, None).is_none());
    }

    #[test]
    fn best_periods() {
        let pts = vec![("a".to_string(), 1., 10.), ("b".into(), 2., 12.), ("c".into(), 2., 9.), ("d".into(), 2., 15.)];
        let pnl = period_pnl(&pts, Some(16.));
        let v: Vec<f64> = pnl.iter().map(|p| p.1).collect();
        assert_eq!(v, vec![2., -6., 12., 2.]);
        let s = best_share(&v, 1);
        assert_eq!((s.total, s.best, s.without(), s.periods), (10., 12., -2., 4));
        assert_eq!(median(&[3., 1., 2.]), 2.);
        assert_eq!(median(&[4., 1., 2., 3.]), 2.5);
    }
}
