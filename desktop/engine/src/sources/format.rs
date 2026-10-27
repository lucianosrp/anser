//! Declarative format definitions: a bank's CSV or Excel export described in
//! a TOML file instead of a Rust module (desktop/docs/formats.md is the
//! guide). The seed of an extension system: "installing the Barclays
//! extension" is dropping `barclays_uk.toml` into the formats directory.
//!
//! Where definitions come from, in priority order (the first one with an id
//! wins):
//! * the directories set with [`set_dirs`]: the desktop app's config dir
//!   (and its copy of the server's formats); on the server,
//!   `$FINANCE_HOME/formats/` (or `$FINANCE_FORMATS_DIR`) by default
//! * the built-in definitions compiled in (`engine/formats/*.toml`)
//!
//! The code sources (`sources/*.rs`) are always tried first; a definition
//! only gets a file none of them claims. A definition can't reuse a code
//! source's key.
//!
//! Rows come out as [`BankRow`]s with the definition's `id` as the source
//! key, so they hash exactly like a code source's (ingest::natural_key) and
//! every dedupe layer (raw_hash, the near-duplicate guards) applies as is.
//!
//! The directories are re-read whenever a `.toml` in them is added, removed
//! or changed (a stat per file per lookup), so a saved or uploaded
//! definition takes effect at once.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::grid::{self, ReadOpts, Row, Val};
use super::{BalanceMethod, BankRow, FileSource, Kind, Parsed, SourceSpec};

// ---------------------------------------------------------------------------
// The definition
// ---------------------------------------------------------------------------

/// One column name, or several (a description built from two columns).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names(pub Vec<String>);

impl Serialize for Names {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => s.serialize_str(one),
            many => many.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Names {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        Ok(match OneOrMany::deserialize(d)? {
            OneOrMany::One(s) => Names(vec![s]),
            OneOrMany::Many(v) => Names(v),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatKind {
    /// a current or savings account
    #[default]
    Bank,
    /// a credit card: kept out of balances and net worth
    Card,
    /// an e-money wallet (PayPal, Wise, ...): bank-like rows
    Wallet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BalanceKind {
    /// the export has a balance column
    Reported,
    /// the balance is the running sum of the amounts
    Cumsum,
    /// no balance (a card)
    None,
}

/// How the amount's sign reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sign {
    /// negative = money out (most bank exports)
    #[default]
    Signed,
    /// positive = money out (many card exports)
    Inverted,
}

/// How the file is recognized.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detect {
    /// column names the header row must all have (case and surrounding
    /// spaces ignored)
    pub columns: Vec<String>,
    /// an optional file-name glob that must match too (`*` and `?`, any
    /// case), e.g. `"statement_*.csv"`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

/// Which column holds what. Names as in the header row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Columns {
    pub date: String,
    /// one signed amount column...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    /// ...or money out / money in in two columns
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
    /// one column or a list (joined with a space, empty ones skipped)
    pub description: Names,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// a debit/credit marker column ("D"/"C", "Af"/"Bij"): with
    /// `parse.debit_markers`, the amount's sign comes from it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

fn dot() -> String {
    ".".into()
}

fn is_dot(s: &String) -> bool {
    s == "."
}

/// How the cells read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParseOpts {
    /// chrono strftime patterns, tried in order (`%d/%m/%Y`); ISO dates are
    /// always accepted too
    #[serde(default, skip_serializing_if = "names_empty")]
    pub date_format: Names,
    /// "." or ","
    #[serde(default = "dot", skip_serializing_if = "is_dot")]
    pub decimal: String,
    /// default: "," when the decimal is ".", "." when it is ","; "" for none
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thousands: Option<String>,
    #[serde(default, skip_serializing_if = "is_signed")]
    pub sign: Sign,
    /// values of `columns.direction` that mean money out
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub debit_markers: Vec<String>,
    /// rows above the header; default: the header is found (the first row,
    /// within the first 30, that has every `detect.columns`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_rows: Option<usize>,
    /// utf-8 (default), latin-1, windows-1252, utf-16
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    /// CSV separator; default: sniffed (`,` `;` tab `|`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delimiter: Option<String>,
    /// Excel: the sheet; default: the first
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
}

fn names_empty(n: &Names) -> bool {
    n.0.is_empty()
}

fn is_signed(s: &Sign) -> bool {
    *s == Sign::Signed
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for ParseOpts {
    fn default() -> Self {
        ParseOpts {
            date_format: Names::default(),
            decimal: dot(),
            thousands: None,
            sign: Sign::Signed,
            debit_markers: vec![],
            skip_rows: None,
            encoding: None,
            delimiter: None,
            sheet: None,
        }
    }
}

/// How to get this bank's export, for the Supported sources page and the
/// setup wizard (`[export]` in the TOML).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportInfo {
    /// what the file is: "CSV", "Excel"
    pub format: String,
    /// the numbered steps, in plain words
    pub how: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    /// one line: what a file covers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A format definition: one `.toml` file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatDef {
    /// the source key its rows are stored under: `[a-z][a-z0-9_]*`
    pub id: String,
    /// display name
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    /// ISO country code
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default)]
    pub kind: FormatKind,
    /// the account's currency, unless `columns.currency` gives it per row
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// default: none for a card, reported with a balance column, else cumsum
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_method: Option<BalanceKind>,
    /// part of the overview's monthly cash flow (an everyday account)
    #[serde(default, skip_serializing_if = "is_false")]
    pub cashflow: bool,
    /// columns whose values join the row's identity, for exports without a
    /// balance where two same-day rows can be alike (a transaction id)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dedup: Vec<String>,
    pub detect: Detect,
    pub columns: Columns,
    #[serde(default)]
    pub parse: ParseOpts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<ExportInfo>,
}

/// Is `id` a usable source key?
pub fn valid_id(id: &str) -> bool {
    let mut c = id.chars();
    (2..=48).contains(&id.len())
        && c.next().is_some_and(|f| f.is_ascii_lowercase())
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_')
}

/// A name for a new definition's id: "Barclays (UK)" -> "barclays_uk".
pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
        } else if !s.ends_with('_') && !s.is_empty() {
            s.push('_');
        }
    }
    let s = s.trim_end_matches('_').to_string();
    let s = if s.chars().next().is_some_and(|c| c.is_ascii_digit()) { format!("f_{s}") } else { s };
    s.chars().take(48).collect()
}

