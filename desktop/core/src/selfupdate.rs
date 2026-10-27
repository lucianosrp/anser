//! The desktop app updating itself from a GitHub release, only when the
//! owner clicks (Settings, General, Updates).
//!
//! * [`check`] asks `releases/latest` (anonymous, 60 requests an hour), at
//!   most once a day: the answer is cached in `update-check.json`.
//! * [`offer`] decides whether that release is worth offering: only a clean
//!   release build (`v0.1.0`) is ever offered a newer stable tag. A
//!   development build (`v0.1.0-3-gabc1234`, `0.1.0+gabc1234`, `-dirty`)
//!   never is, so a local build doesn't nag. Prereleases, drafts and tags
//!   with a suffix (`v0.2.0-rc.1`) are skipped.
//! * [`apply`] downloads the x86_64 tarball and `SHA256SUMS`, refuses a
//!   tarball whose sha256 isn't listed or differs, extracts the one binary
//!   next to the running one and renames it over. No sudo: when that
//!   directory isn't writable the result is [`Applied::Manual`], the command
//!   to run instead.
//! * [`relaunch`] starts the new binary with the same arguments.
//!
//! The setting that turns checks off lives in `updates.json`. Both files sit
//! in `~/.config/finance-desktop` (`FINANCE_DESKTOP_UPDATE_DIR` moves them;
//! `FINANCE_DESKTOP_UPDATE_API` asks another URL than GitHub and
//! `FINANCE_DESKTOP_UPDATE_AS` pretends to be another version: dev aids).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const API: &str = "https://api.github.com/repos/lucianosrp/anser/releases/latest";
/// Ask GitHub at most this often.
pub const CHECK_EVERY_MS: i64 = 24 * 3600 * 1000;
const BIN: &str = "anser";
/// The binary is ~100 MB; refuse anything absurd rather than fill the disk.
const MAX_TARBALL: u64 = 400 << 20;
const MAX_BINARY: u64 = 1 << 30;

/// Only x86_64 Linux has a published desktop build.
pub fn supported() -> bool {
    cfg!(all(target_os = "linux", target_arch = "x86_64"))
}

/// The version this build is, or the dev aid's.
pub fn running() -> String {
    std::env::var("FINANCE_DESKTOP_UPDATE_AS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| crate::VERSION.to_string())
}

// ---------- versions ----------

/// `v1.2.3` (the `v` required), nothing after the patch number.
pub fn parse_tag(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.strip_prefix('v')?.split('.');
    let mut n = || it.next().filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).and_then(|p| p.parse::<u64>().ok());
    let v = (n()?, n()?, n()?);
    it.next().is_none().then_some(v)
}

/// Whether `current` is a build of a release: a clean tag, no git hash.
pub fn is_release_build(current: &str) -> bool {
    parse_tag(current).is_some()
}

/// True when a user running `current` should be offered `latest`.
pub fn newer(current: &str, latest: &str) -> bool {
    match (parse_tag(current), parse_tag(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

// ---------- the release ----------

/// What the latest stable release offers this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    /// the release notes page
    pub url: String,
    /// where the x86_64 desktop tarball and SHA256SUMS download from
    pub tarball: String,
    pub sums: String,
}

impl Release {
}

/// The release's desktop asset (no tag in the name).
pub const TARBALL: &str = "anser-x86_64-linux.tar.xz";

/// The release in a `releases/latest` answer, when it is a stable one with
/// both assets (None: a draft, prerelease, suffixed tag, or no desktop build).
pub fn parse_release(v: &Value) -> Option<Release> {
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = v["tag_name"].as_str()?;
    parse_tag(tag)?;
    let asset = |name: &str| {
        v["assets"].as_array()?.iter().find(|a| a["name"].as_str() == Some(name)).and_then(|a| a["browser_download_url"].as_str()).filter(|u| trusted_download(u)).map(String::from)
    };
    Some(Release {
        tag: tag.to_string(),
        url: v["html_url"].as_str().unwrap_or_default().to_string(),
        tarball: asset(TARBALL)?,
        sums: asset("SHA256SUMS")?,
    })
}

/// Where a release asset may be downloaded from: https, on GitHub or its
/// asset hosts (a poisoned answer can't point the updater elsewhere).
fn trusted_download(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    // the whole authority is compared: a userinfo or a port can't hide the host
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"].contains(&authority)
}

/// The release to offer: newer than this build, and this build a release.
pub fn offer<'a>(current: &str, latest: Option<&'a Release>) -> Option<&'a Release> {
    latest.filter(|r| newer(current, &r.tag))
}

