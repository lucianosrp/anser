//! Doctor: is everything around the app healthy? UI-free.
//!
//! Every check is a plain function that returns a [`Check`]: a [`Status`], a
//! one-line summary, a few facts, and the fixes on offer. The probes that
//! touch the network or the disk are blocking (the app runs each one on the
//! background executor); the verdicts are pure functions of what the probes
//! found, so they are tested here without a network.
//!
//! Fixes come in two kinds ([`Fix`]):
//! * an [`Action`] the app performs on one click: sync, set up the server,
//!   start a local server (`anser server serve`), or an [`Installer`];
//! * a [`Fix::Command`] the owner runs themselves. Anything that changes
//!   system settings (DNS, packages, the compositor) is only ever shown.
//!
//! Not every check applies everywhere: [`CheckId::applies`] filters them on
//! what [`tools::detect`] found (Tailscale and other VPNs, the OS, the package
//! manager, Hyprland) and on the server URL.
//!
//! [`Installer`] is the seam for one-click installs: the desktop integration
//! ([`DesktopInstaller`]) and the web app on the server ([`WebAppInstaller`]).

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::sources::{ApiSource, Sources};
use crate::tools::{self, Env, Os, PkgManager, Vpn};
use crate::{bundle, cache, tailnet, webapp};

// ---------- the vocabulary ----------

/// Worst last: `max()` over a page gives its overall state.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub enum Status {
    Ok,
    Warn,
    Missing,
    Fail,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Missing => "not installed",
            Status::Fail => "fail",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CheckId {
    Server,
    Tailnet,
    Replica,
    Sources,
    Ibkr,
    Binance,
    Okx,
    BankExports,
    Phone,
    WebApp,
    Desktop,
    Gpu,
    Theme,
    Logos,
    Config,
}

impl CheckId {
    /// Page order: the remote server first, then what depends on it.
    pub const ALL: [CheckId; 15] = [
        CheckId::Server, CheckId::Tailnet, CheckId::Replica, CheckId::Sources,
        CheckId::Ibkr, CheckId::Binance, CheckId::Okx, CheckId::BankExports,
        CheckId::Phone, CheckId::WebApp,
        CheckId::Desktop, CheckId::Gpu, CheckId::Theme, CheckId::Logos, CheckId::Config,
    ];

    pub fn title(self) -> &'static str {
        match self {
            CheckId::Server => "Server",
            CheckId::Tailnet => "Tailnet",
            CheckId::Replica => "Local replica",
            CheckId::Sources => "Sources",
            CheckId::Ibkr => "IBKR",
            CheckId::Binance => "Binance",
            CheckId::Okx => "OKX",
            CheckId::BankExports => "Bank exports",
            CheckId::Phone => "Phone access",
            CheckId::WebApp => "Web app",
            CheckId::Desktop => "Desktop integration",
            CheckId::Gpu => "GPU",
            CheckId::Theme => "Theme",
            CheckId::Logos => "Logos",
            CheckId::Config => "Config files",
        }
    }

    /// Does this check mean anything on this machine, for this server?
    ///
    /// * Tailnet: Tailscale is installed, or the server is a `.ts.net` name.
    ///   Its VPN part (another VPN owning the DNS) shows only when one is up.
    /// * Phone access: the server isn't a loopback address.
    /// * GPU: Linux only (the Vulkan driver choice is a Linux concern).
    /// * The rest always apply (Theme reads Hyprland's blur only under it).
    pub fn applies(self, env: &Env, server_url: &str) -> bool {
        let host = url_host(server_url).unwrap_or_default();
        match self {
            CheckId::Tailnet => env.has(Vpn::Tailscale) || tailnet::is_tailnet_name(&host),
            CheckId::Phone => !host.is_empty() && !is_loopback(&host),
            CheckId::Gpu => env.os == Os::Linux,
            CheckId::Server | CheckId::Replica | CheckId::Sources | CheckId::Ibkr | CheckId::Binance | CheckId::Okx | CheckId::BankExports
            | CheckId::WebApp | CheckId::Desktop | CheckId::Theme | CheckId::Logos | CheckId::Config => true,
        }
    }

    /// [`CheckId::ALL`], in order, without the ones that don't apply.
    pub fn applicable(env: &Env, server_url: &str) -> Vec<CheckId> {
        CheckId::ALL.into_iter().filter(|c| c.applies(env, server_url)).collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum InstallKind {
    /// launcher entry, icon, ~/.local/bin symlink
    Desktop,
    /// the WASM build of this app, served by the server at /app
    WebApp,
}

/// What a fix button does in the app.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Install(InstallKind),
    SyncNow,
    /// open the guided server setup (URL, save, test)
    SetupServer,
    /// start this binary as `anser server serve` (the server is built in)
    StartLocalServer,
    /// go to the Upload page
    OpenUpload,
    /// open the pairing form (a code from `anser-server pair`)
    Pair,
    /// admin: switch the server to FINANCE_AUTH=all (reads need a device too)
    LockReads,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fix {
    /// For the owner to run: it changes system settings, so the app never does.
    Command(String),
    Action(Action),
}

#[derive(Clone, Debug)]
pub struct Check {
    pub id: CheckId,
    pub status: Status,
    pub summary: String,
    pub facts: Vec<(String, String)>,
    pub fixes: Vec<Fix>,
}

impl Check {
    pub fn new(id: CheckId, status: Status, summary: impl Into<String>) -> Self {
        Check { id, status, summary: summary.into(), facts: Vec::new(), fixes: Vec::new() }
    }

    pub fn fact(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.facts.push((k.into(), v.into()));
        self
    }

    pub fn fix(mut self, f: Fix) -> Self {
        if !self.fixes.contains(&f) {
            self.fixes.push(f);
        }
        self
    }

    fn worse(mut self, s: Status, summary: impl Into<String>) -> Self {
        if s > self.status {
            self.status = s;
            self.summary = summary.into();
        }
        self
    }
}

/// "42 s", "3 min", "5 h", "2 d"
pub fn ago(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

pub fn bytes(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.0} KB", n as f64 / 1024.),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.),
    }
}

/// The host of `http(s)://host[:port]/…`.
pub fn url_host(url: &str) -> Option<String> {
    let uri: ureq::http::Uri = url.trim().parse().ok()?;
    uri.host().map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string())
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

// ---------- server ----------

/// The replica sync is older than this: warn (the app syncs every 30 min).
pub const STALE_AFTER: u64 = 2 * 3600;

/// `GET /api/version` (anser-server).
#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo {
    pub kind: String,
    pub version: String,
    pub bundle_version: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub rtt: Duration,
    /// the `Server` header (tiny-http for anser-server)
    pub server_header: Option<String>,
    /// None when the server has no /api/version
    pub info: Option<ServerInfo>,
    /// GET /app: Some(true) on 200, Some(false) on 404
    pub webapp: Option<bool>,
}

/// Health, version and the web app, off the UI thread.
pub fn probe(url: &str) -> Result<Probe, String> {
    let base = url.trim().trim_end_matches('/');
    let agent = tailnet::agent(Duration::from_secs(6));
    let started = Instant::now();
    let mut r = agent.get(&format!("{base}/healthz")).call().map_err(|e| format!("{base}: {e}"))?;
    let rtt = started.elapsed();
    if r.status() != 200 {
        return Err(format!("{base}/healthz answered HTTP {}", r.status()));
    }
    let server_header = r.headers().get("server").and_then(|v| v.to_str().ok()).map(String::from);
    let body = r.body_mut().with_config().limit(1024).read_to_vec().unwrap_or_default();
    if !body.starts_with(b"ok") {
        return Err(format!("{base}/healthz did not answer ok: is it the finance server?"));
    }
    let info = agent.get(&format!("{base}/api/version")).call().ok().filter(|r| r.status() == 200).and_then(|mut r| {
        let v: serde_json::Value = serde_json::from_slice(&r.body_mut().with_config().limit(4096).read_to_vec().ok()?).ok()?;
        Some(ServerInfo {
            kind: v.get("kind")?.as_str()?.to_string(),
            version: v.get("version").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
            bundle_version: v.get("bundle_version").and_then(|x| x.as_u64()),
        })
    });
    let webapp = match agent.get(&format!("{base}/app")).call() {
        Ok(r) if r.status() == 200 => Some(true),
        Ok(r) if r.status() == 404 => Some(false),
        _ => None,
    };
    Ok(Probe { rtt, server_header, info, webapp })
}

/// What answers, in words.
pub fn server_kind(p: &Probe) -> String {
    match (&p.info, p.server_header.as_deref()) {
        (Some(i), _) => format!("{} {}", i.kind, i.version),
        (None, Some(h)) if h.contains("tiny-http") => "anser-server (an older build, no /api/version)".into(),
        (None, Some(h)) => format!("unknown ({h})"),
        (None, None) => "unknown".into(),
    }
}

/// The replica's export.json, as far as the doctor cares.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalBundle {
    pub version: Option<u64>,
    /// when the server built it (unix ms)
    pub generated_ms: Option<i64>,
    /// since it was written here
    pub age_secs: Option<u64>,
}

pub fn local_bundle() -> Option<LocalBundle> {
    let b = bundle::Bundle::load(&cache::bundle_path()).ok()?;
    Some(LocalBundle { version: b.at(&["version"]).as_u64(), generated_ms: b.generated_at(), age_secs: cache::age_secs() })
}

/// Where the URL comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UrlSource {
    Env,
    File,
    Default,
}

pub fn url_source() -> UrlSource {
    if std::env::var_os("FINANCE_SERVER").is_some() {
        UrlSource::Env
    } else if cache::saved_server_url().is_some() {
        UrlSource::File
    } else {
        UrlSource::Default
    }
}

