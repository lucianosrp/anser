//! The demo account on this machine (finance_core::demo, profiles): it is
//! a profile of its own whose "server" nothing ever talks to, whatever
//! the environment says; the server file keeps the real URL; leaving it
//! deletes its copy and puts the machine back as it was.

use finance_core::client::{self, Request};
use finance_core::{cache, demo, profiles, sync};

#[test]
fn the_demo_comes_and_goes_without_touching_the_real_account() {
    let root = std::env::temp_dir().join(format!("finance-demo-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (data, cfg) = (root.join("data"), root.join("config"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    std::env::set_var("FINANCE_DESKTOP_DATA", &data);
    std::env::set_var("FINANCE_DESKTOP_SERVER_FILE", cfg.join("server"));
    std::env::remove_var("FINANCE_TOKEN");
    std::fs::write(cfg.join("server"), "https://server.tail1234.ts.net\n").unwrap();
    let real = profiles::active();
    std::fs::create_dir_all(profiles::dir_of(&real.id)).unwrap();
    std::fs::write(profiles::dir_of(&real.id).join("finance.db"), b"SQLite format 3\0real").unwrap();
    assert!(!demo::active());

    // in: its replica first (the app builds it), then the profile
    std::fs::create_dir_all(profiles::dir_of(demo::ID)).unwrap();
    std::fs::write(profiles::dir_of(demo::ID).join("finance.db"), b"SQLite format 3\0demo").unwrap();
    profiles::enter_demo().unwrap();
    assert!(demo::active());
    assert_eq!(cache::dir(), data.join("profiles/_demo"));
    // nothing reaches a server, not even one named in the environment
    std::env::set_var("FINANCE_SERVER", "http://127.0.0.1:9");
    assert_eq!(cache::server_url(), demo::URL);
    std::env::remove_var("FINANCE_SERVER");
    assert_eq!(sync::pull().unwrap_err().to_string(), demo::NOTE);
    let who = client::block_on(client::send(Request::new("GET", "/api/whoami"))).unwrap();
    assert_eq!(who.json()["user"]["name"], demo::NAME);
    let w = client::block_on(client::send(Request::new("PUT", "/api/settings/budgets").json(&serde_json::json!({})))).unwrap();
    assert_eq!((w.status, w.error()), (403, demo::NOTE.to_string()));
    assert!(profiles::set_active_url("https://elsewhere.example").is_err(), "the demo can't be pointed at a server");
    assert!(finance_core::replica::enable(Some("long enough passphrase"), &[]).is_err(), "never encrypted");
    // the server file still names the real server
    assert_eq!(std::fs::read_to_string(cfg.join("server")).unwrap().trim(), "https://server.tail1234.ts.net");
    assert_eq!(profiles::display(&profiles::active()), demo::LABEL);
    assert_eq!(profiles::list().len(), 2);

    // out: the copy goes, the real account is back with its data
    assert!(profiles::leave_demo().unwrap(), "the real account has a local copy");
    assert!(!demo::active());
    assert!(!profiles::dir_of(demo::ID).exists());
    assert_eq!(profiles::active(), real);
    assert_eq!(profiles::list().len(), 1);
    assert_eq!(cache::server_url(), "https://server.tail1234.ts.net");
    assert_eq!(std::fs::read(profiles::dir_of(&real.id).join("finance.db")).unwrap(), b"SQLite format 3\0real");
    let _ = std::fs::remove_dir_all(&root);
}
