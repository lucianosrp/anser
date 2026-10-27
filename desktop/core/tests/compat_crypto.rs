//! Crypto compatibility for the local encrypted copy (argon2 + chacha20poly1305):
//! the passphrase key derivation is pinned by a known-answer vector and a vault
//! written by the versions before the major upgrades must still unlock and read
//! (synthetic data only). `cargo test -p finance-core --test compat_crypto --
//! --ignored write_fixture` rewrites the fixture.

use std::path::{Path, PathBuf};

use finance_core::replica::{self, KeySource, Vault};

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compat/vault");
const PASS: &str = "compat passphrase";
const SALT: [u8; 16] = *b"finance-salt-016";
const M_KIB: u32 = 64;
const T: u32 = 2;
/// Argon2id(PASS, SALT, 64 KiB, t=2, p=1, 32 bytes), written by argon2 0.5.
const KEY_HEX: &str = "a59f6ef7d1fa072c6b9353d3231bd1d25daf12bc4ac346f2cd101cbb46542932";

#[test]
fn argon2_vector() {
    let key = replica::derive(PASS, &SALT, M_KIB, T).unwrap();
    assert_eq!(replica::hex_encode(&key), KEY_HEX);
}

#[test]
fn vault_written_by_the_old_versions_opens() {
    let key = replica::derive(PASS, &SALT, M_KIB, T).unwrap();
    let v = Vault::new(DIR);
    let cfg = v.config().expect("encrypted");
    assert_eq!((cfg.key, cfg.m_kib, cfg.t), (KeySource::Passphrase, M_KIB, T));
    v.check(&key).unwrap();
    assert!(v.check(&replica::derive("wrong", &SALT, M_KIB, T).unwrap()).is_err());
    assert_eq!(v.read(&Path::new(DIR).join("finance.db"), Some(&key)).unwrap(), b"synthetic database bytes");
    assert_eq!(v.read(&Path::new(DIR).join("export.json"), Some(&key)).unwrap(), br#"{"version":1}"#);
    // the file is bound to its name
    let sealed = std::fs::read(Path::new(DIR).join("finance.db.enc")).unwrap();
    assert!(replica::open_sealed(&key, "export.json", &sealed).is_err());
    // what the new code seals opens the same way
    let fresh = replica::seal(&key, "finance.db", b"x").unwrap();
    assert_eq!(&fresh[..8], &sealed[..8]);
    assert_eq!(replica::open_sealed(&key, "finance.db", &fresh).unwrap(), b"x");
}

#[test]
#[ignore = "writes the fixture"]
fn write_fixture() {
    let dir = PathBuf::from(DIR);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("finance.db"), b"synthetic database bytes").unwrap();
    std::fs::write(dir.join("export.json"), br#"{"version":1}"#).unwrap();
    let key = replica::derive(PASS, &SALT, M_KIB, T).unwrap();
    eprintln!("KEY_HEX = {}", replica::hex_encode(&key));
    Vault::new(&dir).enable(&key, KeySource::Passphrase, &SALT, M_KIB, T, &[]).unwrap();
}