pub fn server_check(url: &str, source: UrlSource, probe: &Result<Probe, String>, local: Option<&LocalBundle>,
                    now_ms: i64) -> Check {
    let id = CheckId::Server;
    let from = match source {
        UrlSource::Env => "$FINANCE_SERVER",
        UrlSource::File => "the server file",
        UrlSource::Default => "the default (nothing configured)",
    };
    let p = match probe {
        Err(e) => {
            let (status, summary) = if source == UrlSource::Default {
                (Status::Missing, "No server set up, and nothing answers on the default local address.".to_string())
            } else {
                (Status::Fail, format!("Unreachable: {e}"))
            };
            // the server is built into this binary: it can always start one here
            return Check::new(id, status, summary).fact("URL", url).fact("from", from).fact("app", crate::VERSION)
                .fix(Fix::Action(Action::SetupServer)).fix(Fix::Action(Action::StartLocalServer));
        }
        Ok(p) => p,
    };
    let ms = p.rtt.as_millis();
    let mut c = Check::new(id, Status::Ok, format!("{} answers in {ms} ms.", server_kind(p)))
        .fact("URL", url)
        .fact("from", from)
        .fact("round trip", format!("{ms} ms"))
        .fact("server", server_kind(p))
        .fact("app", crate::VERSION);
    if ms > 1500 {
        c = c.worse(Status::Warn, format!("Reachable, but slow: {ms} ms for /healthz."));
    }
    let want = bundle::SUPPORTED_VERSION;
    let theirs = p.info.as_ref().and_then(|i| i.bundle_version).or(local.and_then(|l| l.version));
    c = c.fact("bundle", match theirs {
        Some(v) => format!("v{v} (this app reads v{want})"),
        None => format!("unknown (this app reads v{want})"),
    });
    if theirs.is_some_and(|v| v != want) {
        c = c.worse(Status::Fail, format!("The server speaks bundle v{}, this app reads v{want}: update one of them.",
            theirs.unwrap_or_default()));
    }
    match local {
        None => {
            c = c.fact("last sync", "never").fix(Fix::Action(Action::SyncNow))
                .worse(Status::Warn, "Reachable, but never synced: there is no local copy yet.");
        }
        Some(l) => {
            if let Some(gen) = l.generated_ms {
                c = c.fact("last sync", format!("{} ago (server time)", ago(((now_ms - gen).max(0) / 1000) as u64)));
            }
            if let Some(age) = l.age_secs {
                c = c.fact("replica age", ago(age));
                if age > STALE_AFTER {
                    c = c.fix(Fix::Action(Action::SyncNow))
                        .worse(Status::Warn, format!("Reachable, but the local copy is {} old.", ago(age)));
                }
            }
        }
    }
    c
}

/// The Server row, with what the server thinks of this device: "paired as
/// X" or "not paired" (and the Pair fix), and, while reads are open to any
/// tailnet device (FINANCE_AUTH=writes), the nudge to lock them.
/// `who`: None when the server didn't answer (nothing to add).
pub fn with_pairing(mut c: Check, who: Option<&Result<crate::client::WhoAmI, String>>) -> Check {
    let Some(who) = who else { return c };
    match who {
        Err(e) => {
            c = c.fact("device", format!("unknown ({e})"));
        }
        Ok(w) if !w.paired => {
            c = c.fact("device", "not paired").fact("auth", w.auth_mode.clone()).fix(Fix::Action(Action::Pair));
            let why = if w.auth_mode == "all" {
                "Not paired: this server wants a device token for everything. Pair with a code from `anser-server pair`."
            } else {
                "Not paired: syncing works, but uploads and settings need a device token. Pair with a code from `anser-server pair`."
            };
            c = c.worse(if w.auth_mode == "all" { Status::Fail } else { Status::Warn }, why);
        }
        Ok(w) => {
            c = c.fact("device", w.label()).fact("auth", w.auth_mode.clone());
            if w.reads_open() {
                c = c.worse(Status::Warn,
                    "Reads are open to every tailnet device: switch to FINANCE_AUTH=all once your devices are paired.");
                if w.admin() && !w.auth_mode_from_env {
                    c = c.fix(Fix::Action(Action::LockReads));
                }
                c = c.fix(Fix::Command("anser-server auth all   # on the server; or FINANCE_AUTH=all in its unit".into()));
            }
        }
    }
    c
}

// ---------- tailnet ----------

/// The command that routes `*.ts.net` lookups to MagicDNS while the VPN
/// keeps every other domain. It changes the system DNS: shown, never run.
pub const DNS_FIX: &str = tools::DNS_FIX;

/// The verdict, with the VPN named by its label (Linux wording).
pub fn tailnet_check(host: &str, system: &Result<Vec<IpAddr>, String>, magic: Option<&Result<Vec<Ipv4Addr>, String>>,
                     vpn: Option<&str>) -> Check {
    let hint = tools::dns_hint(vpn.and_then(Vpn::from_label), vpn, Os::Linux);
    tailnet_verdict(host, system, magic, &hint)
}

/// The verdict on this machine: the VPN, its wording and its fixes come from
/// `env`, and what was found is listed in the facts.
pub fn tailnet_check_env(host: &str, system: &Result<Vec<IpAddr>, String>, magic: Option<&Result<Vec<Ipv4Addr>, String>>,
                         env: &Env) -> Check {
    let owner = env.dns_vpn();
    let hint = tools::dns_hint(owner.map(|f| f.vpn), None, env.os);
    let mut c = tailnet_verdict(host, system, magic, &hint);
    c = c.fact("Tailscale", match env.vpn(Vpn::Tailscale) {
        Some(f) if f.active => "installed, up".to_string(),
        Some(_) => "installed, not up (or not visible here)".to_string(),
        None => "not found on this machine".to_string(),
    });
    let others: Vec<String> = env.other_vpns().map(|f| f.describe()).collect();
    if !others.is_empty() {
        c = c.fact("other VPNs", others.join(", "));
    }
    if c.status != Status::Ok && owner.is_some() {
        if let Some(a) = &hint.advice {
            c = c.fact("also", a.clone());
        }
    }
    c
}

fn tailnet_verdict(host: &str, system: &Result<Vec<IpAddr>, String>, magic: Option<&Result<Vec<Ipv4Addr>, String>>,
                   hint: &tools::DnsHint) -> Check {
    let id = CheckId::Tailnet;
    if !tailnet::is_tailnet_name(host) {
        return Check::new(id, Status::Ok, format!("{host} is not a tailnet name: nothing to check.")).fact("host", host);
    }
    let list = |ips: &[IpAddr]| ips.iter().take(3).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
    let mut c = Check::new(id, Status::Ok, "").fact("host", host);
    c = c.fact("system DNS", match system {
        Ok(ips) if ips.is_empty() => "no answer".into(),
        Ok(ips) => list(ips),
        Err(e) => e.clone(),
    });
    if let Ok(ips) = system {
        if let Some(t) = ips.iter().find(|ip| tailnet::is_tailnet_ip(**ip)) {
            c.summary = format!("The system DNS resolves it to the tailnet address {t}.");
            return c;
        }
    }
    let why = &hint.why;
    match magic {
        Some(Ok(ips)) if !ips.is_empty() => {
            let shown: Vec<IpAddr> = ips.iter().map(|i| IpAddr::V4(*i)).collect();
            let then = if hint.commands.is_empty() { "" } else { " The command routes *.ts.net to MagicDNS (until tailscale restarts)." };
            c = c.fact("MagicDNS", list(&shown))
                .worse(Status::Warn, format!("{why}The app gets through by asking MagicDNS directly; other apps won't.{then}"));
            for cmd in &hint.commands {
                c = c.fix(Fix::Command(cmd.clone()));
            }
            c
        }
        other => {
            let err = match other {
                Some(Err(e)) => e.clone(),
                _ => "no answer".into(),
            };
            c.fact("MagicDNS", err)
                .worse(Status::Fail, format!("{why}MagicDNS (100.100.100.100) doesn't answer either: is Tailscale up?"))
                .fix(Fix::Command("tailscale status".into()))
                .fix(Fix::Command("sudo tailscale up".into()))
        }
    }
}

/// Probe the name both ways, then judge it against `env`. Blocking.
pub fn tailnet(url: &str, env: &Env) -> Check {
    let Some(host) = url_host(url) else {
        return Check::new(CheckId::Tailnet, Status::Fail, format!("Can't read a host name from {url}."));
    };
    if !tailnet::is_tailnet_name(&host) {
        return tailnet_check_env(&host, &Ok(vec![]), None, env);
    }
    let system = tailnet::system_lookup(&host, 443, Duration::from_secs(2))
        .map(|a| a.into_iter().map(|s| s.ip()).collect::<Vec<_>>())
        .map_err(|e| format!("{e:#}"));
    let needs_magic = !system.as_ref().is_ok_and(|ips| ips.iter().any(|ip| tailnet::is_tailnet_ip(*ip)));
    let magic = needs_magic.then(|| tailnet::query_a(tailnet::magic_dns(), &host, Duration::from_millis(1500)).map_err(|e| format!("{e:#}")));
    tailnet_check_env(&host, &system, magic.as_ref(), env)
}

// ---------- replica ----------

