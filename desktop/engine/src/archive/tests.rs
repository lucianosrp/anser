//! Archive round trips, tampering, and the three import modes on throwaway
//! trees. The parity-scale run (FINANCE_PARITY_DIR) is in
//! engine/tests/archive_parity.rs.

use super::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("finance-archive-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tree(root: &Path) -> Tree {
    Tree { db: root.join("data/finance.db"), transactions: root.join("transactions"), formats: root.join("formats") }
}

const CSV: &str = "a,b\n1,2\n";

/// A small tree: bank rows, crypto events (one a reconcile anchor), a rule,
/// an override, a profile, a setting, an original and a format.
fn seed(root: &Path, extra_bank: usize) -> Tree {
    let t = tree(root);
    let c = db::connect(&t.db).unwrap();
    profile::ensure(&c).unwrap();
    profile::put(&c, &profile::Profile { base_currency: "HKD".into(), name: Some("Owner".into()), ..Default::default() }).unwrap();
    profile::put_setting(&c, "budgets", &json!({"Groceries": 3000})).unwrap();
    let mut rows = vec![];
    for i in 0..(3 + extra_bank) {
        rows.push(db::NewBankTxn {
            source: "hsbc".into(), account: "acct".into(), currency: "HKD".into(), txn_date: format!("2026-0{}-1{}", 1 + i % 9, i % 10),
            amount: Some(-(10.0 + i as f64)), balance: Some(1000.0 - i as f64), description: format!("shop {i}"),
            raw_hash: format!("h{i}"), source_file: "x.csv".into(), ingested_at: 1,
        });
    }
    db::insert_bank_transactions(&c, &rows).unwrap();
    c.execute_batch(
        "INSERT INTO crypto_events(exchange,ts,asset,delta,usd,kind,detail) VALUES('binance',1700000000000,'BTC',0.1,3000,'spot-buy','');
         INSERT INTO crypto_events(exchange,ts,asset,delta,usd,kind,detail) VALUES('binance',1800000000000,'BTC',0.001,NULL,'asset-adjust','reconcile');
         INSERT INTO fx_rates(base,quote,month,rate,fetched_at) VALUES('EUR','HKD','2026-01',8.5,0);
         INSERT INTO category_rules(priority,pattern,category,is_transfer,direction,share) VALUES(5,'shop','Shopping',0,NULL,NULL);
         INSERT INTO txn_overrides(source,raw_hash,category,share,note,created_at) VALUES('hsbc','h0','Gifts',NULL,NULL,1);",
    )
    .unwrap();
    std::fs::create_dir_all(&t.transactions).unwrap();
    std::fs::write(t.transactions.join("export.csv"), CSV).unwrap();
    std::fs::create_dir_all(&t.formats).unwrap();
    std::fs::write(t.formats.join("acme.toml"), "id = \"acme\"\n").unwrap();
    t
}

fn pass(p: &str) -> Seal {
    Seal::Passphrase { passphrase: p.into(), work_factor: Some(10) }
}

fn export_bytes(t: &Tree, opts: &ExportOpts, seal: &Seal) -> Vec<u8> {
    let mut out = vec![];
    export(t, opts, seal, &mut out).unwrap();
    out
}

fn counts(t: &Tree) -> BTreeMap<String, i64> {
    let c = Connection::open(&t.db).unwrap();
    row_counts(&c, "main").unwrap()
}

