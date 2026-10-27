//! Pull the replica from the server. Blocking: call it off the UI thread.

use std::io::Read;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::cache;

#[derive(Debug, Clone)]
pub struct SyncReport {
    pub server: String,
    pub bundle_bytes: usize,
    pub db_bytes: usize,
    pub elapsed: Duration,
}

fn get(agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut req = agent.get(url);
    if let Some(b) = crate::auth::bearer(&cache::server_url()) {
        req = req.header("Authorization", &b);
    }
    let resp = req.call().with_context(|| format!("GET {url}"))?;
    if resp.status() == 401 {
        crate::signin::note_status(401);
        bail!("{}", crate::signin::NOT_SIGNED_IN);
    }
    if resp.status() != 200 {
        bail!("GET {url}: HTTP {}", resp.status());
    }
    let mut body = Vec::new();
    resp.into_body()
        .into_reader()
        .take(limit)
        .read_to_end(&mut body)
        .with_context(|| format!("reading {url}"))?;
    Ok(body)
}

/// Fetch both files, validate, then replace the cache. The database is only
/// swapped in when the bundle parsed, so a half-broken server never leaves
/// the two out of step. After a 401, asks again only once per user action
/// (`crate::signin`). The profile picture comes along (best effort).
pub fn pull() -> Result<SyncReport> {
    // the demo account has nothing to pull
    crate::demo::refuse(&cache::server_url())?;
    if !crate::signin::may_request() {
        bail!("{}", crate::signin::NOT_SIGNED_IN);
    }
    let server = cache::server_url();
    // another account on this machine, not signed in yet: never pull as
    // nobody (that would be whatever the server shows anonymous readers)
    if crate::auth::bearer(&server).is_none() && crate::profiles::needs_token() {
        crate::signin::note_status(401);
        bail!("{}", crate::signin::NOT_SIGNED_IN);
    }
    let started = Instant::now();
    // tailnet-aware: a VPN owning the system DNS can't misroute the server's name
    let agent = crate::tailnet::agent(Duration::from_secs(120));

    let bundle = get(&agent, &format!("{server}/api/export.json"), 64 << 20)?;
    let parsed: serde_json::Value = serde_json::from_slice(&bundle).context("export.json is not JSON")?;
    if parsed.get("version").and_then(|v| v.as_u64()) != Some(crate::bundle::SUPPORTED_VERSION) {
        bail!("server speaks bundle version {:?}, this app expects {}", parsed.get("version"),
              crate::bundle::SUPPORTED_VERSION);
    }
    let db = get(&agent, &format!("{server}/api/db.sqlite"), 512 << 20)?;
    if !db.starts_with(b"SQLite format 3\0") {
        bail!("db.sqlite is not a SQLite file");
    }

    // sealed when the local copy is encrypted: no plaintext on disk
    crate::replica::write(cache::DB_FILE, &db)?;
    crate::replica::write(cache::BUNDLE_FILE, &bundle)?;
    crate::signin::note_status(200);
    pull_avatar(&agent, &server);
    Ok(SyncReport { server, bundle_bytes: bundle.len(), db_bytes: db.len(), elapsed: started.elapsed() })
}

/// The profile picture next to the replica, so the app shows it offline:
/// 200 keeps it, 404 (initials) drops the old one, anything else leaves it.
fn pull_avatar(agent: &ureq::Agent, server: &str) {
    let mut req = agent.get(&format!("{server}/api/profile/avatar"));
    if let Some(b) = crate::auth::bearer(server) {
        req = req.header("Authorization", &b);
    }
    let Ok(resp) = req.call() else { return };
    match resp.status().as_u16() {
        200 => {
            let mut png = Vec::new();
            if resp.into_body().into_reader().take(4 << 20).read_to_end(&mut png).is_ok() && png.starts_with(b"\x89PNG") {
                let _ = cache::set_avatar(Some(&png));
            }
        }
        404 => {
            let _ = cache::set_avatar(None);
        }
        _ => {}
    }
}

/// `GET {server}/healthz` with a short timeout; the round trip on "ok".
/// Blocking: call it off the UI thread.
pub fn healthz(server: &str) -> Result<Duration> {
    if crate::demo::is_url(server) {
        bail!("{}", crate::demo::NOTE);
    }
    let started = Instant::now();
    let agent = crate::tailnet::agent(Duration::from_secs(6));
    let url = format!("{}/healthz", server.trim().trim_end_matches('/'));
    let body = get(&agent, &url, 1024)?;
    if !body.starts_with(b"ok") {
        bail!("unexpected reply from {url}");
    }
    Ok(started.elapsed())
}
