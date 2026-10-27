//! Drill-down helpers over the ledger: which rows are spending, the biggest
//! ones in a month, and "same merchant" context for one row. UI-free.

use crate::ledger::{Entry, Kind, Ledger};

/// Whether a category is money moving between the owner's own pockets (or
/// settling with the partner): not spending. The categories table's kind,
/// else the old fragments ([`crate::categories::not_spending`]).
fn not_spending(cat: &str) -> bool {
    crate::categories::not_spending(cat)
}

/// The owner's share of the row in HKD (shared rows count their share only).
pub fn my_hkd(e: &Entry) -> Option<f64> {
    e.hkd.map(|h| h * e.share)
}

/// A bank or card outflow that is real spending, not a transfer.
pub fn is_spending(e: &Entry) -> bool {
    if !matches!(e.kind, Kind::Bank | Kind::Card) || e.amount >= 0.0 {
        return false;
    }
    !not_spending(&e.category)
}

/// A bank or card inflow that is income (pay, interest, refunds, family),
/// not money coming back from the owner's own pockets or investments.
pub fn is_income(e: &Entry) -> bool {
    if !matches!(e.kind, Kind::Bank | Kind::Card) || e.amount <= 0.0 {
        return false;
    }
    !not_spending(&e.category)
}

/// Where a month's money came from and went, by category, in HKD at the
/// owner's share: (income by category, spending by category), each biggest first.
pub fn month_flows(ledger: &Ledger, month: &str) -> (Vec<(String, f64)>, Vec<(String, f64)>) {
    month_flows_where(ledger, month, |_, _| true)
}

/// `month_flows` over the rows `keep` accepts (a filter).
pub fn month_flows_where(ledger: &Ledger, month: &str, keep: impl Fn(usize, &Entry) -> bool) -> (Vec<(String, f64)>, Vec<(String, f64)>) {
    use std::collections::HashMap;
    let (mut inc, mut out): (HashMap<String, f64>, HashMap<String, f64>) = Default::default();
    for (_, e) in ledger.entries.iter().enumerate().filter(|(i, e)| e.date.starts_with(month) && keep(*i, e)) {
        let Some(v) = my_hkd(e) else { continue };
        if is_income(e) {
            *inc.entry(e.category.clone()).or_default() += v;
        } else if is_spending(e) {
            *out.entry(e.category.clone()).or_default() += -v;
        }
    }
    let sorted = |m: HashMap<String, f64>| {
        let mut v: Vec<(String, f64)> = m.into_iter().filter(|(_, x)| *x > 0.5).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v
    };
    (sorted(inc), sorted(out))
}

/// Indices of the `n` biggest spending rows in `month` ("YYYY-MM"), biggest first.
pub fn biggest_spending(ledger: &Ledger, month: &str, n: usize) -> Vec<usize> {
    biggest_spending_where(ledger, month, n, |_, _| true)
}

/// `biggest_spending` over the rows `keep` accepts (a filter).
pub fn biggest_spending_where(ledger: &Ledger, month: &str, n: usize, keep: impl Fn(usize, &Entry) -> bool) -> Vec<usize> {
    let mut rows: Vec<(f64, usize)> = ledger.entries.iter().enumerate()
        .filter(|(i, e)| e.date.starts_with(month) && is_spending(e) && keep(*i, e))
        .map(|(i, e)| (-my_hkd(e).unwrap_or(e.amount), i))
        .collect();
    rows.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    rows.into_iter().take(n).map(|(_, i)| i).collect()
}

/// Card acquirers that prefix the merchant ("KPay*Noodle House", "BBMSL * M007 Yoshinoya").
const ACQUIRERS: [&str; 10] = ["kpay", "kpayhkg", "eftpay", "bbmsl", "sq", "sumup", "zettle", "paypal", "pp", "yoco"];
/// Bank boilerplate that says nothing about the merchant.
const NOISE: [&str; 4] = ["pos", "eps", "dcc", "fps"];

