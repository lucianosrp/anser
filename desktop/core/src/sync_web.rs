//! Web build stand-in for `sync.rs`. Blocking HTTP is impossible in the
//! browser and the page fetched fresh data when it loaded, so a sync is a
//! page reload: the web entry point registers it with [`set_reload_hook`].
//!
//! Never while the server turns this browser away: then a reload would
//! fetch, get its 401 and come back here, a loop (`crate::signin`). The
//! sign-in screen fetches the data in the page instead.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Result};

#[derive(Debug, Clone)]
pub struct SyncReport {
    pub server: String,
    pub bundle_bytes: usize,
    pub db_bytes: usize,
    pub elapsed: Duration,
}

static RELOAD: OnceLock<fn()> = OnceLock::new();

pub fn set_reload_hook(f: fn()) {
    let _ = RELOAD.set(f);
}

/// Reloads the page (when the hook is set); the error is what the app shows
/// for the instant before the page goes away.
pub fn pull() -> Result<SyncReport> {
    if !crate::signin::may_request() {
        bail!("{}", crate::signin::NOT_SIGNED_IN);
    }
    if let Some(reload) = RELOAD.get() {
        reload();
        bail!("reloading…");
    }
    bail!("reload the page to fetch the latest data")
}

pub fn healthz(_server: &str) -> Result<Duration> {
    bail!("the web app is connected to the server it was loaded from")
}
