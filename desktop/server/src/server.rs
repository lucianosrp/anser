//! The HTTP server on tiny_http: a
//! fixed pool of blocking worker threads, no async runtime -- a household, a
//! handful of requests a minute, and the slow paths (live exchange calls)
//! are already bounded at 3s, so threads are the simplest correct thing.
//!
//! Routes: /healthz, /api/version, /favicon.{ico,png} (204), GET /api/live,
//! /api/export.json, /api/ledger.json, /api/db.sqlite, /api/files,
//! /api/sources, /api/capabilities, /api/formats, POST /mcp (mcp.rs), the web
//! app under /app/ and at / (see webapp.rs; until it is installed both
//! answer a short page saying how to install it) with its service worker at
//! /sw.js, POST /sync, /upload, /api/overrides, /api/webapp,
//! PUT /api/formats/{id}; 404 otherwise. The browser UI is the web
//! app: the server renders no dashboard pages of its own.
//! Every response is Cache-Control: no-store, except the web app's files
//! (no-cache + ETag). One timestamped log line per
//! request with elapsed ms; a failed handler logs its error chain and a
//! client that hung up logs 499. A panicking handler answers 500 instead of
//! killing its worker.
//!
//! Default bind 127.0.0.1:8787 (the desktop app's default server too);
//! --bind or FINANCE_BIND moves it.

use std::collections::BTreeMap;
use std::io::Read;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use finance_engine::tenant::{self, Tenant};
use finance_engine::views::Ctx;
use finance_engine::{config, db, export, ingest};

use crate::feed::REQUEST_FEED;
use crate::log::log;
use crate::gate::{self, Who};
use crate::{auth, formats, live, push, syncer, upload, webapp};

/// One default port for server and client (core cache::DEFAULT_SERVER).
pub const DEFAULT_BIND: &str = "127.0.0.1:8787";
const WORKERS: usize = 8;
const TEXT: &str = "text/plain; charset=utf-8";