// ---------- the setting and the cache ----------

fn dir() -> PathBuf {
    std::env::var_os("FINANCE_DESKTOP_UPDATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::config_dir().unwrap_or_default().join("finance-desktop"))
}

/// Whether to look for updates (default: yes).
pub fn checks_enabled() -> bool {
    std::fs::read(dir().join("updates.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["check"].as_bool())
        .unwrap_or(true)
}

pub fn set_checks_enabled(on: bool) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    std::fs::write(dir().join("updates.json"), serde_json::to_vec_pretty(&serde_json::json!({ "check": on }))?)?;
    Ok(())
}

/// The last answer from GitHub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checked {
    pub at_ms: i64,
    /// the latest stable release with a desktop build (None: there is none)
    pub latest: Option<Release>,
}

impl Checked {
    pub fn fresh(&self, now_ms: i64) -> bool {
        (0..CHECK_EVERY_MS).contains(&(now_ms - self.at_ms))
    }
}

pub fn cached() -> Option<Checked> {
    serde_json::from_slice(&std::fs::read(dir().join("update-check.json")).ok()?).ok()
}

fn remember(c: &Checked) {
    let _ = std::fs::create_dir_all(dir());
    if let Ok(b) = serde_json::to_vec(c) {
        let _ = std::fs::write(dir().join("update-check.json"), b);
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .user_agent(format!("anser/{}", crate::VERSION))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut r = agent.get(url).header("Accept", "application/vnd.github+json").call().with_context(|| format!("couldn't reach {}", host(url)))?;
    if !r.status().is_success() {
        bail!("{} answered {}", host(url), r.status());
    }
    Ok(r.body_mut().with_config().limit(limit).read_to_vec()?)
}

fn host(url: &str) -> &str {
    url.split("//").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url)
}

/// Ask GitHub for the latest release now, and remember the answer.
pub fn fetch() -> Result<Checked> {
    // another API URL is for tests and development builds only
    let over = if cfg!(debug_assertions) { std::env::var("FINANCE_DESKTOP_UPDATE_API").ok().filter(|v| !v.is_empty()) } else { None };
    let url = over.unwrap_or_else(|| API.to_string());
    let body = get(&agent(), &url, 4 << 20)?;
    let v: Value = serde_json::from_slice(&body).context("the release answer isn't JSON")?;
    let c = Checked { at_ms: chrono::Utc::now().timestamp_millis(), latest: parse_release(&v) };
    remember(&c);
    Ok(c)
}

/// The cached answer while it is under a day old, else a new one (`force`:
/// always a new one). A development build or an unsupported platform asks
/// nobody.
pub fn check(force: bool) -> Result<Option<Checked>> {
    if !supported() || !is_release_build(&running()) {
        return Ok(None);
    }
    if !force {
        if let Some(c) = cached().filter(|c| c.fresh(chrono::Utc::now().timestamp_millis())) {
            return Ok(Some(c));
        }
    }
    fetch().map(Some)
}

// ---------- applying ----------

/// `SHA256SUMS` (`<hex>  <name>`, a `*` before the name for binary mode).
pub fn parse_sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (h, name) = l.trim().split_once(char::is_whitespace)?;
            let name = name.trim().trim_start_matches('*');
            (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty()).then(|| (name.to_string(), h.to_ascii_lowercase()))
        })
        .collect()
}

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// Refuse unless `sums` lists `name` with the sha256 of `bytes`.
pub fn verify(bytes: &[u8], sums: &str, name: &str) -> Result<()> {
    let want = parse_sums(sums).remove(name).ok_or_else(|| anyhow!("SHA256SUMS has no entry for {name}; not installing"))?;
    let got = sha256_hex(bytes);
    if got != want {
        bail!("{name} doesn't match its checksum in SHA256SUMS; not installing");
    }
    Ok(())
}

