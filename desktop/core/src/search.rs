//! "Search everything": fuzzy text plus filters, over the whole ledger.
//!
//! The query language is [`crate::filter`]'s (`cat:dining`, `src:card`,
//! `acct:`, `merchant:`, `ccy:`, `trip:`, `-cat:rent`, `>500`, `amt:100..500`,
//! `2026-09`, `2026-01..2026-06`, `in`/`out`, and free text). Free text ranks;
//! everything else filters.
//!
//! A few thousand rows: a full scan per keystroke is well under a millisecond
//! for filters and a few milliseconds with fuzzy scoring.

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::filter::{Filter, Trips};
use crate::ledger::Ledger;

/// The parsed query (the filter model).
pub fn parse(input: &str) -> Filter {
    Filter::parse(input)
}

/// Indices into `ledger.entries`, best first, at most `limit`.
/// Empty text: filtered rows in ledger order (newest first).
pub struct Searcher {
    matcher: Matcher,
    buf: Vec<char>,
    /// what `trip:` can name
    trips: Trips,
}

impl Default for Searcher {
    fn default() -> Self {
        Searcher { matcher: Matcher::new(Config::DEFAULT), buf: Vec::new(), trips: Trips::default() }
    }
}

impl Searcher {
    /// The trips `trip:START` resolves against (set once per load).
    pub fn set_trips(&mut self, trips: Trips) {
        self.trips = trips;
    }

    pub fn trips(&self) -> &Trips {
        &self.trips
    }

    pub fn search(&mut self, ledger: &Ledger, input: &str, limit: usize) -> Vec<usize> {
        let f = parse(input);
        let c = f.compile(&self.trips);
        let text = f.text();
        if text.is_empty() {
            return ledger.entries.iter().enumerate()
                .filter(|(i, e)| c.matches(*i, e))
                .map(|(i, _)| i)
                .take(limit)
                .collect();
        }
        // Exact first: when the words appear literally somewhere, only literal
        // matches are shown ("sushiro" should not fuzzily hit "Sell USD.HKD").
        // Fuzzy is the fallback for typos and abbreviations ("ktchn"). Numbers
        // are always literal ("12345" must not hit every row with a 1, 2, 3).
        let numeric = text.chars().all(|ch| ch.is_ascii_digit() || ch == '.' || ch == ',');
        let text = if numeric { text.replace(',', "") } else { text };
        let needle = text.to_lowercase();
        let literal = numeric || ledger.entries.iter().enumerate().any(|(i, e)| c.matches(i, e) && e.haystack.contains(needle.as_str()));
        let kind = if literal { AtomKind::Substring } else { AtomKind::Fuzzy };
        let pattern = Pattern::new(&text, CaseMatching::Ignore, Normalization::Smart, kind);
        let mut scored: Vec<(u32, usize)> = Vec::new();
        for (i, e) in ledger.entries.iter().enumerate() {
            if !c.matches(i, e) {
                continue;
            }
            let hay = Utf32Str::new(&e.haystack, &mut self.buf);
            if let Some(score) = pattern.score(hay, &mut self.matcher) {
                scored.push((score, i));
            }
        }
        // best score first; ties keep ledger order (newest first). Scattered
        // subsequence hits score far below real ones -- drop anything under
        // half the best score so the tail stays relevant
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let floor = scored.first().map_or(0, |s| s.0 / 2);
        scored.into_iter().take_while(|s| s.0 >= floor).take(limit).map(|(_, i)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{DateRange, Dim, Term};

    #[test]
    fn parses_filters() {
        let q = parse("coffee cat:dining src:card >50 2026-09 out");
        assert_eq!(q.text(), "coffee");
        assert_eq!(q.values(Dim::Category, false), vec!["dining"]);
        assert_eq!(q.values(Dim::Source, false), vec!["card"]);
        assert!(q.terms.contains(&Term::Above(50.0)));
        assert_eq!(q.date(), Some(&DateRange::prefix("2026-09")));
        assert_eq!(q.direction(), Some(false));
    }

    fn entry(desc: &str) -> crate::ledger::Entry {
        crate::ledger::Entry {
            kind: crate::ledger::Kind::Card, date: "2026-01-01".into(), source: "card".into(),
            account: String::new(), description: desc.into(), category: "Dining".into(),
            amount: -10.0, currency: "HKD".into(), hkd: Some(-10.0), share: 1.0,
            haystack: desc.to_lowercase(), raw_hash: String::new(),
        }
    }

    #[test]
    fn literal_hits_beat_fuzzy_noise() {
        let ledger = Ledger { entries: vec![entry("Sushiro MOS"), entry("Sell USD.HKD @ 7.8"), entry("Kitchen")], ..Default::default() }; // trades unused
        let mut s = Searcher::default();
        assert_eq!(s.search(&ledger, "sushiro", 10), vec![0]);
        // no literal hit -> fuzzy still finds the abbreviation
        assert_eq!(s.search(&ledger, "ktchn", 10), vec![2]);
    }

    #[test]
    fn plain_number_is_text_not_date() {
        assert_eq!(parse("12345").text(), "12345");
        assert_eq!(parse("2026").date(), Some(&DateRange::prefix("2026")));
    }

    #[test]
    fn filters_narrow_the_fuzzy_text() {
        let mut a = entry("Sushiro MOS");
        a.category = "Groceries".into();
        let ledger = Ledger { entries: vec![a, entry("Sushiro MK")], ..Default::default() };
        let mut s = Searcher::default();
        assert_eq!(s.search(&ledger, "sushiro", 10), vec![0, 1]);
        assert_eq!(s.search(&ledger, "sushiro -cat:groceries", 10), vec![1]);
        assert_eq!(s.search(&ledger, "merchant:sushiro cat:dining,groceries", 10), vec![0, 1]);
    }
}
