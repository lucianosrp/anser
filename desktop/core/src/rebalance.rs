//! Rebalancing math: how far each holding is from a target weight, and how
//! to split the next contribution so the portfolio ends as close to the
//! targets as money alone can take it, without selling anything.
//!
//! The split minimizes the squared distance from the targets after the
//! contribution: each holding gets `max(0, gap - level)`, where `gap` is
//! what it lacks to sit at its target weight of the new total and `level`
//! is the one number that makes the parts add up to the contribution. When
//! every holding is under its target, that is exactly the gaps; holdings
//! above target get nothing until the others catch up.
//!
//! This is arithmetic on the owner's own targets, not a recommendation.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Drift {
    pub symbol: String,
    pub value: f64,
    /// %
    pub weight: f64,
    /// %, normalized so the targets sum to 100
    pub target: f64,
    /// percentage points over (+) or under (-) the target
    pub drift_pp: f64,
    /// value at the target weight minus value now (+ = under target)
    pub gap: f64,
}

/// Targets normalized to sum to 100 (non-positive ones dropped).
pub fn normalize(targets: &BTreeMap<String, f64>) -> BTreeMap<String, f64> {
    let sum: f64 = targets.values().filter(|w| **w > 0.).sum();
    targets.iter().filter(|(_, w)| **w > 0. && sum > 0.).map(|(k, w)| (k.clone(), w / sum * 100.)).collect()
}

/// Every holding and every target, values in one currency.
pub fn drift(values: &[(String, f64)], targets: &BTreeMap<String, f64>) -> Vec<Drift> {
    let t = normalize(targets);
    let mut symbols: Vec<String> = values.iter().map(|v| v.0.clone()).collect();
    for k in t.keys() {
        if !symbols.contains(k) {
            symbols.push(k.clone());
        }
    }
    let total: f64 = values.iter().map(|v| v.1.max(0.)).sum();
    symbols.into_iter().map(|s| {
        let value = values.iter().filter(|v| v.0 == s).map(|v| v.1.max(0.)).sum::<f64>();
        let target = t.get(&s).copied().unwrap_or(0.);
        let weight = if total > 0. { value / total * 100. } else { 0. };
        Drift { gap: target / 100. * total - value, drift_pp: weight - target, symbol: s, value, weight, target }
    }).collect()
}

/// How to split `contribution` across the targets without selling.
/// Returns (symbol, amount) for every holding and target, in `drift` order.
pub fn split(values: &[(String, f64)], targets: &BTreeMap<String, f64>, contribution: f64) -> Vec<(String, f64)> {
    let total: f64 = values.iter().map(|v| v.1.max(0.)).sum::<f64>() + contribution.max(0.);
    let rows = drift(values, targets);
    if contribution <= 0. || rows.iter().all(|r| r.target <= 0.) {
        return rows.into_iter().map(|r| (r.symbol, 0.)).collect();
    }
    let gaps: Vec<f64> = rows.iter().map(|r| r.target / 100. * total - r.value).collect();
    let fill = |level: f64| gaps.iter().map(|g| (g - level).max(0.)).sum::<f64>();
    // fill(level) falls as level rises; find where it equals the contribution
    let (mut lo, mut hi) = (gaps.iter().copied().fold(f64::MAX, f64::min) - contribution, gaps.iter().copied().fold(f64::MIN, f64::max));
    for _ in 0..200 {
        let mid = (lo + hi) / 2.;
        if fill(mid) > contribution { lo = mid } else { hi = mid }
    }
    let level = (lo + hi) / 2.;
    let mut out: Vec<(String, f64)> = rows.iter().zip(&gaps).map(|(r, g)| (r.symbol.clone(), (g - level).max(0.))).collect();
    // exact sum: put the rounding dust on the biggest part
    let dust = contribution - out.iter().map(|o| o.1).sum::<f64>();
    if let Some(big) = out.iter_mut().max_by(|a, b| a.1.total_cmp(&b.1)) {
        big.1 += dust;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }
    fn v(pairs: &[(&str, f64)]) -> Vec<(String, f64)> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }
    fn get(s: &[(String, f64)], k: &str) -> f64 {
        s.iter().find(|x| x.0 == k).unwrap().1
    }

    #[test]
    fn drift_rows() {
        let d = drift(&v(&[("A", 700.), ("B", 300.)]), &t(&[("A", 3.), ("B", 1.), ("C", 1.)]));
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].target, 60.);
        assert!((d[0].drift_pp - 10.).abs() < 1e-9);
        assert!((d[0].gap + 100.).abs() < 1e-9);
        assert_eq!(d[2].value, 0.);
        assert!((d[2].gap - 200.).abs() < 1e-9);
    }

    #[test]
    fn all_under_target_get_their_gaps() {
        // total after = 1000 + 1000; targets 50/50 -> A needs 400, B needs 600
        let s = split(&v(&[("A", 600.), ("B", 400.)]), &t(&[("A", 50.), ("B", 50.)]), 1000.);
        assert!((get(&s, "A") - 400.).abs() < 1e-6 && (get(&s, "B") - 600.).abs() < 1e-6);
    }

    #[test]
    fn no_selling_overweight_gets_nothing() {
        // A is far over: all 100 goes to B
        let s = split(&v(&[("A", 900.), ("B", 100.)]), &t(&[("A", 50.), ("B", 50.)]), 100.);
        assert!(get(&s, "A").abs() < 1e-9 && (get(&s, "B") - 100.).abs() < 1e-6);
        // an untargeted holding gets nothing; the sum is exact
        let s = split(&v(&[("A", 100.), ("B", 100.), ("X", 500.)]), &t(&[("A", 70.), ("B", 30.)]), 300.);
        assert_eq!(get(&s, "X"), 0.);
        assert!((s.iter().map(|x| x.1).sum::<f64>() - 300.).abs() < 1e-9);
        // gaps: A 0.7*1000-100 = 600, B 0.3*1000-100 = 200; 300 to fill:
        // level 250 -> A 350, B 0 ... check A gets more and both >= 0
        assert!(get(&s, "A") > get(&s, "B"));
        assert!((get(&s, "A") - 300.).abs() < 1e-6);
    }

    #[test]
    fn nothing_to_split() {
        assert!(split(&v(&[("A", 1.)]), &t(&[]), 100.).iter().all(|x| x.1 == 0.));
        assert!(split(&v(&[("A", 1.)]), &t(&[("A", 1.)]), 0.).iter().all(|x| x.1 == 0.));
    }
}
