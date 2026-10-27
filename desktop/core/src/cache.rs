//! The local replica of the active account (crate::profiles):
//! `~/.local/share/finance-desktop/profiles/<id>/{finance.db,export.json}`.
//! Files are replaced atomically (write to `*.tmp`, rename), so a sync that
//! dies halfway never leaves a torn cache behind.

use std::path::PathBuf;

use anyhow::{Context, Result};

pub const DB_FILE: &str = "finance.db";
pub const BUNDLE_FILE: &str = "export.json";

/// The active profile's replica folder (each account on this machine has
/// its own; `$FINANCE_DESKTOP_DATA` moves the parent).
pub fn dir() -> PathBuf {
    crate::profiles::dir_of(&crate::profiles::active().id)
}

pub fn db_path() -> PathBuf {
    dir().join(DB_FILE)
}

pub fn bundle_path() -> PathBuf {
    dir().join(BUNDLE_FILE)
}

/// Where the server URL is kept: `$FINANCE_DESKTOP_SERVER_FILE` (dev and
/// screenshots: a file of their own), else `~/.config/finance-desktop/server`.
pub fn server_file() -> PathBuf {
    std::env::var_os("FINANCE_DESKTOP_SERVER_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::config_dir().unwrap_or_default().join("finance-desktop").join("server"))
}

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8787";

/// The first line of [`server_file`], if any.
pub fn saved_server_file_url() -> Option<String> {
    std::fs::read_to_string(server_file())
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().trim_end_matches('/').to_string()))
        .filter(|s| !s.is_empty())
}

/// The active account's server: its profile's URL once the app has made
/// the profiles, else the server file's (a tool that runs before the app,
/// like a laptop server's push, never creates them).
pub fn saved_server_url() -> Option<String> {
    if crate::profiles::file().exists() {
        return Some(crate::profiles::active().url);
    }
    saved_server_file_url()
}

/// Server base URL: the demo's when it is the active account (crate::demo),
/// else `$FINANCE_SERVER`, else the active account's, else a local dev server.
pub fn server_url() -> String {
    // the demo account talks to nobody, whatever the environment says
    if crate::demo::active() {
        return crate::demo::URL.into();
    }
    if let Ok(url) = std::env::var("FINANCE_SERVER") {
        return url.trim_end_matches('/').to_string();
    }
    saved_server_url().unwrap_or_else(|| DEFAULT_SERVER.into())
}

/// The active account's server moved: its profile and the server file.
pub fn save_server_url(url: &str) -> Result<()> {
    crate::profiles::load();
    crate::profiles::set_active_url(url)
}

/// The server file's first line (profiles.rs keeps it on the active URL).
pub fn write_server_file(url: &str) -> Result<()> {
    let path = server_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, format!("{}\n", url.trim().trim_end_matches('/')))
        .with_context(|| format!("writing {}", path.display()))
}

/// Total size of the cached replica files, in bytes.
pub fn size_bytes() -> u64 {
    [db_path(), bundle_path()].iter().filter_map(|p| std::fs::metadata(crate::replica::stored_path(p)).ok()).map(|m| m.len()).sum()
}

/// A plain write; the replica's own files go through [`crate::replica::write`].
pub fn write_atomic(name: &str, bytes: &[u8]) -> Result<()> {
    let dir = dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(name)).context("replacing cache file")?;
    Ok(())
}

/// The profile picture, as the last sync found it (256x256 PNG).
pub const AVATAR_FILE: &str = "avatar.png";

/// The cached profile picture: None for initials, or before a sync.
pub fn avatar_bytes() -> Option<Vec<u8>> {
    // sealed with the rest of the local copy when encryption is on
    crate::replica::read(&dir().join(AVATAR_FILE)).ok()
}

/// Keep the picture (None: there is none, drop the old one).
pub fn set_avatar(png: Option<&[u8]>) -> Result<()> {
    match png {
        Some(b) => crate::replica::write(AVATAR_FILE, b),
        None => {
            let p = dir().join(AVATAR_FILE);
            let _ = std::fs::remove_file(crate::replica::stored_path(&p));
            let _ = std::fs::remove_file(p);
            Ok(())
        }
    }
}

/// Seconds since the cache was last written (the bundle's mtime), if any.
pub fn age_secs() -> Option<u64> {
    let modified = std::fs::metadata(crate::replica::stored_path(&bundle_path())).ok()?.modified().ok()?;
    modified.elapsed().ok().map(|d| d.as_secs())
}
