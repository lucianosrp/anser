//! The owner's upgrade, on a copy of the parity database: the first start
//! of an auth-aware server makes the tree user 1 (admin) in place, backs
//! finance.db up first, and changes nothing in finance.db but adding the
//! profile, user_settings, categories and app_meta tables and the rules'
//! managed columns (every other table's rows and schema
//! are identical). Skipped without $FINANCE_PARITY_DIR. Never touches the
//! parity files themselves: everything runs on a scratch copy.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;

/// table -> (its CREATE statement, every row as text, in rowid order)
fn dump(path: &Path) -> BTreeMap<String, (String, Vec<String>)> {
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut st = c.prepare("SELECT name, sql FROM sqlite_master WHERE type IN ('table', 'index') AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    let tables: Vec<(String, Option<String>)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
    let mut out = BTreeMap::new();
    for (name, sql) in tables {
        let is_table: bool = c.query_row("SELECT type = 'table' FROM sqlite_master WHERE name = ?", [&name], |r| r.get(0)).unwrap();
        let rows = if is_table {
            let mut q = c.prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid")).or_else(|_| c.prepare(&format!("SELECT * FROM \"{name}\""))).unwrap();
            let n = q.column_count();
            q.query_map([], |r| {
                let mut s = String::new();
                for i in 0..n {
                    let v: rusqlite::types::Value = r.get(i)?;
                    s.push_str(&format!("{v:?}|"));
                }
                Ok(s)
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
        } else {
            vec![]
        };
        out.insert(name, (sql.unwrap_or_default(), rows));
    }
    out
}

#[test]
fn owner_tree_becomes_user_one_in_place() {
    let Some(parity) = std::env::var_os("FINANCE_PARITY_DIR").map(std::path::PathBuf::from) else {
        eprintln!("FINANCE_PARITY_DIR unset: skipped");
        return;
    };
    let prod = parity.join("prod.db");
    if !prod.exists() {
        eprintln!("{} missing: skipped", prod.display());
        return;
    }
    let home = std::env::temp_dir().join(format!("finance-owner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("data")).unwrap();
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    let db = home.join("data/finance.db");
    std::fs::copy(&prod, &db).unwrap();
    // the .env of an existing install stays where it is
    std::fs::write(home.join(".env"), "OKX_API_KEY=abcdefghijklmnop1234\nFINANCE_REMOTE=http://x\n").unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", &db);
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::remove_var("FINANCE_AUTH");

    // what the running (pre-auth) server leaves: its own schema and migrations
    drop(finance_engine::db::connect(&db).unwrap());
    let before = dump(&db);

    let boot = finance_server::auth::bootstrap().unwrap();
    assert!(boot.created);
    assert_eq!(boot.default_mode, Some(finance_server::auth::Mode::All), "an existing tree starts closed too");
    assert!(boot.profile_seeded);
    let backup = boot.backup.expect("a backup before the migration");
    assert!(backup.starts_with(home.join("data/backups/pre-auth")));
    assert_eq!(dump(&backup), before, "the backup is the database as it was");

    let after = dump(&db);
    let mut added: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    added.sort();
    // Phase 3 too: the categories table and app_meta; the rules gain their
    // managed columns (origin user, enabled) and nothing else of them moves
    // Phase 4: the investment accounts and instruments
    assert_eq!(added, vec!["app_meta", "categories", "instruments", "investment_accounts", "profile", "user_settings"]);
    assert!(boot.categories_migrated);
    for (name, v) in &before {
        if name == "category_rules" {
            continue;
        }
        assert_eq!(after.get(name), Some(v), "{name} changed");
    }
    let rules = |p: &Path| -> Vec<String> {
        let c = Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut st = c.prepare("SELECT id, priority, pattern, category, is_transfer, direction, share FROM category_rules ORDER BY rowid").unwrap();
        st.query_map([], |r| {
            Ok((0..7).map(|i| format!("{:?}", r.get::<_, rusqlite::types::Value>(i).unwrap())).collect::<Vec<_>>().join("|"))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    assert_eq!(rules(&backup), rules(&db), "the rules' own columns are untouched");
    let c = Connection::open(&db).unwrap();
    let not_user: i64 = c.query_row("SELECT COUNT(*) FROM category_rules WHERE origin <> 'user' OR enabled <> 1", [], |r| r.get(0)).unwrap();
    assert_eq!(not_user, 0, "every existing rule is an enabled user rule");
    let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
    assert_eq!(ok, "ok");

    // user 1, admin, home = the tree; the owner's profile
    let users = finance_server::auth::users().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!((users[0].id, users[0].role.as_str()), (1, "admin"));
    assert_eq!(Path::new(&users[0].home), home.as_path());
    let p = finance_engine::profile::get(&c).unwrap().unwrap();
    assert_eq!((p.base_currency.as_str(), p.home_country.as_deref(), p.onboarding_step.as_deref()), (finance_engine::config::BASE_CURRENCY, None, Some("done")));
    assert!(p.name.as_deref().is_some_and(|n| !n.is_empty()), "the name from the self-transfer rules");
    // server.db is separate and private; no file moved
    assert!(home.join("data/server.db").exists());
    assert!(home.join(".env").exists());
    assert_eq!(finance_engine::secrets::configured_via("OKX_API_KEY"), Some(finance_engine::secrets::Via::DotEnv));

    // a second start changes nothing
    let again = finance_server::auth::bootstrap().unwrap();
    assert!(!again.created && again.backup.is_none() && !again.profile_seeded);
    assert_eq!(dump(&db), after);

    // import .env: the keys move to the store, .env.bak keeps the old file
    let keys = finance_engine::secrets::import_env(1).unwrap();
    assert_eq!(keys, vec!["OKX_API_KEY".to_string()]);
    assert_eq!(finance_engine::secrets::configured_via("OKX_API_KEY"), Some(finance_engine::secrets::Via::Store));
    let env = std::fs::read_to_string(home.join(".env")).unwrap();
    assert!(env.contains("# imported into the secrets store: OKX_API_KEY=") && env.contains("\nFINANCE_REMOTE=http://x"), "{env}");
    assert!(std::fs::read_to_string(home.join(".env.bak")).unwrap().starts_with("OKX_API_KEY="));
    let _ = std::fs::remove_dir_all(&home);
}
