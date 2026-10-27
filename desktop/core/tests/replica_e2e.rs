//! The encrypted replica through the app's own readers, on the parity db
//! ($FINANCE_PARITY_DIR; skips without it): a plain sync, the switch on
//! (passphrase, no keyring under a scratch server file), no plaintext left,
//! Ledger, Extra, Bundle, the Upload page's reads and the Doctor's check all
//! the same, a sync while encrypted, then the switch off.

use std::path::{Path, PathBuf};

use finance_core::{bundle::Bundle, cache, doctor, invest, replica, upload, Ledger};

fn no_plaintext(dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().is_file()) {
        let b = std::fs::read(e.path()).unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        assert!(!b.starts_with(b"SQLite format 3"), "{name} is a plaintext database");
        assert!(!b.windows(9).any(|w| w == b"\"version\""), "{name} holds the plaintext bundle");
    }
}

#[test]
fn encrypted_replica_end_to_end() {
    let Some(p) = std::env::var_os("FINANCE_PARITY_DIR").map(PathBuf::from).filter(|p| p.join("prod.db").exists()) else {
        eprintln!("skipping: FINANCE_PARITY_DIR unset");
        return;
    };
    let root = std::env::temp_dir().join(format!("finance-replica-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (data, cfg) = (root.join("data"), root.join("config"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    std::env::set_var("FINANCE_DESKTOP_DATA", &data);
    std::env::set_var("FINANCE_DESKTOP_SERVER_FILE", cfg.join("server"));
    let db = std::fs::read(p.join("prod.db")).unwrap();
    let bundle = br#"{"version":1,"generated_at":1,"net_worth":{"total_hkd":1.0}}"#;
    // a plain sync
    replica::write(cache::DB_FILE, &db).unwrap();
    replica::write(cache::BUNDLE_FILE, bundle).unwrap();
    let plain = Ledger::load(&cache::db_path()).unwrap();
    let hashes = upload::replica_hashes(&cache::db_path());
    assert!(!plain.entries.is_empty() && !hashes.is_empty());

    let invest_toml = cfg.join("invest.toml");
    std::fs::write(&invest_toml, "[plan]\nibkr = 1\n").unwrap();
    assert!(!replica::keyring_available(), "a scratch server file never touches the keyring");
    assert!(replica::enable(None, &[]).is_err(), "no keyring: a passphrase is required");
    replica::enable(Some("parity passphrase"), std::slice::from_ref(&invest_toml)).unwrap();
    assert!(replica::status().encrypted && !replica::locked());
    no_plaintext(&cache::dir());
    assert!(!invest_toml.exists());
    assert_eq!(replica::read_text(&invest_toml).unwrap(), "[plan]\nibkr = 1\n");

    // every reader, in memory
    let t = std::time::Instant::now();
    let enc = Ledger::load(&cache::db_path()).unwrap();
    eprintln!("encrypted Ledger::load: {:?}", t.elapsed());
    assert_eq!(enc.entries.len(), plain.entries.len());
    assert_eq!(enc.trades.len(), plain.trades.len());
    assert!(invest::Extra::load(&cache::db_path()).is_ok());
    assert_eq!(Bundle::load(&cache::bundle_path()).unwrap().at(&["version"]).as_u64(), Some(1));
    assert_eq!(upload::replica_hashes(&cache::db_path()), hashes);
    let c = doctor::replica(&cache::db_path(), &cache::bundle_path());
    assert_eq!(c.status, doctor::Status::Ok, "{}", c.summary);
    assert!(cache::size_bytes() > 0 && cache::age_secs().is_some());

    // a sync while encrypted leaves no plaintext either
    replica::write(cache::DB_FILE, &db).unwrap();
    replica::write(cache::BUNDLE_FILE, bundle).unwrap();
    no_plaintext(&cache::dir());

    // a wrong passphrase fails cleanly
    assert!(replica::unlock("not the passphrase").is_err());
    assert!(!replica::locked(), "the right key stays in memory");

    replica::disable(std::slice::from_ref(&invest_toml)).unwrap();
    assert!(!replica::status().encrypted);
    assert!(std::fs::read(cache::db_path()).unwrap().starts_with(b"SQLite format 3"));
    assert_eq!(std::fs::read_to_string(&invest_toml).unwrap(), "[plan]\nibkr = 1\n");
    assert_eq!(Ledger::load(&cache::db_path()).unwrap().entries.len(), plain.entries.len());
    let _ = std::fs::remove_dir_all(&root);
}