/// Present, readable, and whole: `PRAGMA quick_check` on a read-only open.
pub fn replica(db: &Path, bundle_path: &Path) -> Check {
    let id = CheckId::Replica;
    // the files on disk: the `.enc` twins when the local copy is encrypted
    let size = |p: &Path| std::fs::metadata(crate::replica::stored_path(p)).map(|m| m.len());
    let (db_size, bundle_size) = (size(db), size(bundle_path));
    let dir = db.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let enc = crate::replica::status_of(db.parent().unwrap_or(Path::new(".")));
    let local = if enc.encrypted {
        enc.label()
    } else {
        "not encrypted. This computer's disk isn't encrypted either: Settings, Sync, \"Encrypt this computer's copy\" seals it".to_string()
    };
    if db_size.is_err() && bundle_size.is_err() {
        return Check::new(id, Status::Missing, "No local copy yet: sync once and everything works offline.")
            .fact("folder", dir).fact("Local copy", local).fix(Fix::Action(Action::SyncNow));
    }
    let mut c = Check::new(id, Status::Ok, "").fact("folder", dir).fact("Local copy", local);
    if enc.encrypted && !enc.unlocked {
        return c.worse(Status::Warn, "The local copy is encrypted and locked: unlock it (or start over and sync again).");
    }
    match db_size {
        Err(_) => c = c.fact("finance.db", "missing").worse(Status::Fail, "finance.db is missing."),
        Ok(n) => {
            let started = Instant::now();
            let verdict = (|| -> Result<(String, i64)> {
                let conn = crate::replica::open_db(db)?;
                let ok: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
                let tables: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0))?;
                Ok((ok, tables))
            })();
            let ms = started.elapsed().as_millis();
            match verdict {
                Ok((ok, tables)) if ok == "ok" => c = c.fact("finance.db", format!("{}, {tables} tables, integrity ok ({ms} ms)", bytes(n))),
                Ok((bad, _)) => c = c.fact("finance.db", format!("{}, integrity: {bad}", bytes(n)))
                    .worse(Status::Fail, "finance.db is damaged: a sync replaces it."),
                Err(e) => c = c.fact("finance.db", format!("{}, can't open: {e:#}", bytes(n)))
                    .worse(Status::Fail, "finance.db can't be read: a sync replaces it."),
            }
        }
    }
    match bundle_size {
        Err(_) => c = c.fact("export.json", "missing").worse(Status::Fail, "export.json is missing."),
        Ok(n) => match bundle::Bundle::load(bundle_path) {
            Ok(b) => {
                let v = b.at(&["version"]).as_u64();
                c = c.fact("export.json", format!("{}, bundle v{}", bytes(n), v.map(|v| v.to_string()).unwrap_or("?".into())));
                if v != Some(bundle::SUPPORTED_VERSION) {
                    c = c.worse(Status::Fail, format!("export.json is bundle v{v:?}; this app reads v{}.", bundle::SUPPORTED_VERSION));
                }
            }
            Err(e) => c = c.fact("export.json", format!("{}, {e:#}", bytes(n))).worse(Status::Fail, "export.json doesn't parse: a sync replaces it."),
        },
    }
    if c.status == Status::Ok {
        c.summary = format!("Present and whole: {} in all.", bytes(db_size.unwrap_or(0) + bundle_size.unwrap_or(0)));
    } else {
        c = c.fix(Fix::Action(Action::SyncNow));
    }
    c
}

// ---------- data sources ----------

/// What the server has, from its capabilities (the last synced bundle):
/// which sources have data, which API source is set up but empty.
pub fn sources_check(caps: &crate::capabilities::Capabilities, now_ms: i64) -> Check {
    let r = caps.report(now_ms);
    let status = match r.level {
        0 => Status::Ok,
        1 | 3 => Status::Warn,
        _ => Status::Missing,
    };
    let mut c = Check::new(CheckId::Sources, status, r.summary);
    for (k, v) in r.rows {
        c = c.fact(k, v);
    }
    if let Some(s) = &caps.server {
        c = c.fact("server", s.clone());
    }
    if status == Status::Missing {
        c = c.fix(Fix::Action(Action::OpenUpload));
    }
    c
}

// ---------- the server's source report (GET /api/sources) ----------

/// An API source's last good fetch older than this: warn.
pub const FETCH_STALE_AFTER: u64 = 2 * 86_400;
/// A bank's newest row older than this: warn (a monthly export was missed).
pub const EXPORT_STALE_DAYS: i64 = 35;
/// Older than this, the account is taken as closed: shown, never warned about.
pub const EXPORT_DORMANT_DAYS: i64 = 365;

/// Where a source's key is set: the server tests it and stores it encrypted,
/// and the next fetch uses it (no restart).
pub const SET_KEY_HOWTO: &str = "Settings, Data sources, Edit on the source: paste the key, Save";

/// The server's source report. Blocking.
pub fn sources_probe(url: &str) -> Result<Sources, String> {
    let base = url.trim().trim_end_matches('/');
    let agent = tailnet::agent(Duration::from_secs(10));
    let mut req = agent.get(&format!("{base}/api/sources"));
    if let Some(b) = crate::auth::bearer(base) {
        req = req.header("Authorization", &b);
    }
    let mut r = req.call().map_err(|e| format!("{base}: {e}"))?;
    match r.status().as_u16() {
        200 => {}
        401 => return Err("the server wants this device paired before it shows its sources (Server row: Pair)".into()),
        404 => return Err("the server has no /api/sources yet (an older build): deploy it".into()),
        s => return Err(format!("/api/sources answered HTTP {s}")),
    }
    let body = r.body_mut().with_config().limit(1 << 20).read_to_vec().map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("/api/sources: {e}"))
}

fn age_ms(now_ms: i64, then: i64) -> u64 {
    ((now_ms - then).max(0) / 1000) as u64
}

fn days_between(today: &str, day: &str) -> Option<i64> {
    let p = |d: &str| chrono::NaiveDate::parse_from_str(d.get(..10)?, "%Y-%m-%d").ok();
    Some((p(today)? - p(day)?).num_days())
}

/// The shared part of an API source's row: configured, last fetch, last error.
fn api_check(id: CheckId, a: &ApiSource, now_ms: i64) -> Check {
    let mut c = Check::new(id, Status::Ok, "")
        .fact("keys", if a.configured { "configured on the server".to_string() } else { format!("not set: {}", a.env_keys.join(", ")) });
    if !a.configured {
        return c.worse(Status::Missing, format!("Not configured: the server has no {}.", a.env_keys.join(" / ")))
            .fact("set it", SET_KEY_HOWTO);
    }
    c = c.fact("last fetch", match a.last_ok_ms {
        Some(ms) => format!("{} ago", ago(age_ms(now_ms, ms))),
        None => "never".into(),
    });
    if let Some(e) = &a.last_error {
        c = c.fact("last error", e.clone());
    }
    match (a.last_ok_ms, &a.last_error) {
        (_, Some(e)) => c.worse(Status::Fail, format!("The last fetch failed: {e}")),
        (None, None) => c.worse(Status::Warn, "Configured, but never fetched: press Sync on the server, or wait for the daily sync."),
        (Some(ms), None) if age_ms(now_ms, ms) > FETCH_STALE_AFTER => {
            let age = ago(age_ms(now_ms, ms));
            c.worse(Status::Warn, format!("The last fetch was {age} ago: the daily sync isn't running."))
        }
        (Some(ms), None) => Check { summary: format!("Fetched {} ago.", ago(age_ms(now_ms, ms))), ..c },
    }
}

/// Where a new Flex token comes from, and the query's section settings.
pub const FLEX_TOKEN_HOWTO: &str = "IBKR Client Portal → Performance & Reports → Flex Queries → Flex Web Service \
    Configuration (the gear) → generate a new token (valid up to a year)";
pub const FLEX_DIVIDENDS_HOWTO: &str = "IBKR Client Portal → Performance & Reports → Flex Queries → edit the Activity \
    query (its ID is IBKR_FLEX_QUERY_ID) → Cash Transactions → tick Dividends, Payment In Lieu Of Dividends and \
    Withholding Tax → Save. The next sync reads them.";

/// IBKR: the API part, the Flex error in words, the snapshot, the sections,
/// and the pricing plan (`money` formats HKD, privacy mode included).
pub fn ibkr_check(src: &Result<Sources, String>, money: fn(f64) -> String) -> Check {
    let id = CheckId::Ibkr;
    let s = match src {
        Err(e) => return Check::new(id, Status::Warn, format!("Can't read the server's sources: {e}")),
        Ok(s) => s,
    };
    let Some(a) = s.api.iter().find(|a| a.key == "ibkr") else {
        return Check::new(id, Status::Warn, "The server didn't report IBKR.");
    };
    let mut c = api_check(id, a, s.generated_ms);
    let Some(x) = &s.ibkr else { return c };
    if let Some(fe) = &x.flex_error {
        c = c.fact("Flex error", format!("{} ({})", fe.words, fe.code));
        c = match fe.kind.as_str() {
            "token_expired" | "token_invalid" => {
                let token = a.env_keys.first().cloned().unwrap_or("IBKR_FLEX_TOKEN".into());
                Check { status: Status::Fail, summary: format!("{} Generate a new one and save it as the server's {token} \
                    (Settings, Data sources, Edit on IBKR).", fe.words), ..c }
                    .fact("new token", FLEX_TOKEN_HOWTO)
            }
            "query_invalid" => Check { status: Status::Fail, summary: format!("{} Fix the query ID \
                (IBKR_FLEX_QUERY_ID) in Settings, Data sources, Edit on IBKR.", fe.words), ..c },
            "transient" | "throttled" => Check { status: Status::Warn, summary: fe.words.clone(), ..c },
            _ => c,
        };
    }
    let today = chrono::DateTime::from_timestamp_millis(s.generated_ms).map(|d| d.date_naive().to_string()).unwrap_or_default();
    match &x.positions_as_of {
        Some(d) => {
            let days = days_between(&today, d).unwrap_or(0);
            c = c.fact("positions", format!("snapshot of {d} ({days} d old)"));
            if days > 4 {
                c = c.worse(Status::Warn, format!("The positions snapshot is {days} days old."));
            }
        }
        None => c = c.fact("positions", "no snapshot yet"),
    }
    if let Some(d) = &x.latest_trade {
        c = c.fact("latest trade", d.clone());
    }
    let sec = &x.sections;
    let yes = |b: bool| if b { "yes" } else { "no" };
    let opt = |b: Option<bool>| b.map(yes).unwrap_or("unknown until the next fetch");
    c = c.fact("Flex sections", format!("trades {}, open positions {}, cash report {}, NAV {}, cash transactions {}{}",
        yes(sec.trades), yes(sec.open_positions), yes(sec.cash_report), yes(sec.nav), yes(sec.cash_transactions),
        if sec.from == "stored" { " (from the stored data)" } else { "" }));
    c = c.fact("dividends, withholding", format!("{}, {}", opt(sec.dividends), opt(sec.withholding)));
    if sec.dividends == Some(false) || sec.withholding == Some(false) {
        c = c.worse(Status::Warn, "The Flex query has no Dividends or Withholding Tax rows, so income and tax withheld \
            can't be tracked: add them to its Cash Transactions section.")
            .fact("add them", FLEX_DIVIDENDS_HOWTO);
    }
    if let Some(p) = &x.plan {
        let saving = p.tiered_saving_yr_hkd;
        c = c.fact("pricing plan", format!("{} ({} of {} trades paid the Fixed fee to the cent)", p.plan, p.matched, p.priced))
            .fact("Tiered instead", if saving >= 0. {
                format!("about {} a year less, at the last year's buying (approximate)", money(saving))
            } else {
                format!("about {} a year more, at the last year's buying (approximate)", money(-saving))
            });
    }
    if c.status == Status::Ok {
        c.summary = format!("{} Positions and trades are current.", c.summary);
    }
    c
}

