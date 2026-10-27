//! Talking to the server from a page: one async call for the desktop app
//! and the web app alike.
//!
//! * native: a thread runs the blocking ureq request (with this machine's
//!   bearer token, see [`crate::auth`]) and hands the reply back through a
//!   one-shot future, so a page awaits it on gpui's foreground executor
//!   without blocking the UI;
//! * web: the entry point registers [`set_fetch_hook`], a `fetch()` on the
//!   page's own origin that sends the session cookie and the
//!   `X-Finance-Request` header the server wants from a browser.
//!
//! Also here: the shapes of `/api/whoami` (shared by both builds).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    /// "/api/whoami"; the server is the configured one (or the page's origin)
    pub path: String,
    pub body: Option<Vec<u8>>,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    /// The server's `error` (JSON) or its text, for a failed call.
    pub fn error(&self) -> String {
        let v = self.json();
        if let Some(e) = v.get("error").and_then(Value::as_str) {
            return e.to_string();
        }
        let t = String::from_utf8_lossy(&self.body).chars().take(200).collect::<String>();
        if t.trim().is_empty() { format!("HTTP {}", self.status) } else { format!("HTTP {}: {t}", self.status) }
    }
}

pub type Reply = Result<Response, String>;
/// Send on native (a thread may run it), not in the browser (JS futures).
#[cfg(feature = "native")]
pub type BoxFuture = Pin<Box<dyn Future<Output = Reply> + Send>>;
#[cfg(not(feature = "native"))]
pub type BoxFuture = Pin<Box<dyn Future<Output = Reply>>>;

impl Request {
    pub fn new(method: &str, path: &str) -> Self {
        Request { method: method.into(), path: path.into(), ..Default::default() }
    }
    pub fn json(mut self, v: &Value) -> Self {
        self.body = Some(serde_json::to_vec(v).unwrap_or_default());
        self.content_type = Some("application/json".into());
        self
    }
}

// ---------- a one-shot channel as a future (native) ----------

struct Slot<T> {
    value: Option<T>,
    waker: Option<Waker>,
}

pub struct Receiver<T>(Arc<Mutex<Slot<T>>>);
pub struct Sender<T>(Arc<Mutex<Slot<T>>>);

pub fn oneshot<T>() -> (Sender<T>, Receiver<T>) {
    let s = Arc::new(Mutex::new(Slot { value: None, waker: None }));
    (Sender(s.clone()), Receiver(s))
}

