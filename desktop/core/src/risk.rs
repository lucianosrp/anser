//! How bumpy each holding is, from its daily closes: volatility, the worst
//! peak-to-trough fall over the whole bar history and since the first buy,
//! and how the holdings move together (correlation of daily returns).
//!
//! Volatility is the standard deviation of daily returns, annualized with
//! √252. Correlations pair the days both holdings traded; London and New
//! York close hours apart, so same-day moves between the two venues
//! partly land on the next day and those pairs read lower than they are.

use crate::ledger::Bar;

pub const TRADING_DAYS: f64 = 252.;

/// The deepest fall from a running peak: (% <= 0, peak index, trough index).
pub fn max_drawdown(closes: &[f64]) -> (f64, usize, usize) {
    let (mut peak, mut peak_i) = (f64::MIN, 0);
    let mut worst = (0., 0, 0);
    for (i, c) in closes.iter().enumerate() {
        if *c > peak {
            peak = *c;
            peak_i = i;
        }
        if peak > 0. {
            let dd = (c / peak - 1.) * 100.;
            if dd < worst.0 {
                worst = (dd, peak_i, i);
            }
        }
    }
    worst
}

/// Daily simple returns.
pub fn returns(closes: &[f64]) -> Vec<f64> {
    closes.windows(2).filter(|w| w[0] > 0.).map(|w| w[1] / w[0] - 1.).collect()
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// Annualized volatility of daily returns, %; None under 20 returns.
pub fn volatility(rets: &[f64]) -> Option<f64> {
    if rets.len() < 20 {
        return None;
    }
    let m = mean(rets);
    let var = rets.iter().map(|r| (r - m).powi(2)).sum::<f64>() / (rets.len() - 1) as f64;
    Some(var.sqrt() * TRADING_DAYS.sqrt() * 100.)
}

/// Pearson correlation; None under 20 pairs or with no variation.
pub fn correlation(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 20 {
        return None;
    }
    let (ma, mb) = (mean(a), mean(b));
    let (mut sab, mut saa, mut sbb) = (0., 0., 0.);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma).powi(2);
        sbb += (y - mb).powi(2);
    }
    (saa > 0. && sbb > 0.).then(|| sab / (saa * sbb).sqrt())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Risk {
    pub symbol: String,
    pub first_bar: String,
    pub bars: usize,
    /// annualized, over the last year of bars / the whole history
    pub vol_1y: Option<f64>,
    pub vol_all: Option<f64>,
    /// worst fall over the whole history, with its peak and trough dates
    pub dd_all: f64,
    pub dd_all_from: String,
    pub dd_all_to: String,
    /// worst fall since the first buy (closes from that day on)
    pub dd_since_buy: Option<f64>,
    /// below the highest close of the history now, %
    pub from_high: f64,
}

/// `since` = the first buy's date.
pub fn risk(symbol: &str, bars: &[Bar], since: Option<&str>) -> Option<Risk> {
    if bars.len() < 2 {
        return None;
    }
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let rets = returns(&closes);
    let last_year = &rets[rets.len().saturating_sub(TRADING_DAYS as usize)..];
    let (dd, p, t) = max_drawdown(&closes);
    let dd_since_buy = since.map(|s| {
        let i = bars.partition_point(|b| b.date.as_str() < s);
        max_drawdown(&closes[i.min(closes.len() - 1)..]).0
    });
    let high = closes.iter().copied().fold(f64::MIN, f64::max);
    Some(Risk {
        symbol: symbol.to_string(),
        first_bar: bars[0].date.clone(),
        bars: bars.len(),
        vol_1y: volatility(last_year),
        vol_all: volatility(&rets),
        dd_all: dd,
        dd_all_from: bars[p].date.clone(),
        dd_all_to: bars[t].date.clone(),
        dd_since_buy,
        from_high: (closes[closes.len() - 1] / high - 1.) * 100.,
    })
}

