//! HSBC credit card: the online
//! "transaction history" CSV and the monthly e-Statement PDFs, as one source.
//!
//! Both formats describe the same ledger, so a purchase that appears in a CSV
//! AND in a statement must hash identically (bank_transactions UNIQUE on
//! source+raw_hash). Canonical row:
//!
//!   txn_date     the transaction date (not the post date)
//!   amount       negative = spent, positive = credit (refund, autopay received)
//!   description  the merchant field with the CSV's channel prefix ("GPAY",
//!                "SALES:") removed and whitespace collapsed -- the PDF prints
//!                the same fixed-width merchant/city/country record without it
//!   dedup        post date + occurrence number, so two identical coffees on
//!                one day stay two rows (the card has no running balance to
//!                tell them apart)
//!
//! The card is a liability, not a cash account: balance_method None keeps it
//! out of the balance sheet and the net-worth sum. PDFs need poppler's
//! `pdftotext` (present on the server and the laptop), always run the same way
//! (`-layout`), so a statement's rows and keys never move.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveDate;
use regex::Regex;

use super::csvread::dict_rows;
use super::pdf;
use super::textfmt;
use super::{has_all, sniff_header, suffix_lower, BalanceMethod, BankRow, FileSource, Kind, Parsed, SourceSpec};

const CSV_HEADER: [&str; 5] = ["Transaction date", "Post date", "Description", "Billing amount", "Credit / Debit"];
const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

fn prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(?:GPAY|SALES:|APAY|SPAY|ONLINE:?)\s+").unwrap())
}

/// "18AUG       18AUG        PAID BY AUTOPAY - THANK YOU        7,412.14CR"
fn pdf_row_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*(\d{2})([A-Z]{3})\s+(\d{2})([A-Z]{3})\s+(.+?)\s+([\d,]+\.\d{2})(CR)?\s*$").unwrap()
    })
}

fn stmt_date_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(\d{1,2}) ([A-Z]{3}) (\d{4})\b").unwrap())
}

fn month_num(m: &str) -> Option<u32> {
    MONTHS.iter().position(|x| *x == m).map(|i| i as u32 + 1)
}

pub fn canonical_description(desc: &str) -> String {
    let stripped = textfmt::strip(desc);
    let d = textfmt::split_ws(&prefix_re().replace(stripped, "")).join(" ");
    // the autopay credit is worded differently in the two formats
    if d == "PAID BY AUTOPAY - THANK YOU" {
        "PAYMENT - THANK YOU".into()
    } else {
        d
    }
}

struct Raw {
    txn: NaiveDate,
    post: NaiveDate,
    amount: f64,
    description: String,
}

/// Number repeats of the same (date, amount, description, post date) so
/// genuine duplicates stay distinct rows.
fn with_dedup(rows: Vec<Raw>) -> Vec<BankRow> {
    let mut seen: HashMap<(NaiveDate, Option<u64>, String, NaiveDate), u32> = HashMap::new();
    rows.into_iter()
        .map(|r| {
            let n = seen.entry((r.txn, super::fkey(Some(r.amount)), r.description.clone(), r.post)).or_insert(0);
            *n += 1;
            BankRow {
                account: String::new(),
                currency: "HKD".into(),
                txn_date: r.txn.format("%Y-%m-%d").to_string(),
                amount: Some(r.amount),
                balance: None,
                description: r.description,
                dedup: format!("{}|{}", r.post.format("%Y-%m-%d"), n),
            }
        })
        .collect()
}

fn raw(txn: NaiveDate, post: NaiveDate, amount: f64, desc: &str) -> Raw {
    Raw { txn, post, amount, description: canonical_description(desc) }
}

pub struct HsbcCardSource;

static SPEC: SourceSpec = SourceSpec {
    key: "hsbc_card",
    kind: Kind::Bank,
    currency: Some("HKD"),
    balance_method: BalanceMethod::None,
    cashflow: false,
    label: "HSBC credit card",
};

