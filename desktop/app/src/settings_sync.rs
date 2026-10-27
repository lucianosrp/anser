//! Budgets, the calendar and saved filters follow the server's
//! user_settings (finance_core::user_settings): on every load the app takes
//! them from the bundle, and the desktop moves its old per-machine toml files
//! up once.
//!
//! The migration, per file: when the server keeps settings, has nothing for
//! that key yet, and the local file exists, the file's content is written
//! to the server and the file is renamed to `<name>.migrated.bak` (the
//! backup, and the sign it's done). When the server already has the key
//! (another device moved first), the local file is left alone and unused.

use finance_core::{user_settings, Bundle};
use gpui_kit::App;

use crate::budgets::Budgets;

/// After each load: the settings, then the Budgets global that pages read.
pub fn on_bundle(bundle: &Bundle, cx: &mut App) {
    user_settings::set_from_bundle(bundle);
    #[cfg(not(target_family = "wasm"))]
    migrate_once();
    if user_settings::server_side() {
        cx.set_global(Budgets::load());
    }
}

/// The local file of each setting, and its JSON form.
#[cfg(not(target_family = "wasm"))]
fn local_files() -> Vec<(&'static str, std::path::PathBuf, fn(&str) -> Option<serde_json::Value>)> {
    fn budgets(s: &str) -> Option<serde_json::Value> {
        serde_json::to_value(toml::from_str::<Budgets>(s).ok()?).ok()
    }
    fn calendar(s: &str) -> Option<serde_json::Value> {
        serde_json::to_value(toml::from_str::<crate::pages::cashflow::Config>(s).ok()?).ok()
    }
    fn filters(s: &str) -> Option<serde_json::Value> {
        serde_json::to_value(toml::from_str::<crate::filterbar::SavedFilters>(s).ok()?).ok()
    }
    vec![
        (crate::budgets::KEY, crate::budgets::path(), budgets),
        (crate::pages::cashflow::KEY, crate::pages::cashflow::config_path(), calendar),
        (crate::filterbar::FILTERS_KEY, crate::filterbar::saved_path(), filters),
    ]
}

#[cfg(not(target_family = "wasm"))]
fn migrate_once() {
    // the demo's settings are made up: this machine's files stay where they are
    if !user_settings::server_side() || finance_core::demo::active() {
        return;
    }
    for (key, path, to_json) in local_files() {
        if user_settings::get(key).is_some() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Some(value) = to_json(&text) else {
            eprintln!("anser: {} doesn't parse; not moved to the server", path.display());
            continue;
        };
        let bak = path.with_extension("toml.migrated.bak");
        if std::fs::copy(&path, &bak).is_err() {
            continue; // no backup, no move
        }
        if user_settings::save(key, value) {
            let _ = std::fs::remove_file(&path);
            eprintln!("anser: moved {} to the server's {key} (backup: {})", path.display(), bak.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_files_become_the_server_json() {
        let files = local_files();
        let b = (files[0].2)("[monthly]\nDining = 2200\n").unwrap();
        assert_eq!(b, serde_json::json!({"monthly": {"Dining": 2200.0}}));
        let c = (files[1].2)("low_balance = 1000.0\naccounts = [\"hsbc\"]\n").unwrap();
        assert_eq!(c["low_balance"], serde_json::json!(1000.0));
        assert_eq!(c["accounts"], serde_json::json!(["hsbc"]));
        let f = (files[2].2)("[[filter]]\nname = \"HK dining\"\nquery = \"cat:dining\"\n").unwrap();
        assert_eq!(f, serde_json::json!({"filter": [{"name": "HK dining", "query": "cat:dining"}]}));
        // and back: what the app reads from the bundle
        let back: crate::filterbar::SavedFilters = serde_json::from_value(f).unwrap();
        assert_eq!(back.filters[0].name, "HK dining");
        assert!((files[0].2)("not toml [").is_none());
    }

    #[test]
    fn migrates_once_with_a_backup() {
        let dir = std::env::temp_dir().join(format!("finance-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let budgets = dir.join("budgets.toml");
        let calendar = dir.join("calendar.toml");
        std::fs::write(&budgets, "[monthly]\nDining = 2200\n").unwrap();
        std::fs::write(&calendar, "low_balance = 1000.0\n").unwrap();
        unsafe { std::env::set_var("FINANCE_DESKTOP_BUDGETS", &budgets); }
        unsafe { std::env::set_var("FINANCE_DESKTOP_CALENDAR", &calendar); }
        unsafe { std::env::set_var("FINANCE_DESKTOP_FILTERS", dir.join("filters.toml")); }
        // nothing listens here: the write fails quietly, the local copy holds
        unsafe { std::env::set_var("FINANCE_SERVER", "http://127.0.0.1:9"); }
        // an older server: nothing moves
        user_settings::set_from_bundle(&Bundle(serde_json::json!({"version": 1})));
        migrate_once();
        assert!(budgets.exists());
        // a server that keeps settings and already has a calendar
        user_settings::set_from_bundle(&Bundle(serde_json::json!({"user_settings": {"calendar": {"low_balance": 5.0}}})));
        migrate_once();
        assert!(!budgets.exists() && budgets.with_extension("toml.migrated.bak").exists());
        assert_eq!(user_settings::get("budgets"), Some(serde_json::json!({"monthly": {"Dining": 2200.0}})));
        assert!(calendar.exists(), "the server's calendar wins; the local file is left alone");
        assert_eq!(Budgets::load().monthly.get("Dining"), Some(&2200.0));
        migrate_once(); // once: nothing left to move
        assert_eq!(std::fs::read_to_string(budgets.with_extension("toml.migrated.bak")).unwrap(), "[monthly]\nDining = 2200\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
