//! The desktop side of encrypted backups (Settings, Backups):
//!
//! * the backup key: an age X25519 identity made on this machine. The
//!   private half stays here, in the OS keyring (else a 0600 file next to
//!   the server file, with a warning); the public recipient goes to the
//!   server, which seals its nightly archives to it. The identity is the
//!   only way to open those archives: [`save_copy`] writes a copy where the
//!   owner chooses.
//! * downloads and uploads of `.finance` archives: the newest nightly one
//!   ([`fetch_latest`]), a passphrase export ([`export_to`]), an import
//!   ([`import`], a dry run unless asked). Streamed to and from disk.
//!
//! Blocking: call these off the UI thread.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache;

#[cfg(feature = "keyring")]
const SERVICE: &str = "finance-desktop";
const KEYRING_USER: &str = "backup-identity";

fn use_keyring() -> bool {
    cfg!(feature = "keyring") && std::env::var_os("FINANCE_DESKTOP_SERVER_FILE").is_none()
}

/// Another account than the machine's first one (crate::profiles) keeps
/// its own backup key: each user seals their archives to their own.
fn profile_suffix() -> Option<String> {
    if !crate::profiles::file().exists() {
        return None;
    }
    let p = crate::profiles::active();
    (p.token_key != p.url).then_some(p.id)
}

#[cfg(feature = "keyring")]
fn keyring_user() -> String {
    match profile_suffix() {
        Some(id) => format!("{KEYRING_USER}:{id}"),
        None => KEYRING_USER.to_string(),
    }
}

/// The 0600 fallback file for the identity.
pub fn identity_file() -> PathBuf {
    let name = match profile_suffix() {
        Some(id) => format!("backup-identity-{id}.txt"),
        None => "backup-identity.txt".into(),
    };
    cache::server_file().with_file_name(name)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(feature = "keyring")]
fn keyring_get() -> Option<String> {
    keyring::Entry::new(SERVICE, &keyring_user()).ok()?.get_password().ok()
}
#[cfg(not(feature = "keyring"))]
fn keyring_get() -> Option<String> {
    None
}
#[cfg(feature = "keyring")]
fn keyring_set(v: Option<&str>) -> bool {
    let Ok(e) = keyring::Entry::new(SERVICE, &keyring_user()) else { return false };
    match v {
        Some(v) => e.set_password(v).is_ok() && keyring_get().as_deref() == Some(v),
        None => e.delete_credential().is_ok(),
    }
}
#[cfg(not(feature = "keyring"))]
fn keyring_set(_v: Option<&str>) -> bool {
    false
}

/// Where the identity is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeptIn {
    Keyring,
    /// a 0600 file (no keyring here): weaker, the UI warns
    File(PathBuf),
}

/// The stored identity's text (`AGE-SECRET-KEY-1…`), if any.
pub fn identity() -> Option<(String, KeptIn)> {
    if use_keyring() {
        if let Some(t) = keyring_get().filter(|t| t.contains("AGE-SECRET-KEY-")) {
            return Some((t, KeptIn::Keyring));
        }
    }
    let f = identity_file();
    std::fs::read_to_string(&f).ok().filter(|t| t.contains("AGE-SECRET-KEY-")).map(|t| (t, KeptIn::File(f)))
}

/// The public recipient of the stored identity.
/// Delete all (this computer): the active account's backup key goes, from
/// the keyring and its file.
pub fn forget_identity() {
    if use_keyring() {
        keyring_set(None);
    }
    let _ = std::fs::remove_file(identity_file());
}

pub fn public_key() -> Option<String> {
    identity().and_then(|(t, _)| t.lines().find_map(|l| l.strip_prefix("# public key: ").map(|p| p.trim().to_string())))
}

/// Keep a new identity (its text and public key), replacing any older one.
pub fn store_identity(text: &str) -> Result<KeptIn> {
    if use_keyring() && keyring_set(Some(text)) {
        let _ = std::fs::remove_file(identity_file());
        return Ok(KeptIn::Keyring);
    }
    write_private(&identity_file(), text.as_bytes())?;
    Ok(KeptIn::File(identity_file()))
}

/// A copy of the identity where the owner chooses (0600).
pub fn save_copy(path: &Path) -> Result<()> {
    let (t, _) = identity().context("no backup key on this computer yet")?;
    write_private(path, t.as_bytes())
}

/// Where fetched archives go: the saved choice, else ~/Backups/finance/archives.
pub fn archives_dir() -> PathBuf {
    if let Some(d) = std::fs::read_to_string(dir_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return expand(&d);
    }
    dirs::home_dir().unwrap_or_default().join("Backups").join("finance").join("archives")
}

fn dir_file() -> PathBuf {
    cache::server_file().with_file_name("backups-dir")
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

pub fn set_archives_dir(dir: &str) -> Result<()> {
    let p = dir_file();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, format!("{}\n", dir.trim()))?;
    Ok(())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn auth(server: &str) -> Option<String> {
    crate::auth::bearer(server)
}

fn error_of(r: &mut ureq::http::Response<ureq::Body>) -> String {
    let body = r.body_mut().with_config().limit(1 << 20).read_to_vec().unwrap_or_default();
    serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| format!("HTTP {}: {}", r.status(), String::from_utf8_lossy(&body).chars().take(160).collect::<String>()))
}

