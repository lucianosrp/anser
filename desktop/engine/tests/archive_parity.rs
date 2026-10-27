//! The archive at the owner's scale: a scratch copy of $FINANCE_PARITY_DIR
//! (prod.db + transactions/) exported, verified, imported fresh into an
//! empty tree and compared (per-table counts and every bundle section), then
//! merged back over both trees (0 rows added). Skips without the data.

use std::path::{Path, PathBuf};

use finance_engine::archive::{self, ExportOpts, ImportOpts, Mode, Seal, Tree, Unseal};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        if e.path().is_file() {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

fn bundle(db: &Path) -> serde_json::Value {
    let conn = rusqlite::Connection::open(db).unwrap();
    let ctx = finance_engine::views::Ctx::offline(&conn);
    let mut b = finance_engine::export::bundle(&ctx, None);
    let o = b.as_object_mut().unwrap();
    o.remove("generated_at");
    o.remove("net_worth"); // its as_of is "now"
    if let Some(ov) = o.get_mut("overview").and_then(|v| v.get_mut("net_worth")).and_then(|v| v.as_object_mut()) {
        ov.remove("as_of");
    }
    b
}

#[test]
fn archive_at_parity_scale() {
    let Some(dir) = std::env::var_os("FINANCE_PARITY_DIR").map(PathBuf::from) else {
        eprintln!("skipping: FINANCE_PARITY_DIR unset");
        return;
    };
    if !dir.join("prod.db").exists() {
        eprintln!("skipping: no parity data");
        return;
    }
    std::env::set_var("FINANCE_TODAY", "2026-10-03");
    std::env::set_var("FINANCE_OFFLINE", "1");
    let root = std::env::temp_dir().join(format!("finance-archive-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = Tree { db: root.join("a/data/finance.db"), transactions: root.join("a/transactions"), formats: root.join("a/formats") };
    std::fs::create_dir_all(root.join("a/data")).unwrap();
    std::fs::copy(dir.join("prod.db"), &src.db).unwrap();
    copy_dir(&dir.join("transactions"), &src.transactions);

    let t = std::time::Instant::now();
    let out = root.join("parity.finance");
    let seal = Seal::Passphrase { passphrase: "parity pass".into(), work_factor: Some(12) };
    let m = archive::export_file(&src, &ExportOpts::default(), &seal, &out).unwrap();
    let size = std::fs::metadata(&out).unwrap().len();
    let raw: u64 = m.entries.values().map(|e| e.size).sum();
    eprintln!("archive: {:.2} MB ({:.2} MB unpacked, {} entries) in {:?}", size as f64 / 1e6, raw as f64 / 1e6, m.entries.len(), t.elapsed());

    let unseal = Unseal::Passphrase("parity pass".into());
    let rep = archive::verify(std::fs::File::open(&out).unwrap(), &unseal).unwrap();
    eprintln!("{}", rep.summary());

    let a = archive::open(std::fs::File::open(&out).unwrap(), &unseal).unwrap();
    let dst = Tree { db: root.join("b/data/finance.db"), transactions: root.join("b/transactions"), formats: root.join("b/formats") };
    archive::import(&a, &dst, ImportOpts { mode: Mode::Fresh, dry_run: false, prefer_archive: false }).unwrap();
    let counts = |db: &Path| archive::row_counts(&rusqlite::Connection::open(db).unwrap(), "main").unwrap();
    let (ca, cb) = (counts(&src.db), counts(&dst.db));
    for (t, n) in &ca {
        assert_eq!(cb.get(t), Some(n), "{t}");
    }
    let (ba, bb) = (bundle(&src.db), bundle(&dst.db));
    for (k, v) in ba.as_object().unwrap() {
        assert_eq!(Some(v), bb.get(k), "bundle section {k} differs after a fresh import");
    }
    eprintln!("fresh import: {} tables, {} bundle sections equal", ca.len(), ba.as_object().unwrap().len());

    // overlapping merges add nothing
    for target in [&dst, &src] {
        let r = archive::import(&a, target, ImportOpts { mode: Mode::Merge, dry_run: false, prefer_archive: false }).unwrap();
        assert_eq!(r.added(), 0, "{}", r.text());
        assert_eq!(r.reingested_rows, 0);
    }
    let after = counts(&dst.db);
    for (t, n) in &cb {
        assert_eq!(after.get(t), Some(n), "{t} after the merges");
    }
    let _ = std::fs::remove_dir_all(&root);
}
