//! Revolut parsers: old GBP statement,
//! new multi-currency statement, old BTC ledger.
//!
//! The old ("legacy") Revolut exports are not properly CSV-quoted:
//! description fields can contain literal unquoted commas (FX-rate/fee notes,
//! or a place name wrapped in doubled quotes like ""Crown, Anchor's
//! Globe""), which shifts a naive comma split. The trailing 7 fields (Paid
//! Out, Paid In, Exchange Out, Exchange In, Balance, Category, Notes) are
//! fixed-width from the right in both the GBP and BTC legacy formats, so we
//! anchor on those and treat everything between the leading date field and
//! that fixed tail as the description.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime};

use super::csvread::{cast_f64, read_table, Table};
use super::grid::{self, ReadOpts, Val};
use super::textfmt;
use super::{
    fkey, has_all, name_lower, read_utf8, sniff_header, BalanceMethod, BankRow, CryptoRow, FileSource, Kind, Parsed,
    SourceSpec,
};

const REVOLUT_NEW_HEADERS: [&str; 9] =
    ["Type", "Product", "Started Date", "Completed Date", "Amount", "Fee", "Currency", "State", "Balance"];
const REVOLUT_BTC_HEADERS: [&str; 3] = ["Paid Out (BTC)", "Paid In (BTC)", "Balance (BTC)"];

struct LegacyRow {
    date_str: String,
    description: String,
    amount: f64,
    balance: f64,
    exchange_out: String,
}

/// An already-stripped decimal or float literal (`inf`, `nan`, exponents
/// accepted; `_` digit separators rejected).
fn parse_float(s: &str) -> Option<f64> {
    if s.contains('_') {
        return None;
    }
    s.parse().ok()
}

fn parse_legacy_rows(paths: &[PathBuf]) -> Result<Vec<LegacyRow>> {
    let mut rows = Vec::new();
    for f in paths {
        let text = read_utf8(f)?;
        for line in textfmt::splitlines(&text).into_iter().skip(1) {
            if textfmt::strip(line).is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split(',').collect();
            let n = parts.len();
            if n < 9 {
                continue;
            }
            let date_str = textfmt::strip(parts[0]);
            let balance_str = textfmt::strip(parts[n - 3]);
            if date_str.is_empty() || balance_str.is_empty() {
                continue;
            }
            let Some(balance) = parse_float(balance_str) else { continue };
            let money = |s: &str| -> Result<f64> {
                let s = textfmt::strip(s);
                if s.is_empty() {
                    Ok(0.0)
                } else {
                    parse_float(s).ok_or_else(|| anyhow!("{}: could not convert {s:?} to float", f.display()))
                }
            };
            let paid_out = money(parts[n - 7])?;
            let paid_in = money(parts[n - 6])?;
            let description = textfmt::strip(&parts[1..n - 7].join(",")).trim_matches('"').to_string();
            rows.push(LegacyRow {
                date_str: date_str.to_string(),
                description,
                amount: paid_in - paid_out,
                balance,
                exchange_out: textfmt::strip(parts[n - 5]).to_string(),
            });
        }
    }
    Ok(rows)
}

pub struct RevolutOldGbpSource;

static OLD_GBP: SourceSpec = SourceSpec {
    key: "revolut_old_gbp",
    kind: Kind::Bank,
    currency: Some("GBP"),
    balance_method: BalanceMethod::Reported,
    cashflow: false,
    label: "Revolut (GBP, legacy)",
};

impl FileSource for RevolutOldGbpSource {
    fn spec(&self) -> &SourceSpec {
        &OLD_GBP
    }

