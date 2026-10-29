//! Source tracking: what span of time each data source covers, for the
//! App → Source tracking page's timeline. One [`Track`] per source with
//! data (the bundle's capabilities: first and last row), cut into the runs
//! the ledger actually has for bank and card statements, so a missing
//! month shows as a hole.
//!
//! [`lags`] finds the case that skews net worth: a bank whose statements
//! stop before the broker's or an exchange's data does. A transfer from the
//! bank after its last statement is already in the investments but not yet
//! out of the cash, so it counts twice until the statement is uploaded.

use chrono::NaiveDate;

use crate::capabilities::Capabilities;
use crate::ledger::Ledger;

/// Two rows of a statement further apart than this: a hole in between.
pub const GAP_DAYS: i64 = 45;
/// A bank this many days behind an investment source gets a lag note.
pub const LAG_DAYS: i64 = 7;
/// Sources whose data ends longer ago than this are closed, not late.
pub const ACTIVE_DAYS: i64 = 90;

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub key: String,
    pub label: String,
    /// bank, card, fund_units, crypto_wallet, exchange, broker
    pub kind: String,
    /// banks, cards, funds, crypto, broker
    pub group: String,
    pub rows: i64,
    pub first: NaiveDate,
    /// the last row (an API source: or its last sync, if later)
    pub last: NaiveDate,
    /// the last row itself
    pub last_row: NaiveDate,
    /// covered spans, oldest first; one span from first to last unless the
    /// ledger shows holes in a statement source
    pub runs: Vec<(NaiveDate, NaiveDate)>,
    /// fetched over an API (exchange, broker) rather than uploaded
    pub api: bool,
}

impl Track {
    /// Days from the end of its data to `today` (0 when current).
    pub fn age(&self, today: NaiveDate) -> i64 {
        (today - self.last).num_days().max(0)
    }

    /// Data within [`ACTIVE_DAYS`]: an account still in use.
    pub fn active(&self, today: NaiveDate) -> bool {
        self.age(today) <= ACTIVE_DAYS
    }

    /// The holes inside its span, oldest first.
    pub fn gaps(&self) -> Vec<(NaiveDate, NaiveDate)> {
        self.runs.windows(2).map(|w| (w[0].1, w[1].0)).collect()
    }
}

/// A bank behind an investment source (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct Lag {
    pub bank: String,
    pub bank_last: NaiveDate,
    pub ahead: String,
    pub ahead_last: NaiveDate,
}

impl Lag {
    pub fn days(&self) -> i64 {
        (self.ahead_last - self.bank_last).num_days()
    }
}

/// The order the groups are shown in.
pub const GROUPS: [(&str, &str); 5] =
    [("banks", "Banks"), ("cards", "Cards"), ("funds", "Funds"), ("crypto", "Crypto"), ("broker", "Broker")];

fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn ms_date(ms: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.date_naive())
}

/// Sorted dates cut where two are more than [`GAP_DAYS`] apart.
fn runs_of(dates: &[NaiveDate]) -> Vec<(NaiveDate, NaiveDate)> {
    let mut out: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    for &d in dates {
        match out.last_mut() {
            Some(r) if (d - r.1).num_days() <= GAP_DAYS => r.1 = r.1.max(d),
            _ => out.push((d, d)),
        }
    }
    out
}

/// Every source with data, in [`GROUPS`] order then the server's.
pub fn tracks(caps: &Capabilities, ledger: &Ledger) -> Vec<Track> {
    let mut out = Vec::new();
    for s in caps.sources.iter().filter(|s| s.has_data) {
        let (Some(first), Some(last_row)) = (s.first.as_deref().and_then(date), s.last.as_deref().and_then(date)) else { continue };
        let api = matches!(s.kind.as_str(), "exchange" | "broker");
        // an API source covers up to its last sync, even with nothing new
        let last = if api { s.last_sync.and_then(ms_date).map_or(last_row, |d| d.max(last_row)) } else { last_row };
        // holes only mean something for statements: an exchange or a
        // wallet without activity for two months has nothing missing
        let mut runs = if matches!(s.kind.as_str(), "bank" | "card") {
            let mut dates: Vec<NaiveDate> = ledger.entries.iter().filter(|e| e.source == s.key).filter_map(|e| date(&e.date)).collect();
            dates.sort_unstable();
            runs_of(&dates)
        } else {
            Vec::new()
        };
        if runs.is_empty() {
            runs.push((first, last));
        } else if let Some(r) = runs.last_mut() {
            r.1 = r.1.max(last);
        }
        out.push(Track { key: s.key.clone(), label: s.label.clone(), kind: s.kind.clone(), group: s.group.clone(), rows: s.rows,
                         first, last, last_row, runs, api });
    }
    let rank = |g: &str| GROUPS.iter().position(|(k, _)| *k == g).unwrap_or(GROUPS.len());
    out.sort_by_key(|t| rank(&t.group));
    out
}