pub struct Reply {
    pub status: u16,
    pub content_type: Option<String>,
    pub location: Option<String>,
    pub body: Vec<u8>,
    /// extra headers; a Cache-Control here replaces the default no-store
    pub headers: Vec<(&'static str, String)>,
    /// a streamed body instead of `body` (length when known, else chunked)
    pub stream: Option<(Box<dyn Read + Send>, Option<usize>)>,
}

impl Reply {
    pub(crate) fn new(status: u16, ctype: &str, body: Vec<u8>) -> Self {
        Reply { status, content_type: Some(ctype.into()), location: None, body, headers: vec![], stream: None }
    }
    fn served(s: webapp::Served) -> Self {
        Reply { status: s.status, content_type: Some(s.content_type.into()), location: None, body: s.body, headers: s.headers, stream: None }
    }
    pub(crate) fn json(v: &Value) -> Self {
        Self::json_status(200, v)
    }
    pub(crate) fn json_status(status: u16, v: &Value) -> Self {
        Self::new(status, "application/json; charset=utf-8", serde_json::to_vec(v).unwrap_or_default())
    }
    pub(crate) fn html(status: u16, body: &str) -> Self {
        Self::new(status, "text/html; charset=utf-8", body.as_bytes().to_vec())
    }
    fn redirect(to: String) -> Self {
        Reply { status: 303, content_type: None, location: Some(to), body: vec![], headers: vec![], stream: None }
    }
    pub(crate) fn text(status: u16, msg: &str) -> Self {
        Self::new(status, TEXT, msg.as_bytes().to_vec())
    }
    /// A streamed download (a file, or an archive being sealed).
    pub(crate) fn stream(ctype: &str, reader: Box<dyn Read + Send>, len: Option<usize>, filename: &str) -> Self {
        let mut r = Self::new(200, ctype, vec![]);
        r.headers.push(("Content-Disposition", format!("attachment; filename=\"{filename}\"")));
        r.stream = Some((reader, len));
        r
    }
}

/// "Drop a CSV in transactions/ and it just works": run_ingest once per
/// data request (cheap on a warm tree: the stat fast path skips unchanged
/// files without reading them). A bad file never fails the request; it is
/// reported (and logged) here instead. None = nothing to report.
pub fn auto_ingest() -> Option<Value> {
    match db::open().and_then(|c| ingest::run_ingest(&c)) {
        Err(e) => Some(json!({"kind": "stale", "text": format!("ingest error: {}", e.to_string().chars().take(120).collect::<String>())})),
        Ok(summary) => {
            let n: usize = summary.iter().map(|(_, s)| s.files_new).sum();
            let failed: usize = summary.iter().map(|(_, s)| s.files_failed).sum();
            if failed > 0 {
                return Some(json!({"kind": "stale", "text": format!("{failed} file(s) could not be read (see the server log)")}));
            }
            (n > 0).then(|| json!({"kind": "fresh", "text": format!("ingested {n} new file(s)")}))
        }
    }
}

fn not_found(path: &str) -> Reply {
    Reply::text(404, &format!("404: no route for {path}"))
}

/// / and /app/ before the web app is installed: what it is and how to get it.
fn not_installed() -> Reply {
    let body = "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>Anser</title><body style=\"font:16px/1.5 system-ui,sans-serif;max-width:36em;margin:2em auto;padding:0 1em\">\
<h1>Anser</h1><p>The web app isn't installed on this server yet.</p>\
<p>Install it from the desktop app (Doctor, then <b>Install web app on the server</b>), or from a checkout:</p>\
<pre style=\"white-space:pre-wrap\">desktop/scripts/build-web.sh --install URL-of-this-server</pre>\
<p>The data stays available meanwhile: <a href=\"/api/export.json\">/api/export.json</a>, \
<a href=\"/api/ledger.json\">/api/ledger.json</a>.</p>";
    Reply::new(404, "text/html; charset=utf-8", body.as_bytes().to_vec())
}

/// A data request: ingest dropped files first, and start a background sync
/// when the ledger is older than a day (the request answers now).
fn data_request() {
    if let Some(badge) = auto_ingest() {
        log(&format!("[ingest] {}", badge["text"].as_str().unwrap_or_default()));
    }
    syncer::maybe_autostart(syncer::last_fetch_ts_ms());
}

pub(crate) fn header(req: &Request, name: &str) -> Option<String> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

pub(crate) fn read_body(req: &mut Request, limit: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    req.as_reader().take(limit as u64 + 1).read_to_end(&mut body)?;
    anyhow::ensure!(body.len() <= limit, "body too large");
    Ok(body)
}

fn get(req: &Request, path: &str, who: &Who) -> Result<Reply> {
    let accept = header(req, "Accept-Encoding");
    Ok(match path {
        "/healthz" => Reply::new(200, "text/plain; charset=utf-8", b"ok".to_vec()),
        // what answers, cheaply: the desktop app's Doctor shows it
        "/api/version" => Reply::json(&json!({
            "kind": "anser-server",
            "version": finance_core::VERSION,
            "bundle_version": export::BUNDLE_VERSION,
        })),
        "/favicon.ico" | "/favicon.png" => Reply { status: 204, content_type: None, location: None, body: vec![], headers: vec![], stream: None },
        // the web app: the bundle in data/webapp/ (webapp.rs)
        "/app" => Reply::redirect("/app/".into()),
        _ if path.starts_with("/app/") => {
            match webapp::file(&path["/app/".len()..], accept.as_deref(), header(req, "If-None-Match").as_deref()) {
                Some(f) => Reply::served(f),
                None if path == "/app/" => not_installed(),
                None => not_found(path),
            }
        }
        // its service worker, from the root so its scope covers / and /app/
        "/sw.js" => match webapp::file("sw.js", accept.as_deref(), header(req, "If-None-Match").as_deref()) {
            Some(f) => Reply::served(f),
            None => not_found(path),
        },
        // the web app is the dashboard
        "/" => match webapp::file("", accept.as_deref(), header(req, "If-None-Match").as_deref()) {
            Some(f) => Reply::served(f),
            None => not_installed(),
        },
        "/api/ledger.json" => {
            data_request();
            Reply::served(webapp::json_bytes(webapp::ledger_json()?, accept.as_deref()))
        }
        // "Just try it" in the browser: made-up data, no sign-in (demo.rs)
        "/api/demo/ledger.json" => Reply::served(webapp::json_bytes(crate::demo::files()?.0.to_vec(), accept.as_deref())),
        "/api/demo/export.json" => Reply::new(200, "application/json; charset=utf-8", crate::demo::files()?.1.to_vec()),
        "/api/live" => Reply::json(&live::net_worth()),
        "/api/export.json" => {
            data_request();
            wait_for_export_warm();
            let net_worth = live::net_worth();
            let conn = db::open()?;
            let body = export::bundle_bytes(&Ctx::new(&conn, &REQUEST_FEED), &net_worth);
            Reply::new(200, "application/json; charset=utf-8", body)
        }
        "/api/db.sqlite" => Reply::new(200, "application/vnd.sqlite3", db::snapshot_bytes()?),
        // each data source's state for the Doctor; never a secret (sources.rs)
        // what the server has (sources, features): the bundle carries it too
        "/api/capabilities" => {
            let conn = db::open()?;
            Reply::json(&finance_engine::views::capabilities::capabilities(&Ctx::new(&conn, &REQUEST_FEED)))
        }
        // the last 4 characters of a key only for a paired device
        "/api/sources" => Reply::json(&serde_json::to_value(crate::sources::sources(&db::open()?, who.device().is_some())?)?),
        "/api/files" => {
            let conn = db::open()?;
            Reply::json(&serde_json::to_value(push::known(&conn, &config::transactions_dir()))?)
        }
        // the format definitions in effect (formats.rs)
        "/api/formats" => Reply::json(&formats::list()),
        _ => not_found(path),
    })
}

fn put(req: &mut Request, path: &str) -> Result<Reply> {
    Ok(match path.strip_prefix("/api/formats/") {
        Some(id) if !id.is_empty() && !id.contains('/') => match formats::handle_put(req, id) {
            Ok(v) => {
                // files in transactions/ that only this definition reads
                if let Some(badge) = auto_ingest() {
                    log(&format!("[ingest] {}", badge["text"].as_str().unwrap_or_default()));
                }
                warm_async();
                Reply::json(&v)
            }
            Err(e) => Reply::text(400, &format!("format refused: {e:#}")),
        },
        _ => not_found(path),
    })
}

fn post(req: &mut Request, path: &str) -> Result<Reply> {
    Ok(match path {
        "/upload" => {
            let qs = match handle_upload(req) {
                Ok(qs) => qs,
                Err(e) => upload::err_query(&format!("upload failed: {}", e.to_string().chars().take(80).collect::<String>())),
            };
            // the outcome rides in the Location (the desktop app and push
            // read it without following; there is no /upload page anymore).
            // The web app asks for JSON instead (fetch can't read a redirect).
            if header(req, "Accept").is_some_and(|a| a.contains("application/json")) {
                let o = finance_core::upload::Outcome::from_location(&format!("/upload?{qs}"));
                Reply::json(&json!({"saved": o.saved, "skipped": o.skipped, "errors": o.errors}))
            } else {
                Reply::redirect(format!("/upload?{qs}"))
            }
        }
        "/sync" => {
            syncer::start();
            // back to the page the button was on
            let back = header(req, "Referer")
                .and_then(|r| r.split_once("://").map(|x| x.1.to_string()))
                .and_then(|r| r.find('/').map(|i| r[i..].to_string()))
                .unwrap_or_else(|| "/".into());
            Reply::redirect(back)
        }
        // "pull the latest now": fetch this user's sources, answer when done
        "/api/refresh" => {
            let t = Instant::now();
            let sources = syncer::refresh();
            let ok = sources.iter().all(|s| s.ok);
            Reply::json(&json!({"ok": ok, "ms": t.elapsed().as_millis() as u64, "sources": sources}))
        }
        "/api/overrides" => {
            let body = read_body(req, 4 << 20)?;
            let conn = db::open()?;
            match push::accept_overrides(&conn, &body) {
                Ok(v) => Reply::json(&v),
                Err(e) => Reply::json_status(400, &json!({"ok": false, "error": format!("{e:#}")})),
            }
        }
        "/api/webapp" => match handle_install(req) {
            Ok(v) => Reply::json(&v),
            Err(e) => Reply::text(400, &format!("install failed: {e:#}")),
        },
        _ => not_found(path),
    })
}

/// Save the parts, then ingest right away (not on the redirected GET: this
/// also covers clients that don't follow the redirect, like `push`).
/// Returns the redirect query.
fn handle_upload(req: &mut Request) -> Result<String> {
    let ctype = header(req, "Content-Type").unwrap_or_default();
    let length: usize = header(req, "Content-Length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    if !ctype.starts_with("multipart/form-data") {
        return Ok(upload::err_query("not a file upload"));
    }
    if length == 0 || length > upload::MAX_BYTES {
        return Ok(upload::err_query("upload too large (20MB max)"));
    }
    let body = read_body(req, upload::MAX_BYTES)?;
    let parts = upload::parse_multipart(&ctype, &body)?;
    let out = upload::save(&config::transactions_dir(), &parts)?;
    if !out.saved.is_empty() {
        auto_ingest();
        warm_async(); // new rows: recompute the views before the next visit
    }
    Ok(out.query())
}

/// POST /api/webapp: install a web app bundle (webapp.rs has the contract).
fn handle_install(req: &mut Request) -> Result<Value> {
    anyhow::ensure!(header(req, "X-Finance-Install").as_deref() == Some("1"), "missing X-Finance-Install: 1");
    let ctype = header(req, "Content-Type").unwrap_or_default();
    anyhow::ensure!(ctype.starts_with("multipart/form-data"), "not a multipart upload");
    let body = read_body(req, webapp::MAX_INSTALL_BYTES)?;
    let parts: Vec<(String, Vec<u8>)> = upload::parse_multipart(&ctype, &body)?.into_iter().map(|p| (p.filename, p.data)).collect();
    let out = webapp::install(&parts)?;
    log(&format!("[webapp] installed {} files, {} bytes", parts.len(), out["bytes"]));
    Ok(out)
}

fn respond(req: Request, r: Reply) -> std::io::Result<()> {
    let mut headers: Vec<Header> = Vec::new();
    let mut add = |k: &str, v: &str| {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            headers.push(h);
        }
    };
    if let Some(ct) = &r.content_type {
        add("Content-Type", ct);
    }
    if let Some(loc) = &r.location {
        add("Location", loc);
    }
    if !r.headers.iter().any(|(k, _)| *k == "Cache-Control") {
        add("Cache-Control", "no-store");
    }
    for (k, v) in &r.headers {
        add(k, v);
    }
    match r.stream {
        Some((reader, len)) => req.respond(Response::new(tiny_http::StatusCode(r.status), headers, reader, len, None)),
        None => {
            let mut resp = Response::from_data(r.body).with_status_code(r.status);
            for h in headers {
                resp.add_header(h);
            }
            req.respond(resp)
        }
    }
}

