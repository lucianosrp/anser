//! The portable archive: one `.finance` file that holds a user's whole
//! tree, encrypted, for off-device backups and moving between servers.
//!
//! The file is in the [age](https://age-encryption.org) format, sealed to a
//! passphrase (scrypt) or to X25519 recipients (`age1…`; the unattended
//! nightly backup). Inside is a zstd-compressed tar:
//!
//! * `manifest.json`: format_version, the app version, bundle_version, the
//!   schema (PRAGMA table_info per table), row counts, the sha256 and size of
//!   every other entry, the scope (full or partial), includes_secrets,
//!   created_at, and the transactions dir the ingest paths were recorded under
//! * `db/finance.sqlite`: a consistent snapshot (the backup API into memory,
//!   [`db::snapshot_bytes_of`]), filtered for a partial export
//! * `originals/<file>`: the transaction exports (transactions/)
//! * `settings/profile.json`, `settings/user_settings.json`,
//!   `settings/formats/<id>.toml`
//! * `secrets/secrets.json`: only when asked for (off by default)
//!
//! Never in it, by construction (only the entries above are ever added):
//! server.db, master.key, the web app, fetch_status.json, the locks.
//!
//! Import ([`import`]) has three modes, each with a dry run that reports per
//! table what would be added, skipped, and what conflicts:
//!
//! * `fresh`: into an empty tree (refused otherwise).
//! * `merge`: natural-key `INSERT OR IGNORE` per table; bank rows go through
//!   ingest's own near-duplicate guard; the originals are saved (same content
//!   is skipped) and re-ingested through the normal dedupe; the target wins
//!   on overrides, rules (matched on pattern + direction), settings and the
//!   broker snapshot unless `prefer_archive`; conflicts are listed; a
//!   different base currency is refused; the crypto reconcile anchors are
//!   skipped (the target's next sync derives its own).
//! * `replace`: a backup of the target first (data/backups/pre-import/),
//!   then an atomic swap of finance.db (WAL and SHM removed); the old
//!   transactions/ is moved aside. Refuses a partial archive. The caller
//!   holds the sync lock.
//!
//! Paths are explicit ([`Tree`]) and the secrets are passed in and out by
//! the caller, so nothing here reads the secrets store or global config
//! beyond [`Tree::configured`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{config, db, profile};

pub const FORMAT_VERSION: u32 = 1;

const DB_ENTRY: &str = "db/finance.sqlite";
const MANIFEST: &str = "manifest.json";
const SECRETS_ENTRY: &str = "secrets/secrets.json";
/// Crypto rows the target's own sync regenerates: never merged.
const RECONCILE_KINDS: [&str; 2] = ["cash-adjust", "asset-adjust"];
/// Decompressed archive size we accept (a hostile file can't fill the disk/RAM).
const MAX_PLAIN: u64 = 4 << 30;

/// Where a user's data lives.
#[derive(Debug, Clone)]
pub struct Tree {
    pub db: PathBuf,
    pub transactions: PathBuf,
    pub formats: PathBuf,
}

impl Tree {
    /// The configured tree (FINANCE_HOME, FINANCE_DB, ...).
    pub fn configured() -> Tree {
        Tree { db: config::db_path(), transactions: config::transactions_dir(), formats: crate::sources::format::default_dir() }
    }

    fn data_dir(&self) -> PathBuf {
        self.db.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    }
}

/// What a partial export keeps. Empty = everything.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    /// YYYY-MM-DD, inclusive
    pub from: Option<String>,
    pub to: Option<String>,
    /// source keys (bank sources, exchanges, `ibkr`, `hsbc_sms`)
    pub sources: Vec<String>,
}

impl Scope {
    pub fn is_full(&self) -> bool {
        self.from.is_none() && self.to.is_none() && self.sources.is_empty()
    }
    fn dated(&self) -> bool {
        self.from.is_some() || self.to.is_some()
    }
}

/// How the archive is sealed.
pub enum Seal {
    /// scrypt; `work_factor` (log2 N) None = age's default (about 1 s here)
    Passphrase { passphrase: String, work_factor: Option<u8> },
    /// `age1…` recipients
    Recipients(Vec<String>),
}

/// How it is opened.
pub enum Unseal {
    Passphrase(String),
    /// an identity file's text: `AGE-SECRET-KEY-1…` lines (comments allowed)
    Identity(String),
}

#[derive(Default)]
pub struct ExportOpts<'a> {
    pub scope: Scope,
    /// the user's decrypted secrets, only when the export includes them
    pub secrets: Option<&'a BTreeMap<String, String>>,
    /// unix ms; None = now
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryInfo {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub app: String,
    pub app_version: String,
    pub bundle_version: i64,
    /// table -> [[name, type, notnull, pk], ...]
    pub schema: BTreeMap<String, Vec<(String, String, bool, i64)>>,
    pub row_counts: BTreeMap<String, i64>,
    pub entries: BTreeMap<String, EntryInfo>,
    pub scope: Scope,
    pub partial: bool,
    pub includes_secrets: bool,
    pub created_at: i64,
    /// the base currency of the profile, when there is one
    pub base_currency: Option<String>,
    /// the source tree's transactions dir: ingested_files paths start with it
    pub transactions_dir: String,
}

/// A decrypted, checked archive, in memory.
pub struct Archive {
    pub manifest: Manifest,
    pub entries: BTreeMap<String, Vec<u8>>,
}

// ---------- helpers ----------

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// A SQLite image as a self-contained rollback-journal file (header bytes
/// 18/19 = 1): a WAL-mode image doesn't open from memory.
fn delete_mode(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() >= 100 && bytes.starts_with(b"SQLite format 3\0") {
        bytes[18] = 1;
        bytes[19] = 1;
    }
    bytes
}

/// An in-memory connection over a database image.
pub fn mem_db(bytes: &[u8], read_only: bool) -> Result<Connection> {
    let bytes = delete_mode(bytes.to_vec());
    let mut c = Connection::open_in_memory()?;
    c.deserialize_read_exact("main", &bytes[..], bytes.len(), read_only).context("loading the database image")?;
    Ok(c)
}

fn image(conn: &Connection, schema: &str) -> Result<Vec<u8>> {
    Ok(delete_mode(conn.serialize(schema)?.to_vec()))
}

