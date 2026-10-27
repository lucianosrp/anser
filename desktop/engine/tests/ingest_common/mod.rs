//! Shared helpers for the ingest test suites. Scratch trees live under the
//! repo's gitignored data/test-tmp/, never in git.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use finance_engine::{db, ingest};
use rusqlite::Connection;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// A fresh, empty scratch dir (wiped if it exists).
pub fn tmp_dir(name: &str) -> PathBuf {
    let d = repo_root().join("data/test-tmp").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

/// One Rust ingest run of `tx` into `db`. Returns total rows added.
pub fn ingest(dbp: &Path, tx: &Path) -> usize {
    let conn = db::connect(dbp).unwrap();
    let s = ingest::run_ingest_dir(&conn, tx).unwrap();
    s.iter().map(|(_, s)| s.rows_added).sum()
}

pub fn count(dbp: &Path, sql: &str) -> i64 {
    let c = Connection::open(dbp).unwrap();
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Every row of `sql`, each rendered as one debug string, sorted -- a
/// multiset, so row ids and insertion order never matter.
pub fn rows(dbp: &Path, sql: &str) -> Vec<String> {
    let c = Connection::open(dbp).unwrap();
    let mut st = c.prepare(sql).unwrap();
    let n = st.column_count();
    let mut out: Vec<String> = st
        .query_map([], |r| {
            let mut v = Vec::new();
            for i in 0..n {
                v.push(format!("{:?}", r.get::<_, rusqlite::types::Value>(i)?));
            }
            Ok(v.join(" | "))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    out.sort();
    out
}

/// Bank rows as stored, minus id/ingested_at/source_file.
pub const BANK: &str = "SELECT source, account, currency, txn_date, amount, balance, description, raw_hash, \
                        category, share FROM bank_transactions";
pub const CRYPTO: &str = "SELECT exchange, ts, asset, delta, usd, kind, detail, source_ref FROM crypto_events";
pub const FUNDS: &str = "SELECT ref, trade_date, fund, units, px FROM fund_subscriptions";

/// Copy every regular file of `src` into `dst`.
pub fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        if e.path().is_file() {
            std::fs::copy(e.path(), dst.join(e.file_name())).unwrap();
        }
    }
}

/// FNV-1a 64 over `lines`, each followed by a newline.
pub fn digest(lines: &[String]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for l in lines {
        for b in l.bytes().chain([b'\n']) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// Assert two multisets are equal, printing the first differences.
pub fn assert_same(what: &str, a: &[String], b: &[String]) {
    if a == b {
        return;
    }
    let mut ca: BTreeMap<&String, i64> = BTreeMap::new();
    for r in a {
        *ca.entry(r).or_default() += 1;
    }
    for r in b {
        *ca.entry(r).or_default() -= 1;
    }
    let diffs: Vec<String> = ca
        .iter()
        .filter(|(_, n)| **n != 0)
        .take(10)
        .map(|(r, n)| format!("  {}{r}", if *n > 0 { "only a: " } else { "only b: " }))
        .collect();
    panic!("{what}: {} vs {} rows differ:\n{}", a.len(), b.len(), diffs.join("\n"));
}