fn check_date_format(f: &str) -> Result<()> {
    let mut out = String::new();
    write!(out, "{}", NaiveDate::from_ymd_opt(2026, 1, 31).unwrap().and_hms_opt(0, 0, 0).unwrap().format(f))
        .map_err(|_| anyhow!("date_format {f:?} isn't a valid strftime pattern"))
}

impl FormatDef {
    pub fn from_toml(text: &str) -> Result<FormatDef> {
        let def: FormatDef = toml::from_str(text)?;
        def.validate()?;
        Ok(def)
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string(self)?)
    }

    /// Every rule a definition must follow, with the reason in words.
    pub fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            bail!("id {:?}: 2-48 chars, lowercase letters, digits and _, starting with a letter", self.id);
        }
        if super::code_sources().iter().any(|s| s.spec().key == self.id) {
            bail!("id {:?} is a built-in source's key", self.id);
        }
        if self.label.trim().is_empty() {
            bail!("label is empty");
        }
        if self.detect.columns.iter().all(|c| c.trim().is_empty()) {
            bail!("detect.columns is empty: the header signature needs at least one column");
        }
        if let Some(e) = &self.export {
            if e.format.trim().is_empty() || e.how.is_empty() || e.how.iter().any(|h| h.trim().is_empty()) {
                bail!("export needs a format and at least one non-empty step in how");
            }
        }
        let c = &self.columns;
        if c.date.trim().is_empty() {
            bail!("columns.date is empty");
        }
        match (&c.amount, &c.debit, &c.credit) {
            (Some(_), None, None) | (None, Some(_), _) | (None, _, Some(_)) => {}
            (Some(_), _, _) => bail!("columns: either amount, or debit/credit, not both"),
            (None, None, None) => bail!("columns: amount (or debit and credit) is missing"),
        }
        if c.description.0.is_empty() {
            bail!("columns.description is empty");
        }
        if self.currency.as_deref().is_none_or(|s| s.trim().is_empty()) && c.currency.is_none() {
            bail!("currency (or columns.currency) is missing");
        }
        if c.direction.is_some() && self.parse.debit_markers.is_empty() {
            bail!("columns.direction needs parse.debit_markers");
        }
        if !matches!(self.parse.decimal.as_str(), "." | ",") {
            bail!("parse.decimal must be \".\" or \",\"");
        }
        if self.parse.thousands.as_deref() == Some(self.parse.decimal.as_str()) {
            bail!("parse.thousands can't be the decimal separator");
        }
        if let Some(d) = &self.parse.delimiter {
            if d.chars().count() != 1 && d != "\\t" {
                bail!("parse.delimiter must be one character");
            }
        }
        if self.balance_method == Some(BalanceKind::Reported) && c.balance.is_none() {
            bail!("balance_method = \"reported\" needs columns.balance");
        }
        for f in &self.parse.date_format.0 {
            check_date_format(f)?;
        }
        if let Some(e) = &self.parse.encoding {
            grid::decode(b"", Some(e))?;
        }
        Ok(())
    }

    pub fn balance(&self) -> BalanceMethod {
        match (self.kind, self.balance_method) {
            (FormatKind::Card, _) | (_, Some(BalanceKind::None)) => BalanceMethod::None,
            (_, Some(BalanceKind::Reported)) => BalanceMethod::Reported,
            (_, Some(BalanceKind::Cumsum)) => BalanceMethod::Cumsum,
            (_, None) if self.columns.balance.is_some() => BalanceMethod::Reported,
            (_, None) => BalanceMethod::Cumsum,
        }
    }

    fn read_opts(&self) -> ReadOpts {
        ReadOpts {
            encoding: self.parse.encoding.clone(),
            delimiter: self.parse.delimiter.as_deref().and_then(|d| if d == "\\t" { Some('\t') } else { d.chars().next() }),
            sheet: self.parse.sheet.clone(),
        }
    }

    fn thousands(&self) -> String {
        match &self.parse.thousands {
            Some(t) => t.clone(),
            None if self.parse.decimal == "," => ".".into(),
            None => ",".into(),
        }
    }

    /// The header row's index in `rows`: `skip_rows`, else the first of the
    /// first 30 rows that has every detect column.
    pub fn header_row(&self, rows: &[Row]) -> Option<usize> {
        let has_all = |r: &Row| {
            let have: HashSet<String> = r.iter().map(|v| norm(&v.text())).collect();
            self.detect.columns.iter().all(|c| have.contains(&norm(c)))
        };
        match self.parse.skip_rows {
            Some(n) => rows.get(n).filter(|r| has_all(r)).map(|_| n),
            None => rows.iter().take(30).position(has_all),
        }
    }

    /// Does this definition describe `path`? Its extension, its filename
    /// glob, and a header with every detect column.
    pub fn matches(&self, path: &Path) -> bool {
        if !grid::is_tabular(path) {
            return false;
        }
        if let Some(g) = &self.detect.filename {
            if !glob(g, &super::name_lower(path)) {
                return false;
            }
        }
        match grid::read(path, &self.read_opts(), Some(40)) {
            Ok(rows) => self.header_row(&rows).is_some(),
            Err(_) => false,
        }
    }

    /// Every row of `path` (the header included), read the definition's way.
    pub fn read(&self, path: &Path) -> Result<Vec<Row>> {
        grid::read(path, &self.read_opts(), None)
    }

    /// Each data row below the header: the bank row, a skip (blank or a
    /// footer without a date), or why it can't be read. Fails only when the
    /// header or a mapped column is missing.
    pub fn rows(&self, rows: &[Row]) -> Result<Vec<RowOut>> {
        let hi = self.header_row(rows).ok_or_else(|| {
            anyhow!("no header row with the columns {}", self.detect.columns.join(", "))
        })?;
        let header: Vec<String> = rows[hi].iter().map(|v| norm(&v.text())).collect();
        let col = |name: &str| -> Result<usize> {
            header.iter().position(|h| *h == norm(name)).ok_or_else(|| anyhow!("column {name:?} isn't in the header"))
        };
        let opt = |name: &Option<String>| -> Result<Option<usize>> { name.as_deref().map(col).transpose() };
        let c = &self.columns;
        let date = col(&c.date)?;
        let (amount, debit, credit) = (opt(&c.amount)?, opt(&c.debit)?, opt(&c.credit)?);
        let desc: Vec<usize> = c.description.0.iter().map(|n| col(n)).collect::<Result<_>>()?;
        let (balance, account, currency, direction) = (opt(&c.balance)?, opt(&c.account)?, opt(&c.currency)?, opt(&c.direction)?);
        let dedup: Vec<usize> = self.dedup.iter().map(|n| col(n)).collect::<Result<_>>()?;
        let thousands = self.thousands();
        let dec = self.parse.decimal.chars().next().unwrap_or('.');
        let markers: Vec<String> = self.parse.debit_markers.iter().map(|m| norm(m)).collect();
        let mut out = Vec::new();
        for (i, r) in rows.iter().enumerate().skip(hi + 1) {
            let cell = |ix: usize| r.get(ix).cloned().unwrap_or(Val::Empty);
            let line = i + 1;
            if r.iter().all(Val::is_empty) || cell(date).is_empty() {
                out.push(RowOut { line, raw: raw(r, date, amount.or(debit), &desc), row: Ok(None) });
                continue;
            }
            let money_empty = [amount, debit, credit].into_iter().flatten().all(|ix| cell(ix).is_empty());
            let txn_date = match parse_date(&cell(date), &self.parse.date_format.0) {
                Ok(d) => d,
                // a footer ("Closing balance", "Total") is no transaction
                Err(_) if money_empty => {
                    out.push(RowOut { line, raw: raw(r, date, amount.or(debit), &desc), row: Ok(None) });
                    continue;
                }
                Err(e) => {
                    out.push(RowOut { line, raw: raw(r, date, amount.or(debit), &desc), row: Err(e) });
                    continue;
                }
            };
            let row = (|| -> std::result::Result<BankRow, String> {
                let num = |ix: Option<usize>, what: &str| -> std::result::Result<Option<f64>, String> {
                    match ix {
                        None => Ok(None),
                        Some(ix) => parse_num(&cell(ix), dec, &thousands).map_err(|e| format!("{what}: {e}")),
                    }
                };
                let mut amt = match amount {
                    Some(_) => num(amount, "amount")?,
                    None => match (num(debit, "debit")?, num(credit, "credit")?) {
                        (None, None) => None,
                        (d, c) => Some(c.unwrap_or(0.0).abs() - d.unwrap_or(0.0).abs()),
                    },
                };
                if let (Some(ix), Some(a)) = (direction, amt) {
                    let out_marker = markers.contains(&norm(&cell(ix).text()));
                    amt = Some(if out_marker { -a.abs() } else { a.abs() });
                }
                if self.parse.sign == Sign::Inverted {
                    amt = amt.map(|a| -a);
                }
                let amt = amt.map(|a| if a == 0.0 { 0.0 } else { a });
                let description = desc.iter().map(|&ix| cell(ix).text()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                let ccy = match currency {
                    Some(ix) => cell(ix).text().to_uppercase(),
                    None => String::new(),
                };
                let ccy = if ccy.is_empty() { self.currency.clone().unwrap_or_default().trim().to_uppercase() } else { ccy };
                if ccy.is_empty() {
                    return Err("no currency".into());
                }
                Ok(BankRow {
                    account: account.map(|ix| cell(ix).text()).unwrap_or_default(),
                    currency: ccy,
                    txn_date,
                    amount: amt,
                    balance: num(balance, "balance")?,
                    description,
                    dedup: dedup.iter().map(|&ix| cell(ix).text()).collect::<Vec<_>>().join("|"),
                })
            })();
            out.push(RowOut { line, raw: raw(r, date, amount.or(debit), &desc), row: row.map(Some) });
        }
        Ok(out)
    }

    /// Parse whole files the way ingest does: any unreadable row fails the
    /// file (with its line), skips are dropped, exact duplicates within the
    /// files collapse (as the code sources' `unique()` does).
    pub fn parse_files(&self, paths: &[PathBuf]) -> Result<Vec<BankRow>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for p in paths {
            let rows = self.read(p)?;
            for r in self.rows(&rows).with_context(|| format!("{}", p.display()))? {
                match r.row {
                    Ok(Some(b)) => {
                        if seen.insert(crate::ingest::natural_key(&self.id, &b)) {
                            out.push(b);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => bail!("{} line {}: {e}", p.display(), r.line),
                }
            }
        }
        Ok(out)
    }
}

/// One data row's outcome, for the mapper's live preview.
#[derive(Debug, Clone, PartialEq)]
pub struct RowOut {
    /// 1-based row number in the file
    pub line: usize,
    /// the raw date, amount and description cells, as text
    pub raw: (String, String, String),
    /// Ok(None): skipped (blank, or a footer without a date)
    pub row: std::result::Result<Option<BankRow>, String>,
}

fn raw(r: &Row, date: usize, amount: Option<usize>, desc: &[usize]) -> (String, String, String) {
    let t = |ix: usize| r.get(ix).map(Val::text).unwrap_or_default();
    (t(date), amount.map(t).unwrap_or_default(), desc.iter().map(|&i| t(i)).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "))
}

/// Header names compare without case and surrounding spaces.
fn norm(s: &str) -> String {
    s.trim().trim_start_matches('\u{feff}').trim().to_lowercase()
}

/// `*` and `?` over the lowercased file name.
pub fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) => a == b && go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    go(&p, &n)
}