impl HsbcCardSource {
    fn parse_csv(path: &Path) -> Result<Vec<BankRow>> {
        let (header, rows) = dict_rows(&std::fs::read(path)?)?;
        let get = |row: &[Option<String>], name: &str| -> Option<String> {
            header.iter().position(|h| h == name).and_then(|i| row[i].clone())
        };
        let d = |s: Option<String>| -> Result<NaiveDate> {
            let s = s.ok_or_else(|| anyhow!("{}: missing date", path.display()))?;
            let parts: Vec<&str> = textfmt::strip(&s).split('/').collect();
            let [dd, mm, yy] = parts[..] else { bail!("{}: bad date {s:?}", path.display()) };
            let num = |x: &str| textfmt::strip(x).parse::<i32>().map_err(|_| anyhow!("bad date {s:?}"));
            NaiveDate::from_ymd_opt(num(yy)?, num(mm)? as u32, num(dd)? as u32)
                .ok_or_else(|| anyhow!("{}: bad date {s:?}", path.display()))
        };
        let mut out = Vec::new();
        for r in &rows {
            let status = get(r, "Transaction status").filter(|s| !s.is_empty()).unwrap_or_else(|| "POSTED".into());
            if textfmt::strip(&status).to_uppercase() != "POSTED" {
                continue; // pending lines change amount/merchant once posted
            }
            let amt_s = get(r, "Billing amount").filter(|s| !s.is_empty()).unwrap_or_else(|| "0".into());
            let amt_s = amt_s.replace(',', "");
            let amt: f64 = textfmt::strip(&amt_s)
                .parse()
                .map_err(|_| anyhow!("{}: bad amount {amt_s:?}", path.display()))?;
            let desc = get(r, "Description").unwrap_or_default();
            out.push(raw(d(get(r, "Transaction date"))?, d(get(r, "Post date"))?, amt, &desc));
        }
        Ok(with_dedup(out))
    }

    fn parse_pdf(path: &Path) -> Result<Vec<BankRow>> {
        parse_statement_text(&pdf::text(path)?).with_context(|| path.display().to_string())
    }
}

/// The rows of one e-Statement, from its `pdftotext -layout` text.
pub fn parse_statement_text(text: &str) -> Result<Vec<BankRow>> {
    let hay = match text.find("Statement date") {
        Some(i) => &text[i..],
        None => text,
    };
    let m = stmt_date_re()
        .captures(hay)
        .ok_or_else(|| anyhow!("no statement date found"))?;
    let stmt_month = month_num(&m[2]).ok_or_else(|| anyhow!("bad statement month"))?;
    let stmt_year: i32 = m[3].parse()?;
    let stmt_day: u32 = m[1].parse()?;
    NaiveDate::from_ymd_opt(stmt_year, stmt_month, stmt_day)
        .ok_or_else(|| anyhow!("bad statement date"))?;
    let d = |day: &str, mon: &str| -> Result<NaiveDate> {
        let mo = month_num(mon).unwrap();
        // a January statement carries December lines: month after the
        // statement's month means the previous year
        let year = stmt_year - i32::from(mo > stmt_month);
        NaiveDate::from_ymd_opt(year, mo, day.parse()?)
            .ok_or_else(|| anyhow!("bad date {day}{mon}"))
    };
    let mut out = Vec::new();
    for line in textfmt::splitlines(text) {
        let Some(r) = pdf_row_re().captures(line) else { continue };
        if month_num(&r[2]).is_none() || month_num(&r[4]).is_none() {
            continue;
        }
        let amt: f64 = r[6].replace(',', "").parse()?;
        let amount = if r.get(7).is_some() { amt } else { -amt };
        out.push(raw(d(&r[3], &r[4])?, d(&r[1], &r[2])?, amount, &r[5]));
    }
    Ok(with_dedup(out))
}

impl FileSource for HsbcCardSource {
    fn spec(&self) -> &SourceSpec {
        &SPEC
    }

    fn matches(&self, path: &Path) -> bool {
        match suffix_lower(path).as_str() {
            ".csv" => has_all(&sniff_header(path), &CSV_HEADER),
            ".pdf" => pdf::head_contains(path, 400, &["Statement of HSBC", "Credit Card"]),
            _ => false,
        }
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        let mut rows = Vec::new();
        for p in paths {
            if suffix_lower(p) == ".pdf" {
                rows.extend(Self::parse_pdf(p)?);
            } else {
                rows.extend(Self::parse_csv(p)?);
            }
        }
        Ok(Parsed::Bank(rows))
    }
}