/// The description after a card acquirer's `*`, original case:
/// "KPAY*NOODLE BAR HONGKONG HK" -> "NOODLE BAR HONGKONG HK".
pub fn strip_acquirer(desc: &str) -> &str {
    if let Some((head, tail)) = desc.rsplit_once('*') {
        if head.split_whitespace().last().is_some_and(|w| ACQUIRERS.contains(&w.to_lowercase().as_str())) {
            return tail.trim_start_matches(['%', ' ']);
        }
    }
    desc
}

/// The merchant words of a description: lowercase, anything up to an
/// acquirer's `*` dropped, words with digits and bank boilerplate dropped.
/// "POS MDC (17JUN24)P BBMSL*042 SUSHIRO MOS" -> "sushiro mos".
pub fn merchant_text(desc: &str) -> String {
    let lower = desc.to_lowercase();
    let mut rest = lower.as_str();
    if let Some((head, tail)) = lower.rsplit_once('*') {
        if head.split_whitespace().last().is_some_and(|w| ACQUIRERS.contains(&w)) {
            rest = tail;
        }
    }
    rest.replace('*', " ")
        .split_whitespace()
        .filter(|w| !w.chars().any(|c| c.is_ascii_digit()))
        .filter(|w| w.chars().any(|c| c.is_alphabetic()))
        .filter(|w| !NOISE.contains(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A merchant key: the first merchant word when it is distinctive (5+
/// letters: "sushiro", "mcdonald's"), else the first two ("city super").
/// Branch names after it don't split a chain.
pub fn merchant_key(desc: &str) -> String {
    let text = merchant_text(desc);
    let words: Vec<&str> = text.split(' ').filter(|w| !w.is_empty()).collect();
    match words.first() {
        None => String::new(),
        Some(w) if w.chars().filter(|c| c.is_alphabetic()).count() >= 5 => w.to_string(),
        Some(_) => words.iter().take(2).copied().collect::<Vec<_>>().join(" "),
    }
}

/// Rows whose merchant text starts with the key of row `idx` (itself
/// included), newest first. Bank and card rows match each other.
pub fn same_merchant(ledger: &Ledger, idx: usize) -> Vec<usize> {
    let Some(e) = ledger.entries.get(idx) else { return Vec::new() };
    let key = merchant_key(&e.description);
    if key.is_empty() {
        return vec![idx];
    }
    let money = |k: Kind| matches!(k, Kind::Bank | Kind::Card);
    ledger.entries.iter().enumerate()
        .filter(|(_, x)| x.kind == e.kind || (money(x.kind) && money(e.kind)))
        .filter(|(_, x)| merchant_text(&x.description).starts_with(&key))
        .map(|(i, _)| i)
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct MerchantStats {
    pub count: usize,
    /// signed, the owner's share
    pub total_hkd: f64,
    /// calendar months from the first to the last row, inclusive
    pub months: u32,
    pub monthly_avg: f64,
    pub first: String,
    pub last: String,
}

/// "YYYY-MM…" -> months since year 0
fn month_index(date: &str) -> Option<u32> {
    let y: u32 = date.get(0..4)?.parse().ok()?;
    let m: u32 = date.get(5..7)?.parse().ok()?;
    Some(y * 12 + m - 1)
}

pub fn merchant_stats(ledger: &Ledger, rows: &[usize]) -> MerchantStats {
    let es: Vec<&Entry> = rows.iter().filter_map(|&i| ledger.entries.get(i)).collect();
    let total_hkd = es.iter().map(|e| my_hkd(e).unwrap_or(0.0)).sum();
    let first = es.iter().map(|e| e.date.as_str()).min().unwrap_or("").to_string();
    let last = es.iter().map(|e| e.date.as_str()).max().unwrap_or("").to_string();
    let months = match (month_index(&first), month_index(&last)) {
        (Some(a), Some(b)) => b - a + 1,
        _ => 1,
    };
    MerchantStats { count: es.len(), total_hkd, months, monthly_avg: total_hkd / months as f64, first, last }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, date: &str, desc: &str, cat: &str, amount: f64, share: f64) -> Entry {
        Entry {
            kind, date: date.into(), source: "bank".into(), account: String::new(), description: desc.into(),
            category: cat.into(), amount, currency: "HKD".into(), hkd: Some(amount), share,
            haystack: String::new(), raw_hash: String::new(),
        }
    }

    fn ledger() -> Ledger {
        Ledger { categories: Vec::new(), rules: Vec::new(), data_rev: 0, income: Vec::new(), trades: Vec::new(), bars: Default::default(), coins: Vec::new(), entries: vec![
            entry(Kind::Card, "2026-06-20", "KPay*Noodle Bar Central HK", "Dining", -120.0, 1.0),
            entry(Kind::Bank, "2026-06-15", "Landlord", "Rent", -10000.0, 0.5),
            entry(Kind::Bank, "2026-06-10", "To savings", "Internal transfer", -50000.0, 1.0),
            entry(Kind::Bank, "2026-06-05", "Card repayment", "Credit-card bill", -8000.0, 1.0),
            entry(Kind::Bank, "2026-06-01", "Salary", "Salary", 30000.0, 1.0),
            entry(Kind::Card, "2026-05-02", "NOODLE BAR 12 CENTRAL", "Dining", -80.0, 1.0),
            entry(Kind::Crypto, "2026-06-03", "buy", "trade", -1.0, 1.0),
            entry(Kind::Card, "2026-04-02", "KPay*Noodle Bar Kowloon", "Dining", -100.0, 1.0),
        ]}
    }

    #[test]
    fn spending_skips_transfers_and_income() {
        let l = ledger();
        let spend: Vec<bool> = l.entries.iter().map(is_spending).collect();
        assert_eq!(spend, vec![true, true, false, false, false, true, false, true]);
    }

    #[test]
    fn biggest_uses_the_owners_share() {
        let l = ledger();
        assert_eq!(biggest_spending(&l, "2026-06", 10), vec![1, 0]);
        assert_eq!(biggest_spending(&l, "2026-06", 1), vec![1]);
        assert!(biggest_spending(&l, "2025-01", 10).is_empty());
    }

    #[test]
    fn merchant_keys() {
        assert_eq!(merchant_key("MCDONALD'S HK 999 HONG KONG HK"), "mcdonald's");
        assert_eq!(merchant_key("KPay*Noodle House HongKong HK"), "noodle");
        assert_eq!(merchant_key("KPayHKG*MAPLE FB MANAG HongKong HK"), "maple");
        assert_eq!(merchant_key("EFTPAY*CURRY CORNER LTD SHA TIN HK"), "curry");
        assert_eq!(merchant_key("BBMSL * M007 Yoshinoya Hong Kong HK"), "yoshinoya");
        assert_eq!(merchant_key("Uber *trip"), "uber trip");
        assert_eq!(merchant_key("CITY SUPER LIMITED SHA TIN HK"), "city super");
        assert_eq!(merchant_key("POS MDC (17JUN24)P BBMSL*042 SUSHIRO MOS"), "sushiro");
        assert_eq!(merchant_text("BBMSL*SushiroMOSPlaza Sha Tin HK"), "sushiromosplaza sha tin hk");
        assert_eq!(merchant_key("Spotify"), "spotify");
        assert_eq!(merchant_key(""), "");
    }

    #[test]
    fn same_merchant_context() {
        let l = ledger();
        let rows = same_merchant(&l, 0);
        assert_eq!(rows, vec![0, 5, 7]);
        let s = merchant_stats(&l, &rows);
        assert_eq!(s.count, 3);
        assert_eq!(s.total_hkd, -300.0);
        assert_eq!(s.months, 3);
        assert_eq!(s.monthly_avg, -100.0);
        assert_eq!((s.first.as_str(), s.last.as_str()), ("2026-04-02", "2026-06-20"));
    }

    #[test]
    fn month_flows_split_income_and_spending() {
        let (inc, out) = month_flows(&ledger(), "2026-01");
        assert!(inc.iter().all(|(c, _)| !c.to_lowercase().contains("transfer")));
        assert!(out.iter().all(|(_, v)| *v > 0.));
    }
}
