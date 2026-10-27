//! What an unfinished sign-in leaves on the server, and that it goes: a
//! pairing code nobody used expires after 10 minutes and is swept (with the
//! used ones) when the next code is made; invites, spent or expired, are
//! swept a week after they expire; a refused signup makes no user and no
//! device; removing a user takes its devices and codes.

use finance_server::auth;
use rusqlite::{params, Connection};

fn count(db: &Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

#[test]
fn half_made_sign_ins_expire_and_are_swept() {
    let home = std::env::temp_dir().join(format!("finance-half-made-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::remove_var("FINANCE_AUTH");
    std::env::remove_var("FINANCE_ALLOW_SIGNUP");
    auth::bootstrap().unwrap();
    let owner = auth::signup("Owner", None, None, "laptop").unwrap();
    assert!(owner.first);
    let db = Connection::open(auth::path()).unwrap();
    let now = || finance_engine::db::now_ms();

    // a code that was never used: refused once it's past 10 minutes ...
    let (stale, expires) = auth::new_pair_code(owner.user_id).unwrap();
    assert!(expires - now() <= auth::CODE_TTL_MS);
    db.execute("UPDATE pair_codes SET expires = ?1 WHERE code_hash = ?2", params![now() - 1, auth::sha256_hex(&stale)]).unwrap();
    assert_eq!(auth::pair(&stale, "phone").unwrap_err().downcast::<auth::CodeError>().unwrap(), auth::CodeError::Expired);
    // ... and gone with the next code; a used one goes too
    let (fresh, _) = auth::new_pair_code(owner.user_id).unwrap();
    assert_eq!(count(&db, "pair_codes"), 1);
    auth::pair(&fresh, "phone").unwrap();
    auth::new_pair_code(owner.user_id).unwrap();
    assert_eq!(count(&db, "pair_codes"), 1, "the used code was swept");

    // invites: an expired one stays a week (a late try hears why), then goes
    let (late, _) = auth::new_invite(owner.user_id).unwrap();
    let (old, _) = auth::new_invite(owner.user_id).unwrap();
    db.execute("UPDATE invites SET expires = ?1 WHERE code_hash = ?2", params![now() - 1, auth::sha256_hex(&late)]).unwrap();
    db.execute("UPDATE invites SET expires = ?1 WHERE code_hash = ?2", params![now() - 8 * 86_400_000, auth::sha256_hex(&old)]).unwrap();
    let devices = auth::devices().unwrap().len();
    // a refused signup leaves nothing half made
    assert!(auth::signup("Sam", None, Some(&late), "sam's laptop").is_err());
    assert_eq!((auth::user_count().unwrap(), auth::devices().unwrap().len()), (1, devices));
    auth::new_invite(owner.user_id).unwrap();
    assert_eq!(count(&db, "invites"), 2, "the week-old expired invite was swept");

    // a removed user's devices and codes go with it
    let (invite, _) = auth::new_invite(owner.user_id).unwrap();
    let sam = auth::signup("Sam", None, Some(&invite), "sam's laptop").unwrap();
    auth::new_pair_code(sam.user_id).unwrap();
    auth::remove_user(sam.user_id, false).unwrap();
    assert_eq!(db.query_row("SELECT COUNT(*) FROM devices WHERE user_id = ?1", [sam.user_id], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(db.query_row("SELECT COUNT(*) FROM pair_codes WHERE user_id = ?1", [sam.user_id], |r| r.get::<_, i64>(0)).unwrap(), 0);
    let _ = std::fs::remove_dir_all(&home);
}