/// A number from a cell: an Excel number as is; text by the separators.
/// Parentheses, a trailing minus or a DR suffix mean negative; currency
/// signs and codes around the number are ignored.
pub fn parse_num(v: &Val, decimal: char, thousands: &str) -> std::result::Result<Option<f64>, String> {
    let s = match v {
        Val::Empty => return Ok(None),
        Val::Num(x) => return Ok(Some(*x)),
        Val::Text(s) => s.trim(),
    };
    if s.is_empty() {
        return Ok(None);
    }
    let mut t = s.replace('\u{2212}', "-");
    let mut neg = false;
    if t.starts_with('(') && t.ends_with(')') {
        neg = true;
        t = t[1..t.len() - 1].to_string();
    }
    let up = t.trim().to_uppercase();
    if up.ends_with("DR") {
        neg = true;
        t = t.trim()[..t.trim().len() - 2].to_string();
    } else if up.ends_with("CR") {
        t = t.trim()[..t.trim().len() - 2].to_string();
    }
    let t = t.trim();
    let t = match t.strip_suffix('-') {
        Some(x) => {
            neg = true;
            x
        }
        None => t,
    };
    let mut t = t.to_string();
    if !thousands.is_empty() {
        t = t.replace(thousands, "");
    }
    // spaces of every kind group digits too ("1 234,56")
    let mut num = String::new();
    for ch in t.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else if ch == decimal {
            num.push('.');
        } else if ch == '-' || ch == '+' {
            if !num.is_empty() {
                return Err(format!("{s:?} isn't a number"));
            }
            if ch == '-' {
                neg = !neg;
            }
        } else if ch.is_alphabetic() || ch.is_whitespace() || "$€£¥₩₹'’".contains(ch) {
            // currency codes and signs, grouping spaces
        } else {
            return Err(format!("{s:?} isn't a number"));
        }
    }
    if num.is_empty() || num == "." || num.matches('.').count() > 1 {
        return Err(format!("{s:?} isn't a number"));
    }
    let x: f64 = num.parse().map_err(|_| format!("{s:?} isn't a number"))?;
    Ok(Some(if neg { -x } else { x }))
}

