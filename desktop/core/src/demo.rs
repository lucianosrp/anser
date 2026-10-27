//! The demo account ("Just try it" on the wizard): a made-up household to
//! look around with, before (or instead of) setting up a server.
//!
//! * Desktop: a profile of its own (crate::profiles, id [`ID`], url
//!   [`URL`]) whose replica the app builds on this machine
//!   (finance_engine::demo, into `profiles/_demo/`). Its URL is not a
//!   server: nothing here ever sends a request for it. It is never
//!   encrypted, never synced, never uploaded; leaving it deletes it.
//! * Browser: the page fetched `/api/demo/*` at boot (the server builds
//!   the same household, no sign-in, no user data) and [`set_active`] says
//!   so for the rest of the session.
//!
//! While it is active every call a page makes ([`crate::client::send`])
//! is answered here, never by a server: reads that have an answer get it
//! (who is signed in, the profile), everything else, and every write, gets
//! a gentle refusal ([`NOTE`]). Sync, pairing, uploads, backups and the
//! web app install refuse with the same words ([`refuse`]).

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::client::{Request, Response};

/// The demo profile's id (and its replica's folder name): no server's
/// profile can have it (profiles::slug never makes an underscore).
pub const ID: &str = "_demo";
/// Its "server": not a URL any HTTP client takes.
pub const URL: &str = "demo:";
/// Whose data it is (finance_engine::demo::NAME).
pub const NAME: &str = "Alex Demo";
/// What the app calls it.
pub const LABEL: &str = "Demo, made-up data";
/// What a refused action says.
pub const NOTE: &str = "Demo data, sign in to use your own.";

/// Is this the demo's URL?
pub fn is_url(url: &str) -> bool {
    url.trim().trim_end_matches('/') == URL
}

#[cfg(not(feature = "native"))]
static WEB: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The browser loaded the demo (the entry point, at boot).
#[cfg(not(feature = "native"))]
pub fn set_active(on: bool) {
    WEB.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is the demo what's on screen? Desktop: the active profile is the demo's
/// (read only when the profiles exist: this never makes them).
pub fn active() -> bool {
    #[cfg(feature = "native")]
    {
        crate::profiles::file().exists() && is_url(&crate::profiles::active().url)
    }
    #[cfg(not(feature = "native"))]
    {
        WEB.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// An error with [`NOTE`] when the demo is on, or `server` is its URL.
pub fn refuse(server: &str) -> anyhow::Result<()> {
    if is_url(server) || active() {
        anyhow::bail!("{NOTE}");
    }
    Ok(())
}

/// The source catalog the engine serves, as a file kept in step with it by
/// a test (engine/tests/catalog_docs.rs), so the demo shows the real text
/// without core seeing the engine.
const CATALOG: &str = include_str!("catalog.json");

/// The demo's profile, as its bundle has it (crate::Bundle::load keeps it).
static PROFILE: Mutex<Option<Value>> = Mutex::new(None);

pub fn remember(bundle: &Value) {
    if let Some(p) = bundle.get("profile").filter(|p| p.is_object()) {
        *PROFILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
    }
}

/// Who the demo is signed in as: its own user, on a server that wants
/// nothing (so no page asks to sign in).
pub fn whoami() -> Value {
    json!({"paired": true, "device": {"id": 0, "name": "this computer"}, "auth_mode": "off", "auth_mode_from_env": false,
           "household": false, "signup": "", "user": {"id": 0, "name": NAME, "role": "user", "data": true}})
}

/// The answer to a page's call while the demo is on.
pub fn reply(req: &Request) -> Response {
    let ok = |v: Value| Response { status: 200, body: serde_json::to_vec(&v).unwrap_or_default() };
    let path = req.path.split('?').next().unwrap_or("");
    if req.method == "GET" {
        match path {
            "/api/whoami" => return ok(whoami()),
            "/api/profile" => {
                let p = PROFILE.lock().unwrap_or_else(|e| e.into_inner()).clone();
                return ok(p.unwrap_or_else(|| json!({"name": NAME, "base_currency": "EUR", "onboarding_step": "done"})));
            }
            "/api/sources/catalog" => return Response { status: 200, body: CATALOG.as_bytes().to_vec() },
            // initials: no picture
            "/api/profile/avatar" => return Response { status: 404, body: vec![] },
            _ => {}
        }
    }
    Response { status: 403, body: serde_json::to_vec(&json!({"ok": false, "error": NOTE})).unwrap_or_default() }
}

// ---------- the browser's way in and out ----------

static WEB_HOOKS: std::sync::OnceLock<(fn(), fn())> = std::sync::OnceLock::new();

/// Web: how to open the demo and how to leave it (the entry point: a
/// navigation to the page with or without `?demo=1`).
pub fn set_web_hooks(enter: fn(), leave: fn()) {
    let _ = WEB_HOOKS.set((enter, leave));
}

/// Web: open the demo. False when this build can't.
pub fn enter_web() -> bool {
    WEB_HOOKS.get().map(|(e, _)| e()).is_some()
}

/// Web: back to the real thing (the sign-in screen, or the data).
pub fn leave_web() -> bool {
    WEB_HOOKS.get().map(|(_, l)| l()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_reads_and_refuses_writes() {
        assert!(is_url("demo:") && is_url(" demo:/") && !is_url("https://server.ts.net"));
        let who: crate::client::WhoAmI = serde_json::from_slice(&reply(&Request::new("GET", "/api/whoami")).body).unwrap();
        assert!(who.known() && !who.no_data_yet() && !who.admin());
        assert_eq!(who.user.unwrap().name, NAME);
        remember(&json!({"profile": {"name": NAME, "base_currency": "EUR"}}));
        let p = reply(&Request::new("GET", "/api/profile"));
        assert_eq!((p.status, p.json()["base_currency"].as_str()), (200, Some("EUR")));
        for (m, path) in [("PUT", "/api/profile"), ("POST", "/upload"), ("GET", "/api/sources"), ("PUT", "/api/settings/budgets"), ("POST", "/sync")] {
            let r = reply(&Request::new(m, path));
            assert_eq!((r.status, r.error()), (403, NOTE.to_string()), "{m} {path}");
        }
        assert_eq!(reply(&Request::new("GET", "/api/profile/avatar")).status, 404);
        let c = reply(&Request::new("GET", "/api/sources/catalog")).json();
        assert!(c["files"].as_array().is_some_and(|f| !f.is_empty()) && c["api"].as_array().is_some_and(|a| !a.is_empty()));
        assert!(refuse("demo:").is_err());
    }
}
