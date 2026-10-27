//! The owner's personal rules (an employer, family, landlords and their
//! shares, the owner's own names, a partner, an office) live in the owner's
//! database, not in the code: DEFAULT_RULES is the generic seed only. This
//! checks, on a COPY of the owner's db, that those rows still categorize as
//! before. The cases are personal too, so they are a fixture in
//! $FINANCE_PARITY_DIR (owner_rules.json: [{description, amount, category,
//! share}]); the test skips when it is absent. Nothing from there is ever
//! committed.

use std::path::PathBuf;

use serde_json::Value;

#[test]
fn the_owners_rules_live_in_the_owners_db() {
    let Some(dir) = std::env::var_os("FINANCE_PARITY_DIR").map(PathBuf::from) else {
        eprintln!("skipping: FINANCE_PARITY_DIR unset");
        return;
    };
    let (db, cases) = (dir.join("prod.db"), dir.join("owner_rules.json"));
    if !db.exists() || !cases.exists() {
        eprintln!("skipping: no owner rules fixture at {}", dir.display());
        return;
    }
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&cases).unwrap()).unwrap();
    let tmp = std::env::temp_dir().join(format!("finance-owner-rules-{}.db", std::process::id()));
    std::fs::copy(&db, &tmp).unwrap();
    let conn = finance_engine::db::connect(&tmp).unwrap();
    // as the server has it: the managed columns, every existing rule the owner's
    finance_engine::categories::ensure(&conn).unwrap();
    let rules = finance_engine::categorize::load_rules(&conn).unwrap();
    let seed: i64 = conn.query_row("SELECT COUNT(*) FROM category_rules WHERE origin = 'seed'", [], |r| r.get(0)).unwrap();
    assert_eq!(seed, 0, "an existing tree's rules are all the owner's (user rules)");
    let mut failed = Vec::new();
    for c in &cases {
        let desc = c["description"].as_str().unwrap();
        let amount = c["amount"].as_f64().unwrap();
        let (cat, share) = finance_engine::categorize::match_rule(desc, amount, &rules);
        if cat != c["category"].as_str().unwrap() || share != c["share"].as_f64() {
            failed.push(format!("{desc:?} {amount}: got {cat} {share:?}, want {} {}", c["category"], c["share"]));
        }
        // and none of them comes from the code any more
        let lower = desc.to_lowercase();
        let from_code = finance_engine::categorize::DEFAULT_RULES
            .iter()
            .find(|r| finance_engine::categorize::pattern_hits(r.1, r.4, &lower, amount))
            .map(|r| r.2);
        if from_code == c["category"].as_str() && from_code != Some("Rent") {
            failed.push(format!("{desc:?}: DEFAULT_RULES still files it to {from_code:?}"));
        }
    }
    let _ = std::fs::remove_file(&tmp);
    assert!(failed.is_empty(), "{failed:#?}");
    eprintln!("{} owner cases OK", cases.len());
}
