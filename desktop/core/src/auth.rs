//! This machine's device token for a server: pairing, storage, whoami.
//!
//! A server hands out a bearer token once, in exchange for a one-time code
//! (`anser-server pair` on the server, or "Pair another device" in Settings). The
//! token is kept per account (crate::profiles: the server URL for the first
//! account on a server, as always; `URL#profile` for another one there):
//!
//! * `$FINANCE_TOKEN` wins (CI, e2e runs, a laptop server's `push`);
//! * else the OS keyring (service `finance-desktop`, the URL as the user),
//!   when built with the `keyring` feature (the desktop app);
//! * else `tokens.json` next to the server file, mode 0600.
//!
//! With `$FINANCE_DESKTOP_SERVER_FILE` set (dev runs, screenshots) the
//! keyring is skipped, so a scratch config never touches the real one.
//!
//! Next to the tokens, in the same store, the resume key for each server
//! (`URL#resume`; crate::signin explains it): one per server and machine,
//! not per account, because signing out takes the account's profile (and
//! with it any `URL#profile` token key) off this machine.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::cache;
pub use crate::client::{DeviceInfo, UserInfo, WhoAmI};

#[cfg(feature = "keyring")]
const SERVICE: &str = "finance-desktop";

fn norm(server: &str) -> String {
    server.trim().trim_end_matches('/').to_string()
}

pub fn tokens_file() -> PathBuf {
    cache::server_file().with_file_name("tokens.json")
}

fn use_keyring() -> bool {
    cfg!(feature = "keyring") && std::env::var_os("FINANCE_DESKTOP_SERVER_FILE").is_none()
}

fn read_file() -> BTreeMap<String, String> {
    std::fs::read(tokens_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_file(map: &BTreeMap<String, String>) -> Result<()> {
    let path = tokens_file();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(&serde_json::to_vec_pretty(map)?)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(feature = "keyring")]
fn keyring_get(server: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, server).ok()?.get_password().ok()
}
#[cfg(not(feature = "keyring"))]
fn keyring_get(_server: &str) -> Option<String> {
    None
}

/// Ok(true) when the keyring took it.
#[cfg(feature = "keyring")]
fn keyring_set(server: &str, token: Option<&str>) -> bool {
    let Ok(e) = keyring::Entry::new(SERVICE, server) else { return false };
    match token {
        Some(t) => e.set_password(t).is_ok(),
        None => e.delete_credential().is_ok(),
    }
}
#[cfg(not(feature = "keyring"))]
fn keyring_set(_server: &str, _token: Option<&str>) -> bool {
    false
}

/// Where this machine keeps its tokens (for the Doctor and Settings).
pub fn store_label() -> String {
    if use_keyring() {
        format!("the OS keyring (else {})", tokens_file().display())
    } else {
        tokens_file().display().to_string()
    }
}

/// The token for `server`, if this machine is paired with it.
pub fn token(server: &str) -> Option<String> {
    if let Some(t) = std::env::var("FINANCE_TOKEN").ok().filter(|t| !t.trim().is_empty()) {
        return Some(t.trim().to_string());
    }
    let s = crate::profiles::token_key(server);
    if use_keyring() {
        if let Some(t) = keyring_get(&s) {
            return Some(t);
        }
    }
    read_file().get(&s).cloned()
}

/// Is a token kept under `key` (a profile's token_key)? `$FINANCE_TOKEN`
/// counts as one for every key, and so does a working keyring that can't
/// read this entry (locked): only a sure "no" says no. With no keyring at
/// all, tokens.json alone answers.
pub fn has_token_key(key: &str) -> bool {
    if std::env::var("FINANCE_TOKEN").is_ok_and(|t| !t.trim().is_empty()) {
        return true;
    }
    (use_keyring() && keyring_has(key)) || read_file().contains_key(key)
}

#[cfg(feature = "keyring")]
fn keyring_has(key: &str) -> bool {
    match keyring::Entry::new(SERVICE, key).map(|e| e.get_password()) {
        Ok(Ok(_)) => true,
        Ok(Err(keyring::Error::NoEntry)) => false,
        _ => crate::replica::keyring_available(),
    }
}
#[cfg(not(feature = "keyring"))]
fn keyring_has(_key: &str) -> bool {
    false
}

/// Keep `token` for `server` (the keyring when it works, else the file).
pub fn save_token(server: &str, token: &str) -> Result<()> {
    keep(&crate::profiles::token_key(server), token)
}

/// Keep `value` under `key` (the keyring when it works, else the file).
fn keep(key: &str, value: &str) -> Result<()> {
    let mut map = read_file();
    if use_keyring() && keyring_set(key, Some(value)) {
        // one place only: drop a stale file copy
        if map.remove(key).is_some() {
            write_file(&map)?;
        }
        return Ok(());
    }
    map.insert(key.to_string(), value.to_string());
    write_file(&map)
}

/// Forget the token for `server` (the active account's, when it is that
/// server).
pub fn forget(server: &str) -> Result<()> {
    forget_key(&crate::profiles::token_key(server))
}

/// Forget the token kept under `key` (a profile's token_key).
pub fn forget_key(key: &str) -> Result<()> {
    let s = key.to_string();
    if use_keyring() {
        keyring_set(&s, None);
    }
    let mut map = read_file();
    if map.remove(&s).is_some() {
        write_file(&map)?;
    }
    Ok(())
}

fn resume_slot(server: &str) -> String {
    format!("{}#resume", norm(server))
}

/// The resume key this machine keeps for `server`, if it ever signed out
/// of it with a code to jump back in.
pub fn resume_key(server: &str) -> Option<String> {
    let slot = resume_slot(server);
    if use_keyring() {
        if let Some(k) = keyring_get(&slot) {
            return Some(k);
        }
    }
    read_file().get(&slot).cloned()
}

/// [`resume_key`], made now (32 random bytes, hex) when there is none.
pub fn ensure_resume_key(server: &str) -> Result<String> {
    if let Some(k) = resume_key(server) {
        return Ok(k);
    }
    use chacha20poly1305::aead::Generate;
    let b: [u8; 32] = chacha20poly1305::Key::generate().into();
    let k: String = b.iter().map(|x| format!("{x:02x}")).collect();
    keep(&resume_slot(server), &k)?;
    Ok(k)
}

/// Forget the resume key for `server` (Delete all on this computer).
pub fn forget_resume_key(server: &str) -> Result<()> {
    forget_key(&resume_slot(server))
}

/// `Authorization` header value for `server`, when paired.
pub fn bearer(server: &str) -> Option<String> {
    // the demo has no token, and no keyring is asked for one
    if crate::demo::is_url(server) {
        return None;
    }
    token(server).map(|t| format!("Bearer {t}"))
}

/// A sensible device name: the hostname.
pub fn default_device_name() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|h| format!("{h} (desktop)"))
        .unwrap_or_else(|| "desktop".into())
}

