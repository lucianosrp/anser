//! Revolut's account-statement PDF (the app's "Statement", PDF flavour).
//!
//! `pdftotext -layout` (the helper Mox and the HSBC card share) gives a
//! "Balance summary" (per product: opening, money out, money in, closing)
//! and one transactions table per product: "Account transactions from ..."
//! for the account, then the vaults' own table. A row is
//!
//!   Date | Description | Money out | Money in | Balance
//!
//! with the amounts left-aligned under their column heading (the heading
//! repeats on every page, so the columns are read from the latest one).
//! Continuation lines (Card: ..., To: ..., FX rates, the foreign amount) are
//! not part of the row.
//!
//! Rows come out as `revolut_new` rows shaped exactly as the CSV / Excel
//! export's (account = currency, "<currency> Savings" for the vaults;
//! amount signed; the balance column; the first description line), so the
//! same transaction hashes the same whichever file it came from.
//!
//! The statement is refused, loudly, when its rows do not add up to its own
//! balance summary, per product.

use std::collections::HashSet;
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Result};
use chrono::NaiveDate;
use regex::Regex;

use super::textfmt;
use super::BankRow;

fn row_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*(\d{1,2})\s+([A-Za-z]{3})\s+(\d{4})\s+(.*)$").unwrap())
}

fn amount_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(-?)([£€$])(-?)([\d,]+\.\d{2})").unwrap())
}

/// Is this the text of a Revolut account statement?
pub fn is_statement(text: &str) -> bool {
    text.contains("Balance summary")
        && text.contains("Account transactions from")
        && text.contains("Money out")
        && text.contains("Revolut")
}

fn cents(sign: &str, digits: &str) -> Result<i64> {
    let whole: f64 = digits.replace(',', "").parse().map_err(|_| anyhow!("bad amount {digits:?}"))?;
    let c = (whole * 100.0).round() as i64;
    Ok(if sign.chars().any(|c| c == '-') { -c } else { c })
}

fn currency_of(symbol: &str) -> Result<&'static str> {
    match symbol {
        "£" => Ok("GBP"),
        "€" => Ok("EUR"),
        "$" => Ok("USD"),
        s => bail!("unknown currency symbol {s:?}"),
    }
}

fn month(m: &str) -> Option<u32> {
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
        .iter()
        .position(|x| x.eq_ignore_ascii_case(m))
        .map(|i| i as u32 + 1)
}

struct Section {
    savings: bool,
    currency: Option<&'static str>,
    rows: Vec<(NaiveDate, i64, Option<i64>, String)>, // date, amount, balance, description
}

