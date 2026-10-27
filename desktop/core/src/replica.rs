//! The optional encrypted local replica ("Encrypt this computer's copy",
//! Settings; off by default). The laptop has no full-disk encryption, so
//! the copy of the owner's finances it keeps can be stored encrypted:
//!
//! * `finance.db` and `export.json` in the cache dir become
//!   `finance.db.enc` and `export.json.enc`: XChaCha20-Poly1305 with a
//!   32-byte data key, one sealed blob per file (they are a few MB), a fresh
//!   nonce per write, the file's name as associated data (so the two can't be
//!   swapped). Writes go to an encrypted `.tmp` and are renamed: no plaintext
//!   ever touches the disk ([`write`]).
//! * Readers decrypt into memory: [`open_db`] deserializes the database
//!   into an in-memory SQLite connection, [`read`] returns the bytes. Every
//!   reader of the replica (Ledger, Bundle, Extra, the Doctor, the Upload
//!   page, the Downloads watcher, the examples) goes through them. With the
//!   switch off they open the plain files, as before.
//! * Small personal config files ([`read_text`], [`write_text`]: the app's
//!   invest.toml, targets.toml, downloads.toml) are sealed the same way.
//! * The key: random, kept in the OS keyring (Secret Service, Keychain,
//!   Credential Manager). Without a keyring the key is derived from a
//!   passphrase with Argon2id (64 MiB, 3 passes), asked for at app start
//!   ([`unlock`]). The key never sits next to the data: `replica.json`
//!   holds only the mode, the salt and a check value.
//! * Lost key: the replica is a cache. [`forget`] deletes the encrypted
//!   files and the next sync fetches everything again.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::cache;

const MAGIC: &[u8; 8] = b"FINREP1\n";
pub const CONFIG_FILE: &str = "replica.json";
const CHECK: &[u8] = b"finance-replica-check";
#[cfg(feature = "keyring")]
const SERVICE: &str = "finance-desktop";

pub type Key = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeySource {
    Keyring,
    Passphrase,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub encrypted: bool,
    pub key: KeySource,
    /// Argon2id salt and costs (passphrase mode)
    #[serde(default)]
    pub salt: String,
    #[serde(default)]
    pub m_kib: u32,
    #[serde(default)]
    pub t: u32,
    /// [`CHECK`] sealed with the key: a wrong key fails cleanly
    pub check: String,
}

/// The unlocked key, in memory only.
static KEY: Mutex<Option<Key>> = Mutex::new(None);

