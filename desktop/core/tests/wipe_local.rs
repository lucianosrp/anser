//! Delete all, on this computer (profiles::wipe_local): every account's
//! local copy, token and the app's settings go; the backup key stays when
//! asked; nothing outside the app's own files is touched.

use finance_core::{auth, profiles};

#[test]
fn delete_all_here_leaves_a_first_run() {
    let root = std::env::temp_dir().join(format!("finance-wipe-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (data, cfg) = (root.join("data"), root.join("config"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    std::env::set_var("FINANCE_DESKTOP_DATA", &data);
    std::env::set_var("FINANCE_DESKTOP_SERVER_FILE", cfg.join("server"));
    std::env::remove_var("FINANCE_TOKEN");
    std::fs::write(cfg.join("server"), "https://server.tail1234.ts.net\n").unwrap();
    let me = profiles::active();
    std::fs::create_dir_all(profiles::dir_of(&me.id)).unwrap();
    std::fs::write(profiles::dir_of(&me.id).join("finance.db"), b"x").unwrap();
    auth::save_token(&me.token_key, "tok").unwrap();
    for f in ["theme.toml", "invest.toml.enc", "backup-identity.txt", "unrelated.txt"] {
        std::fs::write(cfg.join(f), b"x").unwrap();
    }

    profiles::wipe_local(true).unwrap();
    assert!(!data.exists(), "the local copies are gone");
    for gone in ["server", "profiles.json", "tokens.json", "theme.toml", "invest.toml.enc"] {
        assert!(!cfg.join(gone).exists(), "{gone}");
    }
    assert!(cfg.join("backup-identity.txt").exists(), "the backup key was kept");
    assert!(cfg.join("unrelated.txt").exists(), "not the app's: untouched");

    profiles::wipe_local(false).unwrap();
    assert!(!cfg.join("backup-identity.txt").exists());
    let _ = std::fs::remove_dir_all(&root);
}