fn read_json(r: &mut ureq::http::Response<ureq::Body>) -> serde_json::Value {
    r.body_mut().with_config().limit(1 << 20).read_to_vec().ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Trade a code for a token, and keep it: a pairing code, or the code to
/// jump back in this machine got when it signed out (sent with its resume
/// key, which the server wants with it). Blocking.
pub fn pair(server: &str, code: &str, device_name: &str) -> Result<DeviceInfo> {
    crate::demo::refuse(server)?;
    let s = norm(server);
    let agent = crate::tailnet::agent(Duration::from_secs(15));
    let mut body = serde_json::json!({"code": code, "device_name": device_name});
    if let Some(k) = resume_key(&s) {
        body["resume_key"] = k.into();
    }
    let mut r = agent
        .post(&format!("{s}/api/pair"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(&body)?)
        .with_context(|| format!("POST {s}/api/pair"))?;
    let v = read_json(&mut r);
    if r.status() != 200 {
        bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(&format!("HTTP {}", r.status())));
    }
    let token = v.get("token").and_then(|t| t.as_str()).context("no token in the reply")?;
    save_token(&s, token)?;
    Ok(serde_json::from_value(v.get("device").cloned().unwrap_or_default()).unwrap_or_default())
}

/// What a new account came with (`POST /api/signup`).
#[derive(Debug, Clone, Default)]
pub struct NewAccount {
    pub device: DeviceInfo,
    /// the first account on the server: its admin
    pub first: bool,
    /// it has its data on the server (every account made today does; an old
    /// server said false for an invited account)
    pub data: bool,
    /// the server's words about it, if any
    pub note: Option<String>,
}

/// Make an account on `server` (the first one, with an invite, or where
/// signup is open) and keep this machine's token for it. Blocking.
pub fn signup(server: &str, name: &str, email: &str, invite: &str, device_name: &str) -> Result<NewAccount> {
    crate::demo::refuse(server)?;
    let s = norm(server);
    let agent = crate::tailnet::agent(Duration::from_secs(15));
    let body = serde_json::json!({"name": name, "email": email, "invite": invite, "device_name": device_name});
    let mut r = agent
        .post(&format!("{s}/api/signup"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(&body)?)
        .with_context(|| format!("POST {s}/api/signup"))?;
    let v = read_json(&mut r);
    if r.status() != 200 {
        bail!("{}", crate::signin::refusal(r.status().as_u16(), &v));
    }
    let token = v.get("token").and_then(|t| t.as_str()).context("no token in the reply")?;
    save_token(&s, token)?;
    Ok(NewAccount {
        device: serde_json::from_value(v.get("device").cloned().unwrap_or_default()).unwrap_or_default(),
        first: v["first"].as_bool().unwrap_or(false),
        data: v["data"].as_bool().unwrap_or(true),
        note: v["note"].as_str().map(String::from),
    })
}

/// Sign this device out on the server (its token stops working). With
/// `jump_back`, it sends this machine's resume key (made now if need be)
/// and returns the server's code to jump back in (None from a server older
/// than that); without, nothing is left to come back with (*Sign out for
/// good*). Blocking. The local token stays: the caller forgets it.
pub fn logout(server: &str, jump_back: bool) -> Result<Option<String>> {
    crate::demo::refuse(server)?;
    let s = norm(server);
    let mut body = serde_json::json!({});
    if jump_back {
        body["resume_key"] = ensure_resume_key(&s)?.into();
    }
    let agent = crate::tailnet::agent(Duration::from_secs(15));
    let mut req = agent.post(&format!("{s}/api/logout")).header("Content-Type", "application/json");
    if let Some(b) = bearer(&s) {
        req = req.header("Authorization", &b);
    }
    let mut r = req.send(serde_json::to_vec(&body)?).with_context(|| format!("POST {s}/api/logout"))?;
    let v = read_json(&mut r);
    if r.status() != 200 {
        bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(&format!("HTTP {}", r.status())));
    }
    Ok(v.get("reconnect_code").and_then(|c| c.as_str()).filter(|c| !c.is_empty()).map(String::from))
}

/// Who the server thinks this machine is. Blocking.
pub fn whoami(server: &str) -> Result<WhoAmI> {
    if crate::demo::is_url(server) {
        return Ok(serde_json::from_value(crate::demo::whoami())?);
    }
    let s = norm(server);
    let agent = crate::tailnet::agent(Duration::from_secs(6));
    let mut req = agent.get(&format!("{s}/api/whoami"));
    if let Some(b) = bearer(&s) {
        req = req.header("Authorization", &b);
    }
    let mut r = req.call().with_context(|| format!("GET {s}/api/whoami"))?;
    if r.status() == 404 {
        bail!("this server predates pairing (no /api/whoami)");
    }
    if r.status() != 200 {
        bail!("GET /api/whoami: HTTP {}", r.status());
    }
    Ok(serde_json::from_value(read_json(&mut r)).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trip_is_private() {
        let dir = std::env::temp_dir().join(format!("finance-tokens-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FINANCE_DESKTOP_SERVER_FILE", dir.join("server"));
        std::env::remove_var("FINANCE_TOKEN");
        assert_eq!(token("http://server:8787"), None);
        save_token("http://server:8787/", "fin_abc").unwrap();
        assert_eq!(token("http://server:8787").as_deref(), Some("fin_abc"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(tokens_file()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        forget("http://server:8787").unwrap();
        assert_eq!(token("http://server:8787"), None);
        // the resume key: one per server, kept until Delete all, never a token
        assert_eq!(resume_key("http://server:8787"), None);
        let k = ensure_resume_key("http://server:8787/").unwrap();
        assert!(k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()), "{k}");
        assert_eq!(ensure_resume_key("http://server:8787").unwrap(), k, "made once");
        assert_ne!(ensure_resume_key("http://other:8787").unwrap(), k);
        assert!(!has_token_key("http://server:8787"), "a resume key is not a sign-in");
        forget_key("http://server:8787").unwrap();
        assert_eq!(resume_key("http://server:8787").as_deref(), Some(k.as_str()), "signing out keeps it");
        forget_resume_key("http://server:8787").unwrap();
        assert_eq!(resume_key("http://server:8787"), None);
        let w = WhoAmI { paired: true, device: Some(DeviceInfo { id: 1, name: "laptop".into() }), ..Default::default() };
        assert_eq!(w.label(), "paired as laptop");
        assert_eq!(WhoAmI::default().label(), "not paired");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