fn key() -> Option<Key> {
    *KEY.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_key(k: Option<Key>) {
    *KEY.lock().unwrap_or_else(|e| e.into_inner()) = k;
}

// ---------- sealing ----------

fn aad(name: &str) -> Vec<u8> {
    format!("finance-replica:{name}").into_bytes()
}

/// The name a sealed file is bound to: `finance.db` for `finance.db.enc`.
fn bound_name(p: &Path) -> String {
    let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    n.strip_suffix(".enc").unwrap_or(&n).to_string()
}

pub fn seal(key: &Key, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::generate();
    let ct = cipher.encrypt(&nonce, Payload { msg: plain, aad: &aad(name) }).map_err(|_| anyhow::anyhow!("encryption failed"))?;
    let mut out = Vec::with_capacity(MAGIC.len() + 24 + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open_sealed(key: &Key, name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < MAGIC.len() + 24 + 16 || &bytes[..MAGIC.len()] != MAGIC {
        bail!("{name}: not an encrypted replica file");
    }
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(&XNonce::try_from(&bytes[MAGIC.len()..MAGIC.len() + 24]).expect("24 bytes"), Payload { msg: &bytes[MAGIC.len() + 24..], aad: &aad(name) })
        .map_err(|_| anyhow::anyhow!("{name}: the key doesn't open it (wrong passphrase or another key)"))
}

fn random_key() -> Key {
    chacha20poly1305::Key::generate().into()
}

/// Argon2id(passphrase, salt) -> a 32-byte key.
pub fn derive(passphrase: &str, salt: &[u8], m_kib: u32, t: u32) -> Result<Key> {
    let params = argon2::Params::new(m_kib, t, 1, Some(32)).map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
    let mut out = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
    Ok(out)
}

// ---------- the vault: one data dir ----------

/// The replica dir and what's encrypted in it. The public functions use the
/// app's cache dir and the in-memory key; tests use their own.
pub struct Vault {
    pub dir: PathBuf,
}

/// The production costs; tests use cheaper ones.
pub const ARGON_M_KIB: u32 = 64 * 1024;
pub const ARGON_T: u32 = 3;

fn enc_path(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}.enc", p.display()))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
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
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn remove(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", p.display())),
    }
}

impl Vault {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Vault { dir: dir.into() }
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join(CONFIG_FILE)
    }

    pub fn config(&self) -> Option<Config> {
        std::fs::read(self.config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).filter(|c: &Config| c.encrypted)
    }

    pub fn encrypted(&self) -> bool {
        self.config().is_some()
    }

    /// Does `key` open this vault?
    pub fn check(&self, key: &Key) -> Result<()> {
        let c = self.config().context("the local copy isn't encrypted")?;
        let blob = hex_decode(&c.check)?;
        open_sealed(key, "check", &blob).map(|_| ()).map_err(|_| anyhow::anyhow!("wrong passphrase"))
    }

    /// The replica files this vault encrypts.
    fn files(&self) -> [PathBuf; 3] {
        [self.dir.join(cache::DB_FILE), self.dir.join(cache::BUNDLE_FILE), self.dir.join(cache::AVATAR_FILE)]
    }

    /// Turn encryption on with `key`: write the config, then seal the
    /// existing replica (and `extra` files) and remove their plaintext.
    pub fn enable(&self, key: &Key, source: KeySource, salt: &[u8], m_kib: u32, t: u32, extra: &[PathBuf]) -> Result<()> {
        if self.encrypted() {
            bail!("the local copy is already encrypted");
        }
        let cfg = Config {
            encrypted: true, key: source, salt: hex_encode(salt), m_kib, t,
            check: hex_encode(&seal(key, "check", CHECK)?),
        };
        // seal first: a failure leaves the plain copy and the switch off
        let all: Vec<PathBuf> = self.files().into_iter().chain(extra.iter().cloned()).collect();
        for p in &all {
            if let Ok(plain) = std::fs::read(p) {
                write_atomic(&enc_path(p), &seal(key, &bound_name(p), &plain)?)?;
            }
        }
        std::fs::create_dir_all(&self.dir)?;
        write_atomic(&self.config_path(), &serde_json::to_vec_pretty(&cfg)?)?;
        for p in &all {
            if enc_path(p).exists() {
                remove(p)?;
            }
        }
        Ok(())
    }

    /// Turn it off: decrypt back to plain files, drop the config.
    pub fn disable(&self, key: &Key, extra: &[PathBuf]) -> Result<()> {
        self.check(key)?;
        let all: Vec<PathBuf> = self.files().into_iter().chain(extra.iter().cloned()).collect();
        for p in &all {
            let e = enc_path(p);
            if let Ok(sealed) = std::fs::read(&e) {
                write_atomic(p, &open_sealed(key, &bound_name(p), &sealed)?)?;
            }
        }
        remove(&self.config_path())?;
        for p in &all {
            remove(&enc_path(p))?;
        }
        Ok(())
    }

    /// The key is lost: drop the sealed files and the config (a sync
    /// fetches the replica again).
    pub fn forget(&self, extra: &[PathBuf]) -> Result<()> {
        for p in self.files().iter().chain(extra) {
            remove(&enc_path(p))?;
            remove(&PathBuf::from(format!("{}.enc.tmp", p.display())))?;
        }
        remove(&self.config_path())
    }

    /// The file on disk for `path`: its `.enc` twin when this vault is on.
    pub fn stored(&self, path: &Path) -> PathBuf {
        let e = enc_path(path);
        if (self.encrypted() && e.exists()) || (!path.exists() && e.exists()) {
            e
        } else {
            path.to_path_buf()
        }
    }

    /// `path`'s bytes, decrypted in memory when it is stored sealed.
    pub fn read(&self, path: &Path, key: Option<&Key>) -> Result<Vec<u8>> {
        let s = self.stored(path);
        let raw = std::fs::read(&s).with_context(|| format!("reading {}", s.display()))?;
        if s == path {
            return Ok(raw);
        }
        let key = key.context("the local copy is encrypted and locked: unlock it first")?;
        open_sealed(key, &bound_name(path), &raw)
    }

    /// Write `name` in the dir: sealed when on (no plaintext on disk).
    pub fn write(&self, name: &str, bytes: &[u8], key: Option<&Key>) -> Result<()> {
        let p = self.dir.join(name);
        if self.encrypted() {
            let key = key.context("the local copy is encrypted and locked: unlock it to sync")?;
            write_atomic(&enc_path(&p), &seal(key, name, bytes)?)?;
            remove(&p)
        } else {
            write_atomic(&p, bytes)
        }
    }
}

pub fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(s.len() % 2 == 0, "bad hex");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| anyhow::anyhow!("bad hex"))).collect()
}

