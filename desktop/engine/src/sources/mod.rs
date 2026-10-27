//! File sources: one module
//! per export format. To add a source: write the module, add one line to
//! `code_sources()` -- or, for a plain bank CSV / Excel export, write a
//! format definition instead (`format.rs`, desktop/docs/formats.md).
//!
//! `parse()` output contract, by `SourceSpec::kind`:
//!   Bank        -> BankRow: account, currency, txn_date (YYYY-MM-DD),
//!                  amount/balance (None = null), description, dedup
//!   FundUnits   -> FundRow (HSBC unit-trust executions)
//!   CryptoWallet-> CryptoRow: ts (unix ms), asset, delta, usd, kind, detail
//!
//! The API sources (Binance, OKX, IBKR) are fetchers and live in fetch/.

use std::collections::HashSet;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::Result;

pub mod csvread;
pub mod format;
pub mod grid;
pub mod hsbc;
pub mod hsbc_card;
pub mod hsbc_sms;
pub mod mox;
pub mod n26;
pub mod pdf;
pub mod textfmt;
pub mod revolut;
pub mod revolut_pdf;
pub mod trezor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bank,
    FundUnits,
    CryptoWallet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceMethod {
    /// a balance column
    Reported,
    /// derive from amounts
    Cumsum,
    /// a card: kept out of balances / net worth
    None,
}

#[derive(Debug, Clone)]
pub struct SourceSpec {
    /// 'n26', 'hsbc', 'revolut_old_gbp', 'revolut_new', 'trezor', ...
    pub key: &'static str,
    pub kind: Kind,
    /// native currency; None if multi-currency (the column decides)
    pub currency: Option<&'static str>,
    pub balance_method: BalanceMethod,
    /// its flows are part of the monthly cash flow (the overview's in/out
    /// series): the everyday accounts, not a closed or side account. Only
    /// meaningful for a bank with a balance method and one currency.
    pub cashflow: bool,
    /// display name for the dashboard
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BankRow {
    pub account: String,
    pub currency: String,
    /// ISO date, or "" when the export had none
    pub txn_date: String,
    pub amount: Option<f64>,
    pub balance: Option<f64>,
    pub description: String,
    /// optional natural-key discriminator (see ingest::natural_key)
    pub dedup: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FundRow {
    pub reference: String,
    pub trade_date: String,
    pub fund: String,
    pub units: f64,
    pub px: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CryptoRow {
    pub ts: i64,
    pub asset: String,
    pub delta: f64,
    pub usd: Option<f64>,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Bank(Vec<BankRow>),
    FundUnits(Vec<FundRow>),
    Crypto(Vec<CryptoRow>),
}

impl Parsed {
    /// `parsed.height`: rows parsed from the file (ingested_files.row_count)
    pub fn len(&self) -> usize {
        match self {
            Parsed::Bank(r) => r.len(),
            Parsed::FundUnits(r) => r.len(),
            Parsed::Crypto(r) => r.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub trait FileSource: Send + Sync {
    fn spec(&self) -> &SourceSpec;
    fn matches(&self, path: &Path) -> bool;
    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed>;
}

/// Registry, in matching order: the first source whose matches() accepts a
/// file wins, so the order is part of the contract. The code sources come
/// first, then the format definitions in effect (`format::formats()`).
pub fn file_sources() -> Vec<Box<dyn FileSource>> {
    let mut v = code_sources();
    v.extend(format::formats().into_iter().map(|f| Box::new(f) as Box<dyn FileSource>));
    v
}

/// The sources written in Rust (this directory's modules), in matching order.
pub fn code_sources() -> Vec<Box<dyn FileSource>> {
    vec![
        Box::new(n26::N26Source),
        Box::new(hsbc_card::HsbcCardSource), // credit card: transaction-history CSV + e-Statement PDFs
        Box::new(hsbc::HsbcSource),
        Box::new(mox::MoxSource),          // Mox HK monthly statement PDFs
        Box::new(hsbc_sms::HsbcSmsSource), // SMS backup XML -> real unit-trust executions
        Box::new(revolut::RevolutOldGbpSource),
        Box::new(revolut::RevolutNewSource),
        Box::new(revolut::RevolutBtcSource), // historical Revolut BTC ledger -> crypto_events
        Box::new(trezor::TrezorSource),      // cold wallet BTC -> crypto_events
    ]
}

/// The spec of a file source by key ('hsbc', 'n26', ...).
pub fn spec(key: &str) -> Option<SourceSpec> {
    file_sources().into_iter().map(|s| s.spec().clone()).find(|s| s.key == key)
}

/// The source that claims `path`: the first in registry order. A
/// spreadsheet (.xlsx / .xls) goes to Revolut's own Excel export (found by
/// its header row) or to a format definition: the other code sources read
/// text.
pub fn source_for(path: &Path) -> Option<Box<dyn FileSource>> {
    let candidates = if grid::is_excel(path) {
        let mut v: Vec<Box<dyn FileSource>> = vec![Box::new(revolut::RevolutNewSource)];
        v.extend(format::formats().into_iter().map(|f| Box::new(f) as Box<dyn FileSource>));
        v
    } else {
        file_sources()
    };
    candidates.into_iter().find(|s| s.matches(path))
}

/// Bucket candidate files by the first source whose matches() accepts them.
/// Buckets keep first-seen order over the sorted paths.
pub fn classify(paths: &[PathBuf]) -> Vec<(Box<dyn FileSource>, Vec<PathBuf>)> {
    let mut sorted = paths.to_vec();
    sorted.sort();
    let mut buckets: Vec<(Box<dyn FileSource>, Vec<PathBuf>)> = Vec::new();
    for p in sorted {
        let Some(src) = source_for(&p) else {
            continue;
        };
        match buckets.iter_mut().find(|(s, _)| s.spec().key == src.spec().key) {
            Some((_, files)) => files.push(p),
            None => buckets.push((src, vec![p])),
        }
    }
    buckets
}

/// First-line CSV header as a set of stripped, dequoted column names. Read
/// as text: a UTF-8 BOM dropped, invalid bytes replaced, the line ends at \n
/// or a lone \r.
pub fn sniff_header(path: &Path) -> HashSet<String> {
    let Ok(f) = std::fs::File::open(path) else {
        return HashSet::new();
    };
    let mut line = Vec::new();
    for b in BufReader::new(f).bytes() {
        let Ok(b) = b else { return HashSet::new() };
        if b == b'\n' || b == b'\r' {
            break;
        }
        line.push(b);
    }
    let line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&line);
    let text = String::from_utf8_lossy(line);
    textfmt::strip(&text)
        .split(',')
        .map(|c| textfmt::strip(c).trim_matches('"').to_string())
        .collect()
}

/// The file name, lowercased (`path.name.lower()`).
pub(crate) fn name_lower(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// `path.suffix.lower()`
pub(crate) fn suffix_lower(path: &Path) -> String {
    path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default()
}

pub(crate) fn has_all(header: &HashSet<String>, want: &[&str]) -> bool {
    want.iter().all(|w| header.contains(*w))
}

/// Float key for de-duplicating rows: -0.0 == 0.0 and every NaN is one value.
pub(crate) fn fkey(v: Option<f64>) -> Option<u64> {
    v.map(|x| if x == 0.0 { 0 } else if x.is_nan() { f64::NAN.to_bits() } else { x.to_bits() })
}

/// Read a whole file as text the way `read_text(encoding="utf-8")` does.
pub(crate) fn read_utf8(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8(bytes)?)
}
