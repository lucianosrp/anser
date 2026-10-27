//! Who may talk to this server: users, paired devices, one-time pairing
//! codes. Kept in `server.db`, a separate SQLite file next to finance.db:
//! never replicated (`/api/db.sqlite` is finance.db only), never served, not
//! in `anser-server backup`'s output.
//!
//! * users (id, name, role, home, created): the owner's tree is user 1,
//!   admin, home = FINANCE_HOME, made in place on the first start
//!   ([`bootstrap`]); every other user's home is `$FINANCE_HOME/users/<id>`
//!   (homes.rs makes it with the account; finance_engine::tenant resolves
//!   it). Ids are never reused (meta `last_uid`), so a removed user's kept
//!   tree never passes to someone new.
//! * devices (id, user_id, name, token_hash, scopes, created, last_seen): a
//!   device holds a random 256-bit bearer token; only its SHA-256 is kept.
//!   A browser login is a device too (its token rides in a cookie).
//! * pair_codes (code_hash, user_id, expires, used): 8 characters, 10
//!   minutes, one use. `anser-server pair` or the admin action makes one.
//! * reconnects (user_id, device_id, device_name, code_hash, resume_hash,
//!   created, fails): a device signed out with a code to jump back in
//!   ([`sign_out`]). The code (8 characters, shown on that device) works
//!   only with the resume key that device keeps (a random string, sent with
//!   its logout and again with its sign-in), once, with no time limit. Only
//!   hashes are kept. One row per signed-out device and user; an admin's
//!   sign-out of that device ([`revoke`], device_id), removing the user,
//!   or [`MAX_RECONNECT_FAILS`] wrong codes sent with its resume key end it.
//! * invites (code_hash, created_by, expires, used, used_by): an admin's
//!   one-time code, valid 24 hours, that lets someone create an account
//!   ([`signup`]).
//!
//! Accounts ([`signup`]): on a fresh install (no finance.db at the first
//! start) there are no users until the first account is made, from the app
//! or by `anser-server pair`; that one becomes user 1, admin, home =
//! FINANCE_HOME. After that a new account needs an invite, unless
//! FINANCE_ALLOW_SIGNUP opens self-signup (or an admin runs `anser-server
//! user add`). Every account gets its own tree, so every account sees only
//! its own data; an account made before per-user trees gets one at the
//! next start. With more than one tree the server is a household one and
//! the gate wants a device for reads too ([`effective_mode`]).
//!
//! [`Mode`] (FINANCE_AUTH=off|writes|all): an install that already had a
//! finance.db when server.db was first made defaults to `writes` (every
//! non-GET needs a token; reads keep working, so nothing breaks on deploy),
//! a new install to `all`. The default is stored in server.db; the env
//! variable overrides it, the admin action rewrites it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use finance_engine::tenant::{self, Tenant};
use finance_engine::{config, db, profile, secrets};

pub const CODE_TTL_MS: i64 = 10 * 60 * 1000;
/// No 0/O, 1/I/L: read off a terminal or a phone screen without doubt.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
pub const CODE_LEN: usize = 8;
/// The scope of an MCP key (a device row whose token only `/mcp` accepts).
pub const MCP_SCOPE: &str = "mcp";
/// Keys one user may hold.
pub const MCP_KEYS_MAX: usize = 20;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS users (
  id      INTEGER PRIMARY KEY,
  name    TEXT NOT NULL,
  role    TEXT NOT NULL DEFAULT 'user',      -- admin | user
  home    TEXT NOT NULL,                     -- the user's data tree
  created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
  id         INTEGER PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users(id),
  name       TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,           -- sha256 hex of the bearer token
  scopes     TEXT NOT NULL DEFAULT 'read,write',
  created    INTEGER NOT NULL,
  last_seen  INTEGER
);
CREATE TABLE IF NOT EXISTS pair_codes (
  code_hash TEXT PRIMARY KEY,                -- sha256 hex of the code
  user_id   INTEGER NOT NULL,
  expires   INTEGER NOT NULL,
  used      INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS reconnects (
  id          INTEGER PRIMARY KEY,
  user_id     INTEGER NOT NULL,
  device_id   INTEGER NOT NULL,              -- the device signed out (its row is gone)
  device_name TEXT NOT NULL,
  code_hash   TEXT NOT NULL UNIQUE,          -- sha256 hex of the code
  resume_hash TEXT NOT NULL,                 -- sha256 hex of the device's resume key
  created     INTEGER NOT NULL,
  fails       INTEGER NOT NULL DEFAULT 0     -- wrong codes sent with that resume key
);
CREATE TABLE IF NOT EXISTS invites (
  code_hash  TEXT PRIMARY KEY,               -- sha256 hex of the code
  created_by INTEGER NOT NULL,
  expires    INTEGER NOT NULL,
  used       INTEGER NOT NULL DEFAULT 0,
  used_by    INTEGER
);
";

pub const INVITE_TTL_MS: i64 = 24 * 3600 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Writes,
    All,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Mode::Off),
            "writes" => Some(Mode::Writes),
            "all" => Some(Mode::All),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Writes => "writes",
            Mode::All => "all",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Device {
    pub id: i64,
    pub user_id: i64,
    pub user_name: String,
    pub role: String,
    pub name: String,
    pub scopes: String,
    pub created: i64,
    pub last_seen: Option<i64>,
    /// the user has a data tree: without one, no data route is open (an
    /// account made before per-user trees gets one at the next start)
    pub has_data: bool,
    /// the user's tree (users.home): never sent to a client
    #[serde(skip)]
    pub home: String,
}

