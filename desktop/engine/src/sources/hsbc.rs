//! HSBC current account parser. HKD, has a
//! reported balance column.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::NaiveDate;

use super::csvread::{cast_f64, read_table, Cell};
use super::{fkey, has_all, name_lower, sniff_header, BalanceMethod, BankRow, FileSource, Kind, Parsed, SourceSpec};

pub struct HsbcSource;

static SPEC: SourceSpec = SourceSpec {
    key: "hsbc",
    kind: Kind::Bank,
    currency: Some("HKD"),
    balance_method: BalanceMethod::Reported,
    cashflow: true,
    label: "HSBC",
};

/// `.str.replace_all(",", "").str.strip_chars().cast(pl.Float64)`
fn money(v: &Cell) -> Result<Option<f64>> {
    cast_f64(&v.as_ref().map(|s| s.replace(',', "").trim().to_string()))
}

impl FileSource for HsbcSource {
    fn spec(&self) -> &SourceSpec {
        &SPEC
    }

    fn matches(&self, path: &Path) -> bool {
        let name = name_lower(path);
        // the credit-card CSV shares the TransactionHistory name; only the
        // current account has a Balance column
        (name.starts_with("transactionhistory") || name.starts_with("transhist"))
            && has_all(&sniff_header(path), &["Date", "Balance"])
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        type Key = (Option<String>, Option<String>, Option<u64>, Option<u64>);
        let mut seen: HashSet<Key> = HashSet::new();
        let mut out = Vec::new();
        for f in paths {
            // Some HSBC exports have stray tab characters; strip them before parsing.
            let raw: Vec<u8> = std::fs::read(f)?.into_iter().filter(|b| *b != b'\t').collect();
            let t = read_table(&raw)?;
            let (d, desc, amt, bal) =
                (t.col("Date")?, t.col("Description")?, t.col("Billing amount")?, t.col("Balance")?);
            for r in &t.rows {
                let txn_date = match &r[d] {
                    None => None,
                    Some(s) => Some(
                        NaiveDate::parse_from_str(s, "%d/%m/%Y")
                            .map_err(|_| anyhow!("{}: bad Date {s:?}", f.display()))?
                            .format("%Y-%m-%d")
                            .to_string(),
                    ),
                };
                // Unicode whitespace trimmed (str::trim)
                let description = r[desc].as_ref().map(|s| s.trim().to_string());
                let amount = money(&r[amt])?;
                let balance = money(&r[bal])?;
                if !seen.insert((txn_date.clone(), description.clone(), fkey(amount), fkey(balance))) {
                    continue;
                }
                out.push(BankRow {
                    account: String::new(),
                    currency: "HKD".into(),
                    txn_date: txn_date.unwrap_or_default(),
                    amount,
                    balance,
                    description: description.unwrap_or_default(),
                    dedup: String::new(),
                });
            }
        }
        Ok(Parsed::Bank(out))
    }
}
