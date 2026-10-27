//! What the run starts, and takes down again by PID however it ends: a
//! scratch folder, a throwaway anser-server on the demo, headless Chrome.

use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// A folder removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new() -> Result<Scratch> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let p = std::env::temp_dir().join(format!("finance-shots-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p)?;
        Ok(Scratch(p))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child process: SIGTERM on drop, SIGKILL if it hasn't gone in 3 s.
pub struct Proc {
    pub name: &'static str,
    child: Child,
    log: PathBuf,
}

impl Proc {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Fails with the end of its log if the process has exited.
    pub fn check_alive(&mut self) -> Result<()> {
        if let Some(st) = self.child.try_wait()? {
            bail!("{} exited ({st}):\n{}", self.name, tail(&self.log, 30));
        }
        Ok(())
    }

    pub fn log_tail(&self, lines: usize) -> String {
        tail(&self.log, lines)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let pid = self.child.id() as libc::pid_t;
        // SAFETY: a plain kill(2) on our own child's PID
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tail(log: &Path, lines: usize) -> String {
    let s = std::fs::read_to_string(log).unwrap_or_default();
    let v: Vec<&str> = s.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

fn spawn(name: &'static str, mut cmd: Command, log: PathBuf) -> Result<Proc> {
    let out = File::create(&log)?;
    cmd.stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out);
    let child = cmd.spawn().with_context(|| format!("starting {name}"))?;
    Ok(Proc { name, child, log })
}

/// A port nobody listens on right now.
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// GET `path` on 127.0.0.1:`port`: the status code (the body is read and
/// dropped).
pub fn http_get(port: u16, path: &str, timeout: Duration) -> Result<u16> {
    let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2))?;
    s.set_read_timeout(Some(timeout))?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let head = String::from_utf8_lossy(&buf[..buf.len().min(64)]).to_string();
    head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).with_context(|| format!("GET {path}: {head:?}"))
}

/// anser-server serving the demo and `webapp` from a tree of its own under
/// `scratch`: nothing of this machine's setup is read (the environment is
/// cleared, HOME and TMPDIR point inside the scratch folder too).
pub fn server(bin: &Path, scratch: &Path, webapp: &Path, port: u16, today: &str) -> Result<Proc> {
    let home = scratch.join("finance");
    let data = home.join("data");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(scratch.join("home"))?;
    std::fs::create_dir_all(scratch.join("tmp"))?;
    let webapp = webapp.canonicalize().with_context(|| format!("{}", webapp.display()))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&webapp, data.join("webapp"))?;
    let mut cmd = Command::new(bin);
    cmd.arg("serve")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", scratch.join("home"))
        .env("TMPDIR", scratch.join("tmp"))
        .env("FINANCE_HOME", &home)
        .env("FINANCE_OFFLINE", "1")
        .env("FINANCE_BIND", format!("127.0.0.1:{port}"))
        .env("FINANCE_DEMO_TODAY", today)
        .current_dir(scratch);
    // FINANCE_DEMO_HOME=mortgage: the demo household owns (net worth shows the mortgage)
    if let Some(v) = std::env::var_os("FINANCE_DEMO_HOME") {
        cmd.env("FINANCE_DEMO_HOME", v);
    }
    let mut p = spawn("anser-server", cmd, scratch.join("server.log"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        p.check_alive()?;
        if http_get(port, "/healthz", Duration::from_secs(2)).is_ok_and(|c| c == 200) {
            break;
        }
        if Instant::now() > deadline {
            bail!("anser-server didn't answer on {port}:\n{}", p.log_tail(30));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let app = http_get(port, "/app/", Duration::from_secs(10))?;
    if app != 200 {
        bail!("GET /app/ answered {app}: is {} a web build?", webapp.display());
    }
    // the demo is built on first use: once here, not in the first shot's wait
    let demo = http_get(port, "/api/demo/export.json", Duration::from_secs(120))?;
    if demo != 200 {
        bail!("GET /api/demo/export.json answered {demo}:\n{}", p.log_tail(30));
    }
    Ok(p)
}

/// The Chrome to run: --chrome, $CHROME, else the first on PATH.
pub fn find_chrome(given: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = given {
        return Ok(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("CHROME") {
        return Ok(p.into());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["google-chrome-stable", "google-chrome", "chromium", "chromium-browser"] {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    bail!("no Chrome on PATH (google-chrome-stable, chromium); pass --chrome")
}

/// Headless Chrome on a throwaway profile, DevTools on a port of its own
/// choosing. Returns it and (port, browser websocket path). It resolves no
/// host but 127.0.0.1, so a page can't reach the network.
pub fn chrome(bin: &Path, scratch: &Path) -> Result<(Proc, u16, String)> {
    let profile = scratch.join("chrome");
    std::fs::create_dir_all(&profile)?;
    let mut cmd = Command::new(bin);
    cmd.args([
        "--headless=new",
        "--enable-unsafe-swiftshader",
        "--use-angle=swiftshader",
        "--remote-debugging-port=0",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-default-apps",
        "--disable-sync",
        "--disable-features=Translate,MediaRouter,OptimizationHints,AutofillServerCommunication",
        "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1",
        "--hide-scrollbars",
        "--mute-audio",
        "--force-color-profile=srgb",
        "--font-render-hinting=none",
        "--lang=en-US",
    ])
    .arg(format!("--user-data-dir={}", profile.display()))
    .arg("about:blank");
    let mut p = spawn("chrome", cmd, scratch.join("chrome.log"))?;
    let port_file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        p.check_alive()?;
        if let Ok(s) = std::fs::read_to_string(&port_file) {
            let mut lines = s.lines();
            if let (Some(port), Some(path)) = (lines.next().and_then(|l| l.trim().parse().ok()), lines.next()) {
                return Ok((p, port, path.trim().to_string()));
            }
        }
        if Instant::now() > deadline {
            bail!("Chrome didn't open its DevTools port:\n{}", p.log_tail(30));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