impl Device {
    pub fn admin(&self) -> bool {
        self.role == "admin"
    }

    /// An MCP key: good for `/mcp` and nothing else (gate.rs refuses it on
    /// every other route).
    pub fn mcp_only(&self) -> bool {
        self.scopes.split(',').any(|s| s.trim() == MCP_SCOPE)
    }

    /// The tenant this device's requests run as (None: no data tree).
    pub fn tenant(&self) -> Option<Tenant> {
        Tenant::for_user(self.user_id, &self.home)
    }
}

/// Why a pairing code or an invite was refused. Its words go to the client
/// as `error`, its [`CodeError::reason`] as `reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeError {
    Wrong,
    Used,
    Expired,
    /// a jump-back-in code after too many wrong tries from its device
    Locked,
}

impl CodeError {
    pub fn reason(self) -> &'static str {
        match self {
            CodeError::Wrong => "wrong",
            CodeError::Used => "used",
            CodeError::Expired => "expired",
            CodeError::Locked => "locked",
        }
    }
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CodeError::Wrong => "That code isn't right: check it and try again.",
            CodeError::Used => "That code was already used: ask for a new one.",
            CodeError::Expired => "That code has expired: ask for a new one.",
            CodeError::Locked => "Too many wrong codes from this device: sign in with a new pairing code.",
        })
    }
}

impl std::error::Error for CodeError {}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub role: String,
    pub home: String,
    pub created: i64,
    pub devices: i64,
    pub email: Option<String>,
}

pub fn path() -> PathBuf {
    config::root_data_dir().join("server.db")
}