const ISO: [&str; 4] = ["%Y-%m-%d", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S"];

/// A date cell as `YYYY-MM-DD`: the definition's patterns in order, then
/// ISO. A trailing time after a date-only pattern is ignored; an Excel
/// serial number (a date cell formatted as a number) is converted.
pub fn parse_date(v: &Val, formats: &[String]) -> std::result::Result<String, String> {
    let s = match v {
        Val::Num(x) if (1.0..=200_000.0).contains(x) => {
            let base = NaiveDate::from_ymd_opt(1899, 12, 30).unwrap();
            return Ok((base + chrono::Duration::days(x.floor() as i64)).format("%Y-%m-%d").to_string());
        }
        other => other.text(),
    };
    let s = s.trim();
    for f in formats.iter().map(String::as_str).chain(ISO) {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return Ok(d.format("%Y-%m-%d").to_string());
        }
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Ok(d.date().format("%Y-%m-%d").to_string());
        }
        if let Ok((d, rest)) = NaiveDate::parse_and_remainder(s, f) {
            if rest.starts_with([' ', 'T']) {
                return Ok(d.format("%Y-%m-%d").to_string());
            }
        }
    }
    Err(format!("{s:?} isn't a date{}", if formats.is_empty() { String::new() } else { format!(" ({})", formats.join(" or ")) }))
}