impl<T> Sender<T> {
    pub fn send(self, v: T) {
        let waker = {
            let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
            g.value = Some(v);
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

impl<T> Future for Receiver<T> {
    type Output = T;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match g.value.take() {
            Some(v) => Poll::Ready(v),
            None => {
                g.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

// ---------- the call ----------

#[cfg(feature = "native")]
fn blocking(req: &Request) -> Reply {
    let server = crate::cache::server_url();
    let url = format!("{server}{}", req.path);
    let agent = crate::upload::agent(std::time::Duration::from_secs(90));
    let auth = crate::auth::bearer(&server);
    // another account on this machine, not signed in yet: never ask as
    // nobody (a server that lets anonymous reads through would answer with
    // its owner's data); only the sign-in calls go out
    if auth.is_none() && crate::profiles::needs_token() && !crate::profiles::anonymous_ok(&req.path) {
        return Ok(Response { status: 401, body: br#"{"ok":false,"error":"sign in to this account first"}"#.to_vec() });
    }
    macro_rules! with_headers {
        ($b:expr) => {{
            let mut b = $b;
            if let Some(a) = &auth {
                b = b.header("Authorization", a);
            }
            b = b.header("Accept", "application/json");
            if let Some(ct) = &req.content_type {
                b = b.header("Content-Type", ct);
            }
            b
        }};
    }
    let body = req.body.clone().unwrap_or_default();
    let r = match req.method.as_str() {
        "GET" => with_headers!(agent.get(&url)).call(),
        "DELETE" => with_headers!(agent.delete(&url)).call(),
        "PATCH" => with_headers!(agent.patch(&url)).send(&body[..]),
        "PUT" => with_headers!(agent.put(&url)).send(&body[..]),
        _ => with_headers!(agent.post(&url)).send(&body[..]),
    };
    let mut r = r.map_err(|e| format!("{server}: {e}"))?;
    let status = r.status().as_u16();
    let body = r.body_mut().with_config().limit(64 << 20).read_to_vec().map_err(|e| e.to_string())?;
    Ok(Response { status, body })
}

#[cfg(feature = "native")]
pub fn send(req: Request) -> BoxFuture {
    // the demo account has no server: answered here (crate::demo)
    if crate::demo::active() {
        return Box::pin(std::future::ready(Ok(crate::demo::reply(&req))));
    }
    let (tx, rx) = oneshot();
    let spawned = std::thread::Builder::new().name("client".into()).spawn(move || tx.send(blocking(&req)));
    match spawned {
        Ok(_) => Box::pin(rx),
        Err(e) => Box::pin(std::future::ready(Err(e.to_string()))),
    }
}

#[cfg(not(feature = "native"))]
static FETCH: std::sync::OnceLock<fn(Request) -> BoxFuture> = std::sync::OnceLock::new();

/// Web: how requests go out (the entry point's `fetch`).
#[cfg(not(feature = "native"))]
pub fn set_fetch_hook(f: fn(Request) -> BoxFuture) {
    let _ = FETCH.set(f);
}

#[cfg(not(feature = "native"))]
pub fn send(req: Request) -> BoxFuture {
    if crate::demo::active() {
        return Box::pin(std::future::ready(Ok(crate::demo::reply(&req))));
    }
    match FETCH.get() {
        Some(f) => f(req),
        None => Box::pin(std::future::ready(Err("no fetch in this build".to_string()))),
    }
}

// ---------- running a call without a UI context ----------

/// Run `fut` to completion in the background (a settings write from a
/// page's plain `save()`): a thread on native, the page's event loop on the
/// web (the entry point's [`set_spawn_hook`]).
#[cfg(feature = "native")]
pub fn spawn(fut: BoxFuture) {
    let _ = std::thread::Builder::new().name("client-spawn".into()).spawn(move || block_on(fut));
}

#[cfg(feature = "native")]
pub fn block_on<F: Future>(f: F) -> F::Output {
    struct ThreadWaker(std::thread::Thread);
    impl std::task::Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let mut f = std::pin::pin!(f);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

#[cfg(not(feature = "native"))]
static SPAWN: std::sync::OnceLock<fn(BoxFuture)> = std::sync::OnceLock::new();

#[cfg(not(feature = "native"))]
pub fn set_spawn_hook(f: fn(BoxFuture)) {
    let _ = SPAWN.set(f);
}

#[cfg(not(feature = "native"))]
pub fn spawn(fut: BoxFuture) {
    if let Some(f) = SPAWN.get() {
        f(fut);
    }
}

// ---------- files from the browser (web only) ----------

/// Web: pick files with the browser's dialog (names; the argument is the
/// input's `accept`), then send the picked files as multipart `files` to a
/// path (/api/preview, /upload, /api/profile/avatar).
pub type PickHook = fn(&'static str) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>>>>;
pub type SendPickedHook = fn(String) -> BoxFuture;
static PICK: std::sync::OnceLock<(PickHook, SendPickedHook)> = std::sync::OnceLock::new();

pub fn set_file_hooks(pick: PickHook, send_picked: SendPickedHook) {
    let _ = PICK.set((pick, send_picked));
}

pub fn can_pick_files() -> bool {
    PICK.get().is_some()
}

/// Bank exports for Upload.
pub fn pick_files() -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>>>> {
    pick(".csv,.xml,.pdf,.xlsx,.xls")
}

/// One picture for the avatar.
pub fn pick_image() -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>>>> {
    pick("image/png,image/jpeg,image/webp")
}

fn pick(accept: &'static str) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>>>> {
    match PICK.get() {
        Some((p, _)) => p(accept),
        None => Box::pin(std::future::ready(Err("no file picker in this build".to_string()))),
    }
}

pub fn send_picked(path: &str) -> BoxFuture {
    if crate::demo::active() {
        return Box::pin(std::future::ready(Ok(crate::demo::reply(&Request::new("POST", "/upload")))));
    }
    match PICK.get() {
        Some((_, s)) => s(path.to_string()),
        None => Box::pin(std::future::ready(Err("no file picker in this build".to_string()))),
    }
}

// ---------- saving a download (web only) ----------

/// Web: hand bytes to the browser as a download (`name`).
static SAVE: std::sync::OnceLock<fn(&str, &[u8]) -> Result<(), String>> = std::sync::OnceLock::new();

pub fn set_save_hook(f: fn(&str, &[u8]) -> Result<(), String>) {
    let _ = SAVE.set(f);
}

/// Save `bytes` as the file `name` the browser's way. Native: not used (the
/// desktop writes files itself).
pub fn save_download(name: &str, bytes: &[u8]) -> Result<(), String> {
    match SAVE.get() {
        Some(f) => f(name, bytes),
        None => Err("no download in this build".into()),
    }
}

// ---------- shapes ----------

/// One source's outcome in POST /api/refresh.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefreshSource {
    pub source: String,
    pub ok: bool,
    pub ms: u64,
    pub error: Option<String>,
}

/// POST /api/refresh's answer: the server fetched this user's live sources.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefreshReport {
    pub ok: bool,
    pub ms: u64,
    pub sources: Vec<RefreshSource>,
}

impl RefreshReport {
    /// "Live: Binance 1.2s, OKX 0.8s, IBKR failed": what the sync line shows.
    pub fn summary(&self) -> String {
        let name = |s: &str| match s {
            "binance" => "Binance".to_string(),
            "okx" => "OKX".to_string(),
            "ibkr" => "IBKR".to_string(),
            "prices" => "Prices".to_string(),
            other => other.to_string(),
        };
        let parts: Vec<String> = self
            .sources
            .iter()
            .map(|s| if s.ok { format!("{} {:.1}s", name(&s.source), s.ms as f64 / 1000.0) } else { format!("{} failed", name(&s.source)) })
            .collect();
        format!("Live: {}", parts.join(", "))
    }

    /// Why the failed ones failed, for the tooltip.
    pub fn problems(&self) -> String {
        let v: Vec<String> = self.sources.iter().filter(|s| !s.ok).map(|s| format!("{}: {}", s.source, s.error.as_deref().unwrap_or("failed"))).collect();
        v.join("; ")
    }
}

/// Ask the server to fetch fresh data from the live sources now (Ctrl+Shift+K)
/// and wait for its per-source report. The caller pulls the replica after.
pub async fn refresh_live() -> Result<RefreshReport, String> {
    let r = send(Request::new("POST", "/api/refresh")).await?;
    if r.status == 404 {
        return Err("this server can't refresh live data yet (update it)".into());
    }
    if !r.ok() {
        return Err(r.error());
    }
    serde_json::from_slice(&r.body).map_err(|e| format!("unreadable refresh reply: {e}"))
}

/// GET /api/whoami's answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhoAmI {
    pub paired: bool,
    pub device: Option<DeviceInfo>,
    pub user: Option<UserInfo>,
    pub auth_mode: String,
    pub auth_mode_from_env: bool,
    /// the server has more than one user with data (a household server)
    pub household: bool,
    /// how a new account is made: "first" (no users yet), "open", "invite"
    /// ("" from a server that predates accounts made in the app)
    pub signup: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceInfo {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserInfo {
    pub id: i64,
    pub name: String,
    pub role: String,
    /// the account has its data on the server (every account does; false
    /// only for one made before per-user trees, until the next restart)
    pub data: bool,
}

impl Default for UserInfo {
    fn default() -> Self {
        UserInfo { id: 0, name: String::new(), role: String::new(), data: true }
    }
}

impl WhoAmI {
    pub fn admin(&self) -> bool {
        self.user.as_ref().is_some_and(|u| u.role == "admin")
    }

    /// "paired as laptop" / "not paired"
    pub fn label(&self) -> String {
        match (&self.device, self.paired) {
            (Some(d), true) => format!("paired as {}", d.name),
            _ => "not paired".into(),
        }
    }

    /// Signed in, but the account sees no data yet (see [`UserInfo::data`]).
    pub fn no_data_yet(&self) -> bool {
        self.paired && self.user.as_ref().is_some_and(|u| !u.data)
    }

    /// Does the server know this device (or want nothing from it)?
    pub fn known(&self) -> bool {
        self.paired || self.auth_mode == "off"
    }

    /// Reads are open to anyone who reaches the server.
    pub fn reads_open(&self) -> bool {
        self.auth_mode == "writes" || self.auth_mode == "off"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oneshot_wakes() {
        let (tx, rx) = oneshot::<u32>();
        std::thread::spawn(move || tx.send(7));
        // a minimal executor: poll until ready
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut rx = Box::pin(rx);
        let v = loop {
            if let Poll::Ready(v) = rx.as_mut().poll(&mut cx) {
                break v;
            }
            std::thread::yield_now();
        };
        assert_eq!(v, 7);
        let r = Response { status: 422, body: br#"{"error":"nope"}"#.to_vec() };
        assert!(!r.ok());
        assert_eq!(r.error(), "nope");
        // an older server's whoami: no data flag means the data is there
        let w: WhoAmI = serde_json::from_str(r#"{"paired":true,"user":{"id":1,"name":"o","role":"admin"}}"#).unwrap();
        assert!(!w.no_data_yet() && w.known() && w.signup.is_empty());
        let w: WhoAmI = serde_json::from_str(r#"{"paired":true,"user":{"id":2,"name":"s","role":"user","data":false},"signup":"invite"}"#).unwrap();
        assert!(w.no_data_yet());
    }
}

/// "https://myserver.tail1234.ts.net:8787" -> "myserver": the server's
/// short name for labels (an IP address stays whole). Both builds.
pub fn short_host(url: &str) -> &str {
    let host = url.split_once("://").map_or(url, |x| x.1).split(['/', ':']).next().unwrap_or("");
    if host.parse::<std::net::IpAddr>().is_ok() { host } else { host.split('.').next().unwrap_or(host) }
}