/// A database image as an in-memory connection (read-only).
pub fn mem_db(mut bytes: Vec<u8>) -> Result<Connection> {
    anyhow::ensure!(bytes.starts_with(b"SQLite format 3\0"), "not a SQLite database");
    if bytes.len() >= 100 {
        // a WAL-mode header doesn't open from memory
        bytes[18] = 1;
        bytes[19] = 1;
    }
    let mut c = Connection::open_in_memory()?;
    let n = bytes.len();
    c.deserialize_read_exact("main", &bytes[..], n, true).context("loading the replica into memory")?;
    Ok(c)
}

// ---------- the app's vault ----------

fn vault() -> Vault {
    Vault::new(cache::dir())
}

fn use_keyring() -> bool {
    cfg!(feature = "keyring") && std::env::var_os("FINANCE_DESKTOP_SERVER_FILE").is_none()
}

#[cfg(feature = "keyring")]
fn keyring_user() -> String {
    format!("replica-key:{}", cache::dir().display())
}

#[cfg(feature = "keyring")]
fn keyring_get() -> Option<Key> {
    keyring_get_as(&keyring_user())
}

#[cfg(feature = "keyring")]
fn keyring_get_as(user: &str) -> Option<Key> {
    let s = keyring::Entry::new(SERVICE, user).ok()?.get_password().ok()?;
    hex_decode(&s).ok()?.try_into().ok()
}
#[cfg(not(feature = "keyring"))]
fn keyring_get() -> Option<Key> {
    None
}

#[cfg(feature = "keyring")]
fn keyring_set(k: Option<&Key>) -> bool {
    let Ok(e) = keyring::Entry::new(SERVICE, &keyring_user()) else { return false };
    match k {
        Some(k) => e.set_password(&hex_encode(k)).is_ok() && keyring_get().as_ref() == Some(k),
        None => e.delete_credential().is_ok(),
    }
}
#[cfg(not(feature = "keyring"))]
fn keyring_set(_k: Option<&Key>) -> bool {
    false
}

/// An account that goes (profiles.rs): the keyring key of the copy in
/// `dir`, if it ever had one. The folder is the caller's to delete. A
/// no-op without the keyring.
pub fn forget_key_of(dir: &Path) {
    #[cfg(feature = "keyring")]
    if use_keyring() {
        if let Ok(e) = keyring::Entry::new(SERVICE, &format!("replica-key:{}", dir.display())) {
            let _ = e.delete_credential();
        }
    }
    let _ = dir;
}