// ---------------------------------------------------------------------------
// The mapper's helpers: guess a definition from a file
// ---------------------------------------------------------------------------

/// The date patterns the mapper tries, most likely first (day before month:
/// the ambiguous 03/04 reads as 3 April unless a value says otherwise).
pub const DATE_FORMATS: [&str; 16] = [
    "%Y-%m-%d", "%d/%m/%Y", "%m/%d/%Y", "%d.%m.%Y", "%d-%m-%Y", "%Y/%m/%d", "%d/%m/%y", "%m/%d/%y", "%d.%m.%y",
    "%d %b %Y", "%d-%b-%Y", "%d %B %Y", "%b %d, %Y", "%B %d, %Y", "%Y%m%d", "%d-%m-%y",
];

/// A pattern in words, for the mapper's override menu.
pub fn date_format_example(f: &str) -> String {
    let d = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
    let mut s = String::new();
    match write!(s, "{}", d.format(f)) {
        Ok(()) => s,
        Err(_) => f.to_string(),
    }
}

/// The patterns that read the most non-empty values (in [`DATE_FORMATS`]
/// order): every value when one does, else the best ones, so one typo or
/// footer doesn't hide the file's pattern (the preview shows that row red).
pub fn detect_date_formats(values: &[Val]) -> Vec<&'static str> {
    let vals: Vec<&Val> = values.iter().filter(|v| !v.is_empty()).collect();
    if vals.is_empty() {
        return vec![];
    }
    let reads = |f: &str| {
        vals.iter()
            .filter(|v| match v {
                Val::Text(s) => {
                    let s = s.trim();
                    NaiveDate::parse_from_str(s, f).is_ok()
                        || NaiveDateTime::parse_from_str(s, f).is_ok()
                        || NaiveDate::parse_and_remainder(s, f).is_ok_and(|(_, r)| r.starts_with([' ', 'T']))
                }
                _ => true, // Excel dates and serials read whatever the pattern
            })
            .count()
    };
    let counts: Vec<(&'static str, usize)> = DATE_FORMATS.iter().map(|f| (*f, reads(f))).collect();
    let best = counts.iter().map(|c| c.1).max().unwrap_or(0);
    // at least half the values, or it's no date column
    if best == 0 || best * 2 < vals.len() {
        return vec![];
    }
    counts.into_iter().filter(|c| c.1 == best).map(|c| c.0).collect()
}

/// A draft definition for a file no source or format claims: the header row
/// and the columns guessed from their names, the date pattern and the
/// decimal separator from the values.
#[derive(Debug, Clone)]
pub struct Draft {
    pub def: FormatDef,
    /// the file's first rows (header included)
    pub rows: Vec<Row>,
    /// index of the header row in `rows`
    pub header: usize,
    /// the header's column names, as in the file
    pub columns: Vec<String>,
    /// date patterns that read every value of the date column
    pub date_formats: Vec<&'static str>,
    /// Excel: the workbook's sheets
    pub sheets: Vec<String>,
}

const GUESS_ROWS: usize = 400;

fn find(cols: &[String], words: &[&str], taken: &[usize]) -> Option<usize> {
    let n: Vec<String> = cols.iter().map(|c| norm(c)).collect();
    // an exact name first, then a name containing the word
    for w in words {
        if let Some(i) = n.iter().position(|c| c == w).filter(|i| !taken.contains(i)) {
            return Some(i);
        }
    }
    // (short words only exactly: "in" is inside too many names)
    for w in words.iter().filter(|w| w.len() >= 4) {
        if let Some(i) = n.iter().enumerate().position(|(i, c)| !taken.contains(&i) && c.contains(w)) {
            return Some(i);
        }
    }
    None
}