/// The `anser` file out of the release's tar.xz.
pub fn extract_binary(tgz: &[u8]) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    lzma_rs::xz_decompress(&mut std::io::Cursor::new(tgz), &mut raw).map_err(|e| anyhow!("the download isn't a tar.xz: {e}"))?;
    let mut ar = tar::Archive::new(raw.as_slice());
    for e in ar.entries().context("the download isn't a tar")? {
        let mut e = e?;
        let is_bin = e.header().entry_type().is_file() && e.path()?.file_name().is_some_and(|n| n == BIN);
        if is_bin {
            let mut out = Vec::new();
            e.by_ref().take(MAX_BINARY).read_to_end(&mut out)?;
            if out.is_empty() {
                bail!("{BIN} in the download is empty");
            }
            return Ok(out);
        }
    }
    bail!("the download has no {BIN}")
}

/// The running executable's path. Linux appends " (deleted)" to it once an
/// update has replaced the file.
pub fn exe_path() -> Result<PathBuf> {
    Ok(strip_deleted(std::env::current_exe()?))
}

fn strip_deleted(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(s) => PathBuf::from(s),
        None => p,
    }
}

/// Put `bin` in place of `exe`: a temp file in the same directory (so the
/// rename stays on one filesystem), mode 755, then an atomic rename.
pub fn install(bin: &[u8], exe: &Path) -> Result<()> {
    let dir = exe.parent().ok_or_else(|| anyhow!("{} has no directory", exe.display()))?;
    let tmp = dir.join(format!(".{BIN}.update-{}", std::process::id()));
    let put = || -> std::io::Result<()> {
        std::fs::write(&tmp, bin)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, exe)
    };
    put().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow!(e).context(format!("couldn't replace {}", exe.display()))
    })
}

