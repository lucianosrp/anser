//! The accounts on this machine (finance_core::profiles): the first run
//! moves the single replica into the first account's folder without a
//! download and keeps its token key (the URL), a second account on the
//! same server gets its own folder and token key, switching moves
//! cache::dir and the server URL, removing one forgets its token and its
//! copy.

use finance_core::{auth, cache, profiles};

#[test]
fn profiles_migrate_add_switch_remove() {
    let root = std::env::temp_dir().join(format!("finance-profiles-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (data, cfg) = (root.join("data"), root.join("config"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    std::env::set_var("FINANCE_DESKTOP_DATA", &data);
    std::env::set_var("FINANCE_DESKTOP_SERVER_FILE", cfg.join("server"));
    std::env::remove_var("FINANCE_SERVER");
    std::env::remove_var("FINANCE_TOKEN");
    // the machine as it was: one replica, one server, one token
    std::fs::write(cfg.join("server"), "https://server.tail1234.ts.net/\n").unwrap();
    std::fs::write(data.join("finance.db"), b"SQLite format 3\0owner").unwrap();
    std::fs::write(data.join("export.json"), b"{}").unwrap();
    std::fs::write(data.join("replica.json"), b"{}").unwrap();
    std::fs::write(cfg.join("tokens.json"), br#"{"https://server.tail1234.ts.net": "fin_owner"}"#).unwrap();
    assert!(!profiles::file().exists());
    // a tool that runs before the app sees the server file, and makes nothing
    assert_eq!(cache::saved_server_url().as_deref(), Some("https://server.tail1234.ts.net"));
    assert_eq!(auth::token("https://server.tail1234.ts.net").as_deref(), Some("fin_owner"));
    assert!(!profiles::file().exists());

    // the app: the replica moves into the first account's folder
    let dir = cache::dir();
    assert_eq!(dir, data.join("profiles/server.tail1234.ts.net"));
    assert_eq!(std::fs::read(dir.join("finance.db")).unwrap(), b"SQLite format 3\0owner", "moved, not fetched again");
    assert!(dir.join("export.json").exists() && dir.join("replica.json").exists());
    assert!(!data.join("finance.db").exists());
    let first = profiles::active();
    assert_eq!(first.token_key, "https://server.tail1234.ts.net");
    assert_eq!(auth::token(&cache::server_url()).as_deref(), Some("fin_owner"), "the pairing carried over");

    // a second account on the same server
    let sam = profiles::add("https://server.tail1234.ts.net", "Sam").unwrap();
    assert_eq!(sam.id, "server.tail1234.ts.net-sam");
    assert_eq!(sam.token_key, "https://server.tail1234.ts.net#server.tail1234.ts.net-sam");
    profiles::set_active(&sam.id).unwrap();
    assert_eq!(cache::dir(), data.join("profiles/server.tail1234.ts.net-sam"));
    assert_eq!(cache::server_url(), "https://server.tail1234.ts.net");
    assert_eq!(auth::token(&cache::server_url()), None, "Sam's account isn't paired yet");
    auth::save_token(&cache::server_url(), "fin_sam").unwrap();
    assert_eq!(auth::token(&cache::server_url()).as_deref(), Some("fin_sam"));
    profiles::set_identity("Sam", Some(2)).unwrap();
    // back to the owner: their token, their copy
    profiles::set_active(&first.id).unwrap();
    assert_eq!(auth::token(&cache::server_url()).as_deref(), Some("fin_owner"));
    assert_eq!(cache::dir(), dir);
    assert_eq!(std::fs::read_to_string(cfg.join("server")).unwrap().trim(), "https://server.tail1234.ts.net");
    // another server
    let lap = profiles::add("http://127.0.0.1:8787", "").unwrap();
    assert_eq!((lap.id.as_str(), lap.token_key.as_str()), ("127.0.0.1-8787-account", "http://127.0.0.1:8787"));
    assert_eq!(profiles::list().len(), 3);

    // removing Sam's account forgets his token and his copy
    std::fs::create_dir_all(profiles::dir_of(&sam.id)).unwrap();
    std::fs::write(profiles::dir_of(&sam.id).join("finance.db"), b"sam").unwrap();
    profiles::remove(&sam.id).unwrap();
    assert!(!profiles::dir_of(&sam.id).exists());
    let tokens = std::fs::read_to_string(cfg.join("tokens.json")).unwrap();
    assert!(tokens.contains("fin_owner") && !tokens.contains("fin_sam"), "{tokens}");
    profiles::remove(&lap.id).unwrap();
    assert!(profiles::remove(&first.id).is_err(), "the last account stays");
    let _ = std::fs::remove_dir_all(&root);
}