    fn matches(&self, path: &Path) -> bool {
        let name = name_lower(path);
        super::suffix_lower(path) != ".pdf" && name.contains("revolut") && name.contains("gbp")
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for r in parse_legacy_rows(paths)? {
            // "%-d %b %Y", e.g. "3 Jan 2021"
            let d = NaiveDate::parse_from_str(&r.date_str, "%-d %b %Y")
                .map_err(|_| anyhow!("bad Revolut date {:?}", r.date_str))?
                .format("%Y-%m-%d")
                .to_string();
            if !seen.insert((d.clone(), r.description.clone(), fkey(Some(r.amount)), fkey(Some(r.balance)))) {
                continue;
            }
            out.push(BankRow {
                account: "GBP".into(),
                currency: "GBP".into(),
                txn_date: d,
                amount: Some(r.amount),
                balance: Some(r.balance),
                description: r.description,
                dedup: String::new(),
            });
        }
        Ok(Parsed::Bank(out))
    }
}

/// The Excel export as the same table the CSV reader gives: the header row
/// found among the first rows, every cell as the text the CSV holds (a number
/// as its shortest round-trip text, so Amount and Balance parse to the same
/// f64 and the row hashes as its CSV twin). None when no row is the Revolut
/// header. `limit` bounds the rows read (a sniff).
fn xlsx_table(path: &Path, limit: Option<usize>) -> Result<Option<Table>> {
    let grid = grid::read(path, &ReadOpts::default(), limit)?;
    let text = |c: &Val| c.text();
    let Some(at) = grid
        .iter()
        .take(30)
        .position(|r| REVOLUT_NEW_HEADERS.iter().all(|h| r.iter().any(|c| matches!(c, Val::Text(_)) && text(c) == *h)))
    else {
        return Ok(None);
    };
    let header: Vec<String> = grid[at].iter().map(text).collect();
    let rows = grid[at + 1..]
        .iter()
        .map(|r| {
            let mut row: Vec<Option<String>> = r
                .iter()
                .take(header.len())
                .map(|c| match c {
                    Val::Empty => None,
                    Val::Text(s) => Some(s.clone()),
                    Val::Num(x) => Some(textfmt::float_repr(*x)),
                })
                .collect();
            row.resize(header.len(), None);
            row
        })
        .collect();
    Ok(Some(Table { header, rows }))
}

pub struct RevolutNewSource;

static NEW: SourceSpec = SourceSpec {
    key: "revolut_new",
    kind: Kind::Bank,
    currency: None,
    balance_method: BalanceMethod::Reported,
    cashflow: false,
    label: "Revolut",
};

impl FileSource for RevolutNewSource {
    fn spec(&self) -> &SourceSpec {
        &NEW
    }