/// The rows of one statement, from its `pdftotext -layout` text.
pub fn parse_statement_text(text: &str) -> Result<Vec<BankRow>> {
    // (out, in, balance) column starts from the latest table heading
    let mut cols: Option<(usize, usize, usize)> = None;
    let mut sections: Vec<Section> = Vec::new();
    // balance summary: (savings, opening, out, in, closing)
    let mut summary: Vec<(bool, i64, i64, i64, i64)> = Vec::new();

    for line in textfmt::splitlines(text) {
        let t = textfmt::strip(line);
        let lower = t.to_lowercase();
        if t.starts_with("Date") && t.contains("Description") && t.contains("Money out") {
            let at = |w: &str| line.find(w).map(|b| line[..b].chars().count());
            cols = Some((
                at("Money out").ok_or_else(|| anyhow!("no Money out column"))?,
                at("Money in").ok_or_else(|| anyhow!("no Money in column"))?,
                at("Balance").ok_or_else(|| anyhow!("no Balance column"))?,
            ));
            continue;
        }
        if lower.contains("transactions from") {
            sections.push(Section { savings: lower.contains("vault"), currency: None, rows: Vec::new() });
            cols = None;
            continue;
        }
        if sections.is_empty() {
            // the balance summary: product, opening, out, in, closing
            let savings = if t.starts_with("Account (E-Money)") {
                false
            } else if t.starts_with("Personal and Group Vaults") {
                true
            } else {
                continue;
            };
            let a: Vec<_> = amount_re().captures_iter(line).collect();
            if a.len() == 4 {
                let v = |i: usize| cents(&a[i][1], &a[i][4]).map(|c| if a[i][3].is_empty() { c } else { -c });
                summary.push((savings, v(0)?, v(1)?, v(2)?, v(3)?));
            }
            continue;
        }
        let Some(r) = row_re().captures(line) else { continue };
        let (Some((c_out, c_in, c_bal)), Some(sec)) = (cols, sections.last_mut()) else { continue };
        let mo = month(&r[2]).ok_or_else(|| anyhow!("bad month in {line:?}"))?;
        let date = NaiveDate::from_ymd_opt(r[3].parse()?, mo, r[1].parse()?).ok_or_else(|| anyhow!("bad date in {line:?}"))?;
        let body_at = line.len() - r[4].len();
        let (mut out, mut inn, mut bal, mut desc_end) = (None, None, None, line.len());
        for m in amount_re().captures_iter(line) {
            let whole = m.get(0).unwrap();
            if whole.start() < body_at {
                continue;
            }
            desc_end = desc_end.min(whole.start());
            let col = line[..whole.start()].chars().count();
            let ccy = currency_of(&m[2])?;
            sec.currency.get_or_insert(ccy);
            let v = cents(&format!("{}{}", &m[1], &m[3]), &m[4])?;
            let nearest = [(c_out, 0), (c_in, 1), (c_bal, 2)].into_iter().min_by_key(|(c, _)| c.abs_diff(col)).unwrap();
            if nearest.0.abs_diff(col) > 6 {
                bail!("an amount sits under no column: {line:?}");
            }
            let slot = match nearest.1 {
                0 => &mut out,
                1 => &mut inn,
                _ => &mut bal,
            };
            if slot.replace(v).is_some() {
                bail!("two amounts in one column: {line:?}");
            }
        }
        let amount = match (out, inn) {
            (Some(o), None) => -o,
            (None, Some(i)) => i,
            _ => bail!("a row needs one amount, money out or money in: {line:?}"),
        };
        let desc = textfmt::strip(&line[body_at..desc_end]).to_string();
        sec.rows.push((date, amount, bal, desc));
    }

    if sections.is_empty() || summary.is_empty() {
        bail!("not a Revolut statement: no balance summary or no transactions table");
    }
    // the rows must add up to the statement's own figures, per product
    for savings in [false, true] {
        let rows: Vec<_> = sections.iter().filter(|s| s.savings == savings).flat_map(|s| s.rows.iter()).collect();
        let name = if savings { "Vaults" } else { "Account" };
        let Some(&(_, opening, _, _, closing)) = summary.iter().find(|s| s.0 == savings) else {
            if rows.is_empty() {
                continue;
            }
            bail!("{name}: rows without a line in the balance summary");
        };
        let net: i64 = rows.iter().map(|r| r.1).sum();
        // the statement's out / in totals also count reversed pairs that its
        // table leaves out, so only the net movement must tie
        if opening + net != closing {
            bail!(
                "{name}: the rows take the balance from {:.2} to {:.2}; the statement says {:.2} to {:.2}",
                opening as f64 / 100.0,
                (opening + net) as f64 / 100.0,
                opening as f64 / 100.0,
                closing as f64 / 100.0
            );
        }
        if let Some(last) = rows.last().and_then(|r| r.2) {
            if last != closing {
                bail!("{name}: the last row's balance {:.2} is not the closing balance {:.2}", last as f64 / 100.0, closing as f64 / 100.0);
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for s in &sections {
        let ccy = s.currency.unwrap_or("GBP").to_string();
        let account = if s.savings { format!("{ccy} Savings") } else { ccy.clone() };
        for (d, a, b, desc) in &s.rows {
            let row = BankRow {
                account: account.clone(),
                currency: ccy.clone(),
                txn_date: d.format("%Y-%m-%d").to_string(),
                amount: Some(*a as f64 / 100.0),
                balance: b.map(|b| b as f64 / 100.0),
                description: desc.clone(),
                dedup: String::new(),
            };
            // the CSV reader's own de-duplication of identical rows
            if seen.insert((row.account.clone(), row.txn_date.clone(), super::fkey(row.amount), super::fkey(row.balance), row.description.clone())) {
                out.push(row);
            }
        }
    }
    Ok(out)
}
