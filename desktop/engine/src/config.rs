//! Paths, env loading, the clock.
//!
//! Env overrides: FINANCE_HOME (the data tree root; default: the repo root
//! when run from a checkout, else ~/.local/share/finance), FINANCE_DB,
//! FINANCE_DATA_DIR, FINANCE_TRANSACTIONS_DIR, FINANCE_TODAY (YYYY-MM-DD,
//! freezes "today" for golden tests), FINANCE_OFFLINE=1 (fetchers serve
//! cached data only).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::NaiveDate;

/// The base currency of a tree that has no profile row at all (one from
/// before profiles existed, which was Hong Kong dollars throughout): every
/// `_hkd` figure is in the profile's base_currency (views::fx::base_currency).
/// A new user never reaches this: the setup wizard writes the profile.
pub const BASE_CURRENCY: &str = "HKD";
/// The base currency of a profile created without one being chosen.
pub const NEW_PROFILE_CURRENCY: &str = "USD";
/// The net worth's second figure (`total_eur`).
pub const SECONDARY_CURRENCY: &str = "EUR";
pub const DASHBOARD_HOST: &str = "127.0.0.1";
pub const DASHBOARD_PORT: u16 = 8787;

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Root of the server's tree: $FINANCE_HOME (holds the owner's data/,
/// transactions/ and .env, the shared secrets/, and users/<uid>/). Never
/// depends on a tenant: server.db, the web app and the secrets dir hang
/// off it.
pub fn root_home() -> PathBuf {
    if let Some(h) = var("FINANCE_HOME") {
        return PathBuf::from(h);
    }
    checkout_root(&PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share/finance"))
}

/// The server-wide data dir: FINANCE_DATA_DIR, else $FINANCE_HOME/data.
/// It is the owner's data dir too; server.db and the web app live here.
pub fn root_data_dir() -> PathBuf {
    var("FINANCE_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| root_home().join("data"))
}

/// The active tenant's tree root (crate::tenant: the owner's is
/// $FINANCE_HOME, another user's $FINANCE_HOME/users/<uid>).
pub fn home() -> PathBuf {
    crate::tenant::active().home.clone()
}

/// A checkout: `<repo>/desktop/engine` -> `<repo>`, recognized by the Rust
/// workspace (`desktop/Cargo.toml`).
fn checkout_root(manifest_dir: &std::path::Path) -> Option<PathBuf> {
    let repo = manifest_dir.join("../..");
    repo.join("desktop/Cargo.toml").is_file().then(|| repo.canonicalize().unwrap_or(repo))
}

/// The active tenant's data dir (finance.db, fetch_status.json, sync.lock,
/// backups/).
pub fn data_dir() -> PathBuf {
    crate::tenant::active().data_dir.clone()
}

pub fn db_path() -> PathBuf {
    crate::tenant::active().db.clone()
}

pub fn transactions_dir() -> PathBuf {
    crate::tenant::active().transactions.clone()
}

/// The server's .env (in the owner's tree): settings, and the owner's
/// keys from before the secrets store.
pub fn env_path() -> PathBuf {
    root_home().join(".env")
}

/// True when fetchers must not touch the network (golden runs, tests).
pub fn offline() -> bool {
    var("FINANCE_OFFLINE").is_some_and(|v| v != "0")
}

/// THE "today" for all data logic: the UTC date (every ledger timestamp is
/// UTC), or FINANCE_TODAY when pinned.
pub fn utc_today() -> NaiveDate {
    if let Some(d) = var("FINANCE_TODAY").and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()) {
        return d;
    }
    chrono::Utc::now().date_naive()
}

/// .env (KEY=VALUE lines), parsed on first use and again whenever the file
/// changes (mtime or size), so an import or an edit is live without a
/// restart. Values never get logged.
type Stamp = (Option<std::time::SystemTime>, u64);
static ENV: Mutex<Option<(PathBuf, Stamp, HashMap<String, String>)>> = Mutex::new(None);

fn dotenv() -> HashMap<String, String> {
    let path = env_path();
    let md = std::fs::metadata(&path).ok();
    let stamp = (md.as_ref().and_then(|m| m.modified().ok()), md.map(|m| m.len()).unwrap_or(0));
    if let Ok(g) = ENV.lock() {
        if let Some((p, s, m)) = g.as_ref() {
            if *p == path && *s == stamp {
                return m.clone();
            }
        }
    }
    let mut m = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(&path) {
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    m.insert(k.trim().to_string(), v.to_string());
                }
            }
        }
    }
    if let Ok(mut g) = ENV.lock() {
        *g = Some((path, stamp, m.clone()));
    }
    m
}

/// Forget the parsed .env (after rewriting it within the same second).
pub fn reload_dotenv() {
    if let Ok(mut g) = ENV.lock() {
        *g = None;
    }
}

/// One .env value (the secrets store's second provider).
pub fn dotenv_value(key: &str) -> Option<String> {
    dotenv().get(key).cloned()
}

/// A secret or setting. Source keys (secrets::SECRET_KEYS) come from the
/// encrypted store first, then .env, then the process environment; other
/// settings from .env, then the environment.
pub fn env(key: &str) -> Option<String> {
    if crate::secrets::is_secret_key(key) {
        return crate::secrets::lookup(key).map(|(v, _)| v);
    }
    dotenv_value(key).or_else(|| var(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_root_is_the_rust_workspace() {
        let tmp = std::env::temp_dir().join(format!("finance-home-{}", std::process::id()));
        let engine = tmp.join("desktop/engine");
        std::fs::create_dir_all(&engine).unwrap();
        assert_eq!(checkout_root(&engine), None, "no workspace: not a checkout");
        std::fs::write(tmp.join("desktop/Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(checkout_root(&engine), Some(tmp.canonicalize().unwrap()));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
