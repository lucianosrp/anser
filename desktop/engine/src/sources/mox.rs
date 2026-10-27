//! Mox Bank (Hong Kong) monthly statement PDFs.
//!
//! One PDF covers every pocket on the account (HKD, USD, …): activity date,
//! settlement date, a bilingual description, a signed amount in that
//! pocket's currency, and an opening/closing balance. There is no CSV
//! export. `pdftotext -layout` (same helper as the HSBC card) so a
//! statement's rows and keys never move.
//!
//! Canonical row:
//!
//!   account      the pocket ("HKD", "USD", "USD Invest", …)
//!   currency     that pocket's currency
//!   txn_date     the activity date
//!   amount       signed: + in, − out
//!   balance      running, from that month's opening plus the amounts
//!   description  whitespace collapsed; continuation lines joined
//!   dedup        settlement date + occurrence, so two identical same-day
//!                rows stay two rows
//!
//! Opening and closing balance lines are not transactions. Multi-currency,
//! like Revolut: not part of the overview cash-flow series (those are the
//! single-currency everyday accounts).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Datelike, NaiveDate};
use regex::Regex;

use super::pdf;
use super::textfmt;
use super::{suffix_lower, BalanceMethod, BankRow, FileSource, Kind, Parsed, SourceSpec};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn month_num(m: &str) -> Option<u32> {
    MONTHS
        .iter()
        .position(|x| x.eq_ignore_ascii_case(m))
        .map(|i| i as u32 + 1)
}

fn period_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"Statement period:\s*(\d{1,2})\s+([A-Za-z]{3})\s+(\d{4})\s*[-–]\s*(\d{1,2})\s+([A-Za-z]{3})\s+(\d{4})")
            .unwrap()
    })
}

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*([A-Z]{3})\s+Mox\s+(Account|Invest)\b").unwrap())
}

fn row_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            ^\s*
            (\d{1,2})\s+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)
            \s+
            (\d{1,2})\s+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)
            \s+
            (.+?)
            \s+
            ([+-]?[\d,]+\.\d{2})
            \s*$",
        )
        .unwrap()
    })
}

fn corresponding_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+[\d,]+\.\d{2}\s+[A-Z]{3}$").unwrap())
}

fn page_furniture_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(page\s+\d+(\s+of\s+\d+)?|activity\s+settlement\b.*|date\s+date\b.*)$").unwrap())
}

fn money(s: &str) -> Result<f64> {
    s.replace(',', "")
        .parse()
        .map_err(|_| anyhow!("bad amount {s:?}"))
}