pub fn exchange_check(id: CheckId, key: &str, src: &Result<Sources, String>) -> Check {
    match src {
        Err(e) => Check::new(id, Status::Warn, format!("Can't read the server's sources: {e}")),
        Ok(s) => match s.api.iter().find(|a| a.key == key) {
            Some(a) => api_check(id, a, s.generated_ms),
            None => Check::new(id, Status::Warn, format!("The server didn't report {key}.")),
        },
    }
}

/// The Doctor rows of sources this user doesn't use, left out: an API
/// source (IBKR, Binance, OKX) with no keys, never fetched, and (IBKR) no
/// trades or positions; and the Bank exports row of a user who has API
/// sources in use and no file source at all. Settings, Data sources is
/// where an unused source is set up. A report that didn't come: none left
/// out; a user with nothing at all keeps the Bank exports row (it says
/// what to do first).
pub fn unused_api_checks(src: &Result<Sources, String>) -> Vec<CheckId> {
    let Ok(s) = src else { return Vec::new() };
    let in_use = |a: &ApiSource| {
        let broker_data = a.key == "ibkr" && s.ibkr.as_ref().is_some_and(|x| x.latest_trade.is_some() || x.positions_as_of.is_some());
        a.configured || a.last_ok_ms.is_some() || broker_data
    };
    let mut out: Vec<CheckId> = [(CheckId::Ibkr, "ibkr"), (CheckId::Binance, "binance"), (CheckId::Okx, "okx")]
        .into_iter()
        .filter(|(_, key)| s.api.iter().find(|a| a.key == *key).is_some_and(|a| !in_use(a)))
        .map(|(id, _)| id)
        .collect();
    if s.files.is_empty() && s.api.iter().any(in_use) {
        out.push(CheckId::BankExports);
    }
    out
}

/// Bank and wallet exports: the newest row per source. A bank quiet for
/// more than [`EXPORT_STALE_DAYS`] (and less than a year: a closed account
/// is just shown) wants a fresh export.
pub fn exports_check(src: &Result<Sources, String>) -> Check {
    let id = CheckId::BankExports;
    let s = match src {
        Err(e) => return Check::new(id, Status::Warn, format!("Can't read the server's sources: {e}")),
        Ok(s) => s,
    };
    let today = chrono::DateTime::from_timestamp_millis(s.generated_ms).map(|d| d.date_naive().to_string()).unwrap_or_default();
    let mut c = Check::new(id, Status::Ok, "");
    let mut stale = Vec::new();
    for f in &s.files {
        let days = f.newest.as_deref().and_then(|d| days_between(&today, d));
        let note = match days {
            None => "no rows".to_string(),
            Some(d) if f.kind == "bank" && d > EXPORT_DORMANT_DAYS => format!("{} ({d} d, dormant)", f.newest.as_deref().unwrap_or("")),
            Some(d) if f.kind == "bank" && d > EXPORT_STALE_DAYS => {
                stale.push(f.source.clone());
                format!("{} ({d} d old)", f.newest.as_deref().unwrap_or(""))
            }
            Some(d) => format!("{} ({d} d)", f.newest.as_deref().unwrap_or("")),
        };
        c = c.fact(&f.source, format!("{note}, {} rows{}", f.rows, if f.kind == "wallet" { ", wallet" } else { "" }));
    }
    if s.files.is_empty() {
        return c.worse(Status::Missing, "No bank exports yet: drop CSVs on the Upload page.").fix(Fix::Action(Action::OpenUpload));
    }
    if stale.is_empty() {
        c.summary = format!("Every active bank has rows from the last {EXPORT_STALE_DAYS} days.");
        c
    } else {
        c.worse(Status::Warn, format!("No rows for over {EXPORT_STALE_DAYS} days from {}: export a fresh CSV and upload it.",
            stale.join(", "))).fix(Fix::Action(Action::OpenUpload))
    }
}

// ---------- phone and web app ----------

/// Where the phone opens the app: the server's /app (the WASM build).
pub fn webapp_url(server: &str) -> String {
    format!("{}/app", server.trim().trim_end_matches('/'))
}

/// [`webapp_url`] when a phone could open it (the server isn't loopback).
pub fn phone_url(server: &str) -> Option<String> {
    url_host(server).filter(|h| !is_loopback(h)).map(|_| webapp_url(server))
}

pub fn phone_check(server: &str, probe: &Result<Probe, String>) -> Check {
    let url = webapp_url(server);
    let host = url_host(server).unwrap_or_default();
    let c = Check::new(CheckId::Phone, Status::Ok, "").fact("open", url.clone());
    if is_loopback(&host) {
        return c.worse(Status::Warn, "The server URL is this machine's own address, which a phone can't open. \
            Point the app at the server's tailnet URL.").fix(Fix::Action(Action::SetupServer));
    }
    let tail = if tailnet::is_tailnet_name(&host) { " The phone needs Tailscale on." } else { "" };
    match probe {
        Err(_) => c.worse(Status::Warn, format!("Scan to open the web app; the server doesn't answer right now.{tail}")),
        Ok(p) => match p.webapp {
            Some(true) => Check { summary: format!("Scan to open the web app on the phone.{tail}"), ..c },
            Some(false) => c.worse(Status::Missing, format!("The web app isn't on the server yet, so this link has nothing to show \
                (see Web app below).{tail}")),
            None => c.worse(Status::Warn, format!("The server answered /app oddly; the link may not work.{tail}")),
        },
    }
}

/// `missing`: what this machine lacks to build the web app ([`webapp::toolchain`]).
pub fn webapp_check(server: &str, probe: &Result<Probe, String>, missing: Option<&webapp::Missing>) -> Check {
    let mut c = Check::new(CheckId::WebApp, Status::Missing, "Install web app on the server: the app in a browser, for the phone.")
        .fact("path", webapp_url(server));
    c = match probe.as_ref().ok().and_then(|p| p.webapp) {
        Some(true) => Check { status: Status::Ok, summary: "Installed: the server serves the web app at /app.".into(), ..c },
        Some(false) => c.fact("server", "/app answers 404"),
        None => c.fact("server", "unknown (no answer)"),
    };
    // installed or not, the button (re)builds this checkout's pages for the server
    c = c.fix(Fix::Action(Action::Install(InstallKind::WebApp)));
    match missing {
        None => c.fact("build", "ready (wasm32 target and wasm-bindgen)"),
        Some(m) => {
            c = c.fact("build", m.why.clone());
            for cmd in &m.commands {
                c = c.fix(Fix::Command(cmd.clone()));
            }
            c
        }
    }
}

// ---------- GPU ----------

/// The integrated GPUs' Mesa Vulkan drivers, in the order main.rs prefers them.
pub const INTEGRATED_ICDS: [&str; 4] = ["radeon_icd.x86_64.json", "radeon_icd.json", "intel_icd.x86_64.json", "intel_icd.json"];
pub const ICD_DIRS: [&str; 2] = ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"];

#[derive(Clone, Debug, Default)]
pub struct GpuFacts {
    /// file names of the installed Vulkan drivers
    pub icds: Vec<String>,
    /// $VK_DRIVER_FILES (set by the app at launch, or by hand)
    pub driver_files: Option<String>,
    /// $FINANCE_DESKTOP_GPU=any
    pub any: bool,
    /// /proc/cpuinfo's vendor_id
    pub cpu_vendor: Option<String>,
}

pub fn gpu_facts() -> GpuFacts {
    let mut icds: Vec<String> = ICD_DIRS.iter().filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.filter_map(|e| e.ok()?.file_name().into_string().ok()))
        .filter(|n| n.ends_with(".json")).collect();
    icds.sort();
    icds.dedup();
    let cpu_vendor = std::fs::read_to_string("/proc/cpuinfo").ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("vendor_id")).and_then(|l| l.split(':').nth(1)).map(|v| v.trim().to_string()));
    GpuFacts {
        icds,
        driver_files: std::env::var("VK_DRIVER_FILES").ok().or(std::env::var("VK_ICD_FILENAMES").ok()),
        any: std::env::var("FINANCE_DESKTOP_GPU").is_ok_and(|v| v == "any"),
        cpu_vendor,
    }
}

/// How to install the integrated GPU's Mesa Vulkan driver with `pkg`: a
/// command to copy, or (no known package manager) a fact in words.
fn mesa_fix(c: Check, cpu_vendor: Option<&str>, pkg: Option<PkgManager>) -> Check {
    let intel = cpu_vendor == Some("GenuineIntel");
    match pkg.and_then(|m| tools::mesa_vulkan_package(m, intel).map(|p| m.install(p))) {
        Some(cmd) => c.fix(Fix::Command(cmd)),
        None => c.fact("install", format!("your distribution's Mesa Vulkan driver for {} graphics", if intel { "Intel" } else { "AMD Radeon" })),
    }
}

/// `pkg`: the package manager the install command uses ([`tools::Env::pkg`]).
pub fn gpu_check(f: &GpuFacts, pkg: Option<PkgManager>) -> Check {
    let id = CheckId::Gpu;
    let integrated = f.icds.iter().any(|n| INTEGRATED_ICDS.contains(&n.as_str()));
    let nvidia = f.icds.iter().any(|n| n.contains("nvidia"));
    let fix = |c: Check| mesa_fix(c, f.cpu_vendor.as_deref(), pkg);
    let mut c = Check::new(id, Status::Ok, "").fact("drivers", if f.icds.is_empty() { "none".into() } else { f.icds.join(", ") });
    let in_use = match &f.driver_files {
        Some(p) => Path::new(p.split(':').next().unwrap_or(p)).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(p.clone()),
        None => "every installed driver (Vulkan picks)".into(),
    };
    c = c.fact("in use", in_use.clone());
    if f.icds.is_empty() {
        return fix(c.worse(Status::Fail, "No Vulkan driver installed: the app can't draw."));
    }
    if f.any {
        c.summary = "Every driver is visible (FINANCE_DESKTOP_GPU=any): a hybrid laptop wakes its discrete GPU.".into();
        return c;
    }
    match (nvidia, integrated, &f.driver_files) {
        (true, false, _) => fix(c.worse(Status::Missing, "Only the NVIDIA driver is installed, so the app wakes the discrete GPU \
            (about 120 ms at launch, battery while open). Install the integrated GPU's Mesa driver.")),
        (_, true, Some(_)) => Check { summary: format!("Drawing on the integrated GPU only ({in_use})."), ..c },
        (_, true, None) => Check { summary: "The integrated GPU's driver is installed.".into(), ..c },
        _ => Check { summary: format!("Vulkan driver: {in_use}."), ..c },
    }
}