/// The encrypted copy's keyring key follows its folder (the key's entry is
/// named after it): profiles.rs moves the first replica into its account's
/// folder with this. A no-op without the keyring.
pub fn move_key(from: &Path, to: &Path) {
    #[cfg(feature = "keyring")]
    if use_keyring() {
        let user = |d: &Path| format!("replica-key:{}", d.display());
        if let (Ok(old), Ok(new)) = (keyring::Entry::new(SERVICE, &user(from)), keyring::Entry::new(SERVICE, &user(to))) {
            if let Ok(v) = old.get_password() {
                if new.set_password(&v).is_ok() && new.get_password().ok().as_deref() == Some(v.as_str()) {
                    let _ = old.delete_credential();
                }
            }
        }
    }
    let _ = (from, to);
}

/// What the switch and the Doctor show.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub encrypted: bool,
    pub unlocked: bool,
    pub via: Option<KeySource>,
}

impl Status {
    pub fn label(&self) -> String {
        match (self.encrypted, self.via, self.unlocked) {
            (false, _, _) => "not encrypted".into(),
            (true, Some(KeySource::Keyring), true) => "encrypted (key in the OS keyring)".into(),
            (true, Some(KeySource::Keyring), false) => "encrypted, but the keyring has no key for it".into(),
            (true, _, true) => "encrypted (passphrase, unlocked)".into(),
            (true, _, false) => "encrypted (passphrase, locked)".into(),
        }
    }
}

pub fn status() -> Status {
    status_of(&cache::dir())
}

/// The state of the replica kept in `dir` (the Doctor checks the folder it
/// is given, so tests on a scratch folder never see the real copy's state).
pub fn status_of(dir: &Path) -> Status {
    let c = Vault::new(dir).config();
    Status { encrypted: c.is_some(), unlocked: c.is_some() && key().is_some(), via: c.map(|c| c.key) }
}

pub fn is_encrypted() -> bool {
    vault().encrypted()
}

/// Encrypted and no key in memory: the app shows the unlock screen.
pub fn locked() -> bool {
    is_encrypted() && key().is_none()
}

/// At startup: a keyring-kept key unlocks without asking. Ok(true) when
/// unlocked (or nothing to unlock).
pub fn auto_unlock() -> bool {
    let Some(c) = vault().config() else { return true };
    if key().is_some() {
        return true;
    }
    if c.key == KeySource::Keyring && use_keyring() {
        if let Some(k) = keyring_get() {
            if vault().check(&k).is_ok() {
                set_key(Some(k));
                return true;
            }
        }
    }
    false
}

/// Unlock a passphrase-keyed replica. Blocking (~0.5 s of Argon2id).
pub fn unlock(passphrase: &str) -> Result<()> {
    let v = vault();
    let c = v.config().context("the local copy isn't encrypted")?;
    if c.key == KeySource::Keyring {
        bail!("this copy's key was in the OS keyring, and the keyring doesn't have it: start over (sync again)");
    }
    let k = derive(passphrase, &hex_decode(&c.salt)?, c.m_kib, c.t)?;
    v.check(&k)?;
    set_key(Some(k));
    Ok(())
}

/// Can the OS keyring keep the key here?
pub fn keyring_available() -> bool {
    if !use_keyring() {
        return false;
    }
    static PROBED: Mutex<Option<bool>> = Mutex::new(None);
    let mut g = PROBED.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(|| {
        #[cfg(feature = "keyring")]
        {
            let Ok(e) = keyring::Entry::new(SERVICE, "replica-probe") else { return false };
            let ok = e.set_password("probe").is_ok() && e.get_password().ok().as_deref() == Some("probe");
            let _ = e.delete_credential();
            ok
        }
        #[cfg(not(feature = "keyring"))]
        false
    })
}