/// Stream a response body into `path` (0600, via a .part file).
fn save_body(r: &mut ureq::http::Response<ureq::Body>, path: &Path) -> Result<u64> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let part = PathBuf::from(format!("{}.part", path.display()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&part).with_context(|| format!("writing {}", part.display()))?;
    let n = std::io::copy(&mut r.body_mut().with_config().limit(4 << 30).reader(), &mut f);
    let n = match n {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e).context("the download broke off");
        }
    };
    f.sync_all()?;
    std::fs::rename(&part, path)?;
    Ok(n)
}

fn filename_of(r: &ureq::http::Response<ureq::Body>) -> Option<String> {
    let cd = r.headers().get("Content-Disposition")?.to_str().ok()?;
    let n = cd.split("filename=").nth(1)?.trim().trim_matches('"');
    let n = n.rsplit(['/', '\\']).next()?.to_string();
    (!n.is_empty() && !n.starts_with('.')).then_some(n)
}

/// Download the server's newest nightly archive into `dir`. Returns the path.
pub fn fetch_latest(dir: &Path) -> Result<(PathBuf, u64)> {
    let server = cache::server_url();
    crate::demo::refuse(&server)?;
    let url = format!("{server}/api/admin/archives/latest");
    let mut req = agent().get(&url);
    if let Some(a) = auth(&server) {
        req = req.header("Authorization", &a);
    }
    let mut r = req.call().with_context(|| format!("GET {url}"))?;
    if r.status() != 200 {
        bail!("{}", error_of(&mut r));
    }
    let name = filename_of(&r).unwrap_or_else(|| format!("finance-{}.finance", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    let path = dir.join(name);
    let n = save_body(&mut r, &path)?;
    Ok((path, n))
}

/// A passphrase export of the whole account, saved to `path`.
pub fn export_to(path: &Path, passphrase: &str, with_secrets: bool) -> Result<u64> {
    let server = cache::server_url();
    crate::demo::refuse(&server)?;
    let url = format!("{server}/api/account/export");
    let body = serde_json::json!({"passphrase": passphrase, "with_secrets": with_secrets});
    let mut req = agent().post(&url).header("Content-Type", "application/json");
    if let Some(a) = auth(&server) {
        req = req.header("Authorization", &a);
    }
    let mut r = req.send(serde_json::to_vec(&body)?).with_context(|| format!("POST {url}"))?;
    if r.status() != 200 {
        bail!("{}", error_of(&mut r));
    }
    save_body(&mut r, path)
}

/// How an import opens the archive.
pub enum Key {
    Passphrase(String),
    /// the stored backup key
    Identity,
}

/// POST the archive to /api/account/import. `dry_run` reports only.
/// Returns the server's reply (`report`, `text`, `manifest`).
pub fn import(file: &Path, key: &Key, mode: &str, dry_run: bool, prefer_archive: bool) -> Result<Value> {
    let data = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let secret = match key {
        Key::Passphrase(p) => ("passphrase", p.clone()),
        Key::Identity => ("identity", identity().context("no backup key on this computer")?.0),
    };
    let boundary = format!("finance-{}", crate::upload::md5_hex(&data).get(..16).unwrap_or("b"));
    let mut body = Vec::with_capacity(data.len() + 1024);
    for (k, v) in [secret, ("mode", mode.to_string()), ("dry_run", if dry_run { "1" } else { "0" }.into()),
                   ("prefer_archive", if prefer_archive { "1" } else { "0" }.into())] {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
    }
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"archive.finance\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let server = cache::server_url();
    crate::demo::refuse(&server)?;
    let url = format!("{server}/api/account/import");
    let mut req = agent().post(&url).header("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
    if let Some(a) = auth(&server) {
        req = req.header("Authorization", &a);
    }
    let mut r = req.send(&body[..]).with_context(|| format!("POST {url}"))?;
    let status = r.status().as_u16();
    let v: Value = serde_json::from_slice(&r.body_mut().with_config().limit(16 << 20).read_to_vec()?).unwrap_or(Value::Null);
    if status != 200 {
        bail!("{}", v.get("error").and_then(Value::as_str).map(String::from).unwrap_or(format!("HTTP {status}")));
    }
    Ok(v)
}

/// A passphrase's rough strength, for the hint under the field.
pub fn strength(p: &str) -> (u8, &'static str) {
    let n = p.chars().count();
    let classes = [p.chars().any(|c| c.is_lowercase()), p.chars().any(|c| c.is_uppercase()), p.chars().any(|c| c.is_ascii_digit()),
        p.chars().any(|c| !c.is_alphanumeric())].iter().filter(|b| **b).count();
    let words = p.split_whitespace().count();
    match (n, classes, words) {
        (0..=7, _, _) => (0, "too short: 8 characters at least"),
        (_, _, 4..) if n >= 20 => (3, "strong: a few random words is the best kind"),
        (20.., _, _) => (3, "strong"),
        (12..=19, 3.., _) => (2, "good"),
        (12..=19, _, _) => (1, "fair: longer is better (four random words)"),
        _ => (1, "weak: longer is better (four random words)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_hints() {
        assert_eq!(strength("short").0, 0);
        assert_eq!(strength("abcdefghij").0, 1);
        assert_eq!(strength("Abcdefgh1234").0, 2);
        assert_eq!(strength("correct horse battery staple").0, 3);
    }
}
