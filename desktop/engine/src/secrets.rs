//! The secrets store: API keys at rest, encrypted, per user.
//!
//! * `$FINANCE_HOME/secrets/<uid>.json.enc`: a JSON object {KEY: value},
//!   sealed with XChaCha20-Poly1305 (a fresh 24-byte nonce per write, the
//!   file's user id as associated data, so a file copied onto another user
//!   doesn't open).
//! * `$FINANCE_HOME/secrets/master.key` (or `$FINANCE_MASTER_KEY_FILE`): 32
//!   random bytes, created 0600 on the first write. A key file rather than a
//!   passphrase, so the server restarts unattended (the owner's decision).
//!
//! Never under data/ (the replicated, backed-up, served tree) and never in
//! finance.db: [`dir`] refuses a location inside the data dir.
//!
//! Provider order for a secret key ([`lookup`]): the store, then `.env`, then
//! the process environment; [`configured_via`] says which one answered.
//! Reads are cached on the file's mtime and size, so a write (by the server
//! or the CLI) is live on the next read, with no restart.
//!
//! Nothing here logs or returns a value except [`lookup`] to the fetchers;
//! [`hint`] gives the last 4 characters at most.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::config;

/// The user whose secrets a lookup reads: the active tenant's
/// (crate::tenant; the owner, user 1, outside any).
pub fn active_user() -> i64 {
    crate::tenant::uid()
}

/// Every key a source reads. Only these go through the store.
pub const SECRET_KEYS: [&str; 7] = [
    "IBKR_FLEX_TOKEN", "IBKR_FLEX_QUERY_ID", "BINANCE_API_KEY", "BINANCE_API_SECRET", "OKX_API_KEY", "OKX_API_SECRET", "OKX_PASSPHRASE",
];

const MAGIC: &[u8; 6] = b"FSEC1\n";

pub fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS.contains(&key)
}

/// `$FINANCE_HOME/secrets` (the server's, shared by every user: one file
/// per uid), refused when it would sit inside the data dir.
pub fn dir() -> Result<PathBuf> {
    let d = config::root_home().join("secrets");
    let data = config::root_data_dir();
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canon(&d).starts_with(canon(&data)) {
        bail!("the secrets dir {} is inside the data dir {}: refusing (data/ is replicated and served)", d.display(), data.display());
    }
    Ok(d)
}

pub fn master_key_path() -> Result<PathBuf> {
    match std::env::var_os("FINANCE_MASTER_KEY_FILE").filter(|v| !v.is_empty()) {
        Some(p) => Ok(PathBuf::from(p)),
        None => Ok(dir()?.join("master.key")),
    }
}

pub fn store_path(uid: i64) -> Result<PathBuf> {
    Ok(dir()?.join(format!("{uid}.json.enc")))
}

/// `n` bytes from the OS's CSPRNG.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("the OS random source");
    b
}

