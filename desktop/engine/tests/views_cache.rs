//! The view cache and the serialized bundle, on a small made-up database:
//! a cached view is served while the data holds, any write the fingerprint
//! observes is seen by the very next call (no settle delay), and
//! export::bundle_bytes equals a fresh serialization of export::bundle.

use finance_engine::db::{self, NewBankTxn};
use finance_engine::export;
use finance_engine::views::{cache, Ctx};
use serde_json::Value;

fn txn(date: &str, amount: f64, desc: &str, i: usize) -> NewBankTxn {
    NewBankTxn {
        source: "hsbc".into(),
        account: String::new(),
        currency: "HKD".into(),
        txn_date: date.into(),
        amount: Some(amount),
        balance: Some(10_000.0 + amount),
        description: desc.into(),
        raw_hash: format!("h{i}"),
        source_file: "t.csv".into(),
        ingested_at: 1_700_000_000_000 + i as i64,
    }
}

fn fresh_db(name: &str) -> (std::path::PathBuf, rusqlite::Connection) {
    let dir = std::env::temp_dir().join(format!("finance-views-cache-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("finance.db");
    let conn = db::connect(&path).unwrap();
    let rows: Vec<NewBankTxn> = (0..40)
        .map(|i| txn(&format!("2026-{:02}-{:02}", 1 + i % 9, 1 + i % 27), -(10.0 + i as f64), &format!("SHOP {}", i % 7), i))
        .collect();
    db::insert_bank_transactions(&conn, &rows).unwrap();
    (dir, conn)
}

/// The wall-clock fields of a bundle, blanked.
fn untimed(mut v: Value) -> Value {
    v["generated_at"] = Value::Null;
    for path in ["/net_worth", "/overview/net_worth"] {
        if let Some(o) = v.pointer_mut(path).and_then(Value::as_object_mut) {
            o.insert("as_of".into(), Value::Null);
        }
    }
    v
}

#[test]
fn cached_views_follow_the_data() {
    let (dir, conn) = fresh_db("follow");
    let spend = |c: &rusqlite::Connection| cache::spending(&Ctx::offline(c), None, None).unwrap();
    // the first compute seeds the default category rules (a write the
    // fingerprint sees), so the cache holds from the second call on
    let _ = spend(&conn);
    let first = spend(&conn);
    let again = spend(&conn);
    assert!(std::sync::Arc::ptr_eq(&first, &again), "unchanged data: served from the cache");

    // a new row (COUNT) is seen right away, even inside the racy window
    db::insert_bank_transactions(&conn, &[txn("2026-09-15", -500.0, "BIG SHOP", 99)]).unwrap();
    let after_insert = spend(&conn);
    assert_ne!(*after_insert, *first);

    // a recategorization (category content) too
    conn.execute("UPDATE bank_transactions SET category='Rent' WHERE raw_hash='h99'", []).unwrap();
    let after_recat = spend(&conn);
    assert_ne!(*after_recat, *after_insert);

    // another connection's write (a sync, the CLI) as well
    let other = db::connect(&dir.join("finance.db")).unwrap();
    other.execute("DELETE FROM bank_transactions WHERE raw_hash='h99'", []).unwrap();
    drop(other);
    let after_delete = spend(&conn);
    assert_eq!(*after_delete, *first, "back to the original rows: the original view");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bundle_bytes_equal_the_bundle() {
    let (dir, conn) = fresh_db("bytes");
    let ctx = Ctx::offline(&conn);
    let fresh = export::bundle(&ctx, None);
    let nw = fresh["net_worth"].clone();
    for round in 0..2 {
        // round 0 builds the serialized tail, round 1 serves it from the cache
        let bytes = export::bundle_bytes(&Ctx::offline(&conn), &nw);
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(untimed(parsed.clone()), untimed(fresh.clone()), "round {round}");
        // key order and formatting too: re-serializing gives the same bytes
        assert_eq!(serde_json::to_vec(&parsed).unwrap(), bytes, "round {round}");
        // the cached overview's as_of is stamped with the request time
        let as_of = parsed["overview"]["net_worth"]["as_of"].as_str().unwrap();
        assert_eq!(as_of.len(), 32);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_context_that_saw_a_write_caches_nothing_stale() {
    let (dir, conn) = fresh_db("midwrite");
    finance_engine::invest_setup::ensure(&conn).unwrap();
    let _ = cache::costs(&Ctx::offline(&conn)); // settles the default rules' seed
    // a long request (the warm-up pass): reads the setup, then an admin edit
    // commits on another connection before it computes the costs
    let slow = Ctx::offline(&conn);
    assert!(slow.invest().platform().is_none());
    let other = db::connect(&dir.join("finance.db")).unwrap();
    let plan = finance_engine::invest_setup::Account {
        name: "Plan".into(),
        valued_by: "cost".into(),
        platform_flat_month: Some(5.0),
        platform_cliff: Some(0.0),
        ..Default::default()
    };
    finance_engine::invest_setup::put_account(&other, None, &plan).unwrap();
    drop(other);
    let stale = cache::costs(&slow).unwrap();
    assert!(stale["flexinvest"].is_null(), "it answers from what it read");
    let fresh = cache::costs(&Ctx::offline(&conn)).unwrap();
    assert_eq!(fresh["flexinvest"]["cliff_hkd"], serde_json::json!(0), "the next request sees the edit, not the stale value");
    let _ = std::fs::remove_dir_all(dir);
}
