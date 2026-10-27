//! The Phase 3 migration must not move the owner's categorization: on a
//! scratch copy of the parity prod.db, categorize_all before and after
//! categories::ensure gives the same category and share on every row, the
//! category flags answer every question the hard-coded lists did, and a
//! recategorize right after finds nothing to change. Needs personal data:
//! reads $FINANCE_PARITY_DIR/prod.db, skips when absent.

use std::collections::BTreeMap;
use std::path::PathBuf;

use finance_engine::{categories, categorize, db, rules};

fn prod_copy(tag: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("FINANCE_PARITY_DIR")?);
    let src = dir.join("prod.db");
    if !src.exists() {
        return None;
    }
    let tmp = std::env::temp_dir().join(format!("finance-cat-mig-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let dst = tmp.join("finance.db");
    std::fs::copy(&src, &dst).unwrap();
    for ext in ["-wal", "-shm"] {
        let s = dir.join(format!("prod.db{ext}"));
        if s.is_file() {
            std::fs::copy(&s, tmp.join(format!("finance.db{ext}"))).unwrap();
        }
    }
    Some(dst)
}

type Snap = BTreeMap<(String, String), (Option<String>, Option<f64>)>;

fn snapshot(c: &rusqlite::Connection) -> Snap {
    let mut st = c.prepare("SELECT source, raw_hash, category, share FROM bank_transactions").unwrap();
    st.query_map([], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))).unwrap().map(Result::unwrap).collect()
}

#[test]
fn the_migration_keeps_every_row_where_it_was() {
    let Some(path) = prod_copy("rows") else {
        eprintln!("skipping: no FINANCE_PARITY_DIR/prod.db");
        return;
    };
    let c = db::connect(&path).unwrap();
    let stored = snapshot(&c);
    categorize::categorize_all(&c).unwrap();
    let before = snapshot(&c);
    let flags_before = categories::Flags::load(&c);
    let rules_before: Vec<(i64, String, String)> = categorize::load_rules(&c).unwrap().into_iter().map(|r| (r.id, r.pattern, r.category)).collect();

    assert!(categories::ensure(&c).unwrap(), "the migration ran");
    let order_after: Vec<(i64, String, String)> = categorize::load_rules(&c).unwrap().into_iter().map(|r| (r.id, r.pattern, r.category)).collect();
    assert_eq!(rules_before, order_after, "the effective order is the old (priority, id) order");
    let n_rules: i64 = c.query_row("SELECT COUNT(*) FROM category_rules WHERE origin = 'user' AND enabled = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(n_rules as usize, rules_before.len(), "every rule became an enabled user rule");

    let moved = categorize::recategorize(&c).unwrap();
    assert!(moved.is_empty(), "a recategorize after the migration moved {} rows: {:?}", moved.len(), &moved[..moved.len().min(5)]);
    categorize::categorize_all(&c).unwrap();
    let after = snapshot(&c);
    assert_eq!(before.len(), after.len());
    let diffs: Vec<_> = before.iter().filter(|(k, v)| after.get(*k) != Some(*v)).collect();
    assert!(diffs.is_empty(), "{} rows changed: {:?}", diffs.len(), &diffs[..diffs.len().min(5)]);

    // the flags answer as the hard-coded lists did, for every category in use
    let flags_after = categories::Flags::load(&c);
    assert!(flags_after.has_table() && !flags_before.has_table());
    let cats: Vec<String> = categories::list(&c).unwrap().into_iter().map(|x| x.name).collect();
    for n in &cats {
        let n = n.as_str();
        let q = |f: &categories::Flags| {
            (f.is_transfer(n), f.is_income(n), f.is_investment(n), f.is_deposit(n), f.is_partner(n), f.is_reimbursable(n), f.fixed_always(n), f.fixed_never(n))
        };
        assert_eq!(q(&flags_before), q(&flags_after), "{n}");
    }
    assert_eq!(flags_before.transfer_set(), flags_after.transfer_set());
    assert_eq!(flags_before.reimbursed_name(), flags_after.reimbursed_name());

    let changed_since_stored = stored.iter().filter(|(k, v)| after.get(*k) != Some(*v)).count();
    let shared = after.values().filter(|v| v.1.is_some()).count();
    let mut by_cat: BTreeMap<String, usize> = BTreeMap::new();
    for v in after.values() {
        *by_cat.entry(v.0.clone().unwrap_or_default()).or_default() += 1;
    }
    eprintln!(
        "migration proof: {} rows, {} rules (all user, same order), {} categories seeded, {} rows with a share, \
         0 rows changed by the migration ({} differ from the categories stored in the snapshot)",
        after.len(), rules_before.len(), cats.len(), shared, changed_since_stored
    );
    eprintln!("rows per category: {by_cat:?}");
    // the rules list reads the migrated table in the same order
    let listed = rules::list(&c).unwrap();
    assert_eq!(listed.iter().map(|r| r.id).collect::<Vec<_>>(), rules_before.iter().map(|r| r.0).collect::<Vec<_>>());
    assert_eq!(listed.iter().map(|r| r.hits).sum::<i64>() as usize + {
        let o: i64 = c.query_row("SELECT COUNT(*) FROM txn_overrides", [], |r| r.get(0)).unwrap();
        o as usize
    } + by_cat.get("Other").copied().unwrap_or(0) - c.query_row::<i64, _, _>(
        "SELECT COUNT(*) FROM bank_transactions b JOIN txn_overrides o ON o.source = b.source AND o.raw_hash = b.raw_hash WHERE o.category = 'Other'", [], |r| r.get(0)).unwrap() as usize,
        after.len(), "every row is decided by exactly one rule, an override, or nothing");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