    fn matches(&self, path: &Path) -> bool {
        if super::suffix_lower(path) == ".pdf" {
            return super::pdf::text(path).is_ok_and(|t| super::revolut_pdf::is_statement(&t));
        }
        // a workbook is Revolut's only by its header row, never by its name
        if grid::is_excel(path) {
            return xlsx_table(path, Some(30)).is_ok_and(|t| t.is_some());
        }
        name_lower(path).starts_with("account-statement") || has_all(&sniff_header(path), &REVOLUT_NEW_HEADERS)
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        type Key = (bool, Option<String>, Option<String>, Option<u64>, Option<u64>, Option<String>);
        let mut seen: HashSet<Key> = HashSet::new();
        let mut out = Vec::new();
        for f in paths {
            if super::suffix_lower(f) == ".pdf" {
                for r in super::revolut_pdf::parse_statement_text(&super::pdf::text(f)?).map_err(|e| e.context(f.display().to_string()))? {
                    let key: Key = (r.account != r.currency, Some(r.currency.clone()), Some(r.txn_date.clone()), fkey(r.amount), fkey(r.balance), Some(r.description.clone()));
                    if seen.insert(key) {
                        out.push(r);
                    }
                }
                continue;
            }
            let t = if grid::is_excel(f) {
                xlsx_table(f, None)?.ok_or_else(|| anyhow!("{}: no Revolut header row (Type, Product, Started Date, ...)", f.display()))?
            } else {
                read_table(&std::fs::read(f)?)?
            };
            let (prod, state, ccy, done, amt, bal, desc) = (
                t.col("Product")?,
                t.col("State")?,
                t.col("Currency")?,
                t.col("Completed Date")?,
                t.col("Amount")?,
                t.col("Balance")?,
                t.col("Description")?,
            );
            for r in &t.rows {
                if r[state].as_deref() != Some("COMPLETED") {
                    continue;
                }
                let currency = r[ccy].as_ref().map(|s| s.trim().to_string());
                let txn_date = match &r[done] {
                    None => None,
                    Some(s) => Some(
                        // the CSV's "2026-07-02 00:10:00"; a workbook's midnight
                        // date cell reads as the bare date
                        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                            .map(|d| d.date())
                            .or_else(|_| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
                            .map_err(|_| anyhow!("{}: bad Completed Date {s:?}", f.display()))?
                            .format("%Y-%m-%d")
                            .to_string(),
                    ),
                };
                let amount = cast_f64(&r[amt])?;
                let balance = cast_f64(&r[bal])?;
                let savings = r[prod].as_deref().is_some_and(|p| p.trim().eq_ignore_ascii_case("Savings"));
                let key: Key = (savings, currency.clone(), txn_date.clone(), fkey(amount), fkey(balance), r[desc].clone());
                if !seen.insert(key) {
                    continue;
                }
                let currency = currency.unwrap_or_default();
                // a vault is its own pot of money: its balance chain must not
                // interleave with the current account's
                let account = if savings { format!("{currency} Savings") } else { currency.clone() };
                out.push(BankRow {
                    account,
                    currency,
                    txn_date: txn_date.unwrap_or_default(),
                    amount,
                    balance,
                    description: r[desc].clone().unwrap_or_default(),
                    dedup: String::new(),
                });
            }
        }
        Ok(Parsed::Bank(out))
    }
}

/// Historical Revolut BTC ledger -> crypto_events (exchange='revolut').
///
/// Cost basis ("usd") is only populated when the CSV's own Exchange Out fiat
/// leg is denominated in USD; GBP/EUR-denominated buys keep usd=None (the raw
/// fiat amount + currency is preserved in `detail` instead) rather than
/// silently mislabeling a non-USD amount as USD cost basis.
pub struct RevolutBtcSource;

static BTC: SourceSpec = SourceSpec {
    key: "revolut",
    kind: Kind::CryptoWallet,
    currency: None,
    balance_method: BalanceMethod::Reported,
    cashflow: false,
    label: "Revolut (BTC ledger)",
};

impl FileSource for RevolutBtcSource {
    fn spec(&self) -> &SourceSpec {
        &BTC
    }

    fn matches(&self, path: &Path) -> bool {
        let name = name_lower(path);
        super::suffix_lower(path) != ".pdf" && (name.contains("revolut") && name.contains("btc")) || has_all(&sniff_header(path), &REVOLUT_BTC_HEADERS)
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for r in parse_legacy_rows(paths)? {
            // datetime.strptime(..., "%d %b %Y"), midnight UTC
            let d = NaiveDate::parse_from_str(&r.date_str, "%d %b %Y")
                .map_err(|_| anyhow!("bad Revolut date {:?}", r.date_str))?;
            let ts = d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
            if !seen.insert((ts, fkey(Some(r.amount)), r.description.clone())) {
                continue;
            }
            let exch = textfmt::strip(&r.exchange_out);
            let mut usd = None;
            if exch.to_uppercase().starts_with("USD") {
                if let Some(last) = textfmt::split_ws(exch).last() {
                    usd = parse_float(textfmt::strip(&last.replace(',', "")));
                }
            }
            out.push(CryptoRow {
                ts,
                asset: "BTC".into(),
                delta: r.amount,
                usd,
                kind: "revolut-btc".into(),
                detail: format!("{} | exchange_out={}", r.description, exch),
            });
        }
        Ok(Parsed::Crypto(out))
    }
}