/// Turn it on: a random key in the keyring, else a passphrase-derived key
/// (`passphrase` must then be given). `extra`: config files to seal too.
pub fn enable(passphrase: Option<&str>, extra: &[PathBuf]) -> Result<KeySource> {
    // made-up data: nothing to protect, and no key to look after
    crate::demo::refuse("")?;
    let v = vault();
    if keyring_available() && passphrase.is_none() {
        let k = random_key();
        if !keyring_set(Some(&k)) {
            bail!("the OS keyring refused the key: choose a passphrase instead");
        }
        if let Err(e) = v.enable(&k, KeySource::Keyring, &[], 0, 0, extra) {
            keyring_set(None);
            return Err(e);
        }
        set_key(Some(k));
        return Ok(KeySource::Keyring);
    }
    let p = passphrase.filter(|p| !p.is_empty()).context("no OS keyring here: choose a passphrase")?;
    anyhow::ensure!(p.chars().count() >= 8, "the passphrase needs at least 8 characters");
    let salt = random_key();
    let k = derive(p, &salt[..16], ARGON_M_KIB, ARGON_T)?;
    v.enable(&k, KeySource::Passphrase, &salt[..16], ARGON_M_KIB, ARGON_T, extra)?;
    set_key(Some(k));
    Ok(KeySource::Passphrase)
}

/// Turn it off: decrypt back to plain files (unlocked only).
pub fn disable(extra: &[PathBuf]) -> Result<()> {
    let k = key().context("unlock the local copy first")?;
    let via = vault().config().map(|c| c.key);
    vault().disable(&k, extra)?;
    if via == Some(KeySource::Keyring) {
        keyring_set(None);
    }
    set_key(None);
    Ok(())
}

/// The key is gone: drop the sealed copy (sync fetches it again).
pub fn forget(extra: &[PathBuf]) -> Result<()> {
    vault().forget(extra)?;
    if use_keyring() {
        keyring_set(None);
    }
    set_key(None);
    Ok(())
}

/// For the examples and other dev tools: unlock from the keyring, else
/// from `$FINANCE_DESKTOP_PASSPHRASE`. A plain replica needs nothing.
pub fn unlock_for_tools() -> Result<()> {
    if auto_unlock() {
        return Ok(());
    }
    match std::env::var("FINANCE_DESKTOP_PASSPHRASE") {
        Ok(p) => unlock(&p),
        Err(_) => bail!("the local copy is encrypted with a passphrase: set FINANCE_DESKTOP_PASSPHRASE"),
    }
}

/// Unlock the copy kept in `dir` (any account's, not just the active one)
/// for a process with no window (`anser mcp`): from the keyring,
/// else from `$FINANCE_DESKTOP_PASSPHRASE`. A plain copy needs nothing.
/// Afterwards the readers open that folder's files.
pub fn unlock_dir(dir: &Path) -> Result<()> {
    let v = Vault::new(dir);
    let Some(c) = v.config() else { return Ok(()) };
    if key().is_some() {
        return Ok(());
    }
    #[cfg(feature = "keyring")]
    if c.key == KeySource::Keyring && use_keyring() {
        if let Some(k) = keyring_get_as(&format!("replica-key:{}", dir.display())) {
            if v.check(&k).is_ok() {
                set_key(Some(k));
                return Ok(());
            }
        }
    }
    if c.key == KeySource::Keyring {
        bail!("this copy is encrypted with a key in the OS keyring, and the keyring has none for it (is it locked?)");
    }
    let Ok(p) = std::env::var("FINANCE_DESKTOP_PASSPHRASE") else {
        bail!("this copy is encrypted with a passphrase: set FINANCE_DESKTOP_PASSPHRASE");
    };
    let k = derive(&p, &hex_decode(&c.salt)?, c.m_kib, c.t)?;
    v.check(&k)?;
    set_key(Some(k));
    Ok(())
}

// ---------- the readers and writers ----------

/// The replica database, read-only: the file itself, or (encrypted) a
/// private in-memory copy. Every replica reader opens it through here.
pub fn open_db(path: &Path) -> Result<Connection> {
    let v = Vault::new(path.parent().unwrap_or(Path::new(".")));
    let stored = v.stored(path);
    if stored == path {
        return Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_context(|| format!("opening {}", path.display()));
    }
    mem_db(v.read(path, key().as_ref())?)
}

