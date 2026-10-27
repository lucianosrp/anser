//! Local first, remote when available: both halves of the push contract.
//!
//! The same binary runs on the laptop against the local data tree. When the
//! remote (your server) is reachable, `anser-server push` sends it what it lacks:
//!
//! 1. GET  {remote}/healthz        -- short timeout; unreachable = clean exit
//! 2. GET  {remote}/api/files      -- content hashes the remote already has
//!    (every ingested file + anything sitting un-ingested in transactions/)
//!    and the (source, raw_hash) keys of its txn_overrides
//! 3. POST {remote}/upload         -- only the local files whose md5 is missing
//! 4. POST {remote}/api/overrides  -- only the overrides whose key is missing
//!
//! Every step is idempotent on the remote: uploads dedupe by name+content,
//! ingest dedupes rows by natural key, overrides INSERT OR IGNORE on their
//! primary key (the remote's own choice wins on a clash). Repeating a push,
//! or two laptops pushing, is harmless.
//!
//! Remote names resolve through finance_core::tailnet, so a `.ts.net` URL
//! works with a VPN owning the system DNS.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use finance_core::upload::{self, md5_hex};
use finance_engine::config;

/// The extensions ingest reads (and /upload accepts).
pub const EXTS: [&str; 3] = ["csv", "xml", "pdf"];
/// Stay under the remote's 20MB request cap.
const BATCH_BYTES: usize = 15 * 1024 * 1024;

pub fn transaction_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.contains(&e)))
        .collect();
    out.sort();
    out
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct OverrideKey {
    pub source: String,
    pub raw_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Override {
    pub source: String,
    pub raw_hash: String,
    pub category: String,
    pub share: Option<f64>,
    pub note: Option<String>,
    pub created_at: i64,
}

/// GET /api/files (served by the remote).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Known {
    /// md5 (hex) of every file content the remote holds or has ingested
    pub hashes: BTreeSet<String>,
    pub overrides: BTreeSet<OverrideKey>,
}

/// Server side of GET /api/files. Ingested files come from ingested_files
/// (no reads); only a file whose (path, size, mtime) isn't recorded there
/// is hashed -- the same stat fast path auto-ingest uses.
pub fn known(conn: &Connection, dir: &Path) -> Known {
    let mut k = Known::default();
    let mut by_stat: HashMap<(String, i64, i64), String> = HashMap::new();
    if let Ok(mut st) = conn.prepare("SELECT path, file_hash, size, mtime_ns FROM ingested_files") {
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<i64>>(3)?))
        });
        for (path, hash, size, mt) in rows.into_iter().flatten().flatten() {
            if let (Some(s), Some(m)) = (size, mt) {
                by_stat.insert((path, s, m), hash.clone());
            }
            k.hashes.insert(hash);
        }
    }
    for p in transaction_files(dir) {
        let Ok(meta) = p.metadata() else { continue };
        let key = (p.to_string_lossy().into_owned(), meta.len() as i64, mtime_ns(&meta));
        if by_stat.contains_key(&key) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&p) {
            k.hashes.insert(md5_hex(&bytes));
        }
    }
    if let Ok(mut st) = conn.prepare("SELECT source, raw_hash FROM txn_overrides") {
        let rows = st.query_map([], |r| Ok(OverrideKey { source: r.get(0)?, raw_hash: r.get(1)? }));
        k.overrides.extend(rows.into_iter().flatten().flatten());
    }
    k
}

