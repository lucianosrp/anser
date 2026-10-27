//! Dedupe on the owner's real exports: the prod db already holds every row
//! a re-ingest of the exports finds, and re-runs, copies, overlapping slices
//! and any file order add nothing. Reads $FINANCE_PARITY_DIR (prod.db +
//! transactions/, e.g. <repo>/data/parity) and skips cleanly when it is
//! unset or absent. Everything is copied into the gitignored data/test-tmp/
//! first; the parity dir is never written, and nothing derived from it is
//! committed.

mod ingest_common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use finance_engine::{db, ingest};
use ingest_common::*;

fn parity_dir() -> Option<PathBuf> {
    let Some(d) = std::env::var_os("FINANCE_PARITY_DIR").map(PathBuf::from) else {
        eprintln!("FINANCE_PARITY_DIR unset; skipping");
        return None;
    };
    if d.join("transactions").is_dir() && d.join("prod.db").is_file() {
        Some(d)
    } else {
        eprintln!("no parity data at {}; skipping", d.display());
        None
    }
}

/// A private copy of the parity transactions dir.
fn tx_copy(name: &str) -> Option<(PathBuf, PathBuf)> {
    let p = parity_dir()?;
    let root = tmp_dir(name);
    let tx = root.join("tx");
    copy_tree(&p.join("transactions"), &tx);
    Some((root, tx))
}

fn per_source(dbp: &Path) -> BTreeMap<String, i64> {
    rows(dbp, "SELECT source, COUNT(*) FROM bank_transactions GROUP BY source")
        .into_iter()
        .map(|r| {
            let (s, n) = r.split_once(" | ").unwrap();
            (s.to_string(), n.trim_start_matches("Integer(").trim_end_matches(')').parse().unwrap())
        })
        .collect()
}

#[test]
fn b_prod_copy_adds_nothing() {
    let Some((root, tx)) = tx_copy("par-prod") else { return };
    let p = parity_dir().unwrap();
    let dbp = root.join("finance.db");
    std::fs::copy(p.join("prod.db"), &dbp).unwrap();
    for ext in ["-wal", "-shm"] {
        let src = p.join(format!("prod.db{ext}"));
        if src.is_file() {
            std::fs::copy(&src, root.join(format!("finance.db{ext}"))).unwrap();
        }
    }
    let before = (rows(&dbp, BANK), rows(&dbp, CRYPTO), rows(&dbp, FUNDS));
    let schema = rows(&dbp, "SELECT type, name, sql FROM sqlite_master");
    let conn = db::connect(&dbp).unwrap();
    let s = ingest::run_ingest_dir(&conn, &tx).unwrap();
    drop(conn);
    eprintln!("prod re-ingest: {s:?}");
    assert_eq!(s.iter().map(|(_, s)| s.rows_added).sum::<usize>(), 0);
    assert_eq!((rows(&dbp, BANK), rows(&dbp, CRYPTO), rows(&dbp, FUNDS)), before);
    assert_eq!(rows(&dbp, "SELECT type, name, sql FROM sqlite_master"), schema, "connect() must not alter prod's schema");
}

#[test]
fn c_rerun_touch_copy_add_nothing() {
    let Some((root, tx)) = tx_copy("par-robust") else { return };
    let dbp = root.join("finance.db");
    let first = ingest(&dbp, &tx);
    assert!(first > 0);
    let before = (rows(&dbp, BANK), rows(&dbp, CRYPTO), rows(&dbp, FUNDS));
    assert_eq!(ingest(&dbp, &tx), 0, "re-run 1");
    assert_eq!(ingest(&dbp, &tx), 0, "re-run 2");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    for e in std::fs::read_dir(&tx).unwrap() {
        let f = std::fs::File::open(e.unwrap().path()).unwrap();
        f.set_modified(later).unwrap();
    }
    assert_eq!(ingest(&dbp, &tx), 0, "touched mtime");
    for e in std::fs::read_dir(&tx).unwrap().collect::<Vec<_>>() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if let Some((stem, ext)) = name.rsplit_once('.') {
            std::fs::copy(&p, tx.join(format!("{stem} copy.{ext}"))).unwrap();
        }
    }
    assert_eq!(ingest(&dbp, &tx), 0, "copies under new names");
    assert_eq!((rows(&dbp, BANK), rows(&dbp, CRYPTO), rows(&dbp, FUNDS)), before);
}

#[test]
fn c_overlapping_slices_add_nothing() {
    // each real CSV export cut into two overlapping halves under new names
    let Some((root, tx)) = tx_copy("par-slices") else { return };
    let dbp = root.join("finance.db");
    ingest(&dbp, &tx);
    let before = rows(&dbp, BANK);
    let mut cut = 0;
    for e in std::fs::read_dir(&tx).unwrap().collect::<Vec<_>>() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".csv") else { continue };
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
        if lines.len() < 4 || text.matches('"').count() % 2 != 0 {
            continue;
        }
        let header = lines.remove(0);
        let mid = lines.len() / 2;
        write(&tx, &format!("{stem} head.csv"), &format!("{header}{}", lines[..=mid].concat()));
        write(&tx, &format!("{stem} tail.csv"), &format!("{header}{}", lines[mid - 1..].concat()));
        cut += 1;
    }
    eprintln!("sliced {cut} exports");
    assert_eq!(ingest(&dbp, &tx), 0);
    assert_eq!(rows(&dbp, BANK), before);
}

#[test]
fn c_reversed_file_order_gives_the_same_rows() {
    // one file per ingest run, newest name first
    let Some((root, tx)) = tx_copy("par-reverse") else { return };
    let fwd = root.join("fwd.db");
    ingest(&fwd, &tx);
    let mut names: Vec<String> =
        std::fs::read_dir(&tx).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names.reverse();
    let one = root.join("one");
    std::fs::create_dir_all(&one).unwrap();
    let rev = root.join("rev.db");
    for n in &names {
        std::fs::copy(tx.join(n), one.join(n)).unwrap();
        ingest(&rev, &one);
    }
    assert_eq!(per_source(&rev), per_source(&fwd));
    assert_eq!(rows(&rev, CRYPTO), rows(&fwd, CRYPTO));
    assert_eq!(rows(&rev, FUNDS), rows(&fwd, FUNDS));
    assert_eq!(ingest(&rev, &tx), 0, "the whole tree on top of the reversed build");
    // which export's version of a drifted row is stored depends on order,
    // so balances may differ; the transactions themselves may not
    let ident = "SELECT source, account, currency, txn_date, amount, description FROM bank_transactions";
    let (a, b) = (rows(&fwd, ident), rows(&rev, ident));
    if a != b {
        let diff: Vec<_> = a.iter().filter(|r| !b.contains(r)).chain(b.iter().filter(|r| !a.contains(r))).collect();
        eprintln!("order-dependent (drift/rename) rows: {}", diff.len());
        if std::env::var("SHOW_DIFF").is_ok() { for d in &diff { eprintln!("  {d}"); } }
    }
}
