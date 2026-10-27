//! A dry-run ingest, for the desktop app's Upload page: what is this file,
//! and how many of its rows would ingest really add?
//!
//! * [`detect`] -- the source that claims a file (ingest's own registry and
//!   order), or why none does
//! * [`parse_file`] -- detect + parse + the date range, no store involved
//! * [`replica_copy`] -- a private in-memory copy of a database, opened
//!   read-only: nothing here can write to the original
//! * [`dedupe`] -- load parsed rows into that copy through
//!   [`ingest::load_parsed`], the same raw_hash UNIQUEs and near-duplicate
//!   guards a real ingest runs, so "new" means exactly what ingest would add
//!
//! Applying several files to one copy in turn also catches overlap between
//! the files themselves, as ingest would.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

use crate::db;
use crate::ingest::{self, FileLoad};
use crate::sources::{self, FileSource, Parsed, SourceSpec};


/// The extensions ingest reads (any case here; the upload lowercases them,
/// because the server's ingest only globs lowercase ones).
pub const EXTS: [&str; 5] = ["csv", "xml", "pdf", "xlsx", "xls"];

/// Why a file can't be ingested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// not .csv / .xml / .pdf / .xlsx / .xls
    Extension(String),
    /// no source's header / content check matches
    Unknown,
    /// nothing in it
    Empty,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unsupported::Extension(e) if e.is_empty() => f.write_str("no extension: only .csv, .xlsx, .xls, .xml and .pdf exports are read"),
            Unsupported::Extension(e) => write!(f, ".{e} isn't read: only .csv, .xlsx, .xls, .xml and .pdf exports are"),
            Unsupported::Unknown => f.write_str("no known export format matches its header"),
            Unsupported::Empty => f.write_str("the file is empty"),
        }
    }
}

/// One file, parsed.
#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub source: SourceSpec,
    pub parsed: Parsed,
    /// first and last day of its rows (YYYY-MM-DD), when any row has a date
    pub range: Option<(String, String)>,
}

