//! Encrypted backups and the account archive over HTTP and for the nightly
//! timer (the format and the import logic are `finance_engine::archive`).
//!
//! * The backup recipient: an age X25519 public key (`age1…`), kept in
//!   server.db's meta table. Public, never a secret: it can only seal. The
//!   desktop app makes the key pair and keeps the private half.
//! * Nightly: `anser-server backup` (the timer) also writes
//!   `data/backups/archives/finance-YYYYMMDD-HHMMSS.finance`, sealed to the
//!   recipient, when one is set, keeping the newest N (meta
//!   `backup_archive_keep`, default 14).
//! * Admin routes (gate.rs: a device with the admin role):
//!   `GET|PUT /api/admin/backup-recipient`, `GET /api/admin/archives`,
//!   `GET /api/admin/archives/latest` (streamed),
//!   `POST /api/account/export` {passphrase, with_secrets, from, to,
//!   sources} (streamed while it is sealed),
//!   `POST /api/account/import` (multipart: `file`, `passphrase` or
//!   `identity`, `mode`, `dry_run` (default on), `prefer_archive`).
//!
//! A passphrase or identity sent here is used in memory for that request
//! only: never stored, never logged.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tiny_http::Request;

use finance_engine::archive::{self, ExportOpts, ImportOpts, Mode, Scope, Seal, Tree, Unseal};
use finance_engine::{config, secrets};

use crate::log::log;
use crate::server::{header, read_body, Reply};
use crate::{auth, syncer, upload};

pub const RECIPIENT_KEY: &str = "backup_recipient";
pub const KEEP_KEY: &str = "backup_archive_keep";
pub const DEFAULT_KEEP: usize = 14;
/// An uploaded archive: generous for years of exports, bounded for a small server's RAM (a Raspberry Pi).
pub const MAX_IMPORT_BYTES: usize = 256 << 20;
pub const ARCHIVE_TYPE: &str = "application/octet-stream";

pub fn archives_dir() -> PathBuf {
    config::data_dir().join("backups").join("archives")
}

/// The active user's recipient setting: `backup_recipient` for the owner
/// (as before), `backup_recipient:<uid>` for anyone else. Each user seals
/// their own archives to their own key.
fn recipient_key() -> String {
    match secrets::active_user() {
        finance_engine::tenant::OWNER_UID => RECIPIENT_KEY.to_string(),
        uid => format!("{RECIPIENT_KEY}:{uid}"),
    }
}

pub fn recipient() -> Option<String> {
    auth::setting(&recipient_key()).filter(|r| !r.trim().is_empty())
}

pub fn keep() -> usize {
    auth::setting(KEEP_KEY).and_then(|k| k.parse().ok()).unwrap_or(DEFAULT_KEEP).max(1)
}

/// Set (validated) or clear the recipient.
pub fn set_recipient(r: Option<&str>) -> Result<()> {
    match r.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => {
            let parsed = archive::parse_recipients(r)?;
            let canon = parsed.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
            auth::set_setting(&recipient_key(), Some(&canon))
        }
        None => auth::set_setting(&recipient_key(), None),
    }
}

/// The nightly archive, when a recipient is set. None = not configured.
pub fn nightly() -> Result<Option<(PathBuf, u64)>> {
    let Some(r) = recipient() else { return Ok(None) };
    let list: Vec<String> = r.split(',').map(String::from).collect();
    let (path, _) = archive::nightly(&Tree::configured(), &list, &archives_dir(), keep())?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(Some((path, size)))
}