fn handle(mut req: Request) {
    let t0 = Instant::now();
    let method = req.method().clone();
    let url = req.url().to_string();
    let path = url.split_once('?').map_or(url.as_str(), |(p, _)| p).to_string();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // the door: who is asking, and may they (gate.rs)
        let who = identify(&req);
        let m = method.as_str().to_ascii_uppercase();
        if let Err((status, msg)) = admit(&req, &m, &path, &who) {
            return Ok(Reply::json_status(status, &json!({"ok": false, "error": msg})));
        }
        // whose data: the device's user's tree; a request without a device
        // (let in by the mode: one user only, see auth::effective_mode) is
        // the owner's, as before accounts. A device without a tree only got
        // past the gate for a public route, and runs with no tenant at all.
        let tenant = match who.device() {
            Some(d) => d.tenant(),
            None => Some(Tenant::owner()),
        };
        let mut run = || {
            if let Some(r) = crate::api::route(&mut req, &m, &path, &who) {
                return r;
            }
            match method {
                Method::Get | Method::Head => get(&req, &path, &who),
                Method::Post => post(&mut req, &path),
                Method::Put => put(&mut req, &path),
                _ => Ok(Reply::text(405, "method not allowed")),
            }
        };
        match tenant {
            Some(t) => t.enter(run),
            None => run(),
        }
    }));
    let reply = match outcome {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            log(&format!("ERROR {method} {url}\n{e:?}"));
            error_reply(&e.to_string())
        }
        Err(_) => {
            log(&format!("ERROR {method} {url}: handler panicked"));
            error_reply("internal error")
        }
    };
    let mut status = reply.status;
    if respond(req, reply).is_err() {
        status = 499; // client closed the request: logged, not an error
    }
    log(&format!("{method} {path} {status} {:.1}ms", t0.elapsed().as_secs_f64() * 1000.0));
}

