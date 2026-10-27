//! The door every request passes (server.rs calls [`check`] before any
//! route): which routes need a device, which need an admin, and the
//! browser-side defenses. Pure: the request's facts in, a verdict out.
//!
//! * Host: an unknown Host header is refused (DNS rebinding: a page on an
//!   attacker's name that resolves to this server). Allowed: localhost, any
//!   IP literal, `*.ts.net`, this machine's hostname (and `.local`), and
//!   `FINANCE_ALLOWED_HOSTS` (comma-separated).
//! * Non-GET: an Origin, when sent, must name this host; a cookie session
//!   must also send `X-Finance-Request` (a header a cross-site form can't
//!   set without a preflight, which this server never answers).
//! * `/mcp` (the MCP endpoint, mcp.rs) takes a bearer token and nothing else:
//!   no cookie session, no anonymous call in any mode. Its Host must be on
//!   the allow-list whatever the mode, and an Origin, when sent (a browser
//!   page), must be on it too. An MCP key (a device whose scope is `mcp`)
//!   opens `/mcp` and no other route.
//! * Secret and admin routes always need a device, whatever the mode, and
//!   their writes are refused unless the peer is loopback (`tailscale serve`
//!   proxies to loopback) or the request came over TLS.
//! * A device whose account has no data tree (a safety net: every account
//!   gets one) gets only the public routes.
//! * The category and rule routes (categories.rs): their GETs are reads;
//!   every other method, the dry runs (preview, test) included, is a write.
//! * Then the mode: `off` lets everything else through, `writes` wants a
//!   device for every non-GET, `all` for everything but the public routes.
//!   A household server (more than one user with a tree) is always `all`
//!   (auth::effective_mode): a request without a device would be the
//!   owner's.
//!
//! Whose data a request touches is not decided here: server.rs runs every
//! admitted request as its device's user (finance_engine::tenant), so a
//! route only ever sees its caller's tree. Admin routes manage accounts;
//! none of them reads another user's data.

use crate::auth::{Device, Mode};

/// How the device was named.
#[derive(Clone, Debug)]
pub enum Who {
    Nobody,
    Bearer(Device),
    Cookie(Device),
}

