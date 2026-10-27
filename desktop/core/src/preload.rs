//! Web build only: the data the page fetched before the app started.
//!
//! The browser can't open the SQLite replica, so the web entry point fetches
//! `/api/ledger.json` and `/api/export.json` from the server it was served
//! by, hands the bytes over here, and the app's ordinary `Ledger::load` /
//! `Bundle::load` calls read them back, unchanged.

use std::sync::Mutex;

use anyhow::{anyhow, Result};

use crate::{Bundle, Ledger};

static LEDGER: Mutex<Option<Ledger>> = Mutex::new(None);
static BUNDLE: Mutex<Option<Bundle>> = Mutex::new(None);
static AVATAR: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// The profile picture (`GET /api/profile/avatar`), when there is one.
pub fn set_avatar(png: Option<Vec<u8>>) {
    *AVATAR.lock().unwrap_or_else(|e| e.into_inner()) = png;
}

pub fn avatar() -> Option<Vec<u8>> {
    AVATAR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Parse and keep the two payloads. Either may fail on its own; the app then
/// shows its "no data" page with the error.
pub fn set(ledger_json: &[u8], bundle_json: &[u8]) -> Result<()> {
    let ledger = Ledger::from_json(ledger_json)?;
    let bundle = Bundle(serde_json::from_slice(bundle_json).map_err(|e| anyhow!("parsing export.json: {e}"))?);
    *LEDGER.lock().unwrap_or_else(|e| e.into_inner()) = Some(ledger);
    *BUNDLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(bundle);
    Ok(())
}

pub fn ledger() -> Result<Ledger> {
    LEDGER.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| anyhow!("no data loaded from the server"))
}

pub fn bundle() -> Result<Bundle> {
    BUNDLE.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| anyhow!("no data loaded from the server"))
}

/// Whether [`set`] succeeded (the age the app shows is the bundle's own).
pub fn loaded() -> bool {
    BUNDLE.lock().map(|b| b.is_some()).unwrap_or(false)
}
