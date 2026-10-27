//! Accounts that never finished signing in (profiles::prune_unfinished,
//! cancel_unfinished): the start drops them, except the one "Add an
//! account…" just restarted on; closing its wizard drops it and goes back to
//! the account in use before; an account with a token or a local copy
//! stays, the demo stays, the last account on the computer stays.

use finance_core::{auth, cache, demo, profiles};

#[test]
fn unfinished_accounts_go_finished_ones_stay() {
    let root = std::env::temp_dir().join(format!("finance-unfinished-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (data, cfg) = (root.join("data"), root.join("config"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    std::env::set_var("FINANCE_DESKTOP_DATA", &data);
    std::env::set_var("FINANCE_DESKTOP_SERVER_FILE", cfg.join("server"));
    std::env::remove_var("FINANCE_SERVER");
    std::env::remove_var("FINANCE_TOKEN");
    std::fs::write(cfg.join("server"), "https://server.tail1234.ts.net\n").unwrap();
    let url = "https://server.tail1234.ts.net";

    // the owner: signed in, with a local copy
    let me = profiles::active();
    std::fs::create_dir_all(profiles::dir_of(&me.id)).unwrap();
    std::fs::write(profiles::dir_of(&me.id).join("finance.db"), b"SQLite format 3\0owner").unwrap();
    auth::save_token(&me.token_key, "fin_owner").unwrap();
    profiles::set_identity("Alex", Some(1)).unwrap();
    let me = profiles::active();
    assert!(!profiles::is_unfinished(&me));
    assert!(profiles::prune_unfinished(None).is_empty(), "nothing to prune");

    // Add an account…: a profile, made active, the app restarts on its wizard
    let added = profiles::add(url, "").unwrap();
    profiles::set_active(&added.id).unwrap();
    std::fs::create_dir_all(profiles::dir_of(&added.id)).unwrap();
    std::fs::write(profiles::dir_of(&added.id).join("replica.json"), b"{}").unwrap();
    assert!(profiles::is_unfinished(&profiles::active()));
    assert!(profiles::cancellable());
    // that start keeps it
    assert!(profiles::prune_unfinished(Some(&added.id)).is_empty());
    assert_eq!(profiles::active().id, added.id);
    // in the menu it reads as something, not a bare URL
    assert_eq!(profiles::display(&profiles::active()), "Not signed in · server");
    assert_eq!(profiles::display(&me), "Alex · server");

    // quit mid-wizard, start again: it goes, the owner is back, untouched
    assert_eq!(profiles::prune_unfinished(None), [added.id.clone()]);
    assert_eq!(profiles::active(), me);
    assert_eq!(profiles::list(), [me.clone()]);
    assert!(!profiles::dir_of(&added.id).exists());
    assert_eq!(auth::token(url).as_deref(), Some("fin_owner"));
    assert_eq!(std::fs::read(profiles::dir_of(&me.id).join("finance.db")).unwrap(), b"SQLite format 3\0owner");
    assert_eq!(cache::server_url(), url);

    // the wizard's X before signing in: the same, at once
    let added = profiles::add(url, "").unwrap();
    profiles::set_active(&added.id).unwrap();
    assert!(profiles::cancel_unfinished().unwrap());
    assert_eq!(profiles::active(), me);
    assert_eq!(profiles::list().len(), 1);
    assert!(!profiles::cancel_unfinished().unwrap(), "a signed-in account is never cancelled");

    // signed in, no copy yet: it stays (and so does the owner)
    let sam = profiles::add(url, "").unwrap();
    profiles::set_active(&sam.id).unwrap();
    auth::save_token(url, "fin_sam").unwrap();
    profiles::set_identity("Sam", Some(2)).unwrap();
    assert!(!profiles::cancel_unfinished().unwrap());
    assert!(profiles::prune_unfinished(None).is_empty());
    // another server, signed out there but with a copy left: it stays
    let lap = profiles::add("http://127.0.0.1:8787", "Ann").unwrap();
    std::fs::create_dir_all(profiles::dir_of(&lap.id)).unwrap();
    std::fs::write(profiles::dir_of(&lap.id).join("finance.db.enc"), b"sealed").unwrap();
    // a named account signed out with nothing left: a ghost, it goes
    let ghost = profiles::add("https://other.example", "Bob").unwrap();
    profiles::set_active(&me.id).unwrap();
    assert!(!profiles::menu_list().is_empty());
    assert_eq!(profiles::prune_unfinished(None), [ghost.id.clone()]);
    assert_eq!(profiles::list().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), [me.id.as_str(), sam.id.as_str(), lap.id.as_str()]);
    assert_eq!(auth::token(url).as_deref(), Some("fin_owner"));

    // the menu hides an unnamed account with nothing, the active one excepted
    let stray = profiles::add(url, "").unwrap();
    assert!(!profiles::menu_list().iter().any(|p| p.id == stray.id));
    profiles::set_active(&stray.id).unwrap();
    assert!(profiles::menu_list().iter().any(|p| p.id == stray.id));
    profiles::set_active(&me.id).unwrap();
    assert_eq!(profiles::prune_unfinished(None), [stray.id.clone()]);

    // the demo stays, copy or not
    profiles::enter_demo().unwrap();
    assert!(profiles::prune_unfinished(None).is_empty());
    assert!(demo::active());
    profiles::leave_demo().unwrap();
    assert_eq!(profiles::active(), me);

    // a first run's only account is unfinished until it signs in: it stays
    std::env::set_var("FINANCE_DESKTOP_SERVER_FILE", root.join("fresh/server"));
    std::env::set_var("FINANCE_DESKTOP_DATA", root.join("fresh-data"));
    let only = profiles::active();
    assert!(profiles::is_unfinished(&only));
    assert!(!profiles::cancellable());
    assert!(profiles::prune_unfinished(None).is_empty());
    assert!(!profiles::cancel_unfinished().unwrap());
    // two, neither signed in: the one in use stays
    let other = profiles::add("https://other.example", "").unwrap();
    assert_eq!(profiles::prune_unfinished(None), [other.id]);
    assert_eq!(profiles::list(), [only]);
    let _ = std::fs::remove_dir_all(&root);
}