fn round_cents(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn is_opening(desc: &str) -> bool {
    desc.contains("Opening balance")
}

fn is_closing(desc: &str) -> bool {
    desc.contains("Closing balance")
}

struct Raw {
    account: String,
    currency: String,
    txn: NaiveDate,
    settle: NaiveDate,
    amount: f64,
    balance: f64,
    description: String,
}

fn with_dedup(rows: Vec<Raw>) -> Vec<BankRow> {
    let mut seen: HashMap<(String, NaiveDate, Option<u64>, String, NaiveDate), u32> =
        HashMap::new();
    rows.into_iter()
        .map(|r| {
            let n = seen
                .entry((
                    r.account.clone(),
                    r.txn,
                    super::fkey(Some(r.amount)),
                    r.description.clone(),
                    r.settle,
                ))
                .or_insert(0);
            *n += 1;
            BankRow {
                account: r.account,
                currency: r.currency,
                txn_date: r.txn.format("%Y-%m-%d").to_string(),
                amount: Some(r.amount),
                balance: Some(r.balance),
                description: r.description,
                dedup: format!("{}|{n}", r.settle.format("%Y-%m-%d")),
            }
        })
        .collect()
}

fn collapse(s: &str) -> String {
    textfmt::split_ws(s).join(" ")
}

fn strip_corresponding(desc: &str) -> String {
    corresponding_re().replace(desc, "").into_owned()
}

/// Year of a `DD Mon` on this statement: the period's end year, or the
/// year before when the month sits after the period (a January statement
/// carrying December lines).
fn dated(day: &str, mon: &str, period_end: NaiveDate) -> Result<NaiveDate> {
    let mo = month_num(mon).ok_or_else(|| anyhow!("bad month {mon}"))?;
    let year = period_end.year() - i32::from(mo > period_end.month());
    NaiveDate::from_ymd_opt(year, mo, day.parse()?).ok_or_else(|| anyhow!("bad date {day} {mon}"))
}

pub struct MoxSource;

static SPEC: SourceSpec = SourceSpec {
    key: "mox",
    kind: Kind::Bank,
    currency: None,
    balance_method: BalanceMethod::Reported,
    cashflow: false,
    label: "Mox",
};

/// The rows of one monthly statement, from its `pdftotext -layout` text.
pub fn parse_statement_text(text: &str) -> Result<Vec<BankRow>> {
    let cap = period_re()
        .captures(text)
        .ok_or_else(|| anyhow!("no statement period found"))?;
    let end_day: u32 = cap[4].parse()?;
    let end_year: i32 = cap[6].parse()?;
    let end_month = month_num(&cap[5]).ok_or_else(|| anyhow!("bad statement month"))?;
    let period_end = NaiveDate::from_ymd_opt(end_year, end_month, end_day)
        .ok_or_else(|| anyhow!("bad statement period end"))?;

    let mut account = String::new();
    let mut currency = String::new();
    let mut running = 0.0;
    let mut pending: Option<Raw> = None;
    let mut out = Vec::new();

    let flush = |pending: &mut Option<Raw>, out: &mut Vec<Raw>| {
        if let Some(r) = pending.take() {
            out.push(r);
        }
    };

    for line in textfmt::splitlines(text) {
        if let Some(h) = heading_re().captures(line) {
            flush(&mut pending, &mut out);
            let pocket = match &h[2] {
                "Invest" => format!("{} Invest", &h[1]),
                _ => h[1].to_string(),
            };
            // the same heading again is the pocket running onto the next page
            if pocket != account {
                currency = h[1].to_string();
                account = pocket;
                running = 0.0;
            }
            continue;
        }
        if let Some(r) = row_re().captures(line) {
            flush(&mut pending, &mut out);
            if account.is_empty() {
                continue;
            }
            let txn = dated(&r[1], &r[2], period_end)?;
            let settle = dated(&r[3], &r[4], period_end)?;
            let desc = collapse(&strip_corresponding(&r[5]));
            let amt = money(&r[6])?;
            if is_opening(&desc) {
                running = round_cents(amt);
                continue;
            }
            if is_closing(&desc) {
                // the rows must add up to the statement's own figure: a row
                // read wrong (a layout change, an unsigned credit) fails here
                // instead of skewing every balance after it
                if round_cents(amt) != running {
                    bail!("{account} pocket: the rows add up to {running:.2} but the statement's closing balance is {amt:.2}");
                }
                continue;
            }
            // activity rows carry a leading sign; an unsigned leftover is not a txn
            let signed = r[6].starts_with('+') || r[6].starts_with('-');
            if !signed {
                continue;
            }
            running = round_cents(running + amt);
            pending = Some(Raw {
                account: account.clone(),
                currency: currency.clone(),
                txn,
                settle,
                amount: amt,
                balance: running,
                description: desc,
            });
            continue;
        }
        let t = textfmt::strip(line);
        // a blank line, a page number or the column headings repeated on a
        // new page end the row above (they are never part of a description)
        if t.is_empty() || page_furniture_re().is_match(t) {
            flush(&mut pending, &mut out);
            continue;
        }
        if let Some(p) = pending.as_mut() {
            let extra = collapse(t);
            if !extra.is_empty() {
                p.description = collapse(&format!("{} {extra}", p.description));
            }
        }
    }
    flush(&mut pending, &mut out);
    Ok(with_dedup(out))
}

impl FileSource for MoxSource {
    fn spec(&self) -> &SourceSpec {
        &SPEC
    }

    fn matches(&self, path: &Path) -> bool {
        suffix_lower(path) == ".pdf" && pdf::head_contains(path, 800, &["Mox Bank statement"])
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        let mut rows = Vec::new();
        for p in paths {
            rows.extend(
                parse_statement_text(&pdf::text(p)?).with_context(|| p.display().to_string())?,
            );
        }
        Ok(Parsed::Bank(rows))
    }
}
