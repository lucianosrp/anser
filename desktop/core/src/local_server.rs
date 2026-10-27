//! A local install's server: when the server URL is on this machine, the app
//! starts `anser-server serve` itself, as a child it owns, and stops that child
//! when it quits. A server it did not start (a systemd unit, one run by hand)
//! is reused and never touched, and a remote server never concerns this module.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::sync;

/// How long a stopping child gets after SIGTERM before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// How long a started server gets to answer /healthz.
const START_WAIT: Duration = Duration::from_secs(10);

/// "host:port" of an http(s) URL (no path, no credentials).
pub fn host_port(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next()?;
    let auth = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    (!auth.is_empty()).then(|| auth.to_string())
}

/// The URL points at this machine: 127.x.y.z, localhost or ::1.
pub fn is_loopback(url: &str) -> bool {
    let Some(auth) = host_port(url) else { return false };
    let host = match auth.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => auth.rsplit_once(':').map_or(auth.as_str(), |(h, _)| h),
    };
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `anser-server`: beside the app binary, else on `path`, else in `home`'s
/// `.local/bin` (where the deploy installs it, even when $PATH lacks it).
pub fn pick_bin(exe: &Path, path: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    let beside = exe.parent().map(|d| d.join("anser-server"));
    let on_path = path.into_iter().flat_map(|p| std::env::split_paths(p)).map(|d| d.join("anser-server"));
    let local = home.map(|h| h.join(".local/bin/anser-server"));
    beside.into_iter().chain(on_path).chain(local).find(|p| p.is_file())
}

/// [`pick_bin`] for this process.
pub fn local_server_bin(exe: &Path) -> Option<PathBuf> {
    pick_bin(exe, std::env::var_os("PATH").as_deref(), dirs::home_dir().as_deref())
}

/// The documented install's data tree. Set explicitly: anser-server built from a
/// checkout would otherwise fall back to that checkout's own data.
pub fn default_home() -> PathBuf {
    dirs::data_dir().unwrap_or_default().join("finance")
}

/// What the child runs with, on top of the app's own environment.
pub fn child_env(url: &str, home: &Path) -> Vec<(&'static str, String)> {
    let mut env = vec![("FINANCE_HOME", home.display().to_string())];
    env.extend(host_port(url).map(|b| ("FINANCE_BIND", b)));
    env
}

/// A server process this app started.
pub struct LocalServer {
    child: Child,
}