/// Active banks whose statements end [`LAG_DAYS`] or more before the
/// freshest active investment source's last row, most behind first.
pub fn lags(tracks: &[Track], today: NaiveDate) -> Vec<Lag> {
    let Some(ahead) = tracks.iter()
        .filter(|t| matches!(t.kind.as_str(), "broker" | "exchange" | "fund_units") && t.active(today))
        .max_by_key(|t| t.last_row) else { return Vec::new() };
    let mut out: Vec<Lag> = tracks.iter()
        .filter(|t| t.kind == "bank" && t.active(today) && (ahead.last_row - t.last).num_days() >= LAG_DAYS)
        .map(|t| Lag { bank: t.label.clone(), bank_last: t.last, ahead: ahead.label.clone(), ahead_last: ahead.last_row })
        .collect();
    out.sort_by_key(|l| l.bank_last);
    out
}

/// The axis: whole months from the earliest shown day to `today`, with
/// `from` (None: all time) clamping the start.
pub fn window(tracks: &[Track], today: NaiveDate, from: Option<NaiveDate>) -> (NaiveDate, NaiveDate) {
    let earliest = tracks.iter().map(|t| t.first).min().unwrap_or(today);
    let start = from.map_or(earliest, |f| f.max(earliest).min(today));
    let start = NaiveDate::from_ymd_opt(chrono::Datelike::year(&start), chrono::Datelike::month(&start), 1).unwrap_or(start);
    (start, today)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::CapSource;
    use crate::ledger::{Entry, Kind};

    fn d(s: &str) -> NaiveDate {
        date(s).unwrap()
    }

    fn src(key: &str, kind: &str, group: &str, first: &str, last: &str) -> CapSource {
        CapSource { key: key.into(), kind: kind.into(), group: group.into(), label: key.to_uppercase(), has_data: true, rows: 10,
                    first: Some(first.into()), last: Some(last.into()), ..Default::default() }
    }

    fn row(source: &str, date: &str) -> Entry {
        Entry { kind: Kind::Bank, date: date.into(), source: source.into(), account: String::new(), description: String::new(),
                category: String::new(), amount: -1., currency: "HKD".into(), hkd: Some(-1.), share: 1., haystack: String::new(),
                raw_hash: String::new() }
    }

    fn caps(sources: Vec<CapSource>) -> Capabilities {
        Capabilities { known: true, features: Default::default(), sources, server: None }
    }

    #[test]
    fn a_missing_statement_month_is_a_hole() {
        let c = caps(vec![src("bank", "bank", "banks", "2026-01-03", "2026-06-28")]);
        let ledger = Ledger { entries: ["2026-01-03", "2026-02-10", "2026-05-01", "2026-06-01", "2026-06-28"].iter().map(|x| row("bank", x)).collect(),
                              ..Default::default() };
        let t = &tracks(&c, &ledger)[0];
        assert_eq!(t.runs, vec![(d("2026-01-03"), d("2026-02-10")), (d("2026-05-01"), d("2026-06-28"))]);
        assert_eq!(t.gaps(), vec![(d("2026-02-10"), d("2026-05-01"))]);
    }

    #[test]
    fn api_sources_run_to_their_last_sync_and_groups_are_ordered() {
        let mut ex = src("binance", "exchange", "crypto", "2025-01-01", "2026-09-01");
        ex.last_sync = Some(1_790_000_000_000); // 2026-09-21
        let c = caps(vec![ex, src("hsbc", "bank", "banks", "2024-01-01", "2026-09-01")]);
        let t = tracks(&c, &Ledger::default());
        assert_eq!(t.iter().map(|t| t.key.as_str()).collect::<Vec<_>>(), ["hsbc", "binance"]);
        assert_eq!(t[1].last, d("2026-09-21"));
        assert_eq!(t[1].last_row, d("2026-09-01"));
        // no ledger rows: one run over the whole span
        assert_eq!(t[0].runs, vec![(d("2024-01-01"), d("2026-09-01"))]);
    }

    #[test]
    fn a_bank_behind_the_broker_lags_but_a_closed_one_does_not() {
        let c = caps(vec![
            src("hsbc", "bank", "banks", "2024-01-01", "2026-10-02"),
            src("n26", "bank", "banks", "2024-01-01", "2026-10-25"),
            src("old", "bank", "banks", "2019-01-01", "2021-06-22"),
            src("ibkr", "broker", "broker", "2024-01-01", "2026-10-27"),
        ]);
        let t = tracks(&c, &Ledger::default());
        let today = d("2026-10-29");
        let l = lags(&t, today);
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].bank.as_str(), l[0].ahead.as_str(), l[0].days()), ("HSBC", "IBKR", 25));
        assert!(!t.iter().find(|t| t.key == "old").unwrap().active(today));
        // nothing to be ahead of: no lag
        assert!(lags(&t[..3], today).is_empty());
    }

    #[test]
    fn the_window_starts_on_a_month() {
        let c = caps(vec![src("a", "bank", "banks", "2020-02-17", "2026-10-02")]);
        let t = tracks(&c, &Ledger::default());
        let today = d("2026-10-29");
        assert_eq!(window(&t, today, None), (d("2020-02-01"), today));
        assert_eq!(window(&t, today, Some(d("2025-10-29"))), (d("2025-10-01"), today));
        assert_eq!(window(&[], today, None), (d("2026-10-01"), today));
    }
}
