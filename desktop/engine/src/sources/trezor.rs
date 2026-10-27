//! Trezor cold-wallet parser ->
//! crypto_events (BTC deltas + USD cost basis).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use super::csvread::{cast_f64, read_table};
use super::{has_all, name_lower, sniff_header, BalanceMethod, CryptoRow, FileSource, Kind, Parsed, SourceSpec};

const TREZOR_HEADERS: [&str; 5] = ["Timestamp", "Transaction ID", "Amount", "Amount unit", "Fiat (USD)"];

pub struct TrezorSource;

static SPEC: SourceSpec = SourceSpec {
    key: "trezor",
    kind: Kind::CryptoWallet,
    currency: None,
    balance_method: BalanceMethod::Reported,
    cashflow: false,
    label: "Trezor (cold wallet)",
};

impl FileSource for TrezorSource {
    fn spec(&self) -> &SourceSpec {
        &SPEC
    }

    fn matches(&self, path: &Path) -> bool {
        name_lower(path).starts_with("bitcoin") || has_all(&sniff_header(path), &TREZOR_HEADERS)
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        // rows are unique on (ts, txid); of a duplicate group the first one
        // is kept. Rows of one tx id
        // carry the same type and amount in every export seen so far.
        let mut seen: HashSet<(Option<i64>, Option<String>)> = HashSet::new();
        let mut out = Vec::new();
        for f in paths {
            let t = read_table(&std::fs::read(f)?)?;
            let (ts_c, ty, amt, fiat, txid, label) = (
                t.col("Timestamp")?,
                t.col("Type")?,
                t.col("Amount")?,
                t.col("Fiat (USD)")?,
                t.col("Transaction ID")?,
                t.col("Label")?,
            );
            for r in &t.rows {
                let ts = match &r[ts_c] {
                    None => None,
                    Some(s) => Some(
                        s.parse::<i64>().map_err(|_| anyhow!("{}: bad Timestamp {s:?}", f.display()))? * 1000,
                    ),
                };
                if !seen.insert((ts, r[txid].clone())) {
                    continue;
                }
                let ts = ts.ok_or_else(|| anyhow!("{}: row without Timestamp", f.display()))?;
                let amount = cast_f64(&r[amt])?;
                let usd = cast_f64(&r[fiat].as_ref().map(|s| s.replace(',', "").trim_matches('"').to_string()))?;
                // RECV credits the wallet, anything else (SENT, ...) debits it
                let delta = match amount {
                    Some(a) if r[ty].as_deref() == Some("RECV") => a,
                    Some(a) => -a,
                    None => anyhow::bail!("{}: row without Amount", f.display()),
                };
                let detail = match (&r[ty], &r[label]) {
                    (Some(t), Some(l)) if !l.is_empty() => Some(format!("{t}: {l}")),
                    (Some(t), Some(_)) | (Some(t), None) => Some(t.clone()),
                    (None, _) => None,
                };
                let detail = match (detail, &r[txid]) {
                    (Some(d), Some(id)) => {
                        let short: String = id.chars().take(12).collect();
                        format!("{d} txid={short}")
                    }
                    _ => String::new(), // concat_str with a null is null -> ""
                };
                out.push(CryptoRow { ts, asset: "BTC".into(), delta, usd, kind: "trezor-tx".into(), detail });
            }
        }
        Ok(Parsed::Crypto(out))
    }
}
