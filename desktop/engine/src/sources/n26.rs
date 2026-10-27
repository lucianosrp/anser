//! N26 parser. EUR, no balance column --
//! balance is derived via cumsum in assembly.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::NaiveDate;

use super::csvread::{cast_f64, infer_str_column, read_table, Cell};
use super::{fkey, has_all, name_lower, sniff_header, BalanceMethod, BankRow, FileSource, Kind, Parsed, SourceSpec};

const N26_HEADERS: [&str; 5] = ["Booking Date", "Value Date", "Partner Name", "Account Name", "Amount (EUR)"];

pub struct N26Source;

static SPEC: SourceSpec = SourceSpec {
    key: "n26",
    kind: Kind::Bank,
    currency: Some("EUR"),
    balance_method: BalanceMethod::Cumsum,
    cashflow: true,
    label: "N26",
};

impl FileSource for N26Source {
    fn spec(&self) -> &SourceSpec {
        &SPEC
    }

    fn matches(&self, path: &Path) -> bool {
        name_lower(path).starts_with("n26") || has_all(&sniff_header(path), &N26_HEADERS)
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        // N26 has no balance column, so date+partner+amount alone can't tell
        // two genuinely identical same-day purchases apart (the classic
        // false-dedup: a double-charged subscription would collapse to one
        // row). Value Date + Payment Reference join both the unique() subset
        // and the dedup column that ingest folds into the row's natural key.
        type Key = (Option<String>, Cell, Option<u64>, Cell, Cell, Cell);
        let mut seen: HashSet<Key> = HashSet::new();
        let mut out = Vec::new();
        for f in paths {
            let mut t = read_table(&std::fs::read(f)?)?;
            let ix = |n: &str| t.col(n);
            let (bd, vd, pn, an, amt, pr) = (
                ix("Booking Date")?,
                ix("Value Date")?,
                ix("Partner Name")?,
                ix("Account Name")?,
                ix("Amount (EUR)")?,
                ix("Payment Reference")?,
            );
            // scan_csv infers a dtype per column; a text column that happens
            // to be all-numeric is read as a number and re-stringified
            for c in [vd, pn, an, pr] {
                let mut col: Vec<Cell> = t.rows.iter().map(|r| r[c].clone()).collect();
                infer_str_column(&mut col)?;
                for (r, v) in t.rows.iter_mut().zip(col) {
                    r[c] = v;
                }
            }
            for r in &t.rows {
                let txn_date = match &r[bd] {
                    None => None,
                    Some(s) => Some(
                        NaiveDate::parse_from_str(s, "%Y-%m-%d")
                            .map_err(|_| anyhow!("{}: bad Booking Date {s:?}", f.display()))?
                            .format("%Y-%m-%d")
                            .to_string(),
                    ),
                };
                let amount = cast_f64(&r[amt])?;
                let key: Key =
                    (txn_date.clone(), r[pn].clone(), fkey(amount), r[an].clone(), r[vd].clone(), r[pr].clone());
                if !seen.insert(key) {
                    continue;
                }
                out.push(BankRow {
                    account: r[an].clone().unwrap_or_default(),
                    currency: "EUR".into(),
                    txn_date: txn_date.unwrap_or_default(),
                    amount,
                    balance: None,
                    description: r[pn].clone().unwrap_or_default(),
                    dedup: format!(
                        "{}|{}",
                        r[vd].as_deref().unwrap_or_default(),
                        r[pr].as_deref().unwrap_or_default()
                    ),
                });
            }
        }
        Ok(Parsed::Bank(out))
    }
}
