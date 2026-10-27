//! Web build stand-in for `cache.rs`: same API, no filesystem. The "server"
//! is the origin the page was served from, set once by the web entry point;
//! the "replica" is what [`crate::preload`] holds, and nothing is written.

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{bail, Result};

pub const DB_FILE: &str = "finance.db";
pub const BUNDLE_FILE: &str = "export.json";
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8787";

static SERVER: OnceLock<String> = OnceLock::new();

/// The page's origin (e.g. `https://server.tailnet.ts.net:8787`). First call wins.
pub fn set_server_url(url: &str) {
    let _ = SERVER.set(url.trim_end_matches('/').to_string());
}

pub fn dir() -> PathBuf {
    PathBuf::from("(browser memory)")
}

pub fn db_path() -> PathBuf {
    dir().join(DB_FILE)
}

pub fn bundle_path() -> PathBuf {
    dir().join(BUNDLE_FILE)
}

pub fn server_file() -> PathBuf {
    PathBuf::from("(the page's origin)")
}

pub fn saved_server_url() -> Option<String> {
    SERVER.get().cloned()
}

pub fn server_url() -> String {
    saved_server_url().unwrap_or_else(|| DEFAULT_SERVER.into())
}

pub fn save_server_url(_url: &str) -> Result<()> {
    bail!("the web app always talks to the server it was loaded from")
}

pub fn size_bytes() -> u64 {
    0
}

pub fn write_atomic(_name: &str, _bytes: &[u8]) -> Result<()> {
    bail!("the web app keeps no replica on disk")
}

/// The profile picture the page fetched (with the data, or after a change).
pub fn avatar_bytes() -> Option<Vec<u8>> {
    crate::preload::avatar()
}

pub fn set_avatar(png: Option<&[u8]>) -> Result<()> {
    crate::preload::set_avatar(png.map(<[u8]>::to_vec));
    Ok(())
}

/// The page fetched its data at load: fresh, so the app never auto-syncs.
pub fn age_secs() -> Option<u64> {
    crate::preload::loaded().then_some(0)
}