fn bad(msg: impl Into<String>) -> Reply {
    Reply::json_status(400, &json!({"ok": false, "error": msg.into()}))
}

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// The routes here; None = not one of them.
pub fn route(req: &mut Request, method: &str, path: &str) -> Option<Result<Reply>> {
    Some(match (method, path) {
        ("GET", "/api/admin/backup-recipient") => Ok(Reply::json(&json!({
            "recipient": recipient(), "keep": keep(), "dir": archives_dir().display().to_string(),
        }))),
        ("PUT", "/api/admin/backup-recipient") => put_recipient(req),
        ("GET", "/api/admin/archives") => Ok(Reply::json(&json!({
            "archives": archive::list_archives(&archives_dir()).into_iter()
                .map(|(name, size, ms)| json!({"name": name, "size": size, "modified": ms})).collect::<Vec<_>>(),
            "recipient": recipient(),
        }))),
        ("GET", "/api/admin/archives/latest") => latest(),
        ("POST", "/api/account/export") => export(req),
        ("POST", "/api/account/import") => import(req),
        _ => return None,
    })
}

fn put_recipient(req: &mut Request) -> Result<Reply> {
    let body: Value = serde_json::from_slice(&read_body(req, 16 * 1024)?).unwrap_or(Value::Null);
    let r = body.get("recipient").and_then(Value::as_str);
    if let Err(e) = set_recipient(r) {
        return Ok(bad(format!("{e:#}")));
    }
    if let Some(k) = body.get("keep").and_then(Value::as_u64) {
        auth::set_setting(KEEP_KEY, Some(&k.clamp(1, 365).to_string()))?;
    }
    log(&format!("[backup] recipient {}", if recipient().is_some() { "set" } else { "cleared" }));
    Ok(Reply::json(&json!({"ok": true, "recipient": recipient(), "keep": keep()})))
}

fn latest() -> Result<Reply> {
    let dir = archives_dir();
    let Some((name, size, _)) = archive::list_archives(&dir).into_iter().next() else {
        return Ok(Reply::json_status(404, &json!({"ok": false, "error": "no archive yet: set a backup key, then the nightly backup writes one"})));
    };
    let f = std::fs::File::open(dir.join(&name))?;
    Ok(Reply::stream(ARCHIVE_TYPE, Box::new(f), Some(size as usize), &name))
}

/// The export's scope from a JSON body.
pub fn scope_of(v: &Value) -> Scope {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let sources = match v.get("sources") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(String::from).collect(),
        Some(Value::String(s)) => s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        _ => vec![],
    };
    Scope { from: s("from"), to: s("to"), sources }
}

fn export(req: &mut Request) -> Result<Reply> {
    let body: Value = match serde_json::from_slice(&read_body(req, 64 * 1024)?) {
        Ok(v) => v,
        Err(_) => return Ok(bad("the body is not JSON")),
    };
    let pass = body.get("passphrase").and_then(Value::as_str).unwrap_or("").to_string();
    if pass.chars().count() < 8 {
        return Ok(bad("the passphrase needs at least 8 characters"));
    }
    let with_secrets = body.get("with_secrets").and_then(Value::as_bool).unwrap_or(false);
    let scope = scope_of(&body);
    let held = if with_secrets { Some(secrets::load(secrets::active_user())?) } else { None };
    let opts = ExportOpts { scope, secrets: held.as_ref(), created_at: None };
    // build now (errors answer properly), seal while streaming
    let (m, plain) = match archive::build(&Tree::configured(), &opts) {
        Ok(x) => x,
        Err(e) => return Ok(bad(format!("{e:#}"))),
    };
    drop(held);
    log(&format!("[archive] export: {} entries, {} bytes before sealing{}", m.entries.len(), plain.len(),
        if m.includes_secrets { ", with secrets" } else { "" }));
    let (rd, wr) = std::io::pipe()?;
    std::thread::Builder::new().name("export".into()).spawn(move || {
        let w = std::io::BufWriter::new(wr);
        if let Err(e) = archive::seal_into(&Seal::Passphrase { passphrase: pass, work_factor: None }, &plain, w) {
            log(&format!("[archive] export stream ended early: {e}"));
        }
    })?;
    Ok(Reply::stream(ARCHIVE_TYPE, Box::new(rd), None, &format!("finance-{}.finance", stamp())))
}