/// The device behind a request: a bearer token, else the session cookie.
fn identify(req: &Request) -> Who {
    if let Some(t) = header(req, "Authorization").and_then(|a| a.strip_prefix("Bearer ").map(|t| t.trim().to_string())) {
        return auth::device_for_token(&t).map_or(Who::Nobody, Who::Bearer);
    }
    header(req, "Cookie")
        .and_then(|c| gate::session_cookie(&c).map(String::from))
        .and_then(|t| auth::device_for_token(&t))
        .map_or(Who::Nobody, Who::Cookie)
}

fn admit(req: &Request, method: &str, path: &str, who: &Who) -> Result<gate::Class, gate::Refusal> {
    let host = header(req, "Host");
    let fwd = header(req, "X-Forwarded-Host");
    let origin = header(req, "Origin");
    let peer_loopback = req.remote_addr().is_some_and(|a| a.ip().is_loopback());
    let facts = gate::Facts {
        method,
        path,
        host: host.as_deref(),
        forwarded_host: fwd.as_deref(),
        origin: origin.as_deref(),
        x_finance_request: header(req, "X-Finance-Request").is_some(),
        peer_loopback,
        // tiny_http serves plain HTTP; TLS terminates in front (tailscale
        // serve, on loopback), so loopback is the trusted path
        tls: false,
    };
    static EXTRA: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    gate::check(&facts, who, auth::effective_mode(), EXTRA.get_or_init(gate::extra_hosts))
}