/// Guess a definition for `path` (read with `opts`: encoding, delimiter, sheet).
pub fn guess(path: &Path, opts: &ReadOpts) -> Result<Draft> {
    let rows = grid::read(path, opts, Some(GUESS_ROWS))?;
    // the header: the first of the first 30 rows with the most text cells
    // (at least 2), before the first row that looks like data
    let score = |r: &Row| r.iter().filter(|v| matches!(v, Val::Text(s) if !s.trim().is_empty() && parse_num(v, '.', ",").is_err())).count();
    let header = (0..rows.len().min(30)).max_by_key(|&i| (score(&rows[i]), std::cmp::Reverse(i))).filter(|&i| score(&rows[i]) >= 2).context("no header row: the file needs a row of column names")?;
    let columns: Vec<String> = rows[header].iter().map(|v| v.text()).collect();
    let mut taken = vec![];
    let pick = |words: &[&str], taken: &mut Vec<usize>| {
        let i = find(&columns, words, taken);
        taken.extend(i);
        i
    };
    let date = pick(&["date", "booking date", "transaction date", "posted", "posting date", "buchungstag", "boekdatum", "datum", "fecha", "data", "date opération"], &mut taken);
    let balance = pick(&["balance", "saldo", "solde", "running balance"], &mut taken);
    let amount = pick(&["amount", "betrag", "bedrag", "importe", "montant", "importo", "beløb", "belopp", "kwota", "value", "sum"], &mut taken);
    let (debit, credit) = if amount.is_none() {
        (
            pick(&["debit", "paid out", "money out", "withdrawal", "withdrawals", "out", "soll", "débit", "cargo"], &mut taken),
            pick(&["credit", "paid in", "money in", "deposit", "deposits", "in", "haben", "crédit", "abono"], &mut taken),
        )
    } else {
        (None, None)
    };
    let currency = pick(&["currency", "ccy", "währung", "devise", "moneda", "valuta"], &mut taken);
    let account = pick(&["account", "account number", "iban", "konto"], &mut taken);
    let description = pick(
        &["description", "details", "memo", "narrative", "payee", "merchant", "name", "omschrijving", "transaction", "reference", "verwendungszweck", "concepto", "libellé", "beschreibung", "text"],
        &mut taken,
    )
    .or_else(|| (0..columns.len()).find(|i| !taken.contains(i) && !columns[*i].trim().is_empty()));
    let name = |i: Option<usize>| i.map(|i| columns[i].clone());
    let data = &rows[header + 1..];
    let date_vals: Vec<Val> = date.map(|d| data.iter().filter_map(|r| r.get(d).cloned()).collect()).unwrap_or_default();
    let date_formats = detect_date_formats(&date_vals);
    // decimal comma: amounts like 12,34 or 1.234,56
    let money: Vec<String> = [amount, debit, credit, balance]
        .into_iter()
        .flatten()
        .flat_map(|c| data.iter().filter_map(move |r| match r.get(c) { Some(Val::Text(s)) => Some(s.trim().to_string()), _ => None }))
        .collect();
    let comma = money.iter().filter(|s| comma_decimal(s)).count();
    let point = money.iter().filter(|s| point_decimal(s)).count();
    let decimal = if comma > point { "," } else { "." };
    let mut detect: Vec<String> = [date, amount, debit, credit, description, balance].into_iter().flatten().map(|i| columns[i].clone()).collect();
    detect.dedup();
    let def = FormatDef {
        id: String::new(),
        label: String::new(),
        currency: if currency.is_none() { Some(String::new()) } else { None },
        detect: Detect { columns: detect, filename: None },
        columns: Columns {
            date: name(date).unwrap_or_default(),
            amount: name(amount),
            debit: name(debit),
            credit: name(credit),
            description: Names(name(description).into_iter().collect()),
            balance: name(balance),
            account: name(account),
            currency: name(currency),
            direction: None,
        },
        parse: ParseOpts {
            date_format: Names(date_formats.first().map(|f| vec![f.to_string()]).unwrap_or_default()),
            decimal: decimal.into(),
            encoding: opts.encoding.clone(),
            delimiter: opts.delimiter.map(|d| if d == '\t' { "\\t".into() } else { d.to_string() }),
            sheet: opts.sheet.clone(),
            ..ParseOpts::default()
        },
        ..FormatDef::default()
    };
    Ok(Draft {
        def,
        header,
        columns,
        date_formats,
        sheets: if grid::is_excel(path) { grid::sheet_names(path) } else { vec![] },
        rows,
    })
}

fn comma_decimal(s: &str) -> bool {
    let t = s.trim_end_matches(|c: char| !c.is_ascii_digit());
    t.rfind(',').is_some_and(|i| {
        let tail = &t[i + 1..];
        (1..=2).contains(&tail.len()) && tail.chars().all(|c| c.is_ascii_digit()) && !t[i..].contains('.')
    })
}