pub fn load_overrides(conn: &Connection) -> Result<Vec<Override>> {
    let mut st = conn.prepare("SELECT source, raw_hash, category, share, note, created_at FROM txn_overrides")?;
    let rows = st.query_map([], |r| {
        Ok(Override { source: r.get(0)?, raw_hash: r.get(1)?, category: r.get(2)?, share: r.get(3)?, note: r.get(4)?, created_at: r.get(5)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Server side of POST /api/overrides: INSERT OR IGNORE keyed on
/// (source, raw_hash) -- an override the remote already has is never
/// replaced. Recategorizes when anything landed.
pub fn accept_overrides(conn: &Connection, body: &[u8]) -> Result<Value> {
    let rows: Vec<Override> = serde_json::from_slice(body).context("expected a JSON list of overrides")?;
    let mut added = 0;
    for o in &rows {
        added += conn.execute(
            "INSERT OR IGNORE INTO txn_overrides(source,raw_hash,category,share,note,created_at) VALUES(?,?,?,?,?,?)",
            rusqlite::params![o.source, o.raw_hash, o.category, o.share, o.note, o.created_at],
        )?;
    }
    if added > 0 {
        finance_engine::categorize::categorize_all(conn)?;
    }
    Ok(json!({"received": rows.len(), "added": added}))
}

// --- client -------------------------------------------------------------------

/// The remote to push to: --remote, else FINANCE_REMOTE (.env or the
/// environment), else the desktop app's saved server URL.
pub fn default_remote(arg: Option<&str>) -> Option<String> {
    arg.map(String::from)
        .or_else(|| config::env("FINANCE_REMOTE"))
        .or_else(finance_core::cache::saved_server_url)
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
}

/// Is the remote up? GET /healthz with a short timeout.
pub fn probe(remote: &str) -> Result<()> {
    let mut r = upload::agent(Duration::from_secs(4)).get(&format!("{remote}/healthz")).call()?;
    let body = r.body_mut().read_to_string().unwrap_or_default();
    if r.status() != 200 || !body.starts_with("ok") {
        bail!("{remote}/healthz answered {}", r.status());
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub files_sent: usize,
    pub saved: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
    pub overrides_sent: usize,
    pub overrides_added: usize,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut s = format!("{} file(s) sent, {} saved, {} already there", self.files_sent, self.saved.len(), self.skipped.len());
        s.push_str(&format!("; {} override(s) sent, {} added", self.overrides_sent, self.overrides_added));
        if !self.errors.is_empty() {
            s.push_str(&format!("; errors: {}", self.errors.join("; ")));
        }
        s
    }
}

fn upload_batch(agent: &ureq::Agent, remote: &str, batch: &[(String, Vec<u8>)], rep: &mut Report) -> Result<()> {
    let out = upload::post_files(agent, remote, batch)?;
    rep.files_sent += batch.len();
    rep.saved.extend(out.saved);
    rep.skipped.extend(out.skipped);
    rep.errors.extend(out.errors);
    Ok(())
}

/// Push what the remote lacks. Errors when the remote answers but can't
/// take a push (a server too old to have /api/files).
pub fn push(remote: &str, log_line: &dyn Fn(&str)) -> Result<Report> {
    let agent = upload::agent(Duration::from_secs(120));
    let mut r = agent.get(&format!("{remote}/api/files")).call()?;
    if r.status() == 404 {
        bail!("{remote} has no /api/files (not a anser-server server yet)");
    }
    if r.status() != 200 {
        bail!("{remote}/api/files answered {}", r.status());
    }
    let known: Known = serde_json::from_slice(&r.body_mut().with_config().limit(64 << 20).read_to_vec()?)
        .context("/api/files is not the expected JSON")?;
    let mut rep = Report::default();

    // files: by content hash, batched under the remote's request cap
    let mut batch: Vec<(String, Vec<u8>)> = Vec::new();
    let mut batch_bytes = 0;
    for p in transaction_files(&config::transactions_dir()) {
        let bytes = std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
        if known.hashes.contains(&md5_hex(&bytes)) {
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if bytes.len() > BATCH_BYTES {
            rep.errors.push(format!("{name}: too large to push ({} MB)", bytes.len() >> 20));
            continue;
        }
        if batch_bytes + bytes.len() > BATCH_BYTES && !batch.is_empty() {
            upload_batch(&agent, remote, &batch, &mut rep)?;
            batch.clear();
            batch_bytes = 0;
        }
        log_line(&format!("[push] {name}"));
        batch_bytes += bytes.len();
        batch.push((name, bytes));
    }
    if !batch.is_empty() {
        upload_batch(&agent, remote, &batch, &mut rep)?;
    }

    // overrides: by (source, raw_hash)
    let local = finance_engine::db::open().ok().and_then(|c| load_overrides(&c).ok()).unwrap_or_default();
    let missing: Vec<&Override> = local
        .iter()
        .filter(|o| !known.overrides.contains(&OverrideKey { source: o.source.clone(), raw_hash: o.raw_hash.clone() }))
        .collect();
    if !missing.is_empty() {
        let body = serde_json::to_vec(&missing)?;
        let mut req = agent.post(&format!("{remote}/api/overrides")).header("Content-Type", "application/json");
        // the remote's device token: $FINANCE_TOKEN, else the tokens file
        if let Some(b) = finance_core::auth::bearer(remote) {
            req = req.header("Authorization", &b);
        }
        let mut r = req.send(&body[..])?;
        if r.status() == 401 {
            bail!("{remote} wants a device token: pair this machine (anser-server pair on the remote, then the desktop app's Settings), or set FINANCE_TOKEN");
        }
        if r.status() != 200 {
            bail!("{remote}/api/overrides answered {}", r.status());
        }
        let v: Value = serde_json::from_slice(&r.body_mut().read_to_vec()?).unwrap_or(Value::Null);
        rep.overrides_sent = missing.len();
        rep.overrides_added = v.get("added").and_then(Value::as_u64).unwrap_or(0) as usize;
    }
    Ok(rep)
}

/// probe + push, for `serve --push-to` and the CLI: Ok(None) when the remote
/// is unreachable (that is the normal offline case, not an error).
pub fn push_if_reachable(remote: &str, log_line: &dyn Fn(&str)) -> Result<Option<Report>> {
    if let Err(e) = probe(remote) {
        log_line(&format!("[push] {remote} not reachable ({e})"));
        return Ok(None);
    }
    push(remote, log_line).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hashes_and_override_merge() {
        let dir = std::env::temp_dir().join(format!("finance-push-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.csv"), b"x").unwrap();
        std::fs::write(dir.join("note.txt"), b"ignored").unwrap();
        // the full schema: a landed override recategorizes for real
        let conn = finance_engine::db::memory().unwrap();
        conn.execute_batch(
            "INSERT INTO ingested_files(path,source,file_hash,row_count,rows_added,ingested_at,size,mtime_ns)
               VALUES('/gone/old.csv','n26','deadbeef',1,1,0,1,1);
             INSERT INTO txn_overrides(source,raw_hash,category,share,note,created_at) VALUES('hsbc','h1','Rent',NULL,NULL,1);",
        )
        .unwrap();
        let k = known(&conn, &dir);
        assert!(k.hashes.contains("deadbeef"), "ingested, even if the file is gone");
        assert!(k.hashes.contains(&md5_hex(b"x")), "on disk, not ingested yet");
        assert_eq!(k.hashes.len(), 2);
        assert_eq!(k.overrides.len(), 1);

        let push = json!([
            {"source": "hsbc", "raw_hash": "h1", "category": "Other", "share": null, "note": null, "created_at": 2},
            {"source": "n26", "raw_hash": "h2", "category": "Deposit", "share": 0.5, "note": "x", "created_at": 3}
        ]);
        let v = accept_overrides(&conn, push.to_string().as_bytes()).unwrap();
        assert_eq!(v["added"], 1);
        let cat: String = conn.query_row("SELECT category FROM txn_overrides WHERE raw_hash='h1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cat, "Rent", "the remote's own override wins");
        let again = accept_overrides(&conn, push.to_string().as_bytes()).unwrap();
        assert_eq!(again["added"], 0, "idempotent");
        let _ = std::fs::remove_dir_all(dir);
    }
}