fn error_reply(msg: &str) -> Reply {
    Reply::text(500, &format!("Something went wrong: {msg}"))
}

/// The bind address: --bind, else FINANCE_BIND, else DEFAULT_BIND.
pub fn bind_addr(arg: Option<&str>) -> String {
    arg.map(String::from).or_else(|| config::env("FINANCE_BIND")).unwrap_or_else(|| DEFAULT_BIND.into())
}

pub fn bind(addr: &str) -> Result<Arc<Server>> {
    Ok(Arc::new(Server::http(addr).map_err(|e| anyhow::anyhow!("{e}")).with_context(|| format!("binding {addr}"))?))
}

/// Serve forever on `addr`. The caches warm in the background meanwhile.
pub fn serve(addr: &str) -> Result<()> {
    let server = bind(addr)?;
    log(&format!("Finance server (rust) at http://{addr}  (Ctrl-C to stop)"));
    // from here on every tree path is resolved for a tenant (each request
    // runs as its user's): one resolved outside any is a bug, refused
    finance_engine::tenant::set_strict(true);
    warm_all_async();
    crate::syncer::spawn_scheduler();
    run(server);
    Ok(())
}

/// Fill the caches behind the request paths, off them: the live valuation
/// (the network sweep) on one thread, and on another the export bundle's
/// views. Runs at startup and after a background sync or an upload; a
/// request arriving meanwhile waits for this tenant's pass, then serves
/// from the cache.
pub fn warm_async() {
    if !config::offline() {
        live::refresh_async(); // re-warms the views when it lands
    }
    warm_views_async();
}

/// The owner's caches, then every other household member's, one tenant at
/// a time so a small server isn't flooded. Fire-and-forget: the server
/// starts accepting while the first pass runs.
pub fn warm_all_async() {
    let _ = std::thread::Builder::new().name("warm-all".into()).spawn(|| {
        Tenant::owner().enter(|| {
            warm_async();
            wait_until_idle();
        });
        for t in crate::syncer::tenants() {
            if t.uid == tenant::OWNER_UID {
                continue;
            }
            t.enter(|| {
                warm_async();
                wait_until_idle();
            });
        }
    });
}

#[derive(Default)]
struct WarmSlot {
    running: bool,
    /// another warm_views_async arrived while this pass was running
    rerun: bool,
    /// bumped at the end of every pass (success, rerun, panic, spawn fail)
    pass: u64,
}