fn point_decimal(s: &str) -> bool {
    let t = s.trim_end_matches(|c: char| !c.is_ascii_digit());
    t.rfind('.').is_some_and(|i| {
        let tail = &t[i + 1..];
        (1..=2).contains(&tail.len()) && tail.chars().all(|c| c.is_ascii_digit()) && !t[i..].contains(',')
    })
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// A loaded definition as a file source. Leaked: definitions are few and
/// reload rarely, and a `&'static` spec is what every view expects.
pub struct FormatSource {
    pub def: FormatDef,
    spec: SourceSpec,
    /// where it came from: a file, or "built in"
    pub origin: String,
    pub builtin: bool,
    /// the TOML text it was read from
    pub toml: String,
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

impl FormatSource {
    pub fn new(def: FormatDef, origin: String, builtin: bool, toml: String) -> FormatSource {
        let spec = SourceSpec {
            key: leak(&def.id),
            kind: Kind::Bank,
            currency: if def.columns.currency.is_some() { None } else { def.currency.as_deref().map(|c| leak(&c.trim().to_uppercase())) },
            balance_method: def.balance(),
            cashflow: def.cashflow,
            label: leak(def.label.trim()),
        };
        FormatSource { def, spec, origin, builtin, toml }
    }

    /// The spec, for as long as the program runs (the source is leaked).
    pub fn spec_static(this: &'static FormatSource) -> &'static SourceSpec {
        &this.spec
    }
}

impl FileSource for &'static FormatSource {
    fn spec(&self) -> &SourceSpec {
        &self.spec
    }

    fn matches(&self, path: &Path) -> bool {
        self.def.matches(path)
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        Ok(Parsed::Bank(self.def.parse_files(paths)?))
    }
}

/// The definitions compiled in (engine/formats/).
pub const BUILTIN: [(&str, &str); 2] = [
    ("monzo.toml", include_str!("../../formats/monzo.toml")),
    ("barclays_uk.toml", include_str!("../../formats/barclays_uk.toml")),
];

static DIRS: RwLock<Option<Vec<PathBuf>>> = RwLock::new(None);

/// Where user definitions live, in priority order (the first with an id
/// wins). The desktop app sets its own; otherwise `$FINANCE_FORMATS_DIR`,
/// else `$FINANCE_HOME/formats`.
pub fn set_dirs(dirs: Vec<PathBuf>) {
    *DIRS.write().unwrap_or_else(|p| p.into_inner()) = Some(dirs);
    invalidate();
}

pub fn dirs() -> Vec<PathBuf> {
    if let Some(d) = DIRS.read().unwrap_or_else(|p| p.into_inner()).clone() {
        return d;
    }
    vec![default_dir()]
}

/// The server's formats directory for the active tenant: the owner's
/// `$FINANCE_FORMATS_DIR`, else `$FINANCE_HOME/formats`; another user's
/// `users/<uid>/formats`.
pub fn default_dir() -> PathBuf {
    crate::tenant::active().formats.clone()
}

type Stamp = Vec<(PathBuf, u64, i128)>;

struct Loaded {
    stamp: Stamp,
    checked: std::time::Instant,
    formats: Vec<&'static FormatSource>,
    errors: Vec<String>,
}

/// One entry per set of directories (per tenant: each user has a formats
/// dir of their own).
static CACHE: Mutex<Vec<(Vec<PathBuf>, Loaded)>> = Mutex::new(Vec::new());

fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml") && p.is_file())
        .collect();
    v.sort();
    v
}

fn stamp(dirs: &[PathBuf]) -> Stamp {
    dirs.iter()
        .flat_map(|d| toml_files(d))
        .map(|p| {
            let md = std::fs::metadata(&p).ok();
            let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = md
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i128)
                .unwrap_or(0);
            (p, size, mtime)
        })
        .collect()
}

fn load(dirs: &[PathBuf], stamp: Stamp) -> Loaded {
    let mut formats: Vec<&'static FormatSource> = Vec::new();
    let mut errors = Vec::new();
    let mut ids = HashSet::new();
    for (p, _, _) in &stamp {
        let text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{}: {e}", p.display()));
                continue;
            }
        };
        match FormatDef::from_toml(&text) {
            Ok(def) if ids.insert(def.id.clone()) => {
                formats.push(Box::leak(Box::new(FormatSource::new(def, p.display().to_string(), false, text))));
            }
            Ok(def) => errors.push(format!("{}: id {:?} is defined earlier; skipped", p.display(), def.id)),
            Err(e) => errors.push(format!("{}: {e:#}", p.display())),
        }
    }
    let _ = dirs;
    for (name, text) in BUILTIN {
        match FormatDef::from_toml(text) {
            Ok(def) if ids.insert(def.id.clone()) => {
                formats.push(Box::leak(Box::new(FormatSource::new(def, format!("built in ({name})"), true, text.to_string()))));
            }
            Ok(_) => {} // a user file overrides it
            Err(e) => errors.push(format!("built in {name}: {e:#}")),
        }
    }
    Loaded { stamp, checked: std::time::Instant::now(), formats, errors }
}

/// The directories are re-checked at most this often (views look specs up
/// per row); [`save`], [`save_text`] and [`set_dirs`] reload at once.
const RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

fn with_loaded<T>(f: impl FnOnce(&Loaded) -> T) -> T {
    let dirs = dirs();
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    let i = match cache.iter().position(|(d, _)| *d == dirs) {
        Some(i) => i,
        None => {
            let now = stamp(&dirs);
            cache.push((dirs.clone(), load(&dirs, now)));
            cache.len() - 1
        }
    };
    let l = &mut cache[i].1;
    if l.checked.elapsed() >= RECHECK {
        let now = stamp(&dirs);
        if l.stamp == now {
            l.checked = std::time::Instant::now();
        } else {
            *l = load(&dirs, now);
        }
    }
    f(&cache[i].1)
}

/// Forget the loaded definitions: the next lookup re-reads the directories.
pub fn invalidate() {
    CACHE.lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// Every definition in effect: the user's, then the built-in ones.
pub fn formats() -> Vec<&'static FormatSource> {
    with_loaded(|l| l.formats.clone())
}

/// Definitions that failed to load, with the reason.
pub fn load_errors() -> Vec<String> {
    with_loaded(|l| l.errors.clone())
}

/// A definition by id.
pub fn get(id: &str) -> Option<&'static FormatSource> {
    formats().into_iter().find(|f| f.def.id == id)
}

/// Write `def` as `<dir>/<id>.toml` (validated first). Takes effect at once.
pub fn save(dir: &Path, def: &FormatDef) -> Result<PathBuf> {
    def.validate()?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.toml", def.id));
    let tmp = dir.join(format!(".{}.toml.tmp", def.id));
    std::fs::write(&tmp, header_comment(def) + &def.to_toml()?)?;
    std::fs::rename(&tmp, &path)?;
    invalidate();
    Ok(path)
}

