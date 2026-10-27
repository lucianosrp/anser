//! The Upload page's dedupe preview on the owner's real exports: against a
//! replica built from the parity db, every file must preview 0 new rows --
//! each file on its own fresh copy, and all of them in turn on one copy.
//! Reads $FINANCE_PARITY_DIR (e.g. <repo>/data/parity) and skips cleanly
//! when it is unset or absent. The parity dir is only read: the db
//! is copied into the gitignored data/test-tmp/ first.

mod ingest_common;

use std::path::PathBuf;

use finance_engine::preview;
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

#[test]
fn every_real_file_previews_zero_new() {
    let Some(p) = parity_dir() else { return };
    // the replica: a consistent single-file snapshot, like /api/db.sqlite
    let root = tmp_dir("preview-parity");
    let replica = root.join("finance.db");
    std::fs::write(&replica, finance_engine::db::snapshot_bytes_of(&p.join("prod.db")).unwrap()).unwrap();

    let mut files: Vec<PathBuf> =
        std::fs::read_dir(p.join("transactions")).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort();
    let shared = preview::replica_copy(&replica).unwrap();
    let (mut parsed_files, mut rows, mut unsupported) = (0, 0, 0);
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let pf = match preview::parse_file(f).unwrap() {
            Ok(pf) => pf,
            Err(why) => {
                eprintln!("{name}: unsupported ({why})");
                unsupported += 1;
                continue;
            }
        };
        let alone = preview::dedupe(&preview::replica_copy(&replica).unwrap(), &pf, &name).unwrap();
        let together = preview::dedupe(&shared, &pf, &name).unwrap();
        let (lo, hi) = pf.range.clone().unwrap_or_default();
        eprintln!(
            "{name}: {} {} rows {lo}..{hi}, new {} (near-dupes {}), in turn new {}",
            pf.source.key, alone.rows, alone.added, alone.near_dupes, together.added
        );
        assert_eq!(alone.added, 0, "{name}: the replica already has every row");
        assert_eq!(together.added, 0, "{name}: in turn");
        parsed_files += 1;
        rows += alone.rows;
    }
    eprintln!("{parsed_files} files, {rows} rows, all known; {unsupported} unsupported");
    assert!(parsed_files > 0);
    // the replica file itself was only read
    let again = preview::replica_copy(&replica).unwrap();
    let n: i64 = again.query_row("SELECT COUNT(*) FROM bank_transactions", [], |r| r.get(0)).unwrap();
    assert_eq!(n, count(&replica, "SELECT COUNT(*) FROM bank_transactions"));
    let _ = std::fs::remove_dir_all(root);
}