fn tables(conn: &Connection, schema: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare(&format!(
        "SELECT name FROM {schema}.sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ))?;
    let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

fn table_info(conn: &Connection, schema: &str, table: &str) -> Result<Vec<(String, String, bool, i64)>> {
    let mut st = conn.prepare(&format!("PRAGMA {schema}.table_info(\"{table}\")"))?;
    let v = st
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)? != 0, r.get::<_, i64>(5)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

fn count(conn: &Connection, schema: &str, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {schema}.\"{table}\""), [], |r| r.get(0)).unwrap_or(0)
}

/// Row counts of every table of `schema`.
pub fn row_counts(conn: &Connection, schema: &str) -> Result<BTreeMap<String, i64>> {
    Ok(tables(conn, schema)?.into_iter().map(|t| {
        let n = count(conn, schema, &t);
        (t, n)
    }).collect())
}

fn has_table(conn: &Connection, schema: &str, table: &str) -> bool {
    conn.query_row(&format!("SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name=?"), [table], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

fn day_ms(d: &str, end: bool) -> Result<i64> {
    let day = chrono::NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").with_context(|| format!("expected YYYY-MM-DD, got {d:?}"))?;
    let day = if end { day.succ_opt().unwrap_or(day) } else { day };
    Ok(day.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp_millis())
}

fn path_str(p: &Path) -> String {
    crate::sources::textfmt::path_str(p)
}

// ---------- export ----------

/// Cut a snapshot down to `scope` (in place).
fn apply_scope(conn: &Connection, scope: &Scope) -> Result<()> {
    if !scope.sources.is_empty() {
        let list: Vec<String> = scope.sources.iter().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
        let inl = list.iter().map(|s| format!("'{}'", s.replace('\'', "''"))).collect::<Vec<_>>().join(",");
        conn.execute_batch(&format!(
            "DELETE FROM bank_transactions WHERE source NOT IN ({inl});
             DELETE FROM ingested_files WHERE source NOT IN ({inl});
             DELETE FROM crypto_events WHERE exchange NOT IN ({inl});
             DELETE FROM crypto_raw_sources WHERE exchange NOT IN ({inl});
             DELETE FROM crypto_balance_snapshots WHERE exchange NOT IN ({inl});"
        ))?;
        if !list.iter().any(|s| s == "ibkr") {
            for t in ["ibkr_trades", "ibkr_positions", "ibkr_cash", "ibkr_nav", "ibkr_cash_flows", "ibkr_income"] {
                if has_table(conn, "main", t) {
                    conn.execute_batch(&format!("DELETE FROM {t}"))?;
                }
            }
        }
        if !list.iter().any(|s| s == "hsbc_sms") {
            conn.execute_batch("DELETE FROM fund_subscriptions")?;
        }
    }
    if scope.dated() {
        let from = scope.from.as_deref().unwrap_or("0000-01-01");
        let to = scope.to.as_deref().unwrap_or("9999-12-31");
        let (fms, tms) = (day_ms(from, false).unwrap_or(i64::MIN), day_ms(to, true).unwrap_or(i64::MAX));
        if let Some(f) = &scope.from {
            day_ms(f, false)?;
        }
        if let Some(t) = &scope.to {
            day_ms(t, true)?;
        }
        let d = |col: &str| format!("(substr({col},1,10) < '{from}' OR substr({col},1,10) > '{to}')");
        conn.execute(&format!("DELETE FROM bank_transactions WHERE {}", d("txn_date")), [])?;
        conn.execute("DELETE FROM crypto_events WHERE ts < ?1 OR ts >= ?2", params![fms, tms])?;
        conn.execute("DELETE FROM crypto_balance_snapshots WHERE ts < ?1 OR ts >= ?2", params![fms, tms])?;
        conn.execute("DELETE FROM ibkr_trades WHERE ts < ?1 OR ts >= ?2", params![fms, tms])?;
        conn.execute(&format!("DELETE FROM ibkr_nav WHERE {}", d("date")), [])?;
        conn.execute(&format!("DELETE FROM ibkr_cash_flows WHERE {}", d("date")), [])?;
        if has_table(conn, "main", "ibkr_income") {
            conn.execute(&format!("DELETE FROM ibkr_income WHERE {}", d("date")), [])?;
        }
        conn.execute(&format!("DELETE FROM fund_subscriptions WHERE {}", d("trade_date")), [])?;
        // the originals don't come along (they hold rows outside the range):
        // a stale ingest record would keep them from ever being ingested
        conn.execute("DELETE FROM ingested_files", [])?;
    }
    Ok(())
}

/// The transaction exports to carry: every ingestable file (only the
/// chosen sources' for a sources-only partial export, none for a dated one).
fn originals(tree: &Tree, scope: &Scope, conn: &Connection) -> Result<Vec<(String, PathBuf)>> {
    if scope.dated() {
        return Ok(vec![]);
    }
    let files = crate::ingest::candidate_files(&tree.transactions);
    let keep: Option<BTreeSet<String>> = (!scope.sources.is_empty()).then(|| {
        let mut st = match conn.prepare("SELECT path FROM ingested_files") {
            Ok(st) => st,
            Err(_) => return BTreeSet::new(),
        };
        st.query_map([], |r| r.get::<_, String>(0)).map(|r| r.flatten().collect()).unwrap_or_default()
    });
    Ok(files
        .into_iter()
        .filter(|f| keep.as_ref().is_none_or(|k| k.contains(&path_str(f))))
        .filter_map(|f| Some((f.file_name()?.to_string_lossy().into_owned(), f)))
        .collect())
}

fn tar_append(b: &mut tar::Builder<impl Write>, name: &str, data: &[u8], mtime: u64) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o600);
    h.set_mtime(mtime);
    h.set_entry_type(tar::EntryType::Regular);
    b.append_data(&mut h, name, data).with_context(|| format!("adding {name}"))?;
    Ok(())
}

/// Pack a manifest and entries as the zstd tar (manifest first).
fn pack(manifest: &Manifest, entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mtime = (manifest.created_at / 1000).max(0) as u64;
    let enc = zstd::Encoder::new(Vec::new(), 10)?;
    let mut b = tar::Builder::new(enc);
    tar_append(&mut b, MANIFEST, &serde_json::to_vec_pretty(manifest)?, mtime)?;
    for (name, data) in entries {
        tar_append(&mut b, name, data, mtime)?;
    }
    Ok(b.into_inner()?.finish()?)
}

/// The archive's plaintext (a zstd tar) and its manifest.
pub fn build(tree: &Tree, opts: &ExportOpts) -> Result<(Manifest, Vec<u8>)> {
    anyhow::ensure!(tree.db.exists(), "no database at {}", tree.db.display());
    let snap = db::snapshot_bytes_of(&tree.db)?;
    let conn = mem_db(&snap, false)?;
    let db_bytes = if opts.scope.is_full() {
        snap
    } else {
        apply_scope(&conn, &opts.scope)?;
        conn.execute_batch("VACUUM")?;
        image(&conn, "main")?
    };
    let mut entries: Vec<(String, Vec<u8>)> = vec![(DB_ENTRY.into(), db_bytes)];
    for (name, path) in originals(tree, &opts.scope, &conn)? {
        entries.push((format!("originals/{name}"), std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?));
    }
    entries.push(("settings/profile.json".into(), serde_json::to_vec_pretty(&profile::bundle_profile(&conn))?));
    entries.push(("settings/user_settings.json".into(), serde_json::to_vec_pretty(&profile::bundle_settings(&conn))?));
    if let Ok(rd) = std::fs::read_dir(&tree.formats) {
        let mut fs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml") && p.is_file()).collect();
        fs.sort();
        for f in fs {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            entries.push((format!("settings/formats/{name}"), std::fs::read(&f)?));
        }
    }
    if let Some(s) = opts.secrets {
        entries.push((SECRETS_ENTRY.into(), serde_json::to_vec(s)?));
    }
    let schema = tables(&conn, "main")?.into_iter().map(|t| {
        let info = table_info(&conn, "main", &t).unwrap_or_default();
        (t, info)
    }).collect();
    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        app: "anser-server".into(),
        app_version: finance_core::VERSION.into(),
        bundle_version: crate::export::BUNDLE_VERSION as i64,
        schema,
        row_counts: row_counts(&conn, "main")?,
        entries: entries.iter().map(|(n, d)| (n.clone(), EntryInfo { sha256: sha256_hex(d), size: d.len() as u64 })).collect(),
        scope: opts.scope.clone(),
        partial: !opts.scope.is_full(),
        includes_secrets: opts.secrets.is_some(),
        created_at: opts.created_at.unwrap_or_else(db::now_ms),
        base_currency: profile::get(&conn).ok().flatten().map(|p| p.base_currency),
        transactions_dir: path_str(&tree.transactions),
    };
    let plain = pack(&manifest, &entries)?;
    Ok((manifest, plain))
}