/// Save TOML text as received (the server's PUT): parsed and validated,
/// stored verbatim so comments survive.
pub fn save_text(dir: &Path, id: &str, text: &str) -> Result<PathBuf> {
    let def = FormatDef::from_toml(text)?;
    if def.id != id {
        bail!("the file's id is {:?}, not {id:?}", def.id);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{id}.toml"));
    let tmp = dir.join(format!(".{id}.toml.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    invalidate();
    Ok(path)
}

fn header_comment(def: &FormatDef) -> String {
    format!(
        "# {} export format (finance format definition; see desktop/docs/formats.md).\n# Made with the Upload page's column mapper; hand edits are fine.\n\n",
        def.label.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Val {
        Val::Text(s.into())
    }

    #[test]
    fn numbers() {
        let n = |s: &str, d: char, th: &str| parse_num(&t(s), d, th);
        assert_eq!(n("-1,234.56", '.', ","), Ok(Some(-1234.56)));
        assert_eq!(n("1.234,56", ',', "."), Ok(Some(1234.56)));
        assert_eq!(n("(12.00)", '.', ","), Ok(Some(-12.0)));
        assert_eq!(n("12.00-", '.', ","), Ok(Some(-12.0)));
        assert_eq!(n("£ 5.10", '.', ","), Ok(Some(5.1)));
        assert_eq!(n("5.10 EUR", '.', ","), Ok(Some(5.1)));
        assert_eq!(n("12.50 DR", '.', ","), Ok(Some(-12.5)));
        assert_eq!(n("1 234,5", ',', ""), Ok(Some(1234.5)));
        assert_eq!(n("+3", '.', ","), Ok(Some(3.0)));
        assert_eq!(n("", '.', ","), Ok(None));
        assert!(n("abc", '.', ",").is_err());
        assert!(n("1.2.3", '.', "").is_err());
        assert!(n("1-2", '.', "").is_err());
        assert_eq!(parse_num(&Val::Num(-3.5), ',', "."), Ok(Some(-3.5)));
    }

    #[test]
    fn dates() {
        let d = |s: &str, f: &[&str]| parse_date(&t(s), &f.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(d("31/03/2026", &["%d/%m/%Y"]), Ok("2026-03-31".into()));
        assert_eq!(d("2026-03-31", &["%d/%m/%Y"]), Ok("2026-03-31".into()));
        assert_eq!(d("31/03/2026 14:05", &["%d/%m/%Y"]), Ok("2026-03-31".into()));
        assert_eq!(d("2026-03-31 14:05:00", &[]), Ok("2026-03-31".into()));
        assert!(d("03/31/2026", &["%d/%m/%Y"]).is_err());
        assert_eq!(parse_date(&Val::Num(46112.0), &[]), Ok("2026-03-31".into()));
        let vals = vec![t("01/02/2026"), t("13/02/2026"), Val::Empty];
        assert_eq!(detect_date_formats(&vals), vec!["%d/%m/%Y"]);
        let vals = vec![t("01/02/2026"), t("02/13/2026")];
        assert_eq!(detect_date_formats(&vals), vec!["%m/%d/%Y"]);
        assert_eq!(detect_date_formats(&[t("01/02/2026")])[..2], ["%d/%m/%Y", "%m/%d/%Y"]);
        assert_eq!(date_format_example("%d/%m/%Y"), "31/03/2026");
    }

    #[test]
    fn ids_and_globs() {
        assert!(valid_id("barclays_uk") && !valid_id("Barclays") && !valid_id("1x") && !valid_id("a"));
        assert_eq!(slug("Barclays (UK)"), "barclays_uk");
        assert_eq!(slug("1st Bank"), "f_1st_bank");
        assert!(glob("statement_*.csv", "statement_2026-03.csv"));
        assert!(glob("*.CSV", "x.csv"));
        assert!(!glob("statement_?.csv", "statement_12.csv"));
    }

    #[test]
    fn builtins_load_and_validate() {
        for (name, text) in BUILTIN {
            let def = FormatDef::from_toml(text).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(format!("{}.toml", def.id), name);
            // a round trip through to_toml keeps it
            assert_eq!(FormatDef::from_toml(&def.to_toml().unwrap()).unwrap(), def);
        }
    }

    #[test]
    fn rules() {
        let base = FormatDef::from_toml(BUILTIN[0].1).unwrap();
        let bad = |f: &dyn Fn(&mut FormatDef)| {
            let mut d = base.clone();
            f(&mut d);
            d.validate().is_err()
        };
        assert!(bad(&|d| d.id = "n26".into()), "a code source's key");
        assert!(bad(&|d| d.id = "Bad Id".into()));
        assert!(bad(&|d| d.columns.debit = Some("x".into())), "amount and debit");
        assert!(bad(&|d| d.columns.amount = None));
        assert!(bad(&|d| {
            d.currency = None;
            d.columns.currency = None;
        }));
        assert!(bad(&|d| d.parse.decimal = ";".into()));
        assert!(bad(&|d| d.parse.date_format = Names(vec!["%Q".into()])));
        assert!(bad(&|d| d.detect.columns.clear()));
        assert!(toml::from_str::<FormatDef>("id='x'\nlabel='y'\nbogus=1").is_err(), "unknown keys are refused");
    }
}