fn now_ms() -> i64 {
    db::now_ms()
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Constant-time byte comparison (hash lookups go through SQLite's index,
/// but a client-supplied value compared in Rust goes through this).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Open (creating) server.db, 0600.
pub fn open() -> Result<Connection> {
    let p = path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let conn = Connection::open(&p).with_context(|| format!("opening {}", p.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA)?;
    // users.email arrived with accounts made from the app
    let has_email: bool = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'email'", [], |r| r.get::<_, i64>(0))? > 0;
    if !has_email {
        conn.execute_batch("ALTER TABLE users ADD COLUMN email TEXT")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(conn)
}

fn meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get(0)).optional().ok().flatten()
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", params![key, value])?;
    Ok(())
}

/// A server setting kept in server.db's meta table (never a secret).
pub fn setting(key: &str) -> Option<String> {
    if !path().exists() {
        return None;
    }
    meta(&open().ok()?, key)
}

/// Store (Some) or clear (None) a server setting.
pub fn set_setting(key: &str, value: Option<&str>) -> Result<()> {
    let conn = open()?;
    match value {
        Some(v) => set_meta(&conn, key, v),
        None => {
            conn.execute("DELETE FROM meta WHERE key = ?", [key])?;
            Ok(())
        }
    }
}

/// What [`bootstrap`] did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Bootstrap {
    /// server.db was created now
    pub created: bool,
    /// the copy of finance.db made before anything changed
    pub backup: Option<PathBuf>,
    pub default_mode: Option<Mode>,
    pub profile_seeded: bool,
    /// finance.db gained the categories table (Phase 3) now
    pub categories_migrated: bool,
    /// accounts that got their data tree now (made before Phase 5)
    pub homes_made: Vec<i64>,
}

/// First start (and every start after, idempotently): the owner's tree
/// becomes user 1, admin, in place. Before anything touches finance.db, a
/// backup lands in data/backups/pre-auth/ (a directory the daily backup
/// never prunes). Then finance.db gains the profile and user_settings
/// tables, and an existing tree gets the owner's profile.
pub fn bootstrap() -> Result<Bootstrap> {
    Tenant::owner().enter(bootstrap_owner)
}

fn bootstrap_owner() -> Result<Bootstrap> {
    let mut out = Bootstrap::default();
    let fresh = !path().exists();
    let dbp = config::db_path();
    let existing = dbp.exists();
    if fresh && existing {
        let dir = db::backup_dir().join("pre-auth");
        out.backup = Some(db::backup_into(&dbp, &dir, 1000).context("backing up finance.db before the auth migration")?);
    }
    let conn = open()?;
    if meta(&conn, "auth_default").is_none() {
        // decided once, at the first start of an auth-aware server: closed,
        // an existing tree included (the owner opens it up with `auth writes`)
        let m = Mode::All;
        set_meta(&conn, "auth_default", m.as_str())?;
        out.default_mode = Some(m);
    }
    if fresh {
        // an existing tree is the owner's, made user 1 below; a fresh
        // install waits for its first account (the app, or `anser-server pair`)
        set_meta(&conn, "owner", if existing { "tree" } else { "pending" })?;
    }
    out.created = fresh;
    let fin = db::open()?;
    profile::ensure(&fin)?;
    // Phase 3: the categories table and the managed rules (a backup first,
    // once, when an existing tree meets it)
    if existing && !finance_engine::categories::has_table(&fin, "categories") && out.backup.is_none() {
        let dir = db::backup_dir().join("pre-categories");
        out.backup = Some(db::backup_into(&dbp, &dir, 1000).context("backing up finance.db before the categories migration")?);
    }
    if finance_engine::categories::ensure(&fin)? {
        out.categories_migrated = true;
    }
    // Phase 4: the investment accounts and instruments, empty when missing
    finance_engine::invest_setup::ensure(&fin)?;
    if existing && fresh {
        out.profile_seeded = profile::seed_owner(&fin)?;
    }
    if meta(&conn, "owner").as_deref() != Some("pending") {
        let name = profile::get(&fin)?.and_then(|p| p.name);
        insert_owner(&conn, name.as_deref(), None)?;
    }
    // Phase 5: an account made before per-user trees gets its own now
    let homeless: Vec<(i64, String)> = {
        let mut st = conn.prepare("SELECT id, name FROM users WHERE home = '' AND id != 1")?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    for (id, name) in homeless {
        let t = crate::homes::init(id, &name)?;
        conn.execute("UPDATE users SET home = ?1 WHERE id = ?2", params![t.home.display().to_string(), id])?;
        out.homes_made.push(id);
    }
    Ok(out)
}

/// User 1, admin, home = this tree, unless there is one already. Ok(true)
/// when it was made now.
fn insert_owner(conn: &Connection, name: Option<&str>, email: Option<&str>) -> Result<bool> {
    let has_owner: bool = conn.query_row("SELECT COUNT(*) FROM users WHERE id = 1", [], |r| r.get::<_, i64>(0))? > 0;
    if has_owner {
        return Ok(false);
    }
    let name = name.map(str::to_string).or_else(|| std::env::var("USER").ok()).unwrap_or_else(|| "owner".into());
    conn.execute(
        "INSERT INTO users (id, name, role, home, created, email) VALUES (1, ?1, 'admin', ?2, ?3, ?4)",
        params![name, config::root_home().display().to_string(), now_ms(), email],
    )?;
    set_meta(conn, "owner", "claimed")?;
    Ok(true)
}

/// A new non-owner account with its own data tree (homes::init) in
/// `conn`'s transaction. Its id is never one used before (a removed user's
/// kept tree stays theirs): past the highest id ever given out and past any
/// users/<id> directory on disk.
fn insert_user(conn: &Connection, name: &str, email: Option<&str>, role: &str) -> Result<i64> {
    let max_row: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM users", [], |r| r.get(0))?;
    let max_meta: i64 = meta(conn, "last_uid").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut id = max_row.max(max_meta).max(tenant::OWNER_UID) + 1;
    while Tenant::default_home(id).exists() {
        id += 1;
    }
    conn.execute("INSERT INTO users (id, name, role, home, created, email) VALUES (?1, ?2, ?3, '', ?4, ?5)", params![id, name, role, now_ms(), email])?;
    set_meta(conn, "last_uid", &id.to_string())?;
    let t = crate::homes::init(id, name)?;
    conn.execute("UPDATE users SET home = ?1 WHERE id = ?2", params![t.home.display().to_string(), id])?;
    Ok(id)
}

/// `anser-server user add`: an account made by an admin on the server's
/// shell (no invite), with its tree. Returns its id.
pub fn add_user(name: &str, email: Option<&str>, admin: bool) -> Result<i64> {
    let (name, email) = check_identity(name, email)?;
    let mut conn = open()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if tx.query_row("SELECT COUNT(*) FROM users WHERE id = 1", [], |r| r.get::<_, i64>(0))? == 0 {
        anyhow::bail!("this server has no owner yet: make the first account (the app, or anser-server pair) first");
    }
    let id = insert_user(&tx, &name, email.as_deref(), if admin { "admin" } else { "user" })?;
    tx.commit()?;
    Ok(id)
}

/// A user by id or by name (names compared case-insensitively; an
/// ambiguous name is an error).
pub fn find_user(key: &str) -> Result<User> {
    let all = users()?;
    if let Ok(id) = key.trim().parse::<i64>() {
        return all.into_iter().find(|u| u.id == id).with_context(|| format!("no user {id}"));
    }
    let k = key.trim().to_lowercase();
    let mut hits: Vec<User> = all.into_iter().filter(|u| u.name.to_lowercase() == k).collect();
    match hits.len() {
        0 => anyhow::bail!("no user named {key:?} (anser-server user list)"),
        1 => Ok(hits.remove(0)),
        _ => anyhow::bail!("more than one user is named {key:?}: use the id"),
    }
}

/// The tenant of a user by id or name (None-tree users refused).
pub fn tenant_of(key: &str) -> Result<Tenant> {
    let u = find_user(key)?;
    Tenant::for_user(u.id, &u.home).with_context(|| format!("user {} has no data tree", u.id))
}

/// Rename a user (their profile name is theirs to change; this is the
/// account's name in server.db).
pub fn rename_user(id: i64, name: &str) -> Result<bool> {
    let (name, _) = check_identity(name, None)?;
    Ok(open()?.execute("UPDATE users SET name = ?1 WHERE id = ?2", params![name, id])? > 0)
}

/// What removing a user did.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Removed {
    pub devices: usize,
    /// the tree, kept on disk unless purged
    pub home: String,
    pub purged: bool,
}

/// Remove user `id`: their devices, codes and account. Their tree and
/// secrets stay on disk unless `purge`. The owner (user 1) can't be
/// removed.
pub fn remove_user(id: i64, purge: bool) -> Result<Removed> {
    if id == tenant::OWNER_UID {
        anyhow::bail!("user 1 owns this server and its tree: it can't be removed");
    }
    let mut conn = open()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let home: Option<String> = tx.query_row("SELECT home FROM users WHERE id = ?", [id], |r| r.get(0)).optional()?;
    let Some(home) = home else { anyhow::bail!("no user {id}") };
    let devices = tx.execute("DELETE FROM devices WHERE user_id = ?", [id])?;
    tx.execute("DELETE FROM pair_codes WHERE user_id = ?", [id])?;
    tx.execute("DELETE FROM reconnects WHERE user_id = ?", [id])?;
    tx.execute("DELETE FROM users WHERE id = ?", [id])?;
    tx.execute("DELETE FROM meta WHERE key = ?", [format!("backup_recipient:{id}")])?;
    tx.commit()?;
    let mut purged = false;
    if purge {
        purged = crate::homes::purge(id, &home)?;
        secrets::purge(id)?;
    }
    Ok(Removed { devices, home, purged })
}

/// `anser-server pair` on a fresh install: whoever has the server's shell is
/// its owner, so user 1 is made now (named after $USER) when nobody has
/// claimed the server from the app yet.
pub fn ensure_owner() -> Result<()> {
    let conn = open()?;
    insert_owner(&conn, None, None)?;
    Ok(())
}

pub fn user_count() -> Result<i64> {
    Ok(open()?.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
}

/// FINANCE_ALLOW_SIGNUP (default off): anyone who reaches the server may
/// make an account without an invite.
pub fn signup_open() -> bool {
    config::env("FINANCE_ALLOW_SIGNUP").is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// How a new account can be made here: "first" (no users yet: the first
/// account becomes the admin), "open" (FINANCE_ALLOW_SIGNUP), "invite".
pub fn signup_mode() -> &'static str {
    if user_count().unwrap_or(1) == 0 {
        "first"
    } else if signup_open() {
        "open"
    } else {
        "invite"
    }
}

/// The mode in force: FINANCE_AUTH, else the stored default (all when
/// server.db can't say).
pub fn mode() -> Mode {
    if let Some(m) = std::env::var("FINANCE_AUTH").ok().as_deref().and_then(Mode::parse) {
        return m;
    }
    open().ok().and_then(|c| meta(&c, "auth_default")).as_deref().and_then(Mode::parse).unwrap_or(Mode::All)
}

/// More than one account has a data tree: a household server.
pub fn household() -> bool {
    if !path().exists() {
        return false;
    }
    open()
        .and_then(|c| Ok(c.query_row("SELECT COUNT(*) FROM users WHERE home != ''", [], |r| r.get::<_, i64>(0))?))
        .map(|n| n > 1)
        .unwrap_or(false)
}

/// The mode the gate applies: [`mode`], except on a household server,
/// where it is always `all`. A request without a device would otherwise be
/// read (or, under `off`, written) as the owner's: any household member
/// who simply didn't sign in would see the owner's data.
pub fn effective_mode() -> Mode {
    if household() {
        Mode::All
    } else {
        mode()
    }
}

/// Is the mode pinned by the environment (then the admin switch can't move it)?
pub fn mode_from_env() -> bool {
    std::env::var("FINANCE_AUTH").ok().as_deref().and_then(Mode::parse).is_some()
}

/// The admin switch: store `m` as the default.
pub fn set_mode(m: Mode) -> Result<()> {
    set_meta(&open()?, "auth_default", m.as_str())
}

fn random_code() -> String {
    // 31 symbols; rejection sampling keeps every symbol equally likely
    let mut out = String::with_capacity(CODE_LEN);
    while out.len() < CODE_LEN {
        for b in secrets::random_bytes(16) {
            if (b as usize) < CODE_ALPHABET.len() * (256 / CODE_ALPHABET.len()) && out.len() < CODE_LEN {
                out.push(CODE_ALPHABET[b as usize % CODE_ALPHABET.len()] as char);
            }
        }
    }
    out
}

/// "abcd-efgh", " ABCDEFGH " -> "ABCDEFGH".
pub fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// A new one-time code for `user_id`, valid [`CODE_TTL_MS`]. Expired and
/// used codes are swept.
pub fn new_pair_code(user_id: i64) -> Result<(String, i64)> {
    let conn = open()?;
    let now = now_ms();
    conn.execute("DELETE FROM pair_codes WHERE expires < ?1 OR used = 1", [now])?;
    let code = random_code();
    let expires = now + CODE_TTL_MS;
    conn.execute("INSERT INTO pair_codes (code_hash, user_id, expires) VALUES (?1, ?2, ?3)", params![sha256_hex(&code), user_id, expires])?;
    Ok((code, expires))
}

/// A new one-time invite (an admin's "Invite someone"), valid 24 hours.
pub fn new_invite(created_by: i64) -> Result<(String, i64)> {
    let conn = open()?;
    let now = now_ms();
    // spent and expired invites stay a week, so a late try still hears why
    conn.execute("DELETE FROM invites WHERE expires < ?1", [now - 7 * 86_400_000])?;
    let code = random_code();
    let expires = now + INVITE_TTL_MS;
    conn.execute("INSERT INTO invites (code_hash, created_by, expires) VALUES (?1, ?2, ?3)", params![sha256_hex(&code), created_by, expires])?;
    Ok((code, expires))
}

/// Spend a code from `table` (pair_codes or invites): Ok(the row's user
/// column) once, then [`CodeError::Used`]; a past expiry is
/// [`CodeError::Expired`], an unknown code [`CodeError::Wrong`].
fn spend_code(conn: &Connection, table: &str, user_col: &str, code: &str) -> Result<i64> {
    let code = normalize_code(code);
    if code.len() != CODE_LEN {
        return Err(CodeError::Wrong.into());
    }
    let hash = sha256_hex(&code);
    let row: Option<(i64, i64, i64)> = conn
        .query_row(&format!("SELECT {user_col}, expires, used FROM {table} WHERE code_hash = ?1"), [&hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((uid, expires, used)) = row else { return Err(CodeError::Wrong.into()) };
    if used != 0 {
        return Err(CodeError::Used.into());
    }
    if expires < now_ms() {
        return Err(CodeError::Expired.into());
    }
    // the guard on used makes two requests racing for one code end with one winner
    if conn.execute(&format!("UPDATE {table} SET used = 1 WHERE code_hash = ?1 AND used = 0"), [&hash])? != 1 {
        return Err(CodeError::Used.into());
    }
    Ok(uid)
}

/// A device's name as given: no control characters, 80 at most.
fn clean_name(s: &str, fallback: &str) -> String {
    let name: String = s.chars().filter(|c| !c.is_control()).take(80).collect::<String>().trim().to_string();
    if name.is_empty() { fallback.to_string() } else { name }
}

fn add_device(conn: &Connection, user_id: i64, device_name: &str) -> Result<(String, Device)> {
    add_device_scoped(conn, user_id, device_name, "read,write")
}

fn add_device_scoped(conn: &Connection, user_id: i64, device_name: &str, scopes: &str) -> Result<(String, Device)> {
    let token = new_token();
    // past every signed-out device's id too: an admin's sign-out of one
    // (its id) must never reach a device paired since
    let id: i64 = conn.query_row(
        "SELECT MAX((SELECT COALESCE(MAX(id), 0) FROM devices), (SELECT COALESCE(MAX(device_id), 0) FROM reconnects)) + 1",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO devices (id, user_id, name, token_hash, scopes, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, user_id, clean_name(device_name, "device"), sha256_hex(&token), scopes, now_ms()],
    )?;
    let dev = device_by_id(conn, id)?.context("the device just added")?;
    Ok((token, dev))
}

/// What [`signup`] made.
#[derive(Debug)]
pub struct Signup {
    pub token: String,
    pub device: Device,
    pub user_id: i64,
    /// the first account on this server: admin, owner of the data tree
    pub first: bool,
}

/// Why an account can't be made (beyond a bad invite, a [`CodeError`]).
#[derive(Debug, PartialEq, Eq)]
pub enum SignupError {
    /// no invite and FINANCE_ALLOW_SIGNUP is off
    Closed,
    Invalid(&'static str),
}

impl std::fmt::Display for SignupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignupError::Closed => f.write_str("New accounts need an invite: ask an admin for one (Settings, Admin, Invite someone)."),
            SignupError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for SignupError {}

/// "Jane", "jane@example.com" -> checked and trimmed.
pub fn check_identity(name: &str, email: Option<&str>) -> Result<(String, Option<String>), SignupError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(SignupError::Invalid("Enter your name."));
    }
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(SignupError::Invalid("That name is too long."));
    }
    let email = email.map(str::trim).filter(|e| !e.is_empty());
    if let Some(e) = email {
        let ok = e.len() <= 200 && !e.chars().any(char::is_whitespace) && e.split_once('@').is_some_and(|(a, b)| !a.is_empty() && b.contains('.'));
        if !ok {
            return Err(SignupError::Invalid("That email doesn't look right."));
        }
    }
    Ok((name.to_string(), email.map(str::to_string)))
}

/// Make an account and pair the device that asked. The first account on a
/// server with no users becomes user 1, admin, owner of this tree. Any
/// other needs `invite` (or FINANCE_ALLOW_SIGNUP) and gets a tree of its own,
/// users/<id> (homes.rs). One transaction: two racing first signups can't
/// both become the admin, and a refused invite changes nothing.
pub fn signup(name: &str, email: Option<&str>, invite: Option<&str>, device_name: &str) -> Result<Signup> {
    let (name, email) = check_identity(name, email)?;
    let mut conn = open()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let users: i64 = tx.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
    let first = users == 0;
    let user_id = if first {
        insert_owner(&tx, Some(&name), email.as_deref())?;
        1
    } else {
        let invite = invite.map(str::trim).filter(|c| !c.is_empty());
        match invite {
            Some(code) => {
                spend_code(&tx, "invites", "created_by", code)?;
            }
            None if signup_open() => {}
            None => return Err(SignupError::Closed.into()),
        }
        let id = insert_user(&tx, &name, email.as_deref(), "user")?;
        if let Some(code) = invite {
            tx.execute("UPDATE invites SET used_by = ?1 WHERE code_hash = ?2", params![id, sha256_hex(&normalize_code(code))])?;
        }
        id
    };
    let (token, device) = add_device(&tx, user_id, device_name)?;
    tx.commit()?;
    Ok(Signup { token, device, user_id, first })
}

/// A new random 256-bit token: "fin_" + 64 hex.
pub fn new_token() -> String {
    format!("fin_{}", hex::encode(secrets::random_bytes(32)))
}

/// Trade a code for a device token. The code is spent even when the insert
/// below fails afterwards. A refused code is a [`CodeError`].
pub fn pair(code: &str, device_name: &str) -> Result<(String, Device)> {
    pair_with(code, device_name, None)
}

/// [`pair`], from a device that may hold a resume key: then the code may
/// also be that device's jump-back-in code ([`sign_out`]), and any such code
/// of the user it signs in as is spent (it is signed back in here).
pub fn pair_with(code: &str, device_name: &str, resume_key: Option<&str>) -> Result<(String, Device)> {
    let conn = open()?;
    let resume = resume_key.and_then(resume_hash);
    let user_id = match spend_code(&conn, "pair_codes", "user_id", code) {
        Err(e) if e.downcast_ref::<CodeError>() == Some(&CodeError::Wrong) => match &resume {
            Some(rh) => spend_reconnect(&conn, code, rh)?,
            None => return Err(e),
        },
        r => r?,
    };
    if let Some(rh) = &resume {
        conn.execute("DELETE FROM reconnects WHERE user_id = ?1 AND resume_hash = ?2", params![user_id, rh])?;
    }
    add_device(&conn, user_id, device_name)
}

fn device_by_id(conn: &Connection, id: i64) -> Result<Option<Device>> {
    Ok(conn
        .query_row(
            "SELECT d.id, d.user_id, u.name, u.role, d.name, d.scopes, d.created, d.last_seen, u.home
             FROM devices d JOIN users u ON u.id = d.user_id WHERE d.id = ?",
            [id],
            row_device,
        )
        .optional()?)
}

fn row_device(r: &rusqlite::Row) -> rusqlite::Result<Device> {
    Ok(Device {
        id: r.get(0)?,
        user_id: r.get(1)?,
        user_name: r.get(2)?,
        role: r.get(3)?,
        name: r.get(4)?,
        scopes: r.get(5)?,
        created: r.get(6)?,
        last_seen: r.get(7)?,
        has_data: !r.get::<_, String>(8)?.is_empty(),
        home: r.get(8)?,
    })
}

/// The device a bearer token belongs to (last_seen refreshed at most once
/// a minute). Anything malformed is simply no device.
pub fn device_for_token(token: &str) -> Option<Device> {
    let token = token.trim();
    if !token.starts_with("fin_") || token.len() != 68 {
        return None;
    }
    let conn = open().ok()?;
    let dev = conn
        .query_row(
            "SELECT d.id, d.user_id, u.name, u.role, d.name, d.scopes, d.created, d.last_seen, u.home
             FROM devices d JOIN users u ON u.id = d.user_id WHERE d.token_hash = ?",
            [sha256_hex(token)],
            row_device,
        )
        .optional()
        .ok()
        .flatten()?;
    let now = now_ms();
    if dev.last_seen.is_none_or(|t| now - t > 60_000) {
        let _ = conn.execute("UPDATE devices SET last_seen = ?1 WHERE id = ?2", params![now, dev.id]);
    }
    Some(dev)
}

/// Sign a device out: its token goes, and so does its code to jump back
/// in, if it signed out with one (an admin's Sign out of a lost device).
pub fn revoke(device_id: i64) -> Result<bool> {
    let conn = open()?;
    let n = conn.execute("DELETE FROM devices WHERE id = ?", [device_id])?;
    Ok(n + conn.execute("DELETE FROM reconnects WHERE device_id = ?", [device_id])? > 0)
}

/// Wrong codes sent with one resume key before its code stops working.
pub const MAX_RECONNECT_FAILS: i64 = 5;

/// A resume key as sent: 32 to 128 letters, digits, `-` or `_`; its hash.
fn resume_hash(key: &str) -> Option<String> {
    let k = key.trim();
    ((32..=128).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then(|| sha256_hex(k))
}

/// Sign `dev` out. With the resume key its device keeps, it gets a code to
/// jump back in (Some), which then works only with that key, once
/// ([`pair_with`]); an earlier one of this user and key goes. Without one
/// (an older client, or *Sign out for good*) nothing is left behind.
pub fn sign_out(dev: &Device, resume_key: Option<&str>) -> Result<Option<String>> {
    let mut conn = open()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM devices WHERE id = ?", [dev.id])?;
    let code = match resume_key.and_then(resume_hash) {
        Some(rh) => {
            tx.execute("DELETE FROM reconnects WHERE user_id = ?1 AND resume_hash = ?2", params![dev.user_id, rh])?;
            let code = loop {
                let c = random_code();
                let taken: bool = tx.query_row("SELECT COUNT(*) FROM reconnects WHERE code_hash = ?", [sha256_hex(&c)], |r| r.get::<_, i64>(0))? > 0;
                if !taken {
                    break c;
                }
            };
            tx.execute(
                "INSERT INTO reconnects (user_id, device_id, device_name, code_hash, resume_hash, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![dev.user_id, dev.id, dev.name, sha256_hex(&code), rh, now_ms()],
            )?;
            Some(code)
        }
        None => None,
    };
    tx.commit()?;
    Ok(code)
}

/// The user a jump-back-in code signs in as, sent with resume key hash
/// `rh`: spent now. A code that isn't this key's counts against every code
/// of the key; [`MAX_RECONNECT_FAILS`] lock them ([`CodeError::Locked`]).
fn spend_reconnect(conn: &Connection, code: &str, rh: &str) -> Result<i64> {
    let ch = sha256_hex(&normalize_code(code));
    let rows: Vec<(i64, String, i64, i64)> = {
        let mut st = conn.prepare("SELECT id, code_hash, user_id, fails FROM reconnects WHERE resume_hash = ?")?;
        let v = st.query_map([rh], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    if rows.is_empty() {
        return Err(CodeError::Wrong.into());
    }
    let Some((id, _, uid, fails)) = rows.into_iter().find(|r| ct_eq(r.1.as_bytes(), ch.as_bytes())) else {
        conn.execute("UPDATE reconnects SET fails = fails + 1 WHERE resume_hash = ?", [rh])?;
        return Err(CodeError::Wrong.into());
    };
    if fails >= MAX_RECONNECT_FAILS {
        return Err(CodeError::Locked.into());
    }
    // two requests racing for one code end with one winner
    if conn.execute("DELETE FROM reconnects WHERE id = ?", [id])? != 1 {
        return Err(CodeError::Used.into());
    }
    Ok(uid)
}

/// A device signed out with a code to jump back in (Settings, Admin, Users).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SignedOut {
    /// the device's id, as it was: an admin's Sign out of it ([`revoke`]) ends the code
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    /// when it signed out
    pub signed_out: i64,
}

pub fn signed_out() -> Result<Vec<SignedOut>> {
    let conn = open()?;
    let mut st = conn.prepare("SELECT device_id, user_id, device_name, created FROM reconnects ORDER BY device_id")?;
    let v = st
        .query_map([], |r| Ok(SignedOut { id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, signed_out: r.get(3)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

pub fn devices() -> Result<Vec<Device>> {
    let conn = open()?;
    let mut st = conn.prepare(
        "SELECT d.id, d.user_id, u.name, u.role, d.name, d.scopes, d.created, d.last_seen, u.home
         FROM devices d JOIN users u ON u.id = d.user_id ORDER BY d.id",
    )?;
    let v = st.query_map([], row_device)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

pub fn users() -> Result<Vec<User>> {
    let conn = open()?;
    let mut st = conn.prepare(
        "SELECT u.id, u.name, u.role, u.home, u.created, (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id), u.email FROM users u ORDER BY u.id",
    )?;
    let v = st
        .query_map([], |r| {
            Ok(User { id: r.get(0)?, name: r.get(1)?, role: r.get(2)?, home: r.get(3)?, created: r.get(4)?, devices: r.get(5)?, email: r.get(6)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// Failed pairing, login or signup attempts, counted per client: after
/// [`MAX_FAILS`] within the window a client is refused until its oldest
/// failure leaves the window (8 characters from 31 symbols is 2^39; this
/// keeps guessing hopeless). [`GLOBAL_MAX_FAILS`] across all clients is the
/// backstop against a guesser spread over many addresses.
///
/// A code made after a failure resets it: failures older than the newest
/// pairing or jump-back-in code are not counted ([`last_code_made`], read
/// from server.db, so `anser-server pair` in another process counts too).
const MAX_FAILS: usize = 10;
const GLOBAL_MAX_FAILS: usize = 100;
const FAIL_WINDOW_MS: i64 = 10 * 60 * 1000;

#[derive(Default)]
struct Throttle {
    by_client: std::collections::HashMap<String, Vec<i64>>,
    all: Vec<i64>,
}

impl Throttle {
    /// Drop failures out of the window, or from before the last code was made.
    fn prune(&mut self, now: i64, reset: i64) {
        let live = |t: &i64| now - t < FAIL_WINDOW_MS && *t > reset;
        self.all.retain(live);
        self.by_client.retain(|_, v| {
            v.retain(live);
            !v.is_empty()
        });
    }

    /// Milliseconds until `client` may try again; None when it may now.
    fn wait(&mut self, client: &str, now: i64, reset: i64) -> Option<i64> {
        self.prune(now, reset);
        let oldest = |v: &[i64], max: usize| (v.len() >= max).then(|| v.iter().min().map_or(0, |t| t + FAIL_WINDOW_MS - now));
        let own = self.by_client.get(client).and_then(|v| oldest(v, MAX_FAILS));
        own.max(oldest(&self.all, GLOBAL_MAX_FAILS))
    }

    fn note(&mut self, client: &str, now: i64) {
        self.all.push(now);
        self.by_client.entry(client.to_string()).or_default().push(now);
    }
}

static FAILS: std::sync::Mutex<Option<Throttle>> = std::sync::Mutex::new(None);

fn with_throttle<T>(f: impl FnOnce(&mut Throttle) -> T) -> Option<T> {
    FAILS.lock().ok().map(|mut g| f(g.get_or_insert_with(Throttle::default)))
}

/// When the newest pairing or jump-back-in code was made (0: none kept).
/// A pairing code lives [`CODE_TTL_MS`] and is swept after use or expiry,
/// so its birth is `expires - CODE_TTL_MS`.
fn last_code_made() -> i64 {
    let Ok(conn) = open() else { return 0 };
    let q = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, Option<i64>>(0)).ok().flatten();
    let pair = q("SELECT MAX(expires) FROM pair_codes").map_or(0, |e| e - CODE_TTL_MS);
    pair.max(q("SELECT MAX(created) FROM reconnects").unwrap_or(0))
}

/// Who a request is from: the peer's address, or when the peer is loopback
/// (tailscale serve proxies from 127.0.0.1) the right-most address of
/// X-Forwarded-For: the proxy appends the tailnet client to whatever the
/// client sent, so only the last entry is the proxy's own (the ones before
/// it are the client's to choose). From any other peer the header is
/// ignored: anyone could write it.
pub fn client_id(peer: Option<std::net::IpAddr>, forwarded_for: Option<&str>) -> String {
    let Some(peer) = peer else { return "unknown".into() };
    let fwd = forwarded_for.filter(|_| peer.is_loopback()).and_then(|v| v.rsplit(',').next()?.trim().parse::<std::net::IpAddr>().ok());
    fwd.unwrap_or(peer).to_string()
}

/// Milliseconds `client` must wait after too many wrong codes; None: may try.
pub fn throttled(client: &str) -> Option<i64> {
    let (now, reset) = (now_ms(), last_code_made());
    with_throttle(|t| t.wait(client, now, reset)).flatten()
}

pub fn note_failure(client: &str) {
    with_throttle(|t| t.note(client, now_ms()));
}

#[cfg(test)]
pub fn reset_failures() {
    with_throttle(|t| *t = Throttle::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_identity() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().ok();
        // behind tailscale serve: the forwarded client, the entry the proxy appended
        assert_eq!(client_id(ip("127.0.0.1"), Some("100.64.0.7")), "100.64.0.7");
        // what the client wrote before it can't pick a fresh identity per try
        assert_eq!(client_id(ip("127.0.0.1"), Some("6.6.6.6, 100.64.0.7")), "100.64.0.7");
        assert_eq!(client_id(ip("127.0.0.1"), Some("100.64.0.7, junk")), "127.0.0.1");
        assert_eq!(client_id(ip("::1"), Some("fd7a::1")), "fd7a::1");
        // loopback without the header, or with nothing valid: the peer
        assert_eq!(client_id(ip("127.0.0.1"), None), "127.0.0.1");
        assert_eq!(client_id(ip("127.0.0.1"), Some("nope")), "127.0.0.1");
        // anyone else cannot choose who they are
        assert_eq!(client_id(ip("100.64.0.9"), Some("100.64.0.7")), "100.64.0.9");
        assert_eq!(client_id(None, Some("100.64.0.7")), "unknown");
    }

    #[test]
    fn throttle_is_per_client_with_a_global_ceiling() {
        let mut t = Throttle::default();
        let now = 1_000_000_000;
        for i in 0..MAX_FAILS as i64 {
            assert_eq!(t.wait("a", now + i, 0), None);
            t.note("a", now + i);
        }
        // a is locked until its oldest failure leaves the window; b is not
        assert_eq!(t.wait("a", now + 1000, 0), Some(FAIL_WINDOW_MS - 1000));
        assert_eq!(t.wait("b", now + 1000, 0), None);
        // the window passes
        assert_eq!(t.wait("a", now + FAIL_WINDOW_MS + MAX_FAILS as i64, 0), None);
        assert!(t.by_client.is_empty() && t.all.is_empty(), "old entries are pruned");
        // many clients, a few failures each: the ceiling locks everyone
        let mut t = Throttle::default();
        for i in 0..GLOBAL_MAX_FAILS {
            t.note(&format!("c{i}"), now);
        }
        assert_eq!(t.wait("fresh", now + 1, 0), Some(FAIL_WINDOW_MS - 1));
    }

    #[test]
    fn a_new_code_resets_the_window() {
        let mut t = Throttle::default();
        let now = 1_000_000_000;
        for _ in 0..MAX_FAILS {
            t.note("a", now);
        }
        assert!(t.wait("a", now + 10, 0).is_some());
        // a code made after those failures: they no longer count
        assert_eq!(t.wait("a", now + 20, now + 15), None);
        // failures after it count again
        for _ in 0..MAX_FAILS {
            t.note("a", now + 30);
        }
        assert!(t.wait("a", now + 40, now + 15).is_some());
    }

    #[test]
    fn codes_and_tokens_look_right() {
        let c = random_code();
        assert_eq!(c.len(), CODE_LEN);
        assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)), "{c}");
        assert_ne!(random_code(), random_code());
        let t = new_token();
        assert_eq!(t.len(), 68);
        assert_eq!(normalize_code(" abcd-efgh "), "ABCDEFGH");
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"ab", b"abc"));
        assert_eq!(Mode::parse("ALL"), Some(Mode::All));
        assert_eq!(Mode::parse("x"), None);
    }
}

/// One of a user's MCP keys, as Settings lists it: never the secret.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct McpKey {
    pub id: i64,
    pub name: String,
    pub created: i64,
    pub last_used: Option<i64>,
}

/// A new MCP key for `user_id`: a device token with scope `mcp`. The token
/// is returned here and never again (only its SHA-256 is kept).
pub fn add_mcp_key(user_id: i64, name: &str) -> Result<(String, McpKey)> {
    let conn = open()?;
    anyhow::ensure!(mcp_keys(user_id)?.len() < MCP_KEYS_MAX, "too many MCP keys: revoke one first");
    let (token, dev) = add_device_scoped(&conn, user_id, name, MCP_SCOPE)?;
    Ok((token, McpKey { id: dev.id, name: dev.name, created: dev.created, last_used: dev.last_seen }))
}

/// `user_id`'s MCP keys, oldest first.
pub fn mcp_keys(user_id: i64) -> Result<Vec<McpKey>> {
    let conn = open()?;
    let mut st = conn.prepare("SELECT id, name, created, last_seen, scopes FROM devices WHERE user_id = ? ORDER BY id")?;
    let keys = st
        .query_map([user_id], |r| Ok((McpKey { id: r.get(0)?, name: r.get(1)?, created: r.get(2)?, last_used: r.get(3)? }, r.get::<_, String>(4)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(keys.into_iter().filter(|(_, scopes)| scopes.split(',').any(|s| s.trim() == MCP_SCOPE)).map(|(k, _)| k).collect())
}

/// Revoke one of `user_id`'s MCP keys. False when it isn't theirs, or isn't
/// an MCP key (a paired device is signed out another way).
pub fn revoke_mcp_key(user_id: i64, id: i64) -> Result<bool> {
    if !mcp_keys(user_id)?.iter().any(|k| k.id == id) {
        return Ok(false);
    }
    let conn = open()?;
    Ok(conn.execute("DELETE FROM devices WHERE id = ?1 AND user_id = ?2", params![id, user_id])? > 0)
}