/// Parse `age1…` recipients (one per line or comma-separated; `#` comments).
pub fn parse_recipients(text: &str) -> Result<Vec<age::x25519::Recipient>> {
    let mut out = Vec::new();
    for tok in text.lines().map(|l| l.split('#').next().unwrap_or("")).flat_map(|l| l.split([',', ' '])).map(str::trim) {
        if tok.is_empty() {
            continue;
        }
        out.push(tok.parse::<age::x25519::Recipient>().map_err(|e| anyhow::anyhow!("not an age recipient ({tok}): {e}"))?);
    }
    anyhow::ensure!(!out.is_empty(), "no recipient given");
    Ok(out)
}

/// Parse identities (`AGE-SECRET-KEY-1…` lines) from an identity file's text.
pub fn parse_identities(text: &str) -> Result<Vec<age::x25519::Identity>> {
    let ids: Vec<age::x25519::Identity> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("AGE-SECRET-KEY-"))
        .map(|l| l.parse::<age::x25519::Identity>().map_err(|e| anyhow::anyhow!("not an age identity: {e}")))
        .collect::<Result<_>>()?;
    anyhow::ensure!(!ids.is_empty(), "no AGE-SECRET-KEY line in the identity");
    Ok(ids)
}

/// A new X25519 identity: (the identity file text, its public recipient).
pub fn generate_identity() -> (String, String) {
    use age::secrecy::ExposeSecret;
    let id = age::x25519::Identity::generate();
    let public = id.to_public().to_string();
    let created = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let text = format!("# finance backup key, created {created}\n# public key: {public}\n{}\n", id.to_string().expose_secret());
    (text, public)
}

/// The public recipient of an identity file's (first) key.
pub fn identity_public(text: &str) -> Result<String> {
    Ok(parse_identities(text)?[0].to_public().to_string())
}

/// age-encrypt `plain` into `out`.
pub fn seal_into(seal: &Seal, plain: &[u8], out: impl Write) -> Result<()> {
    let enc = match seal {
        Seal::Passphrase { passphrase, work_factor } => {
            anyhow::ensure!(!passphrase.is_empty(), "the passphrase is empty");
            let mut r = age::scrypt::Recipient::new(SecretString::from(passphrase.clone()));
            if let Some(n) = work_factor {
                r.set_work_factor(*n);
            }
            age::Encryptor::with_recipients(std::iter::once(&r as &dyn age::Recipient))?
        }
        Seal::Recipients(list) => {
            let rs = parse_recipients(&list.join("\n"))?;
            age::Encryptor::with_recipients(rs.iter().map(|r| r as &dyn age::Recipient))?
        }
    };
    let mut w = enc.wrap_output(out)?;
    w.write_all(plain)?;
    w.finish()?.flush()?;
    Ok(())
}

/// Export the tree into `out` (an age file). Returns the manifest.
pub fn export(tree: &Tree, opts: &ExportOpts, seal: &Seal, out: impl Write) -> Result<Manifest> {
    let (m, plain) = build(tree, opts)?;
    seal_into(seal, &plain, out)?;
    Ok(m)
}