// ---------- desktop integration ----------

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopPaths {
    /// ~/.local/bin/anser, a symlink to the build
    pub bin: PathBuf,
    /// the launcher entry
    pub launcher: PathBuf,
    /// the icon in the user icon theme (re-colored with the accent)
    pub icon: PathBuf,
}

impl DesktopPaths {
    /// Where scripts/install.sh puts them (XDG_DATA_HOME honored).
    pub fn user() -> Self {
        let data = dirs::data_dir().unwrap_or_default();
        let home = dirs::home_dir().unwrap_or_default();
        DesktopPaths {
            bin: dirs::executable_dir().unwrap_or_else(|| home.join(".local/bin")).join("anser"),
            launcher: data.join("applications/anser.desktop"),
            icon: data.join("icons/hicolor/scalable/apps/anser.svg"),
        }
    }
}

/// `exe`: this binary; `icon_svg`: the icon in the current accent.
pub fn desktop_check(p: &DesktopPaths, exe: &Path, icon_svg: &str) -> Check {
    let id = CheckId::Desktop;
    let mut problems: Vec<&str> = Vec::new();
    let mut missing = 0;
    let mut c = Check::new(id, Status::Ok, "");

    let link = std::fs::read_link(&p.bin);
    let bin = match (&link, p.bin.exists()) {
        (Ok(target), true) => {
            let same = std::fs::canonicalize(target).ok().zip(std::fs::canonicalize(exe).ok()).is_some_and(|(a, b)| a == b);
            format!("→ {}{}", target.display(), if same { " (this build)" } else { " (another build)" })
        }
        (Ok(target), false) => {
            problems.push("the symlink is broken");
            format!("→ {} (missing)", target.display())
        }
        (Err(_), true) => "a file, not a symlink: a rebuild won't update it".into(),
        (Err(_), false) => {
            missing += 1;
            "missing".into()
        }
    };
    c = c.fact("command", format!("{} {bin}", p.bin.display()));

    let launcher = match std::fs::read_to_string(&p.launcher) {
        Err(_) => {
            missing += 1;
            "missing".to_string()
        }
        Ok(s) => match s.lines().find_map(|l| l.strip_prefix("Exec=")) {
            Some(exec) if Path::new(exec.split_whitespace().next().unwrap_or("")).exists() => "ok".into(),
            Some(exec) => {
                problems.push("the launcher runs a missing program");
                format!("runs {exec}, which is missing")
            }
            None => {
                problems.push("the launcher has no Exec line");
                "no Exec line".into()
            }
        },
    };
    c = c.fact("launcher", format!("{} ({launcher})", p.launcher.display()));

    let icon = match std::fs::read_to_string(&p.icon) {
        Err(_) => {
            missing += 1;
            "missing".to_string()
        }
        Ok(s) if s == icon_svg => "matches the accent".into(),
        Ok(_) => {
            problems.push("the icon is in another accent");
            "another accent".into()
        }
    };
    c = c.fact("icon", format!("{} ({icon})", p.icon.display()));

    if missing == 3 {
        return Check { status: Status::Missing, summary: "Not installed: no launcher entry, icon or command.".into(), ..c }
            .fix(Fix::Action(Action::Install(InstallKind::Desktop)));
    }
    if missing > 0 {
        problems.push("parts are missing");
    }
    if problems.is_empty() {
        c.summary = "Launcher, icon and command are in place.".into();
        return c;
    }
    let mut s = problems.join(", ");
    s[..1].make_ascii_uppercase();
    Check { status: Status::Warn, summary: format!("{s}: Repair puts it right."), ..c }.fix(Fix::Action(Action::Install(InstallKind::Desktop)))
}

// ---------- installers: the seam ----------

/// One click, run off the UI thread; `log` gets each step as it happens.
pub trait Installer: Send + Sync {
    fn kind(&self) -> InstallKind;
    /// None: can run now. Some(why): shown disabled with the reason.
    fn unavailable(&self) -> Option<String> {
        None
    }
    fn run(&self, log: &dyn Fn(String)) -> Result<()>;
}

/// scripts/install.sh, natively: the symlink, the icon and the launcher entry,
/// all per user. Nothing is built, so it links the binary that's running.
pub struct DesktopInstaller {
    pub paths: DesktopPaths,
    pub exe: PathBuf,
    pub icon_svg: String,
}

pub fn launcher_entry(bin: &Path) -> String {
    format!("[Desktop Entry]
Type=Application
Name=Anser
GenericName=Personal finance
Comment=Net worth, spending and investments -- local-first
Exec={}
Icon=anser
Terminal=false
Categories=Office;Finance;
Keywords=money;budget;net worth;spending;investments;
StartupWMClass=anser
", bin.display())
}

fn parent_dir(p: &Path) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    Ok(())
}

impl Installer for DesktopInstaller {
    fn kind(&self) -> InstallKind {
        InstallKind::Desktop
    }

    fn run(&self, log: &dyn Fn(String)) -> Result<()> {
        let p = &self.paths;
        log(format!("linking {} → {}", p.bin.display(), self.exe.display()));
        parent_dir(&p.bin)?;
        if std::fs::symlink_metadata(&p.bin).is_ok() {
            std::fs::remove_file(&p.bin).with_context(|| format!("replacing {}", p.bin.display()))?;
        }
        std::os::unix::fs::symlink(&self.exe, &p.bin).with_context(|| format!("linking {}", p.bin.display()))?;

        log(format!("icon in the current accent → {}", p.icon.display()));
        parent_dir(&p.icon)?;
        std::fs::write(&p.icon, &self.icon_svg).with_context(|| format!("writing {}", p.icon.display()))?;

        log(format!("launcher entry → {}", p.launcher.display()));
        parent_dir(&p.launcher)?;
        std::fs::write(&p.launcher, launcher_entry(&p.bin)).with_context(|| format!("writing {}", p.launcher.display()))?;

        // per-user caches only, like install.sh; absent tools are fine
        let refresh: [(&str, Vec<String>); 2] = [
            ("update-desktop-database", vec!["-q".into(), p.launcher.parent().unwrap_or(Path::new(".")).display().to_string()]),
            ("gtk-update-icon-cache", vec!["-q".into(), "-f".into(), "-t".into(),
                p.icon.ancestors().nth(3).unwrap_or(Path::new(".")).display().to_string()]),
        ];
        for (tool, args) in refresh {
            match std::process::Command::new(tool).args(&args).output() {
                Ok(o) if o.status.success() => log(format!("refreshed: {tool}")),
                Ok(_) => log(format!("{tool} complained; launchers pick the entry up anyway")),
                Err(_) => log(format!("{tool} not installed; skipped")),
            }
        }
        if !std::env::var("PATH").unwrap_or_default().split(':').any(|d| Path::new(d) == p.bin.parent().unwrap_or(Path::new(""))) {
            log(format!("note: {} is not on your PATH (the launcher works anyway)", p.bin.parent().unwrap_or(Path::new("")).display()));
        }
        log("done: launch Anser from your launcher".into());
        Ok(())
    }
}

/// The WASM build of this checkout, installed on `server` (served at /app
/// and /): [`webapp::build_and_install`], its build log streamed.
pub struct WebAppInstaller {
    pub server: String,
    /// what [`webapp::toolchain`] found missing: the button is disabled
    pub missing: Option<webapp::Missing>,
}

impl Installer for WebAppInstaller {
    fn kind(&self) -> InstallKind {
        InstallKind::WebApp
    }

    fn unavailable(&self) -> Option<String> {
        self.missing.as_ref().map(|m| format!("{}: run the commands below, then Run again", m.why))
    }

    fn run(&self, log: &dyn Fn(String)) -> Result<()> {
        log(format!("building the web app (desktop/scripts/build-web.sh), then installing it on {}", self.server));
        webapp::build_and_install(&self.server, log)?;
        log("done: open /app on the phone".into());
        Ok(())
    }
}

// ---------- a local server ----------

/// The local server's default bind (the app's default server).
pub const LOCAL_SERVER: &str = cache::DEFAULT_SERVER;

// ---------- theme, logos, config ----------

/// The compositor's blur, from `hyprctl getoption … -j`. None off Hyprland
/// ([`tools::Env::hyprland`]).
pub fn hypr_blur(env: &Env) -> Option<Result<(bool, i64), String>> {
    if !env.hyprland {
        return None;
    }
    let get = |opt: &str| -> Result<String, String> {
        let o = std::process::Command::new("hyprctl").args(["getoption", opt, "-j"]).output().map_err(|e| format!("hyprctl: {e}"))?;
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    };
    Some((|| {
        let enabled = parse_hypr_option(&get("decoration:blur:enabled")?).ok_or("can't read decoration:blur:enabled")?;
        let size = parse_hypr_option(&get("decoration:blur:size")?).unwrap_or(0);
        Ok((enabled != 0, size))
    })())
}

/// `{"option": …, "int": 8}` or `{"option": …, "bool": true}` -> the number.
pub fn parse_hypr_option(json: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("int").and_then(|x| x.as_i64()).or_else(|| v.get("bool").and_then(|x| x.as_bool()).map(i64::from))
}

pub const BLUR_HOWTO: &str = "hyprctl getoption decoration:blur:enabled";