impl Who {
    pub fn device(&self) -> Option<&Device> {
        match self {
            Who::Nobody => None,
            Who::Bearer(d) | Who::Cookie(d) => Some(d),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// health, version, the web app's files, the login page, pairing, the demo
    Public,
    Read,
    Write,
    /// a source's secret: set, delete, test, import
    Secret,
    /// devices, pairing codes, the auth mode
    Admin,
    /// the MCP endpoint: a bearer key only
    Mcp,
}

pub fn is_read(method: &str) -> bool {
    method == "GET" || method == "HEAD"
}

pub fn classify(method: &str, path: &str) -> Class {
    let read = is_read(method);
    if path == "/mcp" {
        return Class::Mcp;
    }
    if read
        && (matches!(path, "/healthz" | "/api/version" | "/favicon.ico" | "/favicon.png" | "/" | "/app" | "/sw.js" | "/login" | "/api/whoami")
            || path.starts_with("/app/")
            // the demo: made-up data, built from nothing of anyone's (demo.rs)
            || matches!(path, "/api/demo/ledger.json" | "/api/demo/export.json"))
    {
        return Class::Public;
    }
    if method == "POST" && matches!(path, "/api/pair" | "/api/login" | "/api/logout" | "/api/signup" | "/api/signup/session") {
        return Class::Public;
    }
    // a user's own archive (export, import), backup key and nightly
    // archives, and a pairing code for another of their own devices: any
    // device, on its own user's tree (its tenant), like a secret
    if path.starts_with("/api/account/")
        || path == "/api/admin/backup-recipient"
        || path == "/api/admin/archives"
        || path.starts_with("/api/admin/archives/")
        || path == "/api/me/pair-code"
        || path == "/api/me/delete"
        // an MCP key's token is shown once, like a pairing code
        || (path.starts_with("/api/mcp/keys") && !read)
    {
        return Class::Secret;
    }
    // the investment setup is the caller's own tree's: a write like any
    // other (the path predates per-user trees)
    if path.starts_with("/api/admin/investments/") {
        return if read { Class::Read } else { Class::Write };
    }
    // installing the web app puts code on the server: admins only; so are
    // users, devices, invites, the auth mode and the owner's .env import
    if path.starts_with("/api/admin/") || path == "/api/secrets/import-env" || path == "/api/webapp" {
        return Class::Admin;
    }
    if let Some(rest) = path.strip_prefix("/api/sources/") {
        if rest.ends_with("/secret") || rest.ends_with("/test") {
            return Class::Secret;
        }
    }
    if read {
        Class::Read
    } else {
        Class::Write
    }
}

/// The host part of a Host header or an Origin: no scheme, port or
/// brackets, lowercase.
pub fn host_name(s: &str) -> String {
    let s = s.trim();
    let s = s.split_once("://").map_or(s, |x| x.1);
    let s = s.split('/').next().unwrap_or("");
    let h = if let Some(rest) = s.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else if s.matches(':').count() > 1 {
        s // a bare IPv6 address
    } else {
        s.split(':').next().unwrap_or("")
    };
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// This machine's hostname (Linux), lowercase.
pub fn machine_hostname() -> Option<String> {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

/// The names this server answers to besides the built-in ones.
pub fn extra_hosts() -> Vec<String> {
    let mut v: Vec<String> = std::env::var("FINANCE_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(host_name)
        .filter(|h| !h.is_empty())
        .collect();
    if let Some(h) = machine_hostname() {
        v.push(format!("{h}.local"));
        v.push(h);
    }
    v
}

pub fn host_allowed(host: &str, extra: &[String]) -> bool {
    let h = host_name(host);
    if h.is_empty() {
        return false;
    }
    h == "localhost"
        || h.ends_with(".localhost")
        || h.parse::<std::net::IpAddr>().is_ok()
        || h.ends_with(".ts.net")
        || extra.iter().any(|e| *e == h)
}

/// The facts of one request.
pub struct Facts<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub host: Option<&'a str>,
    /// X-Forwarded-Host, trusted only from a loopback peer (tailscale serve)
    pub forwarded_host: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub x_finance_request: bool,
    pub peer_loopback: bool,
    pub tls: bool,
}

/// A refusal: status and a plain message.
pub type Refusal = (u16, &'static str);

/// What a device of an account without a data tree hears on a data route
/// (every account gets one when made, or at the next start for one made
/// before per-user trees: this is a safety net).
pub const NO_DATA_YET: &str = "your account has no data tree on this server yet: ask the admin to restart it";

/// Paths a browser posts to with its cookie session (or to get one): they
/// need `X-Finance-Request`.
fn browser_path(path: &str) -> bool {
    matches!(path, "/api/login" | "/api/signup/session")
}

const UNKNOWN_HOST: &str = "unknown Host: add it to FINANCE_ALLOWED_HOSTS if this server should answer to it";

/// `/mcp`: a bearer key, from a Host (and Origin, if any) this server knows.
fn check_mcp(f: &Facts, who: &Who, extra_hosts: &[String]) -> Result<Class, Refusal> {
    if !f.host.is_some_and(|h| host_allowed(h, extra_hosts)) {
        return Err((421, UNKNOWN_HOST));
    }
    if f.origin.is_some_and(|o| !o.is_empty() && !host_allowed(o, extra_hosts)) {
        return Err((403, "cross-origin request refused"));
    }
    match who {
        Who::Bearer(d) if !d.mcp_only() => Err((403, "/mcp takes an MCP key: make one in Settings")),
        Who::Bearer(d) if d.has_data => Ok(Class::Mcp),
        Who::Bearer(_) => Err((403, NO_DATA_YET)),
        Who::Cookie(_) => Err((403, "/mcp takes a bearer key, not a browser session")),
        Who::Nobody => Err((401, "send an MCP key: Authorization: Bearer <key>")),
    }
}

pub fn check(f: &Facts, who: &Who, mode: Mode, extra_hosts: &[String]) -> Result<Class, Refusal> {
    let class = classify(f.method, f.path);
    if class == Class::Mcp {
        return check_mcp(f, who, extra_hosts);
    }
    if !f.host.is_some_and(|h| host_allowed(h, extra_hosts)) {
        return Err((421, UNKNOWN_HOST));
    }
    if who.device().is_some_and(Device::mcp_only) {
        return Err((403, "an MCP key opens /mcp only"));
    }
    let read = is_read(f.method);
    if !read {
        if let Some(origin) = f.origin.filter(|o| !o.is_empty() && *o != "null") {
            let o = host_name(origin);
            let ours = f.host.map(host_name);
            let fwd = f.forwarded_host.filter(|_| f.peer_loopback).map(host_name);
            if Some(&o) != ours.as_ref() && Some(&o) != fwd.as_ref() {
                return Err((403, "cross-origin request refused"));
            }
        }
        let browser_call = matches!(who, Who::Cookie(_)) || browser_path(f.path);
        if browser_call && !f.x_finance_request {
            return Err((403, "missing X-Finance-Request header"));
        }
    }
    if who.device().is_some_and(|d| !d.has_data) && class != Class::Public {
        return Err((403, NO_DATA_YET));
    }
    match class {
        Class::Public => Ok(class),
        Class::Secret | Class::Admin => {
            let Some(dev) = who.device() else { return Err((401, "pair this device first: this needs a device token")) };
            if class == Class::Admin && !dev.admin() {
                return Err((403, "admins only"));
            }
            if !read && !(f.peer_loopback || f.tls) {
                return Err((403, "secrets are only written over loopback or TLS"));
            }
            Ok(class)
        }
        Class::Read if mode == Mode::All && who.device().is_none() => Err((401, "pair this device first")),
        Class::Write if mode != Mode::Off && who.device().is_none() => Err((401, "pair this device first: writes need a device token")),
        _ => Ok(class),
    }
}

/// `finance_session=<token>` from a Cookie header.
pub fn session_cookie(cookie: &str) -> Option<&str> {
    cookie.split(';').map(str::trim).find_map(|kv| kv.strip_prefix("finance_session=")).filter(|v| !v.is_empty())
}

pub const COOKIE_MAX_AGE: i64 = 400 * 24 * 3600;

pub fn set_cookie(token: &str) -> String {
    format!("finance_session={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE}")
}

pub fn clear_cookie() -> String {
    "finance_session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(role: &str) -> Device {
        Device { id: 1, user_id: 1, user_name: "o".into(), role: role.into(), name: "laptop".into(), scopes: "read,write".into(), created: 0, last_seen: None, has_data: true, home: String::new() }
    }

    fn facts<'a>(method: &'a str, path: &'a str) -> Facts<'a> {
        Facts { method, path, host: Some("127.0.0.1:8787"), forwarded_host: None, origin: None, x_finance_request: false, peer_loopback: true, tls: false }
    }

    #[test]
    fn classes() {
        assert_eq!(classify("GET", "/app/finance.wasm"), Class::Public);
        assert_eq!(classify("GET", "/sw.js"), Class::Public);
        assert_eq!(classify("PUT", "/api/formats/acme"), Class::Write);
        assert_eq!(classify("GET", "/api/export.json"), Class::Read);
        assert_eq!(classify("POST", "/upload"), Class::Write);
        assert_eq!(classify("PUT", "/api/sources/binance/secret"), Class::Secret);
        assert_eq!(classify("POST", "/api/sources/okx/test"), Class::Secret);
        assert_eq!(classify("GET", "/api/sources/catalog"), Class::Read);
        assert_eq!(classify("POST", "/api/admin/pair-code"), Class::Admin);
        assert_eq!(classify("POST", "/api/pair"), Class::Public);
        assert_eq!(classify("POST", "/api/account/export"), Class::Secret);
        assert_eq!(classify("GET", "/api/admin/archives/latest"), Class::Secret);
        assert_eq!(classify("POST", "/api/signup"), Class::Public);
        assert_eq!(classify("POST", "/api/admin/invite"), Class::Admin);
        assert_eq!(classify("GET", "/api/admin/users"), Class::Admin);
        assert_eq!(classify("DELETE", "/api/admin/users/3"), Class::Admin);
        // a user's own tree: their investment setup, archive, backup key, devices
        assert_eq!(classify("PUT", "/api/admin/investments/instruments/VT"), Class::Write);
        assert_eq!(classify("PUT", "/api/admin/backup-recipient"), Class::Secret);
        assert_eq!(classify("POST", "/api/me/pair-code"), Class::Secret);
        assert_eq!(classify("GET", "/api/profile/avatar"), Class::Read);
        assert_eq!(classify("PUT", "/api/profile/avatar"), Class::Write);
    }

    #[test]
    fn category_routes_reads_and_writes() {
        // reading categories, rules and suggestions is a read; every change
        // (and the dry runs, which read every row) wants a device
        for (m, p) in [("GET", "/api/categories"), ("GET", "/api/rules"), ("GET", "/api/categorize/suggest")] {
            assert_eq!(classify(m, p), Class::Read, "{m} {p}");
        }
        let writes = [
            ("PUT", "/api/txn/hsbc_card/abc123/category"),
            ("POST", "/api/rules/preview"),
            ("POST", "/api/rules/test"),
            ("POST", "/api/rules"),
            ("PATCH", "/api/rules/12"),
            ("DELETE", "/api/rules/12"),
            ("POST", "/api/rules/starter"),
            ("POST", "/api/categories"),
            ("PATCH", "/api/categories"),
            ("PATCH", "/api/categories/Utilities%20%2F%20Telecom"),
            ("POST", "/api/categories/Dining/merge"),
            ("DELETE", "/api/categories/Dining"),
        ];
        let laptop = Who::Bearer(dev("user"));
        for (m, p) in writes {
            assert_eq!(classify(m, p), Class::Write, "{m} {p}");
            assert_eq!(check(&facts(m, p), &Who::Nobody, Mode::Writes, &[]).unwrap_err().0, 401, "{m} {p} without a device");
            assert!(check(&facts(m, p), &laptop, Mode::Writes, &[]).is_ok(), "{m} {p} with one");
        }
        // a browser session needs the custom header for them too
        let mut f = facts("PATCH", "/api/rules/12");
        assert_eq!(check(&f, &Who::Cookie(dev("user")), Mode::Writes, &[]).unwrap_err().0, 403);
        f.x_finance_request = true;
        assert!(check(&f, &Who::Cookie(dev("user")), Mode::Writes, &[]).is_ok());
    }

    #[test]
    fn an_account_without_data_gets_only_the_public_routes() {
        let mut d = dev("user");
        d.has_data = false;
        let invitee = Who::Bearer(d);
        for mode in [Mode::Off, Mode::Writes, Mode::All] {
            assert_eq!(check(&facts("GET", "/api/export.json"), &invitee, mode, &[]).unwrap_err(), (403, NO_DATA_YET));
            assert_eq!(check(&facts("PUT", "/api/profile"), &invitee, mode, &[]).unwrap_err().0, 403);
            assert!(check(&facts("GET", "/api/whoami"), &invitee, mode, &[]).is_ok());
            assert!(check(&facts("POST", "/api/logout"), &invitee, mode, &[]).is_ok());
        }
    }

    #[test]
    fn the_host_allow_list_holds_in_every_mode() {
        let laptop = Who::Bearer(dev("admin"));
        for mode in [Mode::Off, Mode::Writes, Mode::All] {
            for who in [&Who::Nobody, &laptop] {
                let mut f = facts("GET", "/api/export.json");
                f.host = Some("rebind.attacker.net");
                assert_eq!(check(&f, who, mode, &[]).unwrap_err().0, 421, "{mode:?}");
                f.host = None;
                assert_eq!(check(&f, who, mode, &[]).unwrap_err().0, 421, "{mode:?} without a Host");
            }
        }
    }

    #[test]
    fn modes() {
        let none = Who::Nobody;
        let laptop = Who::Bearer(dev("admin"));
        let x = &[];
        // off: open, except secrets
        assert!(check(&facts("POST", "/upload"), &none, Mode::Off, x).is_ok());
        assert_eq!(check(&facts("PUT", "/api/sources/okx/secret"), &none, Mode::Off, x).unwrap_err().0, 401);
        // writes: reads open, writes need a device
        assert!(check(&facts("GET", "/api/db.sqlite"), &none, Mode::Writes, x).is_ok());
        assert_eq!(check(&facts("POST", "/upload"), &none, Mode::Writes, x).unwrap_err().0, 401);
        assert!(check(&facts("POST", "/upload"), &laptop, Mode::Writes, x).is_ok());
        // all: reads too, but not the public routes
        assert_eq!(check(&facts("GET", "/api/ledger.json"), &none, Mode::All, x).unwrap_err().0, 401);
        assert!(check(&facts("GET", "/"), &none, Mode::All, x).is_ok());
        assert!(check(&facts("POST", "/api/pair"), &none, Mode::All, x).is_ok());
        assert!(check(&facts("GET", "/api/ledger.json"), &laptop, Mode::All, x).is_ok());
        // admin routes want the admin role
        assert_eq!(check(&facts("POST", "/api/admin/pair-code"), &Who::Bearer(dev("user")), Mode::All, x).unwrap_err().0, 403);
        assert!(check(&facts("POST", "/api/admin/pair-code"), &laptop, Mode::All, x).is_ok());
    }

    #[test]
    fn the_mcp_endpoint_takes_a_bearer_key_only() {
        let key = |has_data: bool| {
            let mut d = dev("user");
            d.scopes = "mcp".into();
            d.has_data = has_data;
            d
        };
        assert!(key(true).mcp_only() && !dev("admin").mcp_only());
        for m in ["GET", "POST", "DELETE"] {
            assert_eq!(classify(m, "/mcp"), Class::Mcp, "{m}");
        }
        assert_eq!(classify("GET", "/api/mcp"), Class::Read);
        assert_eq!(classify("PUT", "/api/mcp"), Class::Write);
        assert_eq!(classify("POST", "/api/mcp/keys"), Class::Secret);
        assert_eq!(classify("DELETE", "/api/mcp/keys/3"), Class::Secret);
        let f = |m| facts(m, "/mcp");
        for mode in [Mode::Off, Mode::Writes, Mode::All] {
            // an MCP key; nothing else, in any mode (a normal device's token is refused)
            assert_eq!(check(&f("POST"), &Who::Bearer(key(true)), mode, &[]), Ok(Class::Mcp));
            assert_eq!(check(&f("POST"), &Who::Bearer(dev("user")), mode, &[]).unwrap_err().0, 403, "an ordinary device token");
            assert_eq!(check(&f("POST"), &Who::Nobody, mode, &[]).unwrap_err().0, 401);
            assert_eq!(check(&f("POST"), &Who::Cookie(dev("user")), mode, &[]).unwrap_err().0, 403, "a browser session");
            assert_eq!(check(&f("POST"), &Who::Cookie(key(true)), mode, &[]).unwrap_err().0, 403);
            assert_eq!(check(&f("POST"), &Who::Bearer(key(false)), mode, &[]).unwrap_err(), (403, NO_DATA_YET));
            // the Host allow-list holds even where the mode doesn't ask for it
            let mut rebind = f("POST");
            rebind.host = Some("rebind.attacker.net");
            assert_eq!(check(&rebind, &Who::Bearer(key(true)), mode, &[]).unwrap_err().0, 421);
            rebind.host = None;
            assert_eq!(check(&rebind, &Who::Bearer(key(true)), mode, &[]).unwrap_err().0, 421);
        }
        // an Origin, when sent, must be on the allow-list: a GET included
        for m in ["GET", "POST"] {
            let mut o = f(m);
            for bad in ["https://evil.example.com", "null"] {
                o.origin = Some(bad);
                assert_eq!(check(&o, &Who::Bearer(key(true)), Mode::All, &[]).unwrap_err().0, 403, "{m} {bad}");
            }
            for good in ["http://localhost:3000", "https://box.tail1234.ts.net", "http://127.0.0.1:8787", ""] {
                o.origin = Some(good);
                assert_eq!(check(&o, &Who::Bearer(key(true)), Mode::All, &[]), Ok(Class::Mcp), "{m} {good}");
            }
        }
        // a key opens /mcp and nothing else: no data route, no public one, no setting, no key management
        let k = Who::Bearer(key(true));
        for (m, p) in [
            ("GET", "/api/export.json"), ("GET", "/api/whoami"), ("GET", "/healthz"), ("GET", "/"), ("POST", "/upload"), ("PUT", "/api/mcp"),
            ("POST", "/api/mcp/keys"), ("DELETE", "/api/mcp/keys/1"), ("GET", "/api/admin/devices"), ("POST", "/api/pair"),
        ] {
            for mode in [Mode::Off, Mode::Writes, Mode::All] {
                assert_eq!(check(&facts(m, p), &k, mode, &[]).unwrap_err(), (403, "an MCP key opens /mcp only"), "{m} {p} {mode:?}");
            }
        }
        // the same routes are fine for a normal device
        assert!(check(&facts("GET", "/api/mcp"), &Who::Bearer(dev("user")), Mode::All, &[]).is_ok());
        assert!(check(&facts("POST", "/api/mcp/keys"), &Who::Bearer(dev("user")), Mode::All, &[]).is_ok());
        let mut browser_write = facts("PUT", "/api/mcp");
        assert_eq!(check(&browser_write, &Who::Cookie(dev("user")), Mode::All, &[]).unwrap_err().0, 403, "a cookie write needs X-Finance-Request");
        browser_write.x_finance_request = true;
        assert!(check(&browser_write, &Who::Cookie(dev("user")), Mode::All, &[]).is_ok());
        // minting a key shows a secret: loopback or TLS only
        let mut remote = facts("POST", "/api/mcp/keys");
        remote.peer_loopback = false;
        assert_eq!(check(&remote, &Who::Bearer(dev("user")), Mode::All, &[]).unwrap_err().0, 403);
        remote.tls = true;
        assert!(check(&remote, &Who::Bearer(dev("user")), Mode::All, &[]).is_ok());
    }

    #[test]
    fn secret_writes_only_over_loopback_or_tls() {
        let laptop = Who::Bearer(dev("admin"));
        let mut f = facts("PUT", "/api/sources/binance/secret");
        f.peer_loopback = false;
        assert_eq!(check(&f, &laptop, Mode::Writes, &[]).unwrap_err().0, 403);
        f.tls = true;
        assert!(check(&f, &laptop, Mode::Writes, &[]).is_ok());
    }

    #[test]
    fn host_and_origin() {
        let laptop = Who::Bearer(dev("admin"));
        let extra = vec!["homeserver".to_string()];
        assert!(host_allowed("server.tail1234.ts.net", &extra));
        assert!(host_allowed("homeserver:8787", &extra));
        assert!(host_allowed("[::1]:8787", &extra));
        assert!(host_allowed("192.168.1.5", &extra));
        assert!(!host_allowed("evil.example.com", &extra));
        assert!(!host_allowed("", &extra));
        let mut f = facts("GET", "/api/export.json");
        f.host = Some("rebind.attacker.net");
        assert_eq!(check(&f, &Who::Nobody, Mode::Writes, &extra).unwrap_err().0, 421);
        f.host = None;
        assert_eq!(check(&f, &Who::Nobody, Mode::Writes, &extra).unwrap_err().0, 421);

        let mut f = facts("POST", "/upload");
        f.origin = Some("https://evil.example.com");
        assert_eq!(check(&f, &laptop, Mode::Writes, &extra).unwrap_err().0, 403);
        f.origin = Some("http://127.0.0.1:8787");
        assert!(check(&f, &laptop, Mode::Writes, &extra).is_ok());
        // behind tailscale serve: Host may be the loopback address, the
        // forwarded host is the tailnet name
        f.origin = Some("https://server.tail1234.ts.net");
        f.forwarded_host = Some("server.tail1234.ts.net");
        assert!(check(&f, &laptop, Mode::Writes, &extra).is_ok());
        f.peer_loopback = false;
        assert_eq!(check(&f, &laptop, Mode::Writes, &extra).unwrap_err().0, 403, "a forwarded host counts from loopback only");
    }

    #[test]
    fn cookie_sessions_need_the_custom_header() {
        let browser = Who::Cookie(dev("admin"));
        let mut f = facts("POST", "/upload");
        assert_eq!(check(&f, &browser, Mode::Writes, &[]).unwrap_err().0, 403);
        f.x_finance_request = true;
        assert!(check(&f, &browser, Mode::Writes, &[]).is_ok());
        for path in ["/api/login", "/api/signup/session"] {
            let mut f = facts("POST", path);
            assert_eq!(check(&f, &Who::Nobody, Mode::All, &[]).unwrap_err().0, 403, "{path}");
            f.x_finance_request = true;
            assert!(check(&f, &Who::Nobody, Mode::All, &[]).is_ok());
        }
        // a device signing up (the desktop) sends a bearer, not a cookie
        assert!(check(&facts("POST", "/api/signup"), &Who::Nobody, Mode::All, &[]).is_ok());
        assert!(check(&facts("GET", "/api/export.json"), &browser, Mode::All, &[]).is_ok());
        assert_eq!(session_cookie("a=b; finance_session=fin_x; c=d"), Some("fin_x"));
        assert_eq!(session_cookie("a=b"), None);
        assert!(set_cookie("t").contains("HttpOnly; Secure; SameSite=Strict"));
    }
}