fn import(req: &mut Request) -> Result<Reply> {
    let ctype = header(req, "Content-Type").unwrap_or_default();
    if !ctype.starts_with("multipart/form-data") {
        return Ok(bad("send the archive as multipart/form-data (field file)"));
    }
    let len: usize = header(req, "Content-Length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    if len > MAX_IMPORT_BYTES {
        return Ok(Reply::json_status(413, &json!({"ok": false, "error": format!("the archive is over {} MB", MAX_IMPORT_BYTES >> 20)})));
    }
    let body = read_body(req, MAX_IMPORT_BYTES)?;
    let parts = upload::parse_form(&ctype, &body)?;
    let field = |n: &str| parts.iter().find(|p| p.name == n && p.filename.is_none()).map(|p| p.text());
    let Some(file) = parts.iter().find(|p| p.name == "file") else { return Ok(bad("no file part")) };
    let mode = match field("mode").as_deref().map(Mode::parse) {
        None => Mode::Merge,
        Some(Some(m)) => m,
        Some(None) => return Ok(bad("mode must be fresh, merge or replace")),
    };
    let on = |v: Option<String>, default: bool| v.map(|v| matches!(v.trim(), "1" | "true" | "on" | "yes")).unwrap_or(default);
    let dry_run = on(field("dry_run"), true);
    let prefer_archive = on(field("prefer_archive"), false);
    let unseal = match (field("passphrase").filter(|p| !p.is_empty()), field("identity").filter(|p| !p.trim().is_empty())) {
        (Some(p), _) => Unseal::Passphrase(p),
        (None, Some(i)) => Unseal::Identity(i),
        (None, None) => return Ok(bad("give the passphrase, or the backup key (identity)")),
    };
    let a = match archive::open(&file.data[..], &unseal) {
        Ok(a) => a,
        Err(e) => return Ok(bad(format!("{e:#}"))),
    };
    let (rep, stored) = match run_import(&a, mode, dry_run, prefer_archive) {
        Ok(x) => x,
        Err(e) => return Ok(Reply::json_status(409, &json!({"ok": false, "error": format!("{e:#}"), "manifest": archive::manifest_summary(&a.manifest)}))),
    };
    if !dry_run {
        crate::server::warm_async();
    }
    Ok(Reply::json(&json!({"ok": true, "report": rep, "text": rep.text(), "secrets_stored": stored,
        "manifest": archive::manifest_summary(&a.manifest)})))
}

/// Import under the sync lock (a dry run needs none), then the secrets:
/// fresh and replace take them all, merge only the keys not set here.
/// Returns the report and the secret keys stored.
pub fn run_import(a: &archive::Archive, mode: Mode, dry_run: bool, prefer_archive: bool) -> Result<(archive::ImportReport, Vec<String>)> {
    let _lock = if dry_run { None } else { Some(syncer::try_lock().context("a sync is running: try again when it is done")?) };
    let rep = archive::import(a, &Tree::configured(), ImportOpts { mode, dry_run, prefer_archive })?;
    let mut stored = vec![];
    if let (false, Some(s)) = (dry_run, a.secrets()?) {
        let take: BTreeMap<String, String> = s
            .into_iter()
            .filter(|(k, v)| secrets::is_secret_key(k) && !v.trim().is_empty())
            .filter(|(k, _)| mode != Mode::Merge || prefer_archive || secrets::stored(k).is_none())
            .collect();
        if !take.is_empty() {
            secrets::put(secrets::active_user(), &take)?;
            stored = take.into_keys().collect();
        }
    }
    log(&format!("[archive] import {}{}: {} rows added, {} conflicts{}", mode.as_str(), if dry_run { " (dry run)" } else { "" },
        rep.added(), rep.conflicts(), if stored.is_empty() { String::new() } else { format!(", {} secret key(s) stored", stored.len()) }));
    Ok((rep, stored))
}
