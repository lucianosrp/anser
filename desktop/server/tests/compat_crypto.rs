//! The stored hashes: device tokens, pairing codes and upload digests are
//! looked up by sha256 / md5, so a crate upgrade must not move a single digit
//! (vectors computed with python hashlib).

use md5::{Digest, Md5};

#[test]
fn token_hash_vector() {
    assert_eq!(finance_server::auth::sha256_hex("fin_pair_token-123"), "5f691342d2376d2ced7c2f46b11a473fdabf7f503ed0778ca3fb85ec71d86481");
    assert_eq!(finance_server::auth::sha256_hex(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
}

#[test]
fn upload_digest_vector() {
    assert_eq!(hex::encode(Md5::digest(b"fin_pair_token-123")), "ce304f47f805098a1a759eb320d56bd0");
    assert_eq!(Md5::digest(b"a"), Md5::digest(b"a"), "digests compare as values (upload dedupe)");
}