/// Per tenant (its db path): a request while one runs waits on WARM_CV for
/// this key only, until `pass` moves past the value it saw. A second
/// warm_views_async sets `rerun` so what changed during the pass gets
/// warmed too, without holding waiters for that later pass.
static WARM: Mutex<BTreeMap<PathBuf, WarmSlot>> = Mutex::new(BTreeMap::new());
static WARM_CV: Condvar = Condvar::new();

/// How long a request will wait for the pass it joined before computing
/// the bundle itself.
const EXPORT_WAIT: Duration = Duration::from_secs(30);
/// Startup sequencer: don't hang forever if a tenant's warm-up sticks.
const IDLE_WAIT: Duration = Duration::from_secs(60);

fn wait_while(key: &PathBuf, limit: Duration, pred: impl Fn(&WarmSlot) -> bool) {
    let start = Instant::now();
    let Ok(mut all) = WARM.lock() else { return };
    while all.get(key).is_some_and(|s| pred(s)) {
        let left = limit.saturating_sub(start.elapsed());
        if left.is_zero() {
            return;
        }
        match WARM_CV.wait_timeout(all, left) {
            Ok((g, to)) => {
                all = g;
                if to.timed_out() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

fn wait_until_idle() {
    wait_while(&config::db_path(), IDLE_WAIT, |s| s.running);
}

/// If a warm-up is running for this tenant, wait until that pass ends
/// (up to EXPORT_WAIT). Keyed on this tenant's db path; a later rerun
/// of the same tenant does not hold the request.
fn wait_for_export_warm() {
    let key = config::db_path();
    let Ok(all) = WARM.lock() else { return };
    let Some(st) = all.get(&key) else { return };
    if !st.running {
        return;
    }
    let saw = st.pass;
    drop(all);
    let t = Instant::now();
    wait_while(&key, EXPORT_WAIT, |s| s.pass == saw);
    let secs = t.elapsed().as_secs_f64();
    log(&format!("[warm] export.json waited {secs:.1}s for the warm-up"));
    #[cfg(any(test, feature = "test-hooks"))]
    LAST_WAIT_MS.store((secs * 1000.0) as u64, std::sync::atomic::Ordering::SeqCst);
}

/// Bump `pass`, notify waiters. On abort (panic / spawn fail) clear
/// running and rerun so nobody is left waiting for a dead thread.
/// Returns true if a rerun was queued and should run now.
fn end_pass(key: &PathBuf, abort: bool) -> bool {
    let Ok(mut all) = WARM.lock() else {
        WARM_CV.notify_all();
        return false;
    };
    let st = all.entry(key.clone()).or_default();
    st.pass = st.pass.wrapping_add(1);
    if abort || !st.rerun {
        st.running = false;
        st.rerun = false;
        WARM_CV.notify_all();
        false
    } else {
        st.rerun = false;
        WARM_CV.notify_all();
        true
    }
}

/// Resets running/rerun and notifies waiters if the warm thread panics
/// or returns without a clean end_pass.
struct WarmGuard {
    key: PathBuf,
    disarmed: bool,
}

impl WarmGuard {
    fn new(key: PathBuf) -> Self {
        Self { key, disarmed: false }
    }
    fn end_ok(&mut self) -> bool {
        let rerun = end_pass(&self.key, false);
        if !rerun {
            self.disarmed = true;
        }
        rerun
    }
}

impl Drop for WarmGuard {
    fn drop(&mut self) {
        if !self.disarmed {
            end_pass(&self.key, true);
        }
    }
}

/// The export bundle's views, on a background thread; one at a time. Also run after each live refresh: new tickers re-price the crypto
/// views, and an hourly balance snapshot moves the fingerprint.
pub fn warm_views_async() {
    let key = config::db_path();
    {
        let Ok(mut all) = WARM.lock() else { return };
        let st = all.entry(key.clone()).or_default();
        if st.running {
            st.rerun = true;
            return;
        }
        st.running = true;
        st.rerun = false;
    }
    let k = key.clone();
    // the tenant goes with the thread: it warms this user's views
    let spawned = tenant::spawn("warm", move || {
        let mut guard = WarmGuard::new(k);
        loop {
            park_if_held();
            let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
                panic_if_armed();
                let t = Instant::now();
                if let Ok(conn) = db::open() {
                    let _ = export::bundle_bytes(&Ctx::new(&conn, &REQUEST_FEED), &Value::Null);
                }
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                if ms > 50.0 {
                    log(&format!("[warm] views cached in {ms:.0}ms"));
                }
            }))
            .is_err();
            if panicked || !guard.end_ok() {
                return;
            }
        }
    });
    if spawned.is_err() {
        end_pass(&key, true);
    }
}

#[cfg(any(test, feature = "test-hooks"))]
static WARM_HOLD: Mutex<Option<std::sync::mpsc::Receiver<()>>> = Mutex::new(None);
#[cfg(any(test, feature = "test-hooks"))]
static PANIC_NEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(any(test, feature = "test-hooks"))]
static LAST_WAIT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Park the next warm-up pass until the returned sender is dropped (tests).
#[cfg(any(test, feature = "test-hooks"))]
pub fn hold_next_warm() -> std::sync::mpsc::Sender<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Ok(mut g) = WARM_HOLD.lock() {
        *g = Some(rx);
    }
    tx
}