/// `desktop`: Some((source name, path, parsed?)) when the accent follows the
/// desktop theme ("Omarchy" or "matugen").
/// `blur`: Hyprland's, None off Hyprland. Glass relies on the compositor's
/// blur, but only Hyprland's can be read: elsewhere it is noted, not warned
/// about (a warning nothing on the page can clear would just be noise).
pub fn theme_check(palette: &str, desktop: Option<(&str, &Path, Result<(), String>)>, glass: bool,
                   blur: Option<&Result<(bool, i64), String>>) -> Check {
    let mut c = Check::new(CheckId::Theme, Status::Ok, "").fact("palette", palette);
    let mut ok = vec![format!("The {palette} palette")];
    match desktop {
        None => {}
        Some((source, path, parsed)) => {
            c = c.fact(format!("{source} file"), path.display().to_string());
            match parsed {
                Ok(()) => ok.push(format!("{source} colors found")),
                Err(e) => c = c.worse(Status::Warn, format!("The accent follows {source}, but {}: {e}. The palette's own accent is used.", path.display())),
            }
        }
    }
    if glass {
        match blur {
            None => {
                c = c.fact("compositor blur", "not checked: the Doctor reads it on Hyprland only");
                ok.push("glass (it needs the compositor to blur behind the window)".into());
            }
            Some(Err(e)) => c = c.fact("compositor blur", e.clone()).worse(Status::Warn, format!("Can't read Hyprland's blur: {e}.")),
            Some(Ok((true, size))) if *size > 0 => {
                c = c.fact("compositor blur", format!("on, size {size}"));
                ok.push(format!("Hyprland blurs behind it (size {size})"));
            }
            Some(Ok((on, size))) => c = c.fact("compositor blur", format!("{}, size {size}", if *on { "on" } else { "off" }))
                .worse(Status::Warn, "Glass shows the desktop unblurred: turn on decoration:blur (enabled = true, size above 0) \
                    in your Hyprland config.")
                .fix(Fix::Command(BLUR_HOWTO.into())),
        }
    }
    if c.status == Status::Ok {
        c.summary = format!("{}.", ok.join(", "));
    }
    c
}

pub fn logos_check(count: usize, on: bool) -> Check {
    Check::new(CheckId::Logos, Status::Ok, format!("{count} logos bundled into the binary{}.", if on { "" } else { " (turned off in Settings)" }))
        .fact("shown", if on { "yes" } else { "no" })
}

/// A config file and what reading it found: Ok(false) = not created yet.
pub struct ConfigFile {
    pub name: &'static str,
    pub path: PathBuf,
    pub read: Result<bool, String>,
}

pub fn config_check(files: &[ConfigFile]) -> Check {
    let mut c = Check::new(CheckId::Config, Status::Ok, "");
    let mut bad = Vec::new();
    for f in files {
        let state = match &f.read {
            Ok(true) => "ok".to_string(),
            Ok(false) => "not created (defaults)".to_string(),
            Err(e) => {
                bad.push(f.name);
                format!("error: {e}")
            }
        };
        c = c.fact(f.name, format!("{} ({state})", f.path.display()));
    }
    if bad.is_empty() {
        c.summary = "Every file parses (or isn't needed yet).".into();
        c
    } else {
        c.worse(Status::Fail, format!("{} doesn't parse: the app falls back to defaults. Fix or delete it.", bad.join(", ")))
    }
}

/// Read a config file with `parse` (the app's own TOML types): Ok(false)
/// when it doesn't exist yet, the parse error otherwise.
pub fn read_with(path: &Path, parse: impl FnOnce(&str) -> Result<(), String>) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
        Ok(s) => parse(&s).map(|_| true),
    }
}

