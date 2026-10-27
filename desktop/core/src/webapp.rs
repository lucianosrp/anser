//! The web app (the desktop app built for the browser, `desktop/web`):
//! build the bundle, then install it on the server. Blocking: call these
//! off the UI thread. The Doctor page's "install the web app on the
//! server" is [`build_and_install`].
//!
//! The server half is `POST /api/webapp` in anser-server (server/src/webapp.rs):
//! multipart, the files in the `files` field, plus `X-Finance-Install: 1`.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};

/// Files the server requires in a bundle.
pub const REQUIRED: [&str; 3] = ["index.html", "finance_web.js", "finance_web_bg.wasm"];

/// `desktop/` of the checkout this binary was built from.
pub fn desktop_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Where build-web.sh writes the bundle by default.
pub fn default_out_dir() -> PathBuf {
    desktop_dir().join("target/webapp")
}

/// The Cargo.lock version of `crate` (what build-web.sh pins against).
fn locked_version(lock: &str, krate: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l == format!("name = \"{krate}\"") {
            return lines.next()?.strip_prefix("version = \"")?.strip_suffix('"').map(String::from);
        }
    }
    None
}

/// Why the web app can't be built here, and the commands that fix it.
#[derive(Clone, Debug, PartialEq)]
pub struct Missing {
    pub why: String,
    pub commands: Vec<String>,
}

/// What build-web.sh needs and this machine lacks: None when it can build.
/// Runs `rustup` and `wasm-bindgen`, so call it off the UI thread.
pub fn toolchain() -> Option<Missing> {
    let script = desktop_dir().join("scripts/build-web.sh");
    if !script.is_file() {
        return Some(Missing { why: "this app wasn't built from a checkout (no scripts/build-web.sh)".into(), commands: vec![] });
    }
    let want = std::fs::read_to_string(desktop_dir().join("Cargo.lock")).ok().and_then(|l| locked_version(&l, "wasm-bindgen"));
    let run = |cmd: &str, args: &[&str]| Command::new(cmd).args(args).output().ok().filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let mut why = Vec::new();
    let mut commands = Vec::new();
    if !run("rustup", &["target", "list", "--installed"]).is_some_and(|t| t.lines().any(|l| l.trim() == "wasm32-unknown-unknown")) {
        why.push("the wasm32 target".to_string());
        commands.push("rustup target add wasm32-unknown-unknown".to_string());
    }
    let have = run("wasm-bindgen", &["--version"]).and_then(|v| v.split_whitespace().nth(1).map(String::from));
    let install = format!("cargo install wasm-bindgen-cli --version {}", want.as_deref().unwrap_or("<Cargo.lock's wasm-bindgen>"));
    match (&have, &want) {
        (None, _) => {
            why.push("wasm-bindgen".into());
            commands.push(install);
        }
        (Some(h), Some(w)) if h != w => {
            why.push(format!("wasm-bindgen {w} (this machine has {h})"));
            commands.push(install);
        }
        _ => {}
    }
    (!why.is_empty()).then(|| Missing { why: format!("needs {}", why.join(" and ")), commands })
}

/// Run `scripts/build-web.sh OUT` (cargo + wasm-bindgen; minutes on a cold
/// cache), each line of its output to `log` as it comes. Returns the
/// bundle directory.
pub fn build(out: &Path, log: &dyn Fn(String)) -> Result<PathBuf> {
    let script = desktop_dir().join("scripts/build-web.sh");
    ensure!(script.is_file(), "{} not found (the app wasn't built from a checkout)", script.display());
    let (reader, writer) = std::io::pipe().context("a pipe for the build log")?;
    // the Command holds the pipe's write ends: drop it once spawned, so the
    // read below ends when the script does
    let mut child = {
        let mut cmd = Command::new("bash");
        cmd.arg(&script).arg(out).stdin(Stdio::null()).stdout(writer.try_clone()?).stderr(writer);
        cmd.spawn().with_context(|| format!("running {}", script.display()))?
    };
    let mut tail = std::collections::VecDeque::new();
    for line in BufReader::new(reader).lines() {
        let line = line.unwrap_or_default();
        if tail.len() == 12 {
            tail.pop_front();
        }
        tail.push_back(line.clone());
        log(line);
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("build-web.sh failed:\n{}", Vec::from(tail).join("\n"));
    }
    Ok(out.to_path_buf())
}

/// Every file of a bundle directory (flat), as (name, bytes).
pub fn bundle_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        files.push((name, std::fs::read(entry.path())?));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    for req in REQUIRED {
        ensure!(files.iter().any(|(n, _)| n == req), "{} has no {req}: build it first", dir.display());
    }
    Ok(files)
}

/// What the server reported after an install.
#[derive(Debug, Clone)]
pub struct Installed {
    pub files: usize,
    pub bytes: u64,
    /// where the app now answers
    pub url: String,
}

/// `POST {server}/api/webapp` with the bundle in `dir`.
pub fn install(server: &str, dir: &Path) -> Result<Installed> {
    crate::demo::refuse(server)?;
    let files = bundle_files(dir)?;
    let boundary = format!("finance-webapp-{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let body = crate::upload::encode_multipart(&boundary, &files);
    let server = server.trim().trim_end_matches('/');
    let url = format!("{server}/api/webapp");
    let agent = crate::upload::agent(Duration::from_secs(600));
    let mut r = agent
        .post(&url)
        .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .header("X-Finance-Install", "1")
        .header("Authorization", &crate::auth::bearer(server).unwrap_or_default())
        .send(&body[..])
        .with_context(|| format!("POST {url}"))?;
    let text = r.body_mut().read_to_string().unwrap_or_default();
    if r.status() != 200 {
        bail!("{url} answered HTTP {}: {}", r.status(), text.chars().take(200).collect::<String>());
    }
    let v: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("{url} is not JSON"))?;
    Ok(Installed {
        files: v["installed"].as_array().map(|a| a.len()).unwrap_or(0),
        bytes: v["bytes"].as_u64().unwrap_or(0),
        url: format!("{server}/app/"),
    })
}

/// Build the bundle, then install it on `server`; the build's output and
/// each step go to `log`.
pub fn build_and_install(server: &str, log: &dyn Fn(String)) -> Result<Installed> {
    let dir = build(&default_out_dir(), log)?;
    log(format!("installing on {server} …"));
    let done = install(server, &dir)?;
    log(format!("installed {} files ({}): {}", done.files, crate::doctor::bytes(done.bytes), done.url));
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_locked_version() {
        let lock = "[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.129\"\nsource = \"x\"\n";
        assert_eq!(locked_version(lock, "wasm-bindgen").as_deref(), Some("0.2.129"));
        assert_eq!(locked_version(lock, "gpui"), None);
        let real = std::fs::read_to_string(desktop_dir().join("Cargo.lock")).unwrap();
        assert!(locked_version(&real, "wasm-bindgen").is_some());
    }
}