impl LocalServer {
    fn spawn(mut cmd: Command, log: &Path) -> Result<Self> {
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let out = std::fs::File::create(log).with_context(|| format!("creating {}", log.display()))?;
        let child = cmd.stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out).spawn()
            .with_context(|| format!("starting {:?}", cmd.get_program()))?;
        Ok(LocalServer { child })
    }

    pub fn start(bin: &Path, url: &str, log: &Path) -> Result<Self> {
        let mut cmd = Command::new(bin);
        cmd.arg("serve").envs(child_env(url, &default_home()));
        Self::spawn(cmd, log)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    /// SIGTERM (the server finishes what it is doing), then a kill after the grace.
    pub fn stop(&mut self) {
        if self.exited() {
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let until = Instant::now() + STOP_GRACE;
        while Instant::now() < until {
            if self.exited() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The child this app started, if any (stopped by [`stop_owned`] on quit).
static OWNED: Mutex<Option<LocalServer>> = Mutex::new(None);

/// Stop the server this app started; a no-op when it started none.
pub fn stop_owned() {
    let owned = OWNED.lock().unwrap_or_else(|e| e.into_inner()).take();
    drop(owned);
}

#[derive(Debug, PartialEq)]
pub enum Ensured {
    /// not a local URL: nothing to do
    Remote,
    /// something already answers (a service, a hand-run server, an earlier child)
    Reused,
    /// started here, owned by the app
    Started(u32),
}

pub const NOT_INSTALLED: &str = "anser-server isn't installed on this machine. Install it with \
    curl -fsSL https://raw.githubusercontent.com/lucianosrp/anser/main/install.sh | sh -s -- --server, \
    or build it with cargo build -p finance-server --release and copy it to ~/.local/bin, then connect again.";

/// Make a loopback `url` answer: reuse what is there, else start anser-server
/// as an owned child. Blocking (up to ~10 s): call it off the UI thread.
pub fn ensure(url: &str, exe: &Path, log: &Path) -> Result<Ensured, String> {
    if !is_loopback(url) {
        return Ok(Ensured::Remote);
    }
    if sync::healthz(url).is_ok() {
        return Ok(Ensured::Reused);
    }
    let bin = local_server_bin(exe).ok_or(NOT_INSTALLED)?;
    let mut server = LocalServer::start(&bin, url, log).map_err(|e| format!("{e:#}"))?;
    let pid = server.pid();
    let until = Instant::now() + START_WAIT;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(250));
        if sync::healthz(url).is_ok() {
            // a child that already left (it lost the port to another server) is not owned
            if server.exited() {
                return Ok(Ensured::Reused);
            }
            *OWNED.lock().unwrap_or_else(|e| e.into_inner()) = Some(server);
            return Ok(Ensured::Started(pid));
        }
        if server.exited() {
            return Err(format!("anser-server stopped right after starting; see {}.", log.display()));
        }
    }
    Err(format!("anser-server (pid {pid}) didn't answer within {} s; see {}.", START_WAIT.as_secs(), log.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_urls_only() {
        for u in ["http://127.0.0.1:8787", "http://localhost:8787/", "http://[::1]:9000", "http://127.1.2.3", "http://LOCALHOST:1"] {
            assert!(is_loopback(u), "{u}");
        }
        for u in ["http://192.168.1.5:8787", "https://finance.example.com", "http://100.64.0.1:8787", "http://127.0.0.1.evil.com", "", "http://user@evil.com/@127.0.0.1"] {
            assert!(!is_loopback(u), "{u}");
        }
    }

    #[test]
    fn bind_is_the_urls_host_and_port() {
        assert_eq!(host_port("http://127.0.0.1:8787/x"), Some("127.0.0.1:8787".into()));
        assert_eq!(host_port("http://[::1]:9000"), Some("[::1]:9000".into()));
        let env = child_env("http://localhost:8800", Path::new("/h/finance"));
        assert_eq!(env, vec![("FINANCE_HOME", "/h/finance".to_string()), ("FINANCE_BIND", "localhost:8800".to_string())]);
    }

    #[test]
    fn binary_beside_the_app_beats_path_beats_local_bin() {
        let t = std::env::temp_dir().join(format!("fin-bin-{}", std::process::id()));
        let (app, onpath, home) = (t.join("app"), t.join("path"), t.join("home"));
        for d in [&app, &onpath, &home.join(".local/bin")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let exe = app.join("finance-desktop");
        let path = std::env::join_paths([&onpath]).unwrap();
        assert_eq!(pick_bin(&exe, Some(&path), Some(&home)), None);
        std::fs::write(home.join(".local/bin/anser-server"), "").unwrap();
        assert_eq!(pick_bin(&exe, Some(&path), Some(&home)), Some(home.join(".local/bin/anser-server")));
        std::fs::write(onpath.join("anser-server"), "").unwrap();
        assert_eq!(pick_bin(&exe, Some(&path), Some(&home)), Some(onpath.join("anser-server")));
        std::fs::write(app.join("anser-server"), "").unwrap();
        assert_eq!(pick_bin(&exe, Some(&path), Some(&home)), Some(app.join("anser-server")));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[cfg(unix)]
    #[test]
    fn stop_ends_the_child() {
        let log = std::env::temp_dir().join(format!("fin-sleep-{}.log", std::process::id()));
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let mut s = LocalServer::spawn(cmd, &log).unwrap();
        assert!(!s.exited());
        let started = Instant::now();
        s.stop();
        assert!(s.exited());
        assert!(started.elapsed() < STOP_GRACE, "SIGTERM was enough");
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn a_remote_url_is_left_alone() {
        assert_eq!(ensure("http://192.168.1.5:8787", Path::new("/x"), Path::new("/dev/null")), Ok(Ensured::Remote));
    }
}