/// Correlation of daily returns between every pair, over the days both
/// traded in the last `lookback` bars of each pair's shared history.
/// `m[i][j]` with `n[i][j]` returns behind it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Matrix {
    pub symbols: Vec<String>,
    pub m: Vec<Vec<Option<f64>>>,
    pub n: Vec<Vec<usize>>,
}

pub fn matrix(series: &[(String, &[Bar])], lookback: usize) -> Matrix {
    let k = series.len();
    let mut out = Matrix { symbols: series.iter().map(|s| s.0.clone()).collect(), m: vec![vec![None; k]; k], n: vec![vec![0; k]; k] };
    for i in 0..k {
        out.m[i][i] = Some(1.);
        for j in i + 1..k {
            let (a, b) = (series[i].1, series[j].1);
            // the dates both have, oldest first (both are sorted)
            let (mut x, mut y) = (0, 0);
            let mut pairs: Vec<(f64, f64)> = Vec::new();
            while x < a.len() && y < b.len() {
                match a[x].date.cmp(&b[y].date) {
                    std::cmp::Ordering::Less => x += 1,
                    std::cmp::Ordering::Greater => y += 1,
                    std::cmp::Ordering::Equal => {
                        pairs.push((a[x].close, b[y].close));
                        x += 1;
                        y += 1;
                    }
                }
            }
            let pairs = &pairs[pairs.len().saturating_sub(lookback + 1)..];
            let ra = returns(&pairs.iter().map(|p| p.0).collect::<Vec<_>>());
            let rb = returns(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
            let c = correlation(&ra, &rb);
            out.m[i][j] = c;
            out.m[j][i] = c;
            out.n[i][j] = ra.len();
            out.n[j][i] = ra.len();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes.iter().enumerate().map(|(i, c)| Bar { date: format!("2026-{:02}-{:02}", 1 + i / 28, 1 + i % 28), open: *c, high: *c, low: *c, close: *c }).collect()
    }

    #[test]
    fn drawdown() {
        let (dd, p, t) = max_drawdown(&[100., 120., 90., 110., 60., 130.]);
        assert!((dd - (60. / 120. - 1.) * 100.).abs() < 1e-9);
        assert_eq!((p, t), (1, 4));
        assert_eq!(max_drawdown(&[1., 2., 3.]).0, 0.);
    }

    #[test]
    fn vol_and_since_buy() {
        // alternating +1% / -1%
        let mut c = vec![100.];
        for i in 0..60 {
            let last = c[c.len() - 1];
            c.push(if i % 2 == 0 { last * 1.01 } else { last * 0.99 });
        }
        let b = bars(&c);
        let r = risk("X", &b, Some(&b[30].date)).unwrap();
        let v = r.vol_all.unwrap();
        assert!((v - 0.01 * 252f64.sqrt() * 100.).abs() < 0.2, "{v}");
        assert!(r.dd_since_buy.unwrap() > r.dd_all - 1e-9);
        assert!(volatility(&[0.01; 5]).is_none());
    }

    #[test]
    fn correlations() {
        let a: Vec<f64> = (0..40).map(|i| 100. + ((i * 7) % 11) as f64).collect();
        let b2: Vec<f64> = a.iter().map(|x| x * 2.).collect();
        let inv: Vec<f64> = a.iter().map(|x| 300. - x).collect();
        let (ba, bb, bi) = (bars(&a), bars(&b2), bars(&inv));
        let m = matrix(&[("A".into(), &ba), ("B".into(), &bb), ("I".into(), &bi)], 252);
        assert!((m.m[0][1].unwrap() - 1.).abs() < 1e-9);
        assert!(m.m[0][2].unwrap() < -0.9);
        assert_eq!(m.n[0][1], 39);
        // a short shared history: no number
        let short = bars(&a[..10]);
        let m = matrix(&[("A".into(), &ba), ("S".into(), &short)], 252);
        assert!(m.m[0][1].is_none());
    }
}