#[cfg(unix)]
fn private_dir(d: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn private_dir(d: &Path) -> Result<()> {
    std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    Ok(())
}

/// Write `bytes` to `path` as a 0600 file, atomically (tmp + rename).
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        private_dir(d)?;
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
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// The master key; created (0600) when `create` and missing.
fn master_key(create: bool) -> Result<Option<[u8; 32]>> {
    let p = master_key_path()?;
    match std::fs::read(&p) {
        Ok(b) => {
            let b: [u8; 32] = b.try_into().map_err(|_| anyhow::anyhow!("{} is not a 32-byte key", p.display()))?;
            Ok(Some(b))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            let key = random_bytes(32);
            write_private(&p, &key)?;
            Ok(Some(key.try_into().expect("32 bytes")))
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

fn aad(uid: i64) -> Vec<u8> {
    format!("finance-secrets:{uid}").into_bytes()
}

/// Seal a map for user `uid`: MAGIC, nonce, ciphertext.
pub fn seal(key: &[u8; 32], uid: i64, map: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::generate();
    let plain = serde_json::to_vec(map)?;
    let ct = cipher.encrypt(&nonce, Payload { msg: &plain, aad: &aad(uid) }).map_err(|_| anyhow::anyhow!("encryption failed"))?;
    let mut out = Vec::with_capacity(MAGIC.len() + 24 + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open what [`seal`] wrote. A wrong key, user or a torn file fails.
pub fn open_sealed(key: &[u8; 32], uid: i64, bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    if bytes.len() < MAGIC.len() + 24 + 16 || &bytes[..MAGIC.len()] != MAGIC {
        bail!("not a secrets file");
    }
    let nonce = XNonce::try_from(&bytes[MAGIC.len()..MAGIC.len() + 24]).expect("24 bytes");
    let cipher = XChaCha20Poly1305::new(key.into());
    let plain = cipher
        .decrypt(&nonce, Payload { msg: &bytes[MAGIC.len() + 24..], aad: &aad(uid) })
        .map_err(|_| anyhow::anyhow!("the secrets file doesn't open with this master key"))?;
    Ok(serde_json::from_slice(&plain).context("secrets file content")?)
}

type Stamp = (PathBuf, Option<std::time::SystemTime>, u64);

/// (which file, its mtime and size) -> the decrypted map; one entry per
/// file (per user).
static CACHE: Mutex<Vec<(Stamp, BTreeMap<String, String>)>> = Mutex::new(Vec::new());

fn stamp(p: &Path) -> Stamp {
    let md = std::fs::metadata(p).ok();
    (p.to_path_buf(), md.as_ref().and_then(|m| m.modified().ok()), md.map(|m| m.len()).unwrap_or(0))
}

/// The stored secrets of `uid` (empty when there is no file yet).
pub fn load(uid: i64) -> Result<BTreeMap<String, String>> {
    let p = store_path(uid)?;
    let st = stamp(&p);
    if let Ok(c) = CACHE.lock() {
        if let Some((_, m)) = c.iter().find(|(s, _)| *s == st) {
            return Ok(m.clone());
        }
    }
    let map = match std::fs::read(&p) {
        Ok(bytes) => {
            let key = master_key(false)?.ok_or_else(|| anyhow::anyhow!("{} exists but the master key is missing", p.display()))?;
            open_sealed(&key, uid, &bytes)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    if let Ok(mut c) = CACHE.lock() {
        c.retain(|(s, _)| s.0 != st.0);
        c.push((st, map.clone()));
    }
    Ok(map)
}

fn save(uid: i64, map: &BTreeMap<String, String>) -> Result<()> {
    let key = master_key(true)?.expect("created");
    let p = store_path(uid)?;
    write_private(&p, &seal(&key, uid, map)?)?;
    if let Ok(mut c) = CACHE.lock() {
        c.retain(|(s, _)| s.0 != p);
    }
    Ok(())
}

/// Remove `uid`'s secrets file (the user is purged).
pub fn purge(uid: i64) -> Result<()> {
    let p = store_path(uid)?;
    match std::fs::remove_file(&p) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
    }
    if let Ok(mut c) = CACHE.lock() {
        c.retain(|(s, _)| s.0 != p);
    }
    Ok(())
}

/// Set some keys of `uid` (only [`SECRET_KEYS`]; empty values are refused).
pub fn put(uid: i64, fields: &BTreeMap<String, String>) -> Result<()> {
    let mut map = load(uid)?;
    for (k, v) in fields {
        if !is_secret_key(k) {
            bail!("{k} is not a source secret");
        }
        let v = v.trim();
        if v.is_empty() {
            bail!("{k} is empty");
        }
        map.insert(k.clone(), v.to_string());
    }
    save(uid, &map)
}

/// Drop some keys of `uid`.
pub fn remove(uid: i64, keys: &[&str]) -> Result<()> {
    let mut map = load(uid)?;
    let before = map.len();
    for k in keys {
        map.remove(*k);
    }
    if map.len() != before {
        save(uid, &map)?;
    }
    Ok(())
}

/// The stored value for the active user (never logged).
pub fn stored(key: &str) -> Option<String> {
    load(active_user()).ok()?.get(key).cloned().filter(|v| !v.is_empty())
}

/// Where a key's value comes from, in provider order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Store,
    DotEnv,
    Env,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Store => "store",
            Via::DotEnv => ".env",
            Via::Env => "env",
        }
    }
}

/// The value and its provider: the store, then .env, then the environment.
/// .env and the environment are the owner's (they belong to the server's
/// process and tree): another user's keys come from their store only.
pub fn lookup(key: &str) -> Option<(String, Via)> {
    if let Some(v) = stored(key) {
        return Some((v, Via::Store));
    }
    if !crate::tenant::active().legacy {
        return None;
    }
    if let Some(v) = config::dotenv_value(key) {
        return Some((v, Via::DotEnv));
    }
    std::env::var(key).ok().filter(|v| !v.trim().is_empty()).map(|v| (v, Via::Env))
}

/// Where a missing key is set, for an error message: "in .env" for the
/// owner (as always), the app's Sources settings for anyone else (their
/// keys live only in the store).
pub fn where_to_set() -> &'static str {
    if crate::tenant::active().legacy {
        "in .env"
    } else {
        "(set the source up in Settings, Sources)"
    }
}

pub fn configured_via(key: &str) -> Option<Via> {
    lookup(key).map(|(_, via)| via)
}

/// What a client may see of a value: the last 4 characters, and only of a
/// value long enough that 4 characters give nothing useful away.
pub fn hint(value: &str) -> String {
    let n = value.chars().count();
    if n < 12 {
        return "set".into();
    }
    let tail: String = value.chars().skip(n - 4).collect();
    format!("…{tail}")
}

/// Import the source keys from `.env` into the store of `uid`. Keeps
/// `.env.bak` (the file as it was) and comments the imported lines out of
/// `.env`. Returns the keys imported.
pub fn import_env(uid: i64) -> Result<Vec<String>> {
    // .env is the owner's tree's: nobody else may take its keys
    if uid != crate::tenant::OWNER_UID || !crate::tenant::active().legacy {
        bail!(".env belongs to the owner's tree: only the owner can import it");
    }
    let path = config::env_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut found = BTreeMap::new();
    let mut out = String::with_capacity(text.len() + 256);
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        let parsed = (!t.starts_with('#')).then(|| t.split_once('=')).flatten().map(|(k, v)| {
            (k.trim().to_string(), v.trim().trim_matches('"').trim_matches('\'').to_string())
        });
        match parsed {
            Some((k, v)) if is_secret_key(&k) && !v.is_empty() => {
                found.insert(k, v);
                out.push_str("# imported into the secrets store: ");
                out.push_str(line);
            }
            _ => out.push_str(line),
        }
    }
    if found.is_empty() {
        return Ok(vec![]);
    }
    put(uid, &found)?;
    let bak = path.with_file_name(".env.bak");
    write_private(&bak, text.as_bytes())?;
    write_private(&path, out.as_bytes())?;
    config::reload_dotenv();
    Ok(found.into_keys().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_round_trip_and_tamper() {
        let key: [u8; 32] = random_bytes(32).try_into().unwrap();
        let mut m = BTreeMap::new();
        m.insert("BINANCE_API_KEY".to_string(), "abcdefghijklmnop1234".to_string());
        let sealed = seal(&key, 1, &m).unwrap();
        assert!(!sealed.windows(8).any(|w| w == b"abcdefgh"), "plaintext must not appear");
        assert_eq!(open_sealed(&key, 1, &sealed).unwrap(), m);
        // another user's id (associated data) or another key: refused
        assert!(open_sealed(&key, 2, &sealed).is_err());
        let other: [u8; 32] = random_bytes(32).try_into().unwrap();
        assert!(open_sealed(&other, 1, &sealed).is_err());
        let mut bad = sealed.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(open_sealed(&key, 1, &bad).is_err());
        // fresh nonce per seal
        assert_ne!(seal(&key, 1, &m).unwrap(), sealed);
    }

    #[test]
    fn hints_give_away_at_most_four() {
        assert_eq!(hint("short"), "set");
        assert_eq!(hint("abcdefghijklmnopWXYZ"), "…WXYZ");
    }
}