fn ext_lower(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// The source that claims `path`: the first in ingest's registry order.
pub fn detect(path: &Path) -> std::result::Result<Box<dyn FileSource>, Unsupported> {
    let ext = ext_lower(path);
    if !EXTS.contains(&ext.as_str()) {
        return Err(Unsupported::Extension(ext));
    }
    if blank(path) {
        return Err(Unsupported::Empty);
    }
    sources::source_for(path).ok_or(Unsupported::Unknown)
}

/// Only whitespace in it, read in chunks (a big export isn't held whole to
/// look at its first bytes). Unreadable counts as not blank.
fn blank(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = [0u8; 64 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => return true,
            Ok(n) if buf[..n].iter().all(u8::is_ascii_whitespace) => {}
            Ok(_) => return false,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

/// Can the column mapper describe this file (a CSV or a spreadsheet)?
pub fn mappable(path: &Path) -> bool {
    sources::grid::is_tabular(path)
}

/// The day span of parsed rows.
pub fn date_range(parsed: &Parsed) -> Option<(String, String)> {
    let days: Vec<String> = match parsed {
        Parsed::Bank(rows) => rows.iter().map(|r| r.txn_date.chars().take(10).collect()).collect(),
        Parsed::FundUnits(rows) => rows.iter().map(|r| r.trade_date.chars().take(10).collect()).collect(),
        Parsed::Crypto(rows) => rows
            .iter()
            .filter_map(|r| chrono::DateTime::from_timestamp_millis(r.ts))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .collect(),
    };
    let mut days = days.into_iter().filter(|d| d.len() == 10);
    let first = days.next()?;
    Some(days.fold((first.clone(), first), |(lo, hi), d| {
        if d < lo {
            (d, hi)
        } else if d > hi {
            (lo, d)
        } else {
            (lo, hi)
        }
    }))
}

/// Detect and parse one file. `Ok(Err(..))` = not something ingest reads;
/// `Err` = a known format that failed to parse.
pub fn parse_file(path: &Path) -> Result<std::result::Result<ParsedFile, Unsupported>> {
    let source = match detect(path) {
        Ok(s) => s,
        Err(u) => return Ok(Err(u)),
    };
    let parsed = source.parse(&[path.to_path_buf()]).with_context(|| format!("parsing as {}", source.spec().label))?;
    let range = date_range(&parsed);
    Ok(Ok(ParsedFile { source: source.spec().clone(), parsed, range }))
}

/// A private in-memory copy of the database at `path`, brought up to this
/// schema. The original is opened read-only and only read (backup API).
/// A missing file gives an empty store: every row counts as new.
pub fn replica_copy(path: &Path) -> Result<Connection> {
    let mut mem = Connection::open_in_memory()?;
    if path.exists() {
        let src = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_context(|| format!("opening {} read-only", path.display()))?;
        src.busy_timeout(Duration::from_secs(5))?;
        let backup = rusqlite::backup::Backup::new(&src, &mut mem)?;
        backup.run_to_completion(4096, Duration::ZERO, None).context("copying the replica")?;
    }
    db::apply_schema(&mem)?;
    Ok(mem)
}

/// [`replica_copy`] from an open connection (the desktop's encrypted
/// replica, already decrypted into memory).
pub fn replica_copy_from(src: &Connection) -> Result<Connection> {
    let mut mem = Connection::open_in_memory()?;
    {
        let backup = rusqlite::backup::Backup::new(src, &mut mem)?;
        backup.run_to_completion(4096, Duration::ZERO, None).context("copying the replica")?;
    }
    db::apply_schema(&mem)?;
    Ok(mem)
}

/// Run ingest's row loading for `file` against `conn` (a [`replica_copy`]):
/// the rows it adds are the rows a real ingest would add.
pub fn dedupe(conn: &Connection, file: &ParsedFile, file_name: &str) -> Result<FileLoad> {
    ingest::load_parsed(conn, &file.source, file.parsed.clone(), file_name, db::now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{BankRow, CryptoRow};

    #[test]
    fn ranges() {
        let row = |d: &str| BankRow {
            account: "a".into(),
            currency: "EUR".into(),
            txn_date: d.into(),
            amount: Some(1.),
            balance: None,
            description: "x".into(),
            dedup: String::new(),
        };
        let p = Parsed::Bank(vec![row("2026-03-02"), row(""), row("2025-12-31"), row("2026-01-15")]);
        assert_eq!(date_range(&p), Some(("2025-12-31".into(), "2026-03-02".into())));
        assert_eq!(date_range(&Parsed::Bank(vec![row("")])), None);
        let c = Parsed::Crypto(vec![CryptoRow {
            ts: 1_700_000_000_000,
            asset: "BTC".into(),
            delta: 1.,
            usd: None,
            kind: "deposit".into(),
            detail: String::new(),
        }]);
        assert_eq!(date_range(&c), Some(("2023-11-14".into(), "2023-11-14".into())));
    }

    #[test]
    fn unsupported_files() {
        let dir = std::env::temp_dir().join(format!("finance-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.numbers"), b"PK").unwrap();
        std::fs::write(dir.join("b.csv"), b" \n").unwrap();
        std::fs::write(dir.join("c.csv"), b"what,is,this\n1,2,3\n").unwrap();
        assert_eq!(detect(&dir.join("a.numbers")).err(), Some(Unsupported::Extension("numbers".into())));
        assert_eq!(detect(&dir.join("b.csv")).err(), Some(Unsupported::Empty));
        assert_eq!(detect(&dir.join("c.csv")).err(), Some(Unsupported::Unknown));
        assert!(Unsupported::Unknown.to_string().contains("header"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn copy_never_touches_the_original() {
        let dir = std::env::temp_dir().join(format!("finance-preview-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("replica.db");
        {
            let c = db::connect(&path).unwrap();
            let _: String = c.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0)).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let copy = replica_copy(&path).unwrap();
        copy.execute("INSERT INTO fund_subscriptions(ref,trade_date,fund,units,px) VALUES('r','2026-01-01','f',1,1)", [])
            .unwrap();
        drop(copy);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let n: i64 = Connection::open(&path).unwrap().query_row("SELECT COUNT(*) FROM fund_subscriptions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert!(replica_copy(&dir.join("missing.db")).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