/// Whether this tenant's warm-up is running (tests).
#[cfg(any(test, feature = "test-hooks"))]
pub fn warming() -> bool {
    let key = config::db_path();
    WARM.lock().ok().and_then(|m| m.get(&key).map(|s| s.running)).unwrap_or(false)
}

/// The next warm-up pass panics after it is claimed (tests).
#[cfg(any(test, feature = "test-hooks"))]
pub fn panic_next_warm() {
    PANIC_NEXT.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Last `[warm] export.json waited` duration in milliseconds (tests).
#[cfg(any(test, feature = "test-hooks"))]
pub fn last_wait_ms() -> u64 {
    LAST_WAIT_MS.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(any(test, feature = "test-hooks"))]
fn park_if_held() {
    let rx = WARM_HOLD.lock().ok().and_then(|mut g| g.take());
    if let Some(rx) = rx {
        let _ = rx.recv();
    }
}

#[cfg(not(any(test, feature = "test-hooks")))]
fn park_if_held() {}

#[cfg(any(test, feature = "test-hooks"))]
fn panic_if_armed() {
    if PANIC_NEXT.swap(false, std::sync::atomic::Ordering::SeqCst) {
        panic!("test: warm-up panic");
    }
}

#[cfg(not(any(test, feature = "test-hooks")))]
fn panic_if_armed() {}

/// Answer requests on a bound server with the worker pool (blocks).
pub fn run(server: Arc<Server>) {
    shutdown::install();
    let workers: Vec<_> = (0..WORKERS)
        .map(|i| {
            let s = server.clone();
            std::thread::Builder::new()
                .name(format!("http-{i}"))
                .spawn(move || {
                    // a worker finishes its request before it looks at the flag
                    while !shutdown::requested() {
                        match s.recv() {
                            Ok(req) => handle(req),
                            Err(_) if shutdown::requested() => {}
                            Err(e) => log(&format!("accept failed: {e}")),
                        }
                    }
                })
                .expect("spawn http worker")
        })
        .collect();
    // SIGTERM / Ctrl-C: wake the idle workers; the busy ones end after their request
    let s = server.clone();
    std::thread::spawn(move || {
        while !shutdown::requested() {
            std::thread::sleep(Duration::from_millis(100));
        }
        log("shutting down: finishing the requests in flight");
        for _ in 0..WORKERS {
            s.unblock();
        }
    });
    for w in workers {
        let _ = w.join();
    }
}

/// SIGTERM and Ctrl-C ask the server to stop (a flag, read by [`run`]'s threads).
mod shutdown {
    use std::sync::atomic::{AtomicBool, Ordering};

    static REQUESTED: AtomicBool = AtomicBool::new(false);

    pub fn requested() -> bool {
        REQUESTED.load(Ordering::Relaxed)
    }

    #[cfg(unix)]
    extern "C" fn on_signal(_: libc::c_int) {
        REQUESTED.store(true, Ordering::Relaxed);
    }

    pub fn install() {
        #[cfg(unix)]
        unsafe {
            for sig in [libc::SIGTERM, libc::SIGINT] {
                libc::signal(sig, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
            }
        }
    }
}

#[cfg(test)]
#[path = "warm_start.rs"]
mod warm_start;