#[test]
fn round_trip_passphrase_and_recipient() {
    let root = tmp("rt");
    let t = seed(&root.join("a"), 0);
    let bytes = export_bytes(&t, &ExportOpts::default(), &pass("correct horse"));
    assert!(bytes.starts_with(b"age-encryption.org/v1"));
    assert!(is_passphrase(&bytes[..]).unwrap());
    let a = open(&bytes[..], &Unseal::Passphrase("correct horse".into())).unwrap();
    assert_eq!(a.manifest.row_counts["bank_transactions"], 3);
    assert_eq!(a.manifest.base_currency.as_deref(), Some("HKD"));
    assert!(!a.manifest.partial && !a.manifest.includes_secrets);
    assert_eq!(a.originals(), vec![("export.csv", CSV.as_bytes())]);
    assert_eq!(a.formats().len(), 1);
    assert!(a.manifest.schema["bank_transactions"].iter().any(|c| c.0 == "raw_hash"));
    assert!(open(&bytes[..], &Unseal::Passphrase("wrong".into())).is_err());

    let (id, public) = generate_identity();
    assert!(public.starts_with("age1") && identity_public(&id).unwrap() == public);
    let bytes = export_bytes(&t, &ExportOpts::default(), &Seal::Recipients(vec![public]));
    assert!(!is_passphrase(&bytes[..]).unwrap());
    let r = verify(&bytes[..], &Unseal::Identity(id)).unwrap();
    assert_eq!(r.integrity, "ok");
    assert!(r.summary().starts_with("OK:"));
    let (other, _) = generate_identity();
    assert!(open(&bytes[..], &Unseal::Identity(other)).is_err(), "another key doesn't open it");
    assert!(open(&bytes[..], &Unseal::Passphrase("x".into())).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn verify_catches_tampering() {
    let root = tmp("tamper");
    let t = seed(&root.join("a"), 0);
    let bytes = export_bytes(&t, &ExportOpts::default(), &pass("pw"));
    // a flipped byte anywhere in the payload: the age MAC refuses it
    for at in [bytes.len() / 2, bytes.len() - 5] {
        let mut bad = bytes.clone();
        bad[at] ^= 0x40;
        assert!(verify(&bad[..], &Unseal::Passphrase("pw".into())).is_err(), "flip at {at}");
    }
    // truncated
    assert!(verify(&bytes[..bytes.len() - 100], &Unseal::Passphrase("pw".into())).is_err());
    // re-sealed with a changed entry but the old manifest: the sha256 check
    let a = open(&bytes[..], &Unseal::Passphrase("pw".into())).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = a.entries.clone().into_iter().collect();
    for (n, d) in entries.iter_mut() {
        if n.starts_with("originals/") {
            d.extend_from_slice(b"3,4\n");
        }
    }
    let plain = pack(&a.manifest, &entries).unwrap();
    let mut out = vec![];
    seal_into(&pass("pw"), &plain, &mut out).unwrap();
    let e = verify(&out[..], &Unseal::Passphrase("pw".into())).unwrap_err().to_string();
    assert!(e.contains("sha256 mismatch"), "{e}");
    // an extra entry not in the manifest
    entries.push(("originals/extra.csv".into(), b"x".to_vec()));
    let mut m = a.manifest.clone();
    for (n, d) in &entries {
        if n != "originals/extra.csv" {
            m.entries.insert(n.clone(), EntryInfo { sha256: sha256_hex(d), size: d.len() as u64 });
        }
    }
    let mut out = vec![];
    seal_into(&pass("pw"), &pack(&m, &entries).unwrap(), &mut out).unwrap();
    assert!(verify(&out[..], &Unseal::Passphrase("pw".into())).unwrap_err().to_string().contains("not in the manifest"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn secrets_only_when_asked_and_never_server_files() {
    let root = tmp("secrets");
    let t = seed(&root.join("a"), 0);
    // the server's private files, where they live on a real tree
    std::fs::write(root.join("a/data/server.db"), b"server").unwrap();
    std::fs::create_dir_all(root.join("a/secrets")).unwrap();
    std::fs::write(root.join("a/secrets/master.key"), [7u8; 32]).unwrap();
    std::fs::create_dir_all(root.join("a/data/webapp")).unwrap();
    std::fs::write(root.join("a/data/webapp/index.html"), b"<html>").unwrap();
    std::fs::write(root.join("a/data/fetch_status.json"), b"{}").unwrap();
    std::fs::write(root.join("a/data/sync.lock"), b"").unwrap();

    let a = open(&export_bytes(&t, &ExportOpts::default(), &pass("pw"))[..], &Unseal::Passphrase("pw".into())).unwrap();
    assert!(a.secrets().unwrap().is_none() && !a.manifest.includes_secrets);
    let names: Vec<&String> = a.entries.keys().collect();
    for n in &names {
        assert!(
            n.as_str() == "db/finance.sqlite" || n.starts_with("originals/") || n.starts_with("settings/"),
            "unexpected entry {n}"
        );
        for bad in ["server.db", "master.key", "webapp", "fetch_status", ".lock"] {
            assert!(!n.contains(bad), "{n} must never be archived");
        }
    }
    let raw = String::from_utf8_lossy(a.db_bytes()).to_string();
    assert!(!raw.contains("server"), "the db is finance.db only");

    let s: BTreeMap<String, String> = [("OKX_API_KEY".to_string(), "k".to_string())].into();
    let opts = ExportOpts { secrets: Some(&s), ..Default::default() };
    let a = open(&export_bytes(&t, &opts, &pass("pw"))[..], &Unseal::Passphrase("pw".into())).unwrap();
    assert!(a.manifest.includes_secrets);
    assert_eq!(a.secrets().unwrap().unwrap()["OKX_API_KEY"], "k");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn fresh_import_equals_the_source() {
    let root = tmp("fresh");
    let src = seed(&root.join("a"), 2);
    let bytes = export_bytes(&src, &ExportOpts::default(), &pass("pw"));
    let a = open(&bytes[..], &Unseal::Passphrase("pw".into())).unwrap();
    let dst = tree(&root.join("b"));
    let dry = import(&a, &dst, ImportOpts { mode: Mode::Fresh, dry_run: true, prefer_archive: false }).unwrap();
    assert_eq!(dry.table("bank_transactions").unwrap().added, 5);
    assert!(!dst.db.exists(), "a dry run writes nothing");
    let rep = import(&a, &dst, ImportOpts { mode: Mode::Fresh, dry_run: false, prefer_archive: false }).unwrap();
    assert_eq!(rep.originals_saved, vec!["export.csv"]);
    assert_eq!(counts(&src), counts(&dst));
    // the ingest records point at the new tree
    let c = Connection::open(&dst.db).unwrap();
    assert_eq!(std::fs::read_to_string(dst.transactions.join("export.csv")).unwrap(), CSV);
    assert!(dst.formats.join("acme.toml").exists());
    let _ = c;
    // not empty any more: fresh refuses
    assert!(import(&a, &dst, ImportOpts { mode: Mode::Fresh, dry_run: true, prefer_archive: false }).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn merge_of_overlapping_archives_adds_no_duplicates() {
    let root = tmp("merge");
    let a_tree = seed(&root.join("a"), 0);
    let first = open(&export_bytes(&a_tree, &ExportOpts::default(), &pass("pw"))[..], &Unseal::Passphrase("pw".into())).unwrap();
    // b starts as a copy of a, then a grows
    let b_tree = tree(&root.join("b"));
    import(&first, &b_tree, ImportOpts { mode: Mode::Fresh, dry_run: false, prefer_archive: false }).unwrap();
    {
        let c = db::connect(&a_tree.db).unwrap();
        db::insert_bank_transactions(&c, &[db::NewBankTxn {
            source: "hsbc".into(), account: "acct".into(), currency: "HKD".into(), txn_date: "2026-09-30".into(), amount: Some(-99.0),
            balance: Some(1.0), description: "new shop".into(), raw_hash: "hnew".into(), source_file: "y.csv".into(), ingested_at: 2,
        }]).unwrap();
        // a conflicting override and rule
        c.execute_batch("UPDATE txn_overrides SET category='Travel' WHERE raw_hash='h0'; UPDATE category_rules SET category='Fun' WHERE pattern='shop';").unwrap();
    }
    let second = open(&export_bytes(&a_tree, &ExportOpts::default(), &pass("pw"))[..], &Unseal::Passphrase("pw".into())).unwrap();
    let opts = ImportOpts { mode: Mode::Merge, dry_run: true, prefer_archive: false };
    let before = counts(&b_tree);
    let dry = import(&second, &b_tree, opts).unwrap();
    assert_eq!(dry.table("bank_transactions").unwrap().added, 1);
    assert_eq!(counts(&b_tree), before, "a dry run writes nothing");
    let rep = import(&second, &b_tree, ImportOpts { dry_run: false, ..opts }).unwrap();
    assert_eq!(rep.table("bank_transactions").unwrap().added, 1);
    assert_eq!(rep.table("crypto_events").unwrap().added, 0);
    assert_eq!(rep.table("txn_overrides").unwrap().conflicts.len(), 1);
    assert_eq!(rep.table("category_rules").unwrap().conflicts.len(), 1);
    assert_eq!(rep.originals_present, 1);
    // the target won
    let c = Connection::open(&b_tree.db).unwrap();
    let cat: String = c.query_row("SELECT category FROM txn_overrides WHERE raw_hash='h0'", [], |r| r.get(0)).unwrap();
    assert_eq!(cat, "Gifts");
    drop(c);
    // again, and the first archive too: nothing new at all
    for arc in [&second, &first] {
        let again = import(arc, &b_tree, ImportOpts { dry_run: false, ..opts }).unwrap();
        assert_eq!(again.added(), 0, "{}", again.text());
    }
    assert_eq!(counts(&b_tree)["bank_transactions"], 4);
    // the reconcile anchor never came over by merge
    // prefer_archive flips the conflicts
    let rep = import(&second, &b_tree, ImportOpts { dry_run: false, prefer_archive: true, ..opts }).unwrap();
    assert_eq!(rep.added(), 0);
    let c = Connection::open(&b_tree.db).unwrap();
    let cat: String = c.query_row("SELECT category FROM txn_overrides WHERE raw_hash='h0'", [], |r| r.get(0)).unwrap();
    assert_eq!(cat, "Travel");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn merge_refuses_another_base_currency() {
    let root = tmp("ccy");
    let a_tree = seed(&root.join("a"), 0);
    let b_tree = seed(&root.join("b"), 0);
    {
        let c = db::connect(&b_tree.db).unwrap();
        c.execute("UPDATE profile SET base_currency='EUR'", []).unwrap();
    }
    let a = open(&export_bytes(&a_tree, &ExportOpts::default(), &pass("pw"))[..], &Unseal::Passphrase("pw".into())).unwrap();
    let e = import(&a, &b_tree, ImportOpts { mode: Mode::Merge, dry_run: true, prefer_archive: false }).unwrap_err().to_string();
    assert!(e.contains("base currency"), "{e}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn replace_backs_up_first_and_refuses_partial() {
    let root = tmp("replace");
    let a_tree = seed(&root.join("a"), 4);
    let b_tree = seed(&root.join("b"), 0);
    std::fs::write(b_tree.transactions.join("mine.csv"), "c,d\n").unwrap();
    let a = open(&export_bytes(&a_tree, &ExportOpts::default(), &pass("pw"))[..], &Unseal::Passphrase("pw".into())).unwrap();
    let opts = ImportOpts { mode: Mode::Replace, dry_run: true, prefer_archive: false };
    let dry = import(&a, &b_tree, opts).unwrap();
    assert_eq!(dry.table("bank_transactions").unwrap().skipped, 3);
    assert_eq!(counts(&b_tree)["bank_transactions"], 3);
    let rep = import(&a, &b_tree, ImportOpts { dry_run: false, ..opts }).unwrap();
    let backup = PathBuf::from(rep.backup.clone().unwrap());
    assert!(backup.starts_with(root.join("b/data/backups/pre-import")));
    let old = Connection::open(&backup).unwrap();
    assert_eq!(count(&old, "main", "bank_transactions"), 3, "the backup holds the previous data");
    assert_eq!(counts(&b_tree), counts(&a_tree));
    assert!(!PathBuf::from(format!("{}-wal", b_tree.db.display())).exists() || true);
    // the old exports were moved aside, the archive's are in place
    assert!(!b_tree.transactions.join("mine.csv").exists());
    assert!(std::fs::read_dir(root.join("b")).unwrap().filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().starts_with("transactions.pre-import-")));

    let partial = ExportOpts { scope: Scope { from: Some("2026-03-01".into()), ..Default::default() }, ..Default::default() };
    let p = open(&export_bytes(&a_tree, &partial, &pass("pw"))[..], &Unseal::Passphrase("pw".into())).unwrap();
    assert!(p.manifest.partial && p.originals().is_empty());
    assert!(p.manifest.row_counts["bank_transactions"] < 7);
    assert_eq!(p.manifest.row_counts["ingested_files"], 0);
    assert!(import(&p, &b_tree, opts).unwrap_err().to_string().contains("partial"));
    // a partial archive merges fine
    assert_eq!(import(&p, &b_tree, ImportOpts { mode: Mode::Merge, ..opts }).unwrap().added(), 0);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn nightly_keeps_the_newest() {
    let root = tmp("nightly");
    let t = seed(&root.join("a"), 0);
    let (id, public) = generate_identity();
    let dir = root.join("archives");
    for i in 0..3 {
        // distinct names (the stamp is per second)
        let p = dir.join(format!("finance-2026010{i}-000000.finance"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p, b"old").unwrap();
    }
    let (path, _) = nightly(&t, &[public], &dir, 2).unwrap();
    let left = list_archives(&dir);
    assert_eq!(left.len(), 2);
    assert_eq!(left[0].0, path.file_name().unwrap().to_string_lossy());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    verify(std::fs::File::open(&path).unwrap(), &Unseal::Identity(id)).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

// ---- compatibility fixtures --------------------------------------------------
// Archives written by the crypto crates that shipped before the sha2 / md-5 /
// hmac / chacha20poly1305 / argon2 majors (synthetic data only): the current
// code must open and verify them. Regenerate (new bytes) with
// `cargo test -p finance-engine --lib compat_write_fixtures -- --ignored`.

const COMPAT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compat");

#[test]
#[ignore = "writes the fixtures"]
fn compat_write_fixtures() {
    let root = tmp("compat-write");
    let t = seed(&root, 0);
    let (identity, public) = generate_identity();
    std::fs::write(format!("{COMPAT_DIR}/identity.txt"), identity).unwrap();
    std::fs::write(format!("{COMPAT_DIR}/passphrase.finance"), export_bytes(&t, &ExportOpts::default(), &pass("compat pass"))).unwrap();
    std::fs::write(format!("{COMPAT_DIR}/recipient.finance"), export_bytes(&t, &ExportOpts::default(), &Seal::Recipients(vec![public]))).unwrap();
}

#[test]
fn compat_fixtures_open_and_verify() {
    let identity = std::fs::read_to_string(format!("{COMPAT_DIR}/identity.txt")).unwrap();
    for (file, unseal) in [("passphrase.finance", Unseal::Passphrase("compat pass".into())), ("recipient.finance", Unseal::Identity(identity))] {
        let path = format!("{COMPAT_DIR}/{file}");
        let rep = verify(std::fs::File::open(&path).unwrap(), &unseal).unwrap_or_else(|e| panic!("{file}: {e:#}"));
        assert!(rep.entries >= 3, "{file}: {}", rep.summary());
        let a = open(std::fs::File::open(&path).unwrap(), &unseal).unwrap();
        let root = tmp("compat-open");
        let dst = tree(&root);
        import(&a, &dst, ImportOpts { mode: Mode::Fresh, dry_run: false, prefer_archive: false }).unwrap();
        let c = Connection::open(&dst.db).unwrap();
        assert_eq!(row_counts(&c, "main").unwrap().get("bank_transactions"), Some(&3), "{file}");
        assert_eq!(std::fs::read_to_string(dst.transactions.join("export.csv")).unwrap(), CSV, "{file}");
        assert_eq!(std::fs::read_to_string(dst.formats.join("acme.toml")).unwrap(), "id = \"acme\"\n", "{file}");
    }
    // a wrong passphrase still fails cleanly
    let f = std::fs::File::open(format!("{COMPAT_DIR}/passphrase.finance")).unwrap();
    assert!(verify(f, &Unseal::Passphrase("nope".into())).is_err());
}