/// Export into `path` atomically (a 0600 `.tmp`, then rename).
pub fn export_file(tree: &Tree, opts: &ExportOpts, seal: &Seal, path: &Path) -> Result<Manifest> {
    if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("finance.tmp");
    let r = (|| {
        let f = private_file(&tmp)?;
        let mut w = std::io::BufWriter::new(f);
        let m = export(tree, opts, seal, &mut w)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok::<_, anyhow::Error>(m)
    })();
    match r {
        Ok(m) => {
            std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
            Ok(m)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn private_file(p: &Path) -> Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(p).with_context(|| format!("writing {}", p.display()))
}

// ---------- open / verify ----------

/// Is this an age passphrase (scrypt) file? Err when it isn't an age file.
pub fn is_passphrase(r: impl Read) -> Result<bool> {
    Ok(age::Decryptor::new(r).context("not an age-encrypted .finance archive")?.is_scrypt())
}

/// Decrypt, decompress and unpack; every entry checked against the manifest.
pub fn open(r: impl Read, unseal: &Unseal) -> Result<Archive> {
    let dec = age::Decryptor::new(r).context("not an age-encrypted .finance archive")?;
    let reader: Box<dyn Read> = match unseal {
        Unseal::Passphrase(p) => {
            if !dec.is_scrypt() {
                bail!("this archive is sealed to a backup key, not a passphrase: open it with the identity");
            }
            let id = age::scrypt::Identity::new(SecretString::from(p.clone()));
            Box::new(dec.decrypt(std::iter::once(&id as &dyn age::Identity)).map_err(|e| anyhow::anyhow!("can't open the archive: {e}"))?)
        }
        Unseal::Identity(text) => {
            if dec.is_scrypt() {
                bail!("this archive is sealed with a passphrase, not a backup key");
            }
            let ids = parse_identities(text)?;
            Box::new(dec.decrypt(ids.iter().map(|i| i as &dyn age::Identity)).map_err(|e| anyhow::anyhow!("can't open the archive: {e}"))?)
        }
    };
    let z = zstd::Decoder::new(reader.take(MAX_PLAIN))?;
    let mut t = tar::Archive::new(z);
    let mut entries = BTreeMap::new();
    let mut manifest: Option<Manifest> = None;
    for e in t.entries().context("reading the archive")? {
        let mut e = e.context("reading the archive (damaged or tampered)")?;
        let name = e.path()?.to_string_lossy().into_owned();
        anyhow::ensure!(!name.contains("..") && !name.starts_with('/'), "unsafe entry name {name:?}");
        let mut data = Vec::new();
        e.read_to_end(&mut data).context("reading the archive (damaged or tampered)")?;
        if name == MANIFEST {
            manifest = Some(serde_json::from_slice(&data).context("manifest.json")?);
        } else {
            anyhow::ensure!(entries.insert(name.clone(), data).is_none(), "{name} appears twice");
        }
    }
    let manifest = manifest.context("no manifest.json: not a finance archive")?;
    if manifest.format_version > FORMAT_VERSION {
        bail!("archive format {} is newer than this server reads ({FORMAT_VERSION}): update anser-server", manifest.format_version);
    }
    for (name, info) in &manifest.entries {
        let data = entries.get(name).with_context(|| format!("{name} is listed in the manifest but missing"))?;
        anyhow::ensure!(data.len() as u64 == info.size && sha256_hex(data) == info.sha256, "{name}: sha256 mismatch (tampered or damaged)");
    }
    if let Some(extra) = entries.keys().find(|k| !manifest.entries.contains_key(*k)) {
        bail!("{extra} is not in the manifest (tampered)");
    }
    anyhow::ensure!(entries.contains_key(DB_ENTRY), "no {DB_ENTRY} in the archive");
    Ok(Archive { manifest, entries })
}

impl Archive {
    pub fn db_bytes(&self) -> &[u8] {
        &self.entries[DB_ENTRY]
    }

    pub fn connection(&self) -> Result<Connection> {
        mem_db(self.db_bytes(), true)
    }

    /// (file name, bytes) of the transaction exports.
    pub fn originals(&self) -> Vec<(&str, &[u8])> {
        self.entries.iter().filter_map(|(k, v)| Some((k.strip_prefix("originals/")?, v.as_slice()))).collect()
    }

    pub fn formats(&self) -> Vec<(&str, &[u8])> {
        self.entries.iter().filter_map(|(k, v)| Some((k.strip_prefix("settings/formats/")?, v.as_slice()))).collect()
    }

    pub fn secrets(&self) -> Result<Option<BTreeMap<String, String>>> {
        self.entries.get(SECRETS_ENTRY).map(|b| serde_json::from_slice(b).context("secrets.json")).transpose()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub manifest: Manifest,
    pub entries: usize,
    pub bytes: u64,
    pub integrity: String,
    pub originals: usize,
}

impl VerifyReport {
    pub fn summary(&self) -> String {
        let m = &self.manifest;
        let created = chrono::DateTime::from_timestamp_millis(m.created_at).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default();
        format!(
            "OK: {} entries ({} originals), {:.1} MB unpacked, db integrity {}; {} bank rows, {} crypto events; made {created} by {} {}{}{}",
            self.entries, self.originals, self.bytes as f64 / 1e6, self.integrity,
            m.row_counts.get("bank_transactions").unwrap_or(&0), m.row_counts.get("crypto_events").unwrap_or(&0),
            m.app, m.app_version,
            if m.partial { "; partial" } else { "" },
            if m.includes_secrets { "; includes secrets" } else { "" },
        )
    }
}

/// Open the archive and check it end to end: the age MAC, every entry's
/// sha256, the database's integrity and its row counts.
pub fn verify(r: impl Read, unseal: &Unseal) -> Result<VerifyReport> {
    let a = open(r, unseal)?;
    let conn = a.connection()?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    anyhow::ensure!(integrity == "ok", "the database fails its integrity check: {integrity}");
    let counts = row_counts(&conn, "main")?;
    for (t, n) in &a.manifest.row_counts {
        anyhow::ensure!(counts.get(t) == Some(n), "{t}: {} rows, the manifest says {n}", counts.get(t).copied().unwrap_or(0));
    }
    if let Some(s) = a.secrets()? {
        let _ = s;
    }
    Ok(VerifyReport {
        entries: a.entries.len(),
        bytes: a.entries.values().map(|v| v.len() as u64).sum(),
        integrity,
        originals: a.originals().len(),
        manifest: a.manifest,
    })
}

// ---------- import ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Fresh,
    Merge,
    Replace,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fresh" => Some(Mode::Fresh),
            "merge" => Some(Mode::Merge),
            "replace" => Some(Mode::Replace),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Fresh => "fresh",
            Mode::Merge => "merge",
            Mode::Replace => "replace",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImportOpts {
    pub mode: Mode,
    pub dry_run: bool,
    /// merge: the archive wins conflicts (overrides, rules, settings, the broker snapshot)
    pub prefer_archive: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TableReport {
    pub table: String,
    pub added: i64,
    pub skipped: i64,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    pub mode: String,
    pub dry_run: bool,
    pub tables: Vec<TableReport>,
    /// originals saved into transactions/ (or that would be)
    pub originals_saved: Vec<String>,
    /// originals the target already had (same content)
    pub originals_present: usize,
    /// rows the re-ingest of the originals added (merge)
    pub reingested_rows: usize,
    pub notes: Vec<String>,
    pub backup: Option<String>,
    pub includes_secrets: bool,
}

impl ImportReport {
    pub fn added(&self) -> i64 {
        self.tables.iter().map(|t| t.added).sum()
    }
    pub fn conflicts(&self) -> usize {
        self.tables.iter().map(|t| t.conflicts.len()).sum()
    }
    pub fn table(&self, name: &str) -> Option<&TableReport> {
        self.tables.iter().find(|t| t.table == name)
    }
    /// A plain-text report (the CLI prints it).
    pub fn text(&self) -> String {
        let mut s = format!("{} import{}:\n", self.mode, if self.dry_run { " (dry run: nothing written)" } else { "" });
        for t in &self.tables {
            if t.added == 0 && t.skipped == 0 && t.conflicts.is_empty() {
                continue;
            }
            s += &format!("  {:<26} +{:<7} skipped {:<7}{}\n", t.table, t.added, t.skipped,
                if t.conflicts.is_empty() { String::new() } else { format!(" conflicts {}", t.conflicts.len()) });
            for c in t.conflicts.iter().take(20) {
                s += &format!("      {c}\n");
            }
        }
        s += &format!("  originals: {} new, {} already there\n", self.originals_saved.len(), self.originals_present);
        if self.reingested_rows > 0 {
            s += &format!("  re-ingest added {} rows\n", self.reingested_rows);
        }
        for n in &self.notes {
            s += &format!("  note: {n}\n");
        }
        if let Some(b) = &self.backup {
            s += &format!("  backup of the previous database: {b}\n");
        }
        s
    }
}

/// Bank/crypto/broker rows in the target: "empty" means none of them.
fn target_rows(tree: &Tree) -> Result<i64> {
    if !tree.db.exists() {
        return Ok(0);
    }
    let c = Connection::open_with_flags(&tree.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut n = 0;
    for t in ["bank_transactions", "crypto_events", "ibkr_trades", "ingested_files", "fund_subscriptions"] {
        if has_table(&c, "main", t) {
            n += count(&c, "main", t);
        }
    }
    Ok(n)
}

fn md5_hex(b: &[u8]) -> String {
    use md5::Md5;
    hex::encode(Md5::digest(b))
}

/// Save the originals into `dir`: same content anywhere there is skipped,
/// a taken name gets " (n)". Returns (saved names, already present).
fn save_originals(a: &Archive, dir: &Path, write: bool) -> Result<(Vec<String>, usize)> {
    let have: BTreeSet<String> = crate::ingest::candidate_files(dir).iter().filter_map(|p| std::fs::read(p).ok()).map(|b| md5_hex(&b)).collect();
    if write {
        std::fs::create_dir_all(dir)?;
    }
    let (mut saved, mut present) = (vec![], 0);
    for (name, data) in a.originals() {
        if have.contains(&md5_hex(data)) {
            present += 1;
            continue;
        }
        let name = crate::sources::textfmt::path_str(Path::new(name));
        let clean: String = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
        let (stem, ext) = match clean.rfind('.') {
            Some(i) if i > 0 => (&clean[..i], &clean[i..]),
            _ => (clean.as_str(), ""),
        };
        let mut target = dir.join(&clean);
        let mut n = 2;
        while target.exists() {
            target = dir.join(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        if write {
            std::fs::write(&target, data).with_context(|| format!("writing {}", target.display()))?;
        }
        saved.push(target.file_name().unwrap().to_string_lossy().into_owned());
    }
    Ok((saved, present))
}

fn save_formats(a: &Archive, dir: &Path, overwrite: bool, write: bool, notes: &mut Vec<String>) -> Result<()> {
    for (name, data) in a.formats() {
        let p = dir.join(Path::new(name).file_name().unwrap_or_default());
        match std::fs::read(&p) {
            Ok(old) if old == data => continue,
            Ok(_) if !overwrite => {
                notes.push(format!("format {name}: the target has its own version, kept"));
                continue;
            }
            _ => {}
        }
        if write {
            std::fs::create_dir_all(dir)?;
            std::fs::write(&p, data)?;
        }
        notes.push(format!("format {name}: {}", if write { "installed" } else { "would be installed" }));
    }
    if write && !a.formats().is_empty() {
        crate::sources::format::invalidate();
    }
    Ok(())
}

/// Point the archive's ingest records at this tree's transactions dir.
fn rewrite_paths(conn: &Connection, schema: &str, from: &str, to: &Path) -> Result<()> {
    if from.is_empty() || !has_table(conn, schema, "ingested_files") {
        return Ok(());
    }
    let to = path_str(to);
    if from == to {
        return Ok(());
    }
    let (from, to) = (format!("{}/", from.trim_end_matches('/')), format!("{}/", to.trim_end_matches('/')));
    conn.execute(
        &format!("UPDATE OR IGNORE {schema}.ingested_files SET path = ?2 || substr(path, length(?1) + 1) WHERE substr(path, 1, length(?1)) = ?1"),
        params![from, to],
    )?;
    Ok(())
}

/// The whole archive as this tree's database, atomically (fresh, replace).
fn install_db(a: &Archive, tree: &Tree) -> Result<()> {
    let conn = mem_db(a.db_bytes(), false)?;
    db::apply_schema(&conn)?;
    rewrite_paths(&conn, "main", &a.manifest.transactions_dir, &tree.transactions)?;
    let bytes = image(&conn, "main")?;
    drop(conn);
    std::fs::create_dir_all(tree.data_dir())?;
    let tmp = tree.db.with_extension("db.import-tmp");
    {
        let mut f = private_file(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &tree.db).with_context(|| format!("replacing {}", tree.db.display()))?;
    for ext in ["-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{ext}", tree.db.display()));
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
    }
    // WAL again, the schema, the migrations (as the server opens it)
    db::connect(&tree.db)?;
    Ok(())
}

fn whole_tables(a: &Archive) -> Result<Vec<TableReport>> {
    Ok(a.manifest.row_counts.iter().map(|(t, n)| TableReport { table: t.clone(), added: *n, ..Default::default() }).collect())
}

/// Import `a` into `tree`. Replace: the caller holds the sync lock.
pub fn import(a: &Archive, tree: &Tree, opts: ImportOpts) -> Result<ImportReport> {
    let mut rep = ImportReport { mode: opts.mode.as_str().into(), dry_run: opts.dry_run, includes_secrets: a.manifest.includes_secrets, ..Default::default() };
    match opts.mode {
        Mode::Fresh => {
            let n = target_rows(tree)?;
            if n > 0 {
                bail!("the target is not empty ({n} rows): use merge or replace");
            }
            rep.tables = whole_tables(a)?;
            let (saved, present) = save_originals(a, &tree.transactions, !opts.dry_run)?;
            (rep.originals_saved, rep.originals_present) = (saved, present);
            save_formats(a, &tree.formats, true, !opts.dry_run, &mut rep.notes)?;
            if !opts.dry_run {
                install_db(a, tree)?;
            }
        }
        Mode::Replace => {
            if a.manifest.partial {
                bail!("a partial archive can't replace a tree: use merge");
            }
            rep.tables = whole_tables(a)?;
            if tree.db.exists() {
                let c = Connection::open_with_flags(&tree.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                for t in rep.tables.iter_mut() {
                    if has_table(&c, "main", &t.table) {
                        t.skipped = count(&c, "main", &t.table);
                    }
                }
                rep.notes.push("skipped = the target's rows that the archive replaces".into());
            }
            let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
            let aside = tree.transactions.with_file_name(format!(
                "{}.pre-import-{ts}",
                tree.transactions.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "transactions".into())
            ));
            let has_files = !crate::ingest::candidate_files(&tree.transactions).is_empty();
            if opts.dry_run {
                if has_files {
                    rep.notes.push(format!("transactions/ would be moved aside to {}", aside.display()));
                }
                rep.originals_saved = a.originals().iter().map(|(n, _)| n.to_string()).collect();
                save_formats(a, &tree.formats, true, false, &mut rep.notes)?;
                return Ok(rep);
            }
            if tree.db.exists() {
                let dir = tree.data_dir().join("backups").join("pre-import");
                let b = db::backup_into(&tree.db, &dir, usize::MAX)?;
                rep.backup = Some(b.display().to_string());
            }
            if has_files {
                std::fs::rename(&tree.transactions, &aside).with_context(|| format!("moving {} aside", tree.transactions.display()))?;
                rep.notes.push(format!("the previous transactions/ is at {}", aside.display()));
            }
            let (saved, present) = save_originals(a, &tree.transactions, true)?;
            (rep.originals_saved, rep.originals_present) = (saved, present);
            save_formats(a, &tree.formats, true, true, &mut rep.notes)?;
            install_db(a, tree)?;
        }
        Mode::Merge => merge(a, tree, opts, &mut rep)?,
    }
    Ok(rep)
}

fn common_cols(conn: &Connection, table: &str, skip: &[&str]) -> Result<Vec<String>> {
    let main: BTreeSet<String> = table_info(conn, "main", table)?.into_iter().map(|c| c.0).collect();
    Ok(table_info(conn, "src", table)?.into_iter().map(|c| c.0).filter(|c| main.contains(c) && !skip.contains(&c.as_str())).collect())
}

fn quoted(cols: &[String]) -> String {
    cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(",")
}

/// INSERT OR IGNORE from src into main on the table's own key.
fn or_ignore(conn: &Connection, table: &str, skip_cols: &[&str], filter: &str) -> Result<TableReport> {
    let cols = quoted(&common_cols(conn, table, skip_cols)?);
    let total: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM src.\"{table}\" {filter}"), [], |r| r.get(0))?;
    let added = conn.execute(&format!("INSERT OR IGNORE INTO main.\"{table}\" ({cols}) SELECT {cols} FROM src.\"{table}\" {filter}"), [])? as i64;
    Ok(TableReport { table: table.into(), added, skipped: total - added, conflicts: vec![] })
}

fn merge(a: &Archive, tree: &Tree, opts: ImportOpts, rep: &mut ImportReport) -> Result<()> {
    // the target: the real database, or a private in-memory copy for a dry run
    let mut conn = if opts.dry_run { crate::preview::replica_copy(&tree.db)? } else { db::connect(&tree.db)? };
    profile::ensure(&conn)?;
    conn.execute_batch("ATTACH DATABASE ':memory:' AS src")?;
    {
        let img = delete_mode(a.db_bytes().to_vec());
        conn.deserialize_read_exact("src", &img[..], img.len(), false).context("loading the archive's database")?;
    }
    // the base currency must agree
    let theirs: Option<String> = if has_table(&conn, "src", "profile") {
        conn.query_row("SELECT base_currency FROM src.profile WHERE id=1", [], |r| r.get(0)).optional()?
    } else {
        None
    };
    let ours: Option<String> = conn.query_row("SELECT base_currency FROM main.profile WHERE id=1", [], |r| r.get(0)).optional()?;
    if let (Some(t), Some(o)) = (&theirs, &ours) {
        if !t.eq_ignore_ascii_case(o) {
            bail!("the archive's base currency is {t}, this tree's is {o}: refusing to merge (amounts would mix currencies)");
        }
    }
    rewrite_paths(&conn, "src", &a.manifest.transactions_dir, &tree.transactions)?;
    if has_table(&conn, "src", "ibkr_income") {
        conn.execute_batch(db::IBKR_INCOME_SCHEMA)?;
    }

    let tx = conn.transaction()?;
    let c: &Connection = &tx;
    let known: BTreeSet<&str> = [
        "bank_transactions", "ingested_files", "crypto_events", "crypto_prices_monthly", "equity_prices", "crypto_raw_sources",
        "crypto_balance_snapshots", "fx_rates", "ibkr_trades", "ibkr_positions", "ibkr_cash", "ibkr_nav", "ibkr_cash_flows",
        "ibkr_income", "fund_subscriptions", "category_rules", "txn_overrides", "profile", "user_settings", "categories", "app_meta",
        "investment_accounts", "instruments",
    ].into_iter().collect();
    let src_tables = tables(c, "src")?;
    for t in &src_tables {
        if !known.contains(t.as_str()) {
            rep.tables.push(TableReport { table: t.clone(), skipped: count(c, "src", t), conflicts: vec![], added: 0 });
            rep.notes.push(format!("{t}: not a table this server knows, skipped"));
        }
    }
    let has = |t: &str| src_tables.iter().any(|x| x == t);

    // bank rows: through ingest's near-duplicate guard, per source
    if has("bank_transactions") {
        let mut by_source: BTreeMap<String, Vec<db::NewBankTxn>> = BTreeMap::new();
        {
            let mut st = c.prepare("SELECT source, account, currency, txn_date, amount, balance, description, raw_hash, source_file, ingested_at FROM src.bank_transactions ORDER BY id")?;
            let rows = st.query_map([], |r| Ok(db::NewBankTxn {
                source: r.get(0)?, account: r.get(1)?, currency: r.get(2)?, txn_date: r.get(3)?, amount: r.get(4)?, balance: r.get(5)?,
                description: r.get(6)?, raw_hash: r.get(7)?, source_file: r.get(8)?, ingested_at: r.get(9)?,
            }))?;
            for row in rows {
                let row = row?;
                by_source.entry(row.source.clone()).or_default().push(row);
            }
        }
        let mut t = TableReport { table: "bank_transactions".into(), ..Default::default() };
        for (source, rows) in by_source {
            let total = rows.len() as i64;
            let (kept, _near) = crate::ingest::reject_known_duplicates(c, &source, rows)?;
            // (insert_bank_transactions opens its own transaction: inline it)
            let mut ins = c.prepare_cached(
                "INSERT OR IGNORE INTO main.bank_transactions(source,account,currency,txn_date,amount,balance,description,raw_hash,source_file,ingested_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
            )?;
            let mut added = 0i64;
            for r in &kept {
                added += ins.execute(params![r.source, r.account, r.currency, r.txn_date, r.amount, r.balance, r.description, r.raw_hash, r.source_file, r.ingested_at])? as i64;
            }
            t.added += added;
            t.skipped += total - added;
        }
        rep.tables.push(t);
    }
    for table in ["ingested_files", "crypto_prices_monthly", "equity_prices", "crypto_raw_sources", "crypto_balance_snapshots", "fx_rates",
                  "ibkr_trades", "ibkr_nav", "ibkr_cash_flows", "ibkr_income", "fund_subscriptions"] {
        if has(table) && has_table(c, "main", table) {
            rep.tables.push(or_ignore(c, table, &[], "")?);
        }
    }
    if has("crypto_events") {
        let kinds = RECONCILE_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
        let mut t = or_ignore(c, "crypto_events", &["id"], &format!("WHERE kind NOT IN ({kinds})"))?;
        let anchors: i64 = c.query_row(&format!("SELECT COUNT(*) FROM src.crypto_events WHERE kind IN ({kinds})"), [], |r| r.get(0))?;
        t.skipped += anchors;
        if anchors > 0 {
            rep.notes.push(format!("crypto_events: {anchors} reconcile anchors skipped (the next sync derives this tree's own)"));
        }
        rep.tables.push(t);
    }
    // the broker snapshot: point in time, never mixed
    for table in ["ibkr_positions", "ibkr_cash"] {
        if !has(table) {
            continue;
        }
        let n = count(c, "src", table);
        let mut t = TableReport { table: table.into(), ..Default::default() };
        if n == 0 {
            rep.tables.push(t);
            continue;
        }
        let mine = count(c, "main", table);
        if mine == 0 || opts.prefer_archive {
            let cols = quoted(&common_cols(c, table, &[])?);
            c.execute(&format!("DELETE FROM main.{table}"), [])?;
            t.added = c.execute(&format!("INSERT INTO main.{table} ({cols}) SELECT {cols} FROM src.{table}"), [])? as i64;
            if mine > 0 {
                t.conflicts.push(format!("the archive's snapshot replaced this tree's {mine} rows"));
            }
        } else {
            t.skipped = n;
            let (a_asof, m_asof): (Option<String>, Option<String>) = (
                c.query_row(&format!("SELECT MAX(as_of) FROM src.{table}"), [], |r| r.get(0)).unwrap_or(None),
                c.query_row(&format!("SELECT MAX(as_of) FROM main.{table}"), [], |r| r.get(0)).unwrap_or(None),
            );
            if a_asof != m_asof {
                t.conflicts.push(format!("snapshot as of {} kept; the archive's is as of {}", m_asof.unwrap_or_default(), a_asof.unwrap_or_default()));
            }
        }
        rep.tables.push(t);
    }
    // rules: matched on pattern + direction
    if has("category_rules") {
        let mut t = TableReport { table: "category_rules".into(), ..Default::default() };
        type Rule = (i64, String, String, i64, Option<String>, Option<f64>);
        let load = |schema: &str| -> Result<Vec<Rule>> {
            let mut st = c.prepare(&format!("SELECT priority, pattern, category, is_transfer, direction, share FROM {schema}.category_rules ORDER BY priority, id"))?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<rusqlite::Result<_>>()?;
            Ok(v)
        };
        let mine = load("main")?;
        for (pr, pat, cat, tr, dir, share) in load("src")? {
            match mine.iter().find(|m| m.1 == pat && m.4 == dir) {
                None => {
                    c.execute("INSERT INTO main.category_rules(priority,pattern,category,is_transfer,direction,share) VALUES(?,?,?,?,?,?)",
                        params![pr, pat, cat, tr, dir, share])?;
                    t.added += 1;
                }
                Some(m) if m.2 == cat && m.3 == tr && m.5 == share => t.skipped += 1,
                Some(m) => {
                    t.skipped += 1;
                    let d = dir.as_deref().unwrap_or("any");
                    if opts.prefer_archive {
                        c.execute("UPDATE main.category_rules SET category=?, is_transfer=?, share=? WHERE pattern=? AND direction IS ?",
                            params![cat, tr, share, pat, dir])?;
                        t.conflicts.push(format!("rule {pat:?} ({d}): {} -> {cat} (the archive's)", m.2));
                    } else {
                        t.conflicts.push(format!("rule {pat:?} ({d}): kept {}, the archive says {cat}", m.2));
                    }
                }
            }
        }
        rep.tables.push(t);
    }
    // categories (Phase 3): the target keeps its flags, new names come in;
    // app_meta (data_rev) is this tree's own
    if has("categories") {
        crate::categories::ensure(c)?;
        rep.tables.push(or_ignore(c, "categories", &[], "")?);
    }
    // the investment setup (Phase 4): the target keeps its accounts and
    // instruments, new ones come in
    if has("investment_accounts") || has("instruments") {
        crate::invest_setup::ensure(c)?;
        for t in ["investment_accounts", "instruments"] {
            if has(t) {
                rep.tables.push(or_ignore(c, t, &[], "")?);
            }
        }
    }
    // overrides: the target wins unless prefer_archive
    if has("txn_overrides") {
        let mut t = TableReport { table: "txn_overrides".into(), ..Default::default() };
        let rows: Vec<(String, String, String, Option<f64>, Option<String>, i64)> = {
            let mut st = c.prepare("SELECT source, raw_hash, category, share, note, created_at FROM src.txn_overrides")?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<rusqlite::Result<_>>()?;
            v
        };
        for (source, hash, cat, share, note, at) in rows {
            let mine: Option<(String, Option<f64>)> = c
                .query_row("SELECT category, share FROM main.txn_overrides WHERE source=? AND raw_hash=?", params![source, hash], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            match mine {
                None => {
                    c.execute("INSERT INTO main.txn_overrides(source,raw_hash,category,share,note,created_at) VALUES(?,?,?,?,?,?)",
                        params![source, hash, cat, share, note, at])?;
                    t.added += 1;
                }
                Some((mc, ms)) if mc == cat && ms == share => t.skipped += 1,
                Some((mc, _)) => {
                    t.skipped += 1;
                    let short = &hash[..hash.len().min(8)];
                    if opts.prefer_archive {
                        c.execute("UPDATE main.txn_overrides SET category=?, share=?, note=? WHERE source=? AND raw_hash=?", params![cat, share, note, source, hash])?;
                        t.conflicts.push(format!("override {source}/{short}: {mc} -> {cat} (the archive's)"));
                    } else {
                        t.conflicts.push(format!("override {source}/{short}: kept {mc}, the archive says {cat}"));
                    }
                }
            }
        }
        rep.tables.push(t);
    }
    // profile and settings
    if has("profile") {
        let mut t = TableReport { table: "profile".into(), ..Default::default() };
        if ours.is_none() && theirs.is_some() {
            let cols = quoted(&common_cols(c, "profile", &[])?);
            t.added = c.execute(&format!("INSERT OR IGNORE INTO main.profile ({cols}) SELECT {cols} FROM src.profile"), [])? as i64;
        } else {
            t.skipped = count(c, "src", "profile");
        }
        rep.tables.push(t);
    }
    if has("user_settings") {
        let mut t = TableReport { table: "user_settings".into(), ..Default::default() };
        let rows: Vec<(String, String, i64)> = {
            let mut st = c.prepare("SELECT key, value, updated_at FROM src.user_settings")?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
            v
        };
        for (k, v, at) in rows {
            let mine: Option<String> = c.query_row("SELECT value FROM main.user_settings WHERE key=?", [&k], |r| r.get(0)).optional()?;
            match mine {
                None => {
                    c.execute("INSERT INTO main.user_settings(key,value,updated_at) VALUES(?,?,?)", params![k, v, at])?;
                    t.added += 1;
                }
                Some(m) if m == v => t.skipped += 1,
                Some(_) => {
                    t.skipped += 1;
                    if opts.prefer_archive {
                        c.execute("UPDATE main.user_settings SET value=?, updated_at=? WHERE key=?", params![v, at, k])?;
                        t.conflicts.push(format!("setting {k}: the archive's replaced this tree's"));
                    } else {
                        t.conflicts.push(format!("setting {k}: kept this tree's"));
                    }
                }
            }
        }
        rep.tables.push(t);
    }
    tx.commit()?;
    conn.execute_batch("DETACH DATABASE src")?;
    crate::categorize::categorize_all(&conn)?;

    // the originals: saved, then re-ingested through the normal dedupe
    let (saved, present) = save_originals(a, &tree.transactions, !opts.dry_run)?;
    (rep.originals_saved, rep.originals_present) = (saved, present);
    save_formats(a, &tree.formats, opts.prefer_archive, !opts.dry_run, &mut rep.notes)?;
    if !opts.dry_run && !rep.originals_saved.is_empty() {
        let before: i64 = conn.query_row("SELECT COUNT(*) FROM bank_transactions", [], |r| r.get(0))?;
        crate::ingest::run_ingest_dir(&conn, &tree.transactions)?;
        crate::categorize::categorize_all(&conn)?;
        let after: i64 = conn.query_row("SELECT COUNT(*) FROM bank_transactions", [], |r| r.get(0))?;
        rep.reingested_rows = (after - before).max(0) as usize;
    }
    Ok(())
}

/// The newest archives in `dir` (`finance-*.finance`), newest first.
pub fn list_archives(dir: &Path) -> Vec<(String, u64, i64)> {
    let mut v: Vec<(String, u64, i64)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !(name.starts_with("finance-") && name.ends_with(".finance")) {
                        return None;
                    }
                    let md = e.metadata().ok()?;
                    let ms = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
                    Some((name, md.len(), ms))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

/// Write a nightly archive into `dir` and keep the newest `keep`.
pub fn nightly(tree: &Tree, recipients: &[String], dir: &Path, keep: usize) -> Result<(PathBuf, Manifest)> {
    std::fs::create_dir_all(dir)?;
    let name = format!("finance-{}.finance", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let path = dir.join(name);
    let m = export_file(tree, &ExportOpts::default(), &Seal::Recipients(recipients.to_vec()), &path)?;
    let all = list_archives(dir);
    for (old, _, _) in all.iter().skip(keep.max(1)) {
        let _ = std::fs::remove_file(dir.join(old));
    }
    Ok((path, m))
}

/// A JSON summary of a manifest for the APIs.
pub fn manifest_summary(m: &Manifest) -> Value {
    json!({
        "format_version": m.format_version, "app_version": m.app_version, "created_at": m.created_at,
        "partial": m.partial, "scope": m.scope, "includes_secrets": m.includes_secrets,
        "base_currency": m.base_currency, "row_counts": m.row_counts,
        "originals": m.entries.keys().filter(|k| k.starts_with("originals/")).count(),
    })
}

#[cfg(test)]
mod tests;