/// The server file: a URL on its first line.
pub fn read_server_file(path: &Path) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Err(_) => Ok(false),
        Ok(s) => {
            let line = s.lines().next().unwrap_or("").trim();
            if line.starts_with("http://") || line.starts_with("https://") {
                line.parse::<ureq::http::Uri>().map(|_| true).map_err(|e| format!("{line:?}: {e}"))
            } else {
                Err(format!("first line {line:?} is not an http(s) URL"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_ok(ms: u64) -> Result<Probe, String> {
        Ok(Probe {
            rtt: Duration::from_millis(ms),
            server_header: Some("tiny-http (Rust)".into()),
            info: Some(ServerInfo { kind: "anser-server".into(), version: "0.1.0".into(), bundle_version: Some(1) }),
            webapp: Some(false),
        })
    }

    /// 2027-01-15 08:00 UTC
    const NOW: i64 = 1_800_000_000_000;

    fn fresh() -> LocalBundle {
        LocalBundle { version: Some(1), generated_ms: Some(NOW - 600_000), age_secs: Some(600) }
    }

    #[test]
    fn server_healthy_and_fresh() {
        let c = server_check("https://server.x.ts.net:8787", UrlSource::File, &probe_ok(80), Some(&fresh()), NOW);
        assert_eq!(c.status, Status::Ok, "{}", c.summary);
        assert!(c.summary.contains("anser-server 0.1.0") && c.summary.contains("80 ms"));
        assert!(c.facts.iter().any(|(k, v)| k == "last sync" && v.starts_with("10 min")));
        assert!(c.fixes.is_empty());
    }

    #[test]
    fn server_stale_never_synced_and_mismatched() {
        let old = LocalBundle { age_secs: Some(3 * 86_400), ..fresh() };
        let c = server_check("u", UrlSource::File, &probe_ok(80), Some(&old), NOW);
        assert_eq!(c.status, Status::Warn);
        assert_eq!(c.fixes, vec![Fix::Action(Action::SyncNow)]);
        let c = server_check("u", UrlSource::File, &probe_ok(80), None, NOW);
        assert_eq!((c.status, c.fixes.clone()), (Status::Warn, vec![Fix::Action(Action::SyncNow)]));
        let mut p = probe_ok(80);
        p.as_mut().unwrap().info.as_mut().unwrap().bundle_version = Some(2);
        let c = server_check("u", UrlSource::File, &p, Some(&old), NOW);
        assert_eq!(c.status, Status::Fail, "a version mismatch beats staleness");
        assert!(c.summary.contains("v2"));
    }

    #[test]
    fn server_unreachable_or_unset() {
        let down: Result<Probe, String> = Err("host not found".into());
        let c = server_check("https://server.x.ts.net", UrlSource::File, &down, None, NOW);
        assert_eq!(c.status, Status::Fail);
        assert_eq!(c.fixes, vec![Fix::Action(Action::SetupServer), Fix::Action(Action::StartLocalServer)]);
        let c = server_check(cache::DEFAULT_SERVER, UrlSource::Default, &down, None, NOW);
        assert_eq!((c.status, c.fixes.len()), (Status::Missing, 2));
    }

    #[test]
    fn server_kind_from_header_when_no_version_route() {
        let mut p = probe_ok(5).unwrap();
        p.info = None;
        assert!(server_kind(&p).starts_with("anser-server"));
        p.server_header = Some("nginx/1.27".into());
        assert_eq!(server_kind(&p), "unknown (nginx/1.27)");
    }

    #[test]
    fn tailnet_verdicts() {
        let host = "server.tail1.ts.net";
        let public: Result<Vec<IpAddr>, String> = Ok(vec!["2403:2500:400:20::e8e".parse().unwrap()]);
        let magic: Result<Vec<Ipv4Addr>, String> = Ok(vec![Ipv4Addr::new(100, 90, 1, 2)]);
        let c = tailnet_check(host, &public, Some(&magic), Some("NordVPN"));
        assert_eq!(c.status, Status::Warn);
        assert!(c.summary.starts_with("NordVPN owns the system DNS"));
        assert_eq!(c.fixes, vec![Fix::Command(DNS_FIX.into())]);

        let good: Result<Vec<IpAddr>, String> = Ok(vec!["100.90.1.2".parse().unwrap()]);
        assert_eq!(tailnet_check(host, &good, None, None).status, Status::Ok);

        let dead: Result<Vec<Ipv4Addr>, String> = Err("timeout".into());
        assert_eq!(tailnet_check(host, &public, Some(&dead), None).status, Status::Fail);
        assert_eq!(tailnet_check("127.0.0.1", &Ok(vec![]), None, None).status, Status::Ok);
    }

    fn found(vpn: Vpn, active: bool, iface: &str) -> tools::VpnFound {
        let signals = if active { vec![format!("interface {iface}")] } else { vec!["on PATH".into()] };
        tools::VpnFound { vpn, installed: true, active, signals }
    }

    fn env_with(vpns: Vec<tools::VpnFound>) -> Env {
        Env { vpns, pkg: Some(PkgManager::Pacman), ..Env::bare(Os::Linux) }
    }

    #[test]
    fn tailnet_with_other_vpns() {
        let host = "server.tail1.ts.net";
        let public: Result<Vec<IpAddr>, String> = Ok(vec!["2403:2500:400:20::e8e".parse().unwrap()]);
        let magic: Result<Vec<Ipv4Addr>, String> = Ok(vec![Ipv4Addr::new(100, 90, 1, 2)]);
        let ts = found(Vpn::Tailscale, true, "tailscale0");

        // NordVPN up: its wording and the resolvectl command
        let env = env_with(vec![ts.clone(), found(Vpn::NordVpn, true, "nordlynx")]);
        let c = tailnet_check_env(host, &public, Some(&magic), &env);
        assert_eq!(c.status, Status::Warn);
        assert!(c.summary.starts_with("NordVPN owns the system DNS"), "{}", c.summary);
        assert_eq!(c.fixes, vec![Fix::Command(DNS_FIX.into())]);
        assert!(c.facts.iter().any(|(k, v)| k == "other VPNs" && v.contains("nordlynx")));

        // Twingate up: its own hint
        let env = env_with(vec![ts.clone(), found(Vpn::Twingate, true, "sdwan0")]);
        let c = tailnet_check_env(host, &public, Some(&magic), &env);
        assert!(c.summary.starts_with("Twingate resolves every name"), "{}", c.summary);
        assert!(c.facts.iter().any(|(k, v)| k == "also" && v.contains("Twingate")));

        // no other VPN: the plain wording, no VPN facts
        let env = env_with(vec![ts.clone()]);
        let c = tailnet_check_env(host, &public, Some(&magic), &env);
        assert!(c.summary.starts_with("The system DNS doesn't give"), "{}", c.summary);
        assert!(!c.facts.iter().any(|(k, _)| k == "other VPNs" || k == "also"));

        // installed but not up: listed, but it isn't blamed
        let env = env_with(vec![ts.clone(), found(Vpn::Mullvad, false, "")]);
        let c = tailnet_check_env(host, &public, Some(&magic), &env);
        assert!(c.summary.starts_with("The system DNS doesn't give"));
        assert!(c.facts.iter().any(|(k, v)| k == "other VPNs" && v == "Mullvad (installed, not up)"));

        // resolves fine: ok, whatever VPN is up
        let good: Result<Vec<IpAddr>, String> = Ok(vec!["100.90.1.2".parse().unwrap()]);
        let env = env_with(vec![ts, found(Vpn::NordVpn, true, "nordlynx")]);
        assert_eq!(tailnet_check_env(host, &good, None, &env).status, Status::Ok);

        // macOS: no resolvectl to offer
        let env = Env { os: Os::MacOs, ..env_with(vec![found(Vpn::NordVpn, true, "utun4")]) };
        let c = tailnet_check_env(host, &public, Some(&magic), &env);
        assert!(c.fixes.is_empty() && c.status == Status::Warn);
    }

    #[test]
    fn which_checks_apply() {
        let tailnet_url = "https://server.tail1.ts.net:8787";
        let local = "http://127.0.0.1:8787";
        let lan = "http://192.168.1.20:8787";
        let bare = Env::bare(Os::Linux);
        let with_ts = env_with(vec![found(Vpn::Tailscale, true, "tailscale0")]);

        // Tailnet: a tailnet server, or Tailscale installed
        assert!(CheckId::Tailnet.applies(&bare, tailnet_url));
        assert!(!CheckId::Tailnet.applies(&bare, lan), "no Tailscale, no tailnet server: hidden");
        assert!(CheckId::Tailnet.applies(&with_ts, lan));

        // Phone: never for loopback
        assert!(!CheckId::Phone.applies(&bare, local));
        assert!(!CheckId::Phone.applies(&bare, "http://localhost:8787"));
        assert!(CheckId::Phone.applies(&bare, lan) && CheckId::Phone.applies(&bare, tailnet_url));

        // GPU: Linux only
        assert!(CheckId::Gpu.applies(&bare, local));
        assert!(!CheckId::Gpu.applies(&Env::bare(Os::MacOs), local));

        // the rest always, in ALL's order
        let mac_local = CheckId::applicable(&Env::bare(Os::MacOs), local);
        assert_eq!(mac_local, vec![CheckId::Server, CheckId::Replica, CheckId::Sources, CheckId::Ibkr, CheckId::Binance, CheckId::Okx,
            CheckId::BankExports, CheckId::WebApp, CheckId::Desktop, CheckId::Theme, CheckId::Logos, CheckId::Config]);
        assert_eq!(CheckId::applicable(&with_ts, tailnet_url).len(), CheckId::ALL.len());
    }

    #[test]
    fn hyprland_gates_the_blur() {
        // off Hyprland nothing is run and glass is noted, not warned about
        let off = Env::bare(Os::Linux);
        assert!(hypr_blur(&off).is_none());
        let c = theme_check("glass", None, true, hypr_blur(&off).as_ref());
        assert_eq!(c.status, Status::Ok);
        // under Hyprland the blur is judged
        let off_blur = Ok((false, 0));
        assert_eq!(theme_check("glass", None, true, Some(&off_blur)).status, Status::Warn);
    }

    fn sources() -> Sources {
        use crate::sources::{FileSource, Ibkr, Sections};
        let api = |key: &str, ok: Option<i64>, err: Option<&str>| ApiSource {
            key: key.into(), label: key.into(), configured: true, env_keys: vec![format!("{}_KEY", key.to_uppercase())],
            last_ok_ms: ok, last_attempt_ms: ok, last_error: err.map(String::from), ..Default::default()
        };
        Sources {
            generated_ms: NOW,
            api: vec![api("ibkr", Some(NOW - 3_600_000), None), api("binance", Some(NOW - 3 * 86_400_000), None),
                      api("okx", Some(NOW - 600_000), Some("okx: HTTP 401"))],
            files: vec![
                FileSource { source: "hsbc".into(), kind: "bank".into(), newest: Some("2027-01-10".into()), rows: 10 },
                FileSource { source: "n26".into(), kind: "bank".into(), newest: Some("2026-11-01".into()), rows: 5 },
                FileSource { source: "revolut_old".into(), kind: "bank".into(), newest: Some("2021-06-22".into()), rows: 5 },
                FileSource { source: "trezor".into(), kind: "wallet".into(), newest: Some("2026-02-05".into()), rows: 8 },
            ],
            ibkr: Some(Ibkr {
                latest_trade: Some("2027-01-14".into()),
                positions_as_of: Some("2027-01-14".into()),
                sections: Sections { from: "statement".into(), trades: true, open_positions: true, cash_report: true, nav: true,
                    cash_transactions: true, dividends: Some(true), withholding: Some(true) },
                flex_error: None,
                plan: Some(crate::fees::PlanGuess { plan: "Fixed".into(), priced: 20, matched: 20, tiered_saving_yr_hkd: 120. }),
            }),
        }
    }

    fn money(v: f64) -> String {
        format!("HK$ {v:.0}")
    }

    #[test]
    fn source_verdicts() {
        // NOW is 2027-01-15
        let s = sources();
        let c = ibkr_check(&Ok(s.clone()), money);
        assert_eq!(c.status, Status::Ok, "{}: {:?}", c.summary, c.facts);
        assert!(c.facts.iter().any(|(k, v)| k == "Tiered instead" && v.contains("HK$ 120") && v.contains("less")));
        assert_eq!(exchange_check(CheckId::Binance, "binance", &Ok(s.clone())).status, Status::Warn, "3 days since the last fetch");
        let okx = exchange_check(CheckId::Okx, "okx", &Ok(s.clone()));
        assert_eq!(okx.status, Status::Fail);
        assert!(okx.summary.contains("HTTP 401"));

        let e = exports_check(&Ok(s.clone()));
        assert_eq!((e.status, e.fixes.clone()), (Status::Warn, vec![Fix::Action(Action::OpenUpload)]));
        assert!(e.summary.contains("n26") && !e.summary.contains("hsbc") && !e.summary.contains("revolut_old"), "{}", e.summary);

        // the token expired: fail, with the how-to, nothing to run
        let mut x = s.clone();
        x.api[0].last_error = Some("ibkr: Flex SendRequest: error 1012: Token has expired.".into());
        x.ibkr.as_mut().unwrap().flex_error = Some(crate::sources::FlexError { code: 1012, kind: "token_expired".into(),
            words: "The Flex token has expired.".into() });
        let c = ibkr_check(&Ok(x), money);
        assert_eq!(c.status, Status::Fail);
        assert!(c.summary.contains("IBKR_KEY") && c.facts.iter().any(|(k, _)| k == "new token"));
        assert!(c.fixes.is_empty() && c.summary.contains("Settings, Data sources"));

        // no dividends in the query: warn, with the steps
        let mut x = s.clone();
        x.ibkr.as_mut().unwrap().sections.dividends = Some(false);
        let c = ibkr_check(&Ok(x), money);
        assert_eq!(c.status, Status::Warn);
        assert!(c.facts.iter().any(|(k, v)| k == "add them" && v.contains("Withholding Tax")));

        // not configured; an old server
        let mut x = s.clone();
        x.api[1].configured = false;
        assert_eq!(exchange_check(CheckId::Binance, "binance", &Ok(x)).status, Status::Missing);
        assert_eq!(ibkr_check(&Err("no /api/sources".into()), money).status, Status::Warn);
    }

    #[test]
    fn unused_api_sources_leave_the_page() {
        let s = sources();
        assert!(unused_api_checks(&Ok(s.clone())).is_empty(), "all three configured");
        assert!(unused_api_checks(&Err("no /api/sources".into())).is_empty(), "no report: every row stays");
        // never set up: no keys, never fetched, no broker data
        let mut x = s.clone();
        for a in &mut x.api {
            a.configured = false;
            a.last_ok_ms = None;
        }
        x.ibkr = None;
        assert_eq!(unused_api_checks(&Ok(x.clone())), vec![CheckId::Ibkr, CheckId::Binance, CheckId::Okx]);
        // keys removed, but it fetched before (or IBKR still has positions): shown
        x.api[1].last_ok_ms = Some(NOW);
        x.ibkr = s.ibkr.clone();
        assert_eq!(unused_api_checks(&Ok(x)), vec![CheckId::Okx]);
    }

    #[test]
    fn the_bank_exports_row_needs_a_file_source_or_nothing_else() {
        let s = sources();
        // API sources only: no bank export to be missing
        let mut x = s.clone();
        x.files.clear();
        assert_eq!(unused_api_checks(&Ok(x.clone())), vec![CheckId::BankExports]);
        // nothing at all (no keys, no data, no files): the row stays, it says where to start
        for a in &mut x.api {
            a.configured = false;
            a.last_ok_ms = None;
        }
        x.ibkr = None;
        assert_eq!(unused_api_checks(&Ok(x.clone())), vec![CheckId::Ibkr, CheckId::Binance, CheckId::Okx]);
        // a file source with data keeps it, whatever the API sources do
        let mut y = s.clone();
        y.api.iter_mut().for_each(|a| a.configured = false);
        assert!(!unused_api_checks(&Ok(y)).contains(&CheckId::BankExports));
    }

    #[test]
    fn phone_and_webapp() {
        let p = probe_ok(5);
        let c = phone_check("https://server.x.ts.net:8787/", &p);
        assert_eq!(c.status, Status::Missing);
        assert_eq!(c.facts[0].1, "https://server.x.ts.net:8787/app");
        assert!(c.summary.contains("Tailscale on"));
        assert_eq!(webapp_check("https://server.x.ts.net:8787", &p, None).fixes, vec![Fix::Action(Action::Install(InstallKind::WebApp))]);
        let missing = webapp::Missing { why: "needs the wasm32 target".into(), commands: vec!["rustup target add wasm32-unknown-unknown".into()] };
        let c = webapp_check("https://server.x.ts.net:8787", &p, Some(&missing));
        assert_eq!(c.fixes[1], Fix::Command("rustup target add wasm32-unknown-unknown".into()));
        let inst = WebAppInstaller { server: "https://server.x.ts.net:8787".into(), missing: Some(missing) };
        assert!(inst.unavailable().is_some_and(|w| w.contains("wasm32")));
        assert!(WebAppInstaller { server: String::new(), missing: None }.unavailable().is_none());
        let mut up = p.clone().unwrap();
        up.webapp = Some(true);
        assert_eq!(phone_check("https://server.x.ts.net:8787", &Ok(up.clone())).status, Status::Ok);
        assert_eq!(webapp_check("https://server.x.ts.net:8787", &Ok(up), None).status, Status::Ok);
        assert_eq!(phone_check("http://127.0.0.1:8787", &p).status, Status::Warn, "a phone can't open loopback");
    }

    #[test]
    fn gpu_verdicts() {
        let f = |icds: &[&str], files: Option<&str>| GpuFacts {
            icds: icds.iter().map(|s| s.to_string()).collect(),
            driver_files: files.map(String::from),
            any: false,
            cpu_vendor: Some("AuthenticAMD".into()),
        };
        let pac = Some(PkgManager::Pacman);
        let c = gpu_check(&f(&["nvidia_icd.json", "radeon_icd.json"], Some("/usr/share/vulkan/icd.d/radeon_icd.json")), pac);
        assert_eq!(c.status, Status::Ok);
        assert!(c.summary.contains("radeon_icd.json"));
        let c = gpu_check(&f(&["nvidia_icd.json"], None), pac);
        assert_eq!((c.status, c.fixes.clone()), (Status::Missing, vec![Fix::Command("sudo pacman -S vulkan-radeon".into())]));
        let mut intel = f(&["nvidia_icd.json"], None);
        intel.cpu_vendor = Some("GenuineIntel".into());
        assert_eq!(gpu_check(&intel, pac).fixes, vec![Fix::Command("sudo pacman -S vulkan-intel".into())]);
        assert_eq!(gpu_check(&f(&[], None), pac).status, Status::Fail);
        // the detected package manager words the command
        assert_eq!(gpu_check(&intel, Some(PkgManager::Apt)).fixes, vec![Fix::Command("sudo apt install mesa-vulkan-drivers".into())]);
        assert_eq!(gpu_check(&f(&[], None), Some(PkgManager::Dnf)).fixes,
            vec![Fix::Command("sudo dnf install mesa-vulkan-drivers".into())]);
        // none known: a hint in words, no command
        let c = gpu_check(&f(&["nvidia_icd.json"], None), None);
        assert!(c.fixes.is_empty() && c.facts.iter().any(|(k, v)| k == "install" && v.contains("Radeon")));
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("finance-doctor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn desktop_paths_are_anser() {
        // the command install.sh links: Doctor looked for the pre-rename name
        let p = DesktopPaths::user();
        assert_eq!(p.bin.file_name().unwrap(), "anser");
        assert_eq!(p.launcher.file_name().unwrap(), "anser.desktop");
    }

    #[test]
    fn desktop_install_then_check() {
        let d = scratch("desktop");
        let paths = DesktopPaths { bin: d.join("bin/anser"), launcher: d.join("apps/anser.desktop"),
            icon: d.join("icons/hicolor/scalable/apps/anser.svg") };
        let exe = d.join("build/anser");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        let c = desktop_check(&paths, &exe, "<svg/>");
        assert_eq!((c.status, c.fixes.clone()), (Status::Missing, vec![Fix::Action(Action::Install(InstallKind::Desktop))]));

        let lines = std::sync::Mutex::new(Vec::new());
        DesktopInstaller { paths: paths.clone(), exe: exe.clone(), icon_svg: "<svg/>".into() }
            .run(&|l| lines.lock().unwrap().push(l)).unwrap();
        assert!(lines.lock().unwrap().last().unwrap().starts_with("done"));
        let c = desktop_check(&paths, &exe, "<svg/>");
        assert_eq!(c.status, Status::Ok, "{}: {:?}", c.summary, c.facts);
        assert!(c.facts[0].1.contains("(this build)"));
        assert!(std::fs::read_to_string(&paths.launcher).unwrap().contains(&format!("Exec={}", paths.bin.display())));

        // another accent: repairable; the installer runs twice cleanly
        let c = desktop_check(&paths, &exe, "<svg fill='red'/>");
        assert_eq!(c.status, Status::Warn);
        assert!(c.summary.contains("another accent"));
        DesktopInstaller { paths: paths.clone(), exe: exe.clone(), icon_svg: "<svg fill='red'/>".into() }.run(&|_| {}).unwrap();
        assert_eq!(desktop_check(&paths, &exe, "<svg fill='red'/>").status, Status::Ok);

        // a broken symlink
        std::fs::remove_file(&exe).unwrap();
        assert_eq!(desktop_check(&paths, &exe, "<svg fill='red'/>").status, Status::Warn);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn replica_states() {
        let d = scratch("replica");
        let (db, b) = (d.join("finance.db"), d.join("export.json"));
        let c = replica(&db, &b);
        assert_eq!((c.status, c.fixes.clone()), (Status::Missing, vec![Fix::Action(Action::SyncNow)]));
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);").unwrap();
        std::fs::write(&b, r#"{"version": 1}"#).unwrap();
        let c = replica(&db, &b);
        assert_eq!(c.status, Status::Ok, "{}: {:?}", c.summary, c.facts);
        assert!(c.facts.iter().any(|(k, v)| k == "finance.db" && v.contains("integrity ok")));
        std::fs::write(&db, b"not a database at all, just bytes that go on for a while").unwrap();
        std::fs::write(&b, "{").unwrap();
        let c = replica(&db, &b);
        assert_eq!((c.status, c.fixes.clone()), (Status::Fail, vec![Fix::Action(Action::SyncNow)]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn theme_and_hyprland() {
        assert_eq!(parse_hypr_option(r#"{"option": "decoration:blur:enabled", "bool": true, "set": true }"#), Some(1));
        assert_eq!(parse_hypr_option(r#"{"option": "decoration:blur:size", "int": 8, "set": true }"#), Some(8));
        assert_eq!(parse_hypr_option("no such option"), None);
        assert_eq!(theme_check("warm", None, false, None).status, Status::Ok);
        let on = Ok((true, 8));
        assert_eq!(theme_check("glass", None, true, Some(&on)).status, Status::Ok);
        let off = Ok((false, 8));
        let c = theme_check("glass", None, true, Some(&off));
        assert_eq!((c.status, c.fixes.len()), (Status::Warn, 1));
        // off Hyprland the blur can't be read: noted, not warned about
        let c = theme_check("glass", None, true, None);
        assert_eq!(c.status, Status::Ok);
        assert!(c.facts.iter().any(|(k, v)| k == "compositor blur" && v.contains("Hyprland only")));
        assert!(hypr_blur(&Env::bare(Os::Linux)).is_none(), "never runs hyprctl off Hyprland");
        let c = theme_check("warm", Some(("Omarchy", Path::new("/nope/colors.toml"), Err("not found".into()))), false, None);
        assert_eq!(c.status, Status::Warn);
    }

    #[test]
    fn config_files() {
        let d = scratch("config");
        let server = d.join("server");
        assert_eq!(read_server_file(&server), Ok(false));
        std::fs::write(&server, "https://server.x.ts.net:8787\n").unwrap();
        assert_eq!(read_server_file(&server), Ok(true));
        std::fs::write(&server, "server.x.ts.net\n").unwrap();
        assert!(read_server_file(&server).is_err());
        let c = config_check(&[
            ConfigFile { name: "server", path: server.clone(), read: read_server_file(&server) },
            ConfigFile { name: "budgets", path: d.join("budgets.toml"), read: Ok(false) },
        ]);
        assert_eq!(c.status, Status::Fail);
        assert!(c.summary.starts_with("server doesn't parse"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn small_helpers() {
        assert_eq!(ago(42), "42 s");
        assert_eq!(ago(3 * 3600 + 5), "3 h");
        assert_eq!(url_host("https://Server.x.ts.net:8787/a").as_deref(), Some("Server.x.ts.net"));
        assert_eq!(url_host("http://[::1]:8787").as_deref(), Some("::1"));
        assert!(is_loopback("127.0.0.1") && is_loopback("localhost") && !is_loopback("server.x.ts.net"));
        assert_eq!(webapp_url("https://a.b/"), "https://a.b/app");
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use crate::client::{DeviceInfo, UserInfo, WhoAmI};

    #[test]
    fn server_row_says_paired_or_not_and_nudges_to_lock_reads() {
        let base = || Check::new(CheckId::Server, Status::Ok, "answers");
        let unpaired = WhoAmI { auth_mode: "writes".into(), ..Default::default() };
        let c = with_pairing(base(), Some(&Ok(unpaired)));
        assert_eq!(c.status, Status::Warn);
        assert!(c.facts.contains(&("device".into(), "not paired".into())));
        assert!(c.fixes.contains(&Fix::Action(Action::Pair)));

        let admin = WhoAmI {
            paired: true, auth_mode: "writes".into(),
            device: Some(DeviceInfo { id: 1, name: "laptop".into() }),
            user: Some(UserInfo { id: 1, name: "o".into(), role: "admin".into(), data: true }), ..Default::default()
        };
        let c = with_pairing(base(), Some(&Ok(admin.clone())));
        assert!(c.facts.contains(&("device".into(), "paired as laptop".into())));
        assert!(c.summary.contains("FINANCE_AUTH=all") && c.fixes.contains(&Fix::Action(Action::LockReads)));
        let locked = WhoAmI { auth_mode: "all".into(), ..admin };
        let c = with_pairing(base(), Some(&Ok(locked)));
        assert_eq!((c.status, c.fixes.len()), (Status::Ok, 0));
        assert_eq!(with_pairing(base(), None).facts.len(), 0);
    }
}
