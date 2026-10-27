//! Crypto compatibility: known-answer vectors and files written by the
//! versions of sha2 / hmac / md-5 / chacha20poly1305 that shipped before the
//! major upgrades (synthetic data only). The vectors for the hashes were
//! computed outside Rust (python hashlib/hmac); the sealed secrets file was
//! written by the old code: `cargo test -p finance-engine --test compat_crypto
//! -- --ignored write_secrets_fixture` regenerates it (new nonce; then the
//! opening test must still pass against the old copy kept in git history).

use std::collections::BTreeMap;

use hmac::{Hmac, KeyInit, Mac};
use md5::{Digest as _, Md5};
use sha2::Sha256;

const MSG: &[u8] = b"finance-compat";
const KEY: [u8; 32] = *b"0123456789abcdef0123456789abcdef";
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compat/secrets.sealed");

#[test]
fn hash_vectors() {
    assert_eq!(hex::encode(Sha256::digest(MSG)), "cc57beb6ff64a49e1cb4a1b003977e1a29bd16e09e915357aaf54214060ba5dd");
    assert_eq!(hex::encode(Md5::digest(MSG)), "c3931402f6777dbb328d310177a598bf");
    assert_eq!(finance_engine::archive::sha256_hex(MSG), "cc57beb6ff64a49e1cb4a1b003977e1a29bd16e09e915357aaf54214060ba5dd");
    // incremental md5 (core::upload::md5_file streams through io::copy)
    let mut h = Md5::new();
    h.update(&MSG[..5]);
    h.update(&MSG[5..]);
    assert_eq!(hex::encode(h.finalize()), "c3931402f6777dbb328d310177a598bf");
}

#[test]
fn hmac_vectors() {
    // the exchanges' request signatures
    assert_eq!(
        finance_engine::fetch::okx::sign("secret", "1700000000000", "/api/v5/account/balance?ccy=BTC"),
        "SpJSD3AUk5UytxPqhkWWDWkuI4YQC6PTtECjPsN9VMA="
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
    mac.update(b"symbol=BTCUSDT&timestamp=1700000000000");
    assert_eq!(hex::encode(mac.finalize().into_bytes()), "6244d11c958f45ac56733152cb3cb1831d23a2b3709b3a88b8b42a072aceb410");
}

fn map() -> BTreeMap<String, String> {
    BTreeMap::from([("OKX_KEY".to_string(), "k-123".to_string()), ("OKX_SECRET".to_string(), "s-456".to_string())])
}

/// A secrets file sealed by the old chacha20poly1305 opens, and the new one
/// seals something the same reader opens (same layout: MAGIC, 24-byte nonce).
#[test]
fn sealed_secrets_fixture_opens() {
    let bytes = std::fs::read(FIXTURE).expect("fixture");
    assert_eq!(finance_engine::secrets::open_sealed(&KEY, 7, &bytes).unwrap(), map());
    assert!(finance_engine::secrets::open_sealed(&KEY, 8, &bytes).is_err(), "bound to the user");
    let mut other = KEY;
    other[0] ^= 1;
    assert!(finance_engine::secrets::open_sealed(&other, 7, &bytes).is_err(), "bound to the key");
    let fresh = finance_engine::secrets::seal(&KEY, 7, &map()).unwrap();
    assert_eq!(fresh.len(), bytes.len());
    assert_eq!(&fresh[..6], &bytes[..6], "same magic");
    assert_eq!(finance_engine::secrets::open_sealed(&KEY, 7, &fresh).unwrap(), map());
}

#[test]
#[ignore = "writes the fixture"]
fn write_secrets_fixture() {
    std::fs::write(FIXTURE, finance_engine::secrets::seal(&KEY, 7, &map()).unwrap()).unwrap();
}
