//! db.rs housekeeping: connect() is idempotent on its own schema, backup
//! prunes only its own files, snapshot bytes are a self-contained database.

mod ingest_common;

use finance_engine::db;
use ingest_common::*;

#[test]
fn connect_twice_keeps_the_schema() {
    let d = tmp_dir("st-connect");
    let p = d.join("finance.db");
    drop(db::connect(&p).unwrap());
    let a = rows(&p, "SELECT type, name, sql FROM sqlite_master");
    drop(db::connect(&p).unwrap());
    assert_eq!(rows(&p, "SELECT type, name, sql FROM sqlite_master"), a);
    let c = db::connect(&p).unwrap();
    let mode: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal");
}

#[test]
fn backup_prunes_only_its_own_files() {
    let d = tmp_dir("st-backup");
    let p = d.join("finance.db");
    db::connect(&p).unwrap().execute("INSERT INTO fx_rates VALUES('EUR','HKD','2026-01',8.5,0)", []).unwrap();
    let dir = d.join("backups");
    for n in ["finance-20200101-000000.db", "finance-20200102-000000.db", "finance-20200103-000000.db"] {
        write(&dir, n, "old");
    }
    write(&dir, "finance-handmade.db", "keep me");
    let dest = db::backup_into(&p, &dir, 2).unwrap();
    assert_eq!(count(&dest, "SELECT COUNT(*) FROM fx_rates"), 1);
    let mut left: Vec<String> =
        std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    let newest = dest.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(left, vec!["finance-20200103-000000.db".to_string(), newest, "finance-handmade.db".into()]);
}

#[test]
fn snapshot_is_a_standalone_db() {
    let d = tmp_dir("st-snap");
    let p = d.join("finance.db");
    db::connect(&p).unwrap().execute("INSERT INTO fx_rates VALUES('EUR','HKD','2026-01',8.5,0)", []).unwrap();
    let bytes = db::snapshot_bytes_of(&p).unwrap();
    assert!(bytes.starts_with(b"SQLite format 3\0"));
    // journal_mode=DELETE: header bytes 18/19 are 1 (legacy), not 2 (WAL)
    assert_eq!((bytes[18], bytes[19]), (1, 1));
    let out = d.join("snap.db");
    std::fs::write(&out, &bytes).unwrap();
    assert_eq!(count(&out, "SELECT COUNT(*) FROM fx_rates"), 1);
}