/// Whether a file can be made in `exe`'s directory (so the swap can work).
pub fn writable(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else { return false };
    let probe = dir.join(format!(".{BIN}.probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The command to update by hand when this install isn't ours to write.
pub fn manual_command(r: &Release, exe: &Path) -> String {
    let dir = exe.parent().unwrap_or(Path::new("/"));
    format!("curl -fL {} | sudo tar -xJ -C {} {BIN}", r.tarball, dir.display())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Downloading,
    Verifying,
    Installing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// installed; restart to run it
    Done,
    /// the directory isn't writable: run this instead
    Manual(String),
}

/// Download, verify and install `r` over the running executable.
pub fn apply(r: &Release, progress: impl Fn(Stage)) -> Result<Applied> {
    if !supported() {
        bail!("there is no desktop build for this platform");
    }
    let exe = exe_path()?;
    if !writable(&exe) {
        return Ok(Applied::Manual(manual_command(r, &exe)));
    }
    progress(Stage::Downloading);
    let a = agent();
    // both at once: the checksum file is tiny, but there is no reason to queue
    let (tgz, sums) = std::thread::scope(|s| {
        let sums = s.spawn(|| get(&a, &r.sums, 1 << 20));
        let tgz = get(&a, &r.tarball, MAX_TARBALL);
        (tgz, sums.join().unwrap_or_else(|_| Err(anyhow!("the checksum download crashed"))))
    });
    let (tgz, sums) = (tgz?, String::from_utf8(sums?).context("SHA256SUMS isn't text")?);
    progress(Stage::Verifying);
    verify(&tgz, &sums, TARBALL)?;
    let bin = extract_binary(&tgz)?;
    progress(Stage::Installing);
    install(&bin, &exe)?;
    Ok(Applied::Done)
}

/// Start the (new) binary with this process's arguments and end this one.
pub fn relaunch() -> ! {
    // exec doesn't run Drop: stop the local anser-server this app owns first
    crate::local_server::stop_owned();
    if let Ok(exe) = exe_path() {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(std::env::args_os().skip(1));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let e = cmd.exec();
            eprintln!("restarting failed: {e}");
        }
        #[cfg(not(unix))]
        {
            let _ = cmd.spawn();
        }
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut t = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            t.append_data(&mut h, name, *data).unwrap();
        }
        let raw = t.into_inner().unwrap();
        let mut out = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(raw), &mut out).unwrap();
        out
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("finance-selfupdate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn tags() {
        assert_eq!(parse_tag("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_tag("v10.20.30"), Some((10, 20, 30)));
        for bad in ["0.1.0", "v0.1", "v0.1.0-rc.1", "v0.1.0-3-gabc1234", "v0.1.0-3-gabc1234-dirty", "0.1.0+gabc1234", "v1.2.3.4", "v1..3", ""] {
            assert_eq!(parse_tag(bad), None, "{bad}");
        }
    }

    #[test]
    fn who_is_offered_what() {
        assert!(newer("v0.1.0", "v0.2.0"));
        assert!(newer("v0.9.0", "v0.10.0"), "numbers, not text");
        assert!(newer("v0.1.9", "v1.0.0"));
        assert!(!newer("v0.2.0", "v0.2.0"), "equal");
        assert!(!newer("v0.3.0", "v0.2.0"), "a newer build than the release");
        // dev builds are never offered anything
        for dev in ["v0.1.0-3-gabc1234", "v0.1.0-3-gabc1234-dirty", "0.1.0+gabc1234", "0.1.0+gabc1234-dirty", "0.1.0", "v0.1.0-dirty"] {
            assert!(!newer(dev, "v9.0.0"), "{dev}");
            assert!(!is_release_build(dev), "{dev}");
        }
        // pre-release tags are never the target
        assert!(!newer("v0.1.0", "v0.2.0-rc.1"));
        assert!(is_release_build("v0.1.0"));
    }

    #[test]
    fn only_github_hosts_are_downloaded_from() {
        for ok in [
            "https://github.com/o/r/releases/download/v1.0.0/a.tar.xz",
            "https://objects.githubusercontent.com/x?y=1",
            "https://release-assets.githubusercontent.com/x",
        ] {
            assert!(trusted_download(ok), "{ok}");
        }
        for bad in [
            "http://github.com/o/r/a",
            "https://example.test/a",
            "https://github.com.evil.example/a",
            "https://github.com@evil.example/a",
            "https://evil.example/github.com/a",
            "https://github.com:444/a",
            "https://evilgithub.com/a",
            "ftp://github.com/a",
            "",
        ] {
            assert!(!trusted_download(bad), "{bad}");
        }
        let mut v = release_json("v0.2.0");
        assert!(parse_release(&v).is_some());
        v["assets"][0]["browser_download_url"] = json!("https://evil.example/d.tgz");
        assert!(parse_release(&v).is_none(), "an asset from elsewhere makes the release unusable");
    }

    fn release_json(tag: &str) -> Value {
        json!({
            "tag_name": tag, "html_url": "https://example.test/notes", "draft": false, "prerelease": false,
            "assets": [
                {"name": TARBALL.to_string(), "browser_download_url": "https://github.com/o/r/releases/download/v0.2.0/d.tgz"},
                {"name": "anser-server-x86_64-linux.tar.xz", "browser_download_url": "https://github.com/o/r/releases/download/v0.2.0/s.tgz"},
                {"name": "SHA256SUMS", "browser_download_url": "https://github.com/o/r/releases/download/v0.2.0/SHA256SUMS"},
            ]
        })
    }

    #[test]
    fn the_release_answer() {
        let r = parse_release(&release_json("v0.2.0")).unwrap();
        assert_eq!((r.tag.as_str(), r.url.as_str(), r.tarball.as_str(), r.sums.as_str()), ("v0.2.0", "https://example.test/notes", "https://github.com/o/r/releases/download/v0.2.0/d.tgz", "https://github.com/o/r/releases/download/v0.2.0/SHA256SUMS"));
        assert_eq!(offer("v0.1.0", Some(&r)), Some(&r));
        assert_eq!(offer("v0.2.0", Some(&r)), None);
        assert_eq!(offer("v0.1.0-1-gabc", Some(&r)), None);

        let mut pre = release_json("v0.2.0");
        pre["prerelease"] = json!(true);
        assert!(parse_release(&pre).is_none());
        let mut draft = release_json("v0.2.0");
        draft["draft"] = json!(true);
        assert!(parse_release(&draft).is_none());
        assert!(parse_release(&release_json("v0.2.0-rc.1")).is_none());
        let mut no_sums = release_json("v0.2.0");
        no_sums["assets"].as_array_mut().unwrap().pop();
        assert!(parse_release(&no_sums).is_none());
        assert!(parse_release(&json!({"message": "Not Found"})).is_none());
    }

    #[test]
    fn the_cache_is_fresh_for_a_day() {
        let c = Checked { at_ms: 1_000, latest: None };
        assert!(c.fresh(1_000 + CHECK_EVERY_MS - 1));
        assert!(!c.fresh(1_000 + CHECK_EVERY_MS));
        assert!(!c.fresh(0), "a clock that went back asks again");
    }

    #[test]
    fn sums_file() {
        let h = "a".repeat(64);
        let text = format!("{h}  finance-web-v1.tar.gz\n{}  *anser-server.tar.gz\nnot a line\n{h}\n", "B".repeat(64));
        let m = parse_sums(&text);
        assert_eq!(m.len(), 2);
        assert_eq!(m["finance-web-v1.tar.gz"], h);
        assert_eq!(m["anser-server.tar.gz"], "b".repeat(64), "binary marker dropped, hex lowercased");
    }

    #[test]
    fn verification_refuses() {
        let data = b"the tarball";
        let name = TARBALL;
        let sums = format!("{}  {name}\n", sha256_hex(data));
        assert!(verify(data, &sums, name).is_ok());
        let e = verify(b"tampered", &sums, name).unwrap_err().to_string();
        assert!(e.contains("doesn't match"), "{e}");
        let e = verify(data, &sums, "other.tar.gz").unwrap_err().to_string();
        assert!(e.contains("no entry"), "{e}");
        assert!(verify(data, "", name).is_err());
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn the_binary_comes_out_of_the_tarball() {
        let t = tarball(&[("README", b"hi"), ("anser-v0.2.0/anser", b"\x7fELFnew")]);
        assert_eq!(extract_binary(&t).unwrap(), b"\x7fELFnew");
        assert!(extract_binary(&tarball(&[("README", b"hi")])).is_err());
        assert!(extract_binary(b"not xz").is_err());
        // a directory or a file merely containing the name doesn't count
        assert!(extract_binary(&tarball(&[("anser-extra", b"x")])).is_err());
    }

    /// What the release workflow makes is the xz CLI's output (CRC64, its own
    /// block layout), not lzma-rs's: check that one too where `xz` exists.
    #[test]
    fn reads_what_the_xz_cli_writes() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut t = tar::Builder::new(Vec::new());
        let body = vec![7u8; 300_000];
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        t.append_data(&mut h, "anser", body.as_slice()).unwrap();
        let raw = t.into_inner().unwrap();
        let Ok(mut child) = Command::new("xz").args(["-9", "-c"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() else { return };
        let mut stdin = child.stdin.take().unwrap();
        let feed = std::thread::spawn(move || stdin.write_all(&raw));
        let out = child.wait_with_output().unwrap().stdout;
        feed.join().unwrap().unwrap();
        assert_eq!(extract_binary(&out).unwrap(), body);
    }

    #[test]
    fn the_swap_on_temp_files() {
        let d = scratch("swap");
        let exe = d.join("finance-desktop");
        std::fs::write(&exe, b"old").unwrap();
        assert!(writable(&exe));
        install(b"new binary", &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777, 0o755);
        }
        let left: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 1, "no temp file left behind: {left:?}");
        // a directory that doesn't exist: an error, the old file untouched
        assert!(install(b"x", &d.join("nope").join("finance-desktop")).is_err());
        assert!(!writable(&d.join("nope").join("finance-desktop")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_read_only_directory_gets_the_command() {
        let r = parse_release(&release_json("v0.2.0")).unwrap();
        let c = manual_command(&r, Path::new("/usr/local/bin/anser"));
        assert_eq!(c, "curl -fL https://github.com/o/r/releases/download/v0.2.0/d.tgz | sudo tar -xJ -C /usr/local/bin anser");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_dir_is_not_writable() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("ro");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
        let exe = d.join("finance-desktop");
        // root ignores the mode bits
        if !std::fs::write(d.join("probe"), b"").is_ok() {
            assert!(!writable(&exe));
        }
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn deleted_suffix_is_dropped() {
        assert_eq!(strip_deleted(PathBuf::from("/opt/bin/anser (deleted)")), PathBuf::from("/opt/bin/anser"));
        assert_eq!(strip_deleted(PathBuf::from("/opt/bin/anser")), PathBuf::from("/opt/bin/anser"));
    }
}
