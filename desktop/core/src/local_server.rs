//! A local install's server: when the server URL is on this machine, the app
//! starts its own binary as `anser server serve` (the server is built in),
//! as a child it owns, and stops that child when it quits. The child is
//! known by its [`Child`] handle (its pid), never by a process pattern. A
//! server it did not start (a systemd unit, one run by hand) is reused and
//! never touched, and a remote server never concerns this module.

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

/// The documented install's data tree. Set explicitly: the server built from a
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

/// The child: this very binary, `server serve`, with [`child_env`]. The
/// running executable's path, as the self-update sees it (a replaced file's
/// " (deleted)" suffix dropped).
pub fn child_command(url: &str, home: &Path) -> Result<Command> {
    let exe = crate::selfupdate::exe_path().context("finding this app's binary")?;
    let mut cmd = Command::new(exe);
    cmd.args(["server", "serve"]).envs(child_env(url, home));
    Ok(cmd)
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

    pub fn start(url: &str, log: &Path) -> Result<Self> {
        Self::spawn(child_command(url, &default_home())?, log)
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

/// Make a loopback `url` answer: reuse what is there, else start
/// `anser server serve` as an owned child. Blocking (up to ~10 s): call it
/// off the UI thread.
pub fn ensure(url: &str, log: &Path) -> Result<Ensured, String> {
    if !is_loopback(url) {
        return Ok(Ensured::Remote);
    }
    if sync::healthz(url).is_ok() {
        return Ok(Ensured::Reused);
    }
    let mut server = LocalServer::start(url, log).map_err(|e| format!("{e:#}"))?;
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
            return Err(format!("the local server stopped right after starting; see {}.", log.display()));
        }
    }
    Err(format!("the local server (pid {pid}) didn't answer within {} s; see {}.", START_WAIT.as_secs(), log.display()))
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
    fn the_child_is_this_binary_running_server_serve() {
        let cmd = child_command("http://127.0.0.1:8800", Path::new("/h/finance")).unwrap();
        assert_eq!(Path::new(cmd.get_program()), crate::selfupdate::exe_path().unwrap());
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["server", "serve"]);
        let mut env: Vec<_> = cmd.get_envs().map(|(k, v)| (k.to_str().unwrap(), v.and_then(|v| v.to_str()))).collect();
        env.sort();
        assert_eq!(env, [("FINANCE_BIND", Some("127.0.0.1:8800")), ("FINANCE_HOME", Some("/h/finance"))]);
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
        assert_eq!(ensure("http://192.168.1.5:8787", Path::new("/dev/null")), Ok(Ensured::Remote));
    }
}