/// A replica file's bytes (decrypted when sealed).
pub fn read(path: &Path) -> Result<Vec<u8>> {
    Vault::new(path.parent().unwrap_or(Path::new("."))).read(path, key().as_ref())
}

/// The file that holds `path` on disk (its size and mtime).
pub fn stored_path(path: &Path) -> PathBuf {
    Vault::new(path.parent().unwrap_or(Path::new("."))).stored(path)
}

/// Write a replica file into the cache dir (sync): sealed when on.
pub fn write(name: &str, bytes: &[u8]) -> Result<()> {
    vault().write(name, bytes, key().as_ref())
}

/// A small personal config file (invest.toml, ...): its `.enc` twin
/// decrypted when there is one, else the plain file.
pub fn read_text(path: &Path) -> Option<String> {
    let e = enc_path(path);
    if e.exists() {
        let k = key()?;
        return open_sealed(&k, &bound_name(path), &std::fs::read(&e).ok()?).ok().and_then(|b| String::from_utf8(b).ok());
    }
    std::fs::read_to_string(path).ok()
}

/// Write a personal config file: sealed while the replica is encrypted.
pub fn write_text(path: &Path, text: &str) -> Result<()> {
    if is_encrypted() {
        let k = key().context("unlock the local copy first")?;
        write_atomic(&enc_path(path), &seal(&k, &bound_name(path), text.as_bytes())?)?;
        return remove(path);
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    remove(&enc_path(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("finance-replica-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn db_image() -> Vec<u8> {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE ingested_files(path TEXT, file_hash TEXT); INSERT INTO ingested_files VALUES('/t/secret-bank.csv','h1');").unwrap();
        c.serialize("main").unwrap().to_vec()
    }

    const BUNDLE: &[u8] = br#"{"version":1,"net_worth":{"total_hkd":123456.78}}"#;

    /// Nothing in `dir` holds a plaintext marker.
    fn no_plaintext(dir: &Path) {
        for e in std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().is_file()) {
            let b = std::fs::read(e.path()).unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            assert!(!b.starts_with(b"SQLite format 3"), "{name} is a plaintext database");
            for marker in [&b"secret-bank"[..], b"123456.78", b"total_hkd"] {
                assert!(!b.windows(marker.len()).any(|w| w == marker), "{name} holds plaintext");
            }
        }
    }

    #[test]
    fn round_trip_and_no_plaintext_after_a_sync() {
        let dir = tmp("rt");
        let v = Vault::new(&dir);
        let key = random_key();
        let salt = [1u8; 16];
        let k = derive("a passphrase", &salt, 256, 1).unwrap();
        assert_ne!(k, key);
        v.enable(&k, KeySource::Passphrase, &salt, 256, 1, &[]).unwrap();
        // a sync: both files written through the vault
        v.write(cache::DB_FILE, &db_image(), Some(&k)).unwrap();
        v.write(cache::BUNDLE_FILE, BUNDLE, Some(&k)).unwrap();
        no_plaintext(&dir);
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| n.ends_with(".enc") || n == CONFIG_FILE), "{names:?}");
        // read back in memory
        assert_eq!(v.read(&dir.join(cache::BUNDLE_FILE), Some(&k)).unwrap(), BUNDLE);
        let c = mem_db(v.read(&dir.join(cache::DB_FILE), Some(&k)).unwrap()).unwrap();
        let p: String = c.query_row("SELECT path FROM ingested_files", [], |r| r.get(0)).unwrap();
        assert_eq!(p, "/t/secret-bank.csv");
        // locked: a clean error, no write either
        assert!(v.read(&dir.join(cache::DB_FILE), None).unwrap_err().to_string().contains("locked"));
        assert!(v.write(cache::DB_FILE, b"x", None).is_err());
        no_plaintext(&dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn enabling_on_an_empty_replica_seals_the_first_sync() {
        // the wizard's Protect card turns it on before the first sync
        let dir = tmp("first");
        let v = Vault::new(&dir);
        let salt = [2u8; 16];
        let k = derive("a passphrase", &salt, 256, 1).unwrap();
        v.enable(&k, KeySource::Passphrase, &salt, 256, 1, &[]).unwrap();
        assert!(v.encrypted());
        v.write(cache::DB_FILE, &db_image(), Some(&k)).unwrap();
        v.write(cache::BUNDLE_FILE, BUNDLE, Some(&k)).unwrap();
        no_plaintext(&dir);
        assert_eq!(v.read(&dir.join(cache::BUNDLE_FILE), Some(&k)).unwrap(), BUNDLE);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wrong_key_fails_cleanly() {
        let dir = tmp("wrong");
        let v = Vault::new(&dir);
        let salt = [2u8; 16];
        let k = derive("right one!", &salt, 256, 1).unwrap();
        v.write(cache::BUNDLE_FILE, BUNDLE, None).unwrap();
        v.enable(&k, KeySource::Passphrase, &salt, 256, 1, &[]).unwrap();
        let bad = derive("wrong one!", &salt, 256, 1).unwrap();
        assert_eq!(v.check(&bad).unwrap_err().to_string(), "wrong passphrase");
        let e = v.read(&dir.join(cache::BUNDLE_FILE), Some(&bad)).unwrap_err().to_string();
        assert!(e.contains("doesn't open"), "{e}");
        assert!(v.disable(&bad, &[]).is_err());
        assert!(v.encrypted(), "still on after a failed disable");
        // a sealed file can't stand in for the other one
        std::fs::copy(dir.join("export.json.enc"), dir.join("finance.db.enc")).unwrap();
        assert!(v.read(&dir.join(cache::DB_FILE), Some(&k)).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn toggling_both_ways() {
        let dir = tmp("toggle");
        let cfg_dir = dir.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let extra = vec![cfg_dir.join("invest.toml"), cfg_dir.join("targets.toml")];
        std::fs::write(&extra[0], "return_pct = 6\n[plan]\nibkr = 5000\n").unwrap();
        let v = Vault::new(&dir);
        v.write(cache::DB_FILE, &db_image(), None).unwrap();
        v.write(cache::BUNDLE_FILE, BUNDLE, None).unwrap();
        assert!(!v.encrypted());
        assert_eq!(v.stored(&dir.join(cache::DB_FILE)), dir.join(cache::DB_FILE));

        let k = random_key();
        v.enable(&k, KeySource::Keyring, &[], 0, 0, &extra).unwrap();
        assert!(v.encrypted());
        no_plaintext(&dir);
        assert!(!extra[0].exists() && enc_path(&extra[0]).exists());
        assert!(!extra[1].exists() && !enc_path(&extra[1]).exists(), "a missing file stays missing");
        assert_eq!(v.read(&dir.join(cache::BUNDLE_FILE), Some(&k)).unwrap(), BUNDLE);
        assert!(v.enable(&k, KeySource::Keyring, &[], 0, 0, &extra).is_err(), "already on");

        v.disable(&k, &extra).unwrap();
        assert!(!v.encrypted());
        assert_eq!(std::fs::read(dir.join(cache::BUNDLE_FILE)).unwrap(), BUNDLE);
        assert!(std::fs::read(dir.join(cache::DB_FILE)).unwrap().starts_with(b"SQLite format 3"));
        assert!(std::fs::read_to_string(&extra[0]).unwrap().contains("ibkr = 5000"));
        assert!(!dir.join("finance.db.enc").exists() && !enc_path(&extra[0]).exists());

        // on again, then the key is lost: forget leaves an empty cache
        v.enable(&k, KeySource::Keyring, &[], 0, 0, &extra).unwrap();
        v.forget(&extra).unwrap();
        assert!(!v.encrypted());
        assert!(!dir.join("finance.db").exists() && !dir.join("finance.db.enc").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plain_open_db_is_unchanged() {
        let dir = tmp("plain");
        std::fs::write(dir.join("finance.db"), db_image()).unwrap();
        let c = open_db(&dir.join("finance.db")).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM ingested_files", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert!(open_db(&dir.join("missing.db")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
