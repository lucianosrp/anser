//! Sending transaction files to the server: the client half of `POST
//! /upload` (anser-server, server/src/upload.rs). The desktop app's Upload
//! page and `anser-server push` share it.
//!
//! The contract:
//! * `multipart/form-data`, file parts in the field `files`, at most
//!   [`MAX_BYTES`] per request (Content-Length is checked first)
//! * only `.csv` / `.xml` / `.pdf` / `.xlsx` / `.xls` ([`EXTS`]), nothing
//!   empty; names are sanitized
//!   ([`sanitize`]). Same name + same content is skipped; same name + new
//!   content gets a ` (2)` suffix
//! * the reply is a `303` whose `Location` is `/upload?saved=..&skipped=..&err=..`
//!   ([`Outcome::from_location`])
//! * the server ingests during the POST, so a sync right after an upload
//!   sees the new rows
//!
//! What the server already has, by content: `GET /api/files`, and the
//! replica's `ingested_files` table (also offline), whose `file_hash` is the
//! same md5 ([`md5_hex`]).

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use md5::{Digest, Md5};
use rusqlite::Connection;

/// The extensions the server takes.
pub const EXTS: [&str; 5] = [".csv", ".xml", ".pdf", ".xlsx", ".xls"];

/// The server's request cap: 20 MB.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;
/// Room for the multipart framing around one file.
const FRAMING: usize = 1024;

/// tailnet-aware, statuses as values, redirects NOT followed: the 303 from
/// /upload carries the outcome in its Location.
pub fn agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    ureq::Agent::with_parts(config, ureq::unversioned::transport::DefaultConnector::new(), crate::tailnet::Resolver)
}

/// Lowercase hex md5: ingested_files.file_hash, and /api/files' hashes.
pub fn md5_hex(bytes: &[u8]) -> String {
    Md5::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// [`md5_hex`] of a file, read in chunks (not held whole).
pub fn md5_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut h = Md5::new();
    let mut f = std::fs::File::open(path)?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The server's file-name rule: the base name (after the last `/`), then every char
/// outside `[A-Za-z0-9._ ()-]` becomes `_`, then strip `" ."`.
pub fn sanitize(raw: &str) -> String {
    let base = raw.rsplit('/').next().unwrap_or(raw);
    let cleaned: String =
        base.chars().map(|c| if c.is_ascii_alphanumeric() || "._ ()-".contains(c) { c } else { '_' }).collect();
    cleaned.trim_matches(|c| c == ' ' || c == '.').to_string()
}

/// The name to upload a file under: sanitized like the server will, with
/// the extension lowercased, because ingest only globs lowercase `*.csv`,
/// `*.xml`, `*.pdf`, `*.xlsx`, `*.xls` (an uploaded `X.CSV` would sit there
/// unread).
pub fn upload_name(file_name: &str) -> String {
    let name = sanitize(file_name);
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}{}", &name[..i], name[i..].to_lowercase()),
        _ => name,
    }
}

/// Can `bytes` go in one request?
pub fn fits(bytes: usize) -> bool {
    bytes + FRAMING <= MAX_BYTES
}

/// A multipart/form-data body with every file in the `files` field.
pub fn encode_multipart(boundary: &str, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, data) in files {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"files\"; filename=\"{}\"\r\n", name.replace(['"', '\r', '\n'], "_"))
                .as_bytes(),
        );
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

/// What the server did with an upload: names as it saved them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub saved: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

impl Outcome {
    /// Parse `saved=..&skipped=..&err=..` from the /upload redirect.
    pub fn from_location(location: &str) -> Outcome {
        let mut out = Outcome::default();
        let q = location.split_once('?').map(|x| x.1).unwrap_or("");
        for kv in q.split('&') {
            let Some((k, v)) = kv.split_once('=') else { continue };
            let v = decode_form(v);
            match k {
                "saved" => out.saved.extend(v.split(", ").map(String::from)),
                "skipped" => out.skipped.extend(v.split(", ").map(String::from)),
                "err" => out.errors.push(v),
                _ => {}
            }
        }
        out
    }
}

/// `application/x-www-form-urlencoded` value decoding (`+` and `%XX`).
pub fn decode_form(s: &str) -> String {
    let s = s.replace('+', " ");
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn boundary() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("finance-upload-{nanos:x}")
}

/// `POST {server}/upload` with `files` (name, bytes) in one request; the
/// caller keeps it under [`MAX_BYTES`].
pub fn post_files(agent: &ureq::Agent, server: &str, files: &[(String, Vec<u8>)]) -> Result<Outcome> {
    crate::demo::refuse(server)?;
    let boundary = boundary();
    let body = encode_multipart(&boundary, files);
    if body.len() > MAX_BYTES {
        bail!("upload too large ({} MB, the server takes 20 MB)", body.len() >> 20);
    }
    let url = format!("{}/upload", server.trim_end_matches('/'));
    let mut req = agent.post(&url).header("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
    if let Some(b) = crate::auth::bearer(server) {
        req = req.header("Authorization", &b);
    }
    let r = req
        .send(&body[..])
        .with_context(|| format!("POST {url}"))?;
    let loc = r.headers().get("Location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if r.status() != 303 || loc.is_empty() {
        bail!("{url} answered HTTP {}", r.status());
    }
    Ok(Outcome::from_location(&loc))
}

/// The content hashes the server holds, from `GET {server}/api/files` (the
/// push contract). `Ok(None)` when the server has no such route: then
/// [`replica_hashes`] is all there is.
pub fn server_hashes(agent: &ureq::Agent, server: &str) -> Result<Option<BTreeSet<String>>> {
    crate::demo::refuse(server)?;
    let url = format!("{}/api/files", server.trim_end_matches('/'));
    let mut req = agent.get(&url);
    if let Some(b) = crate::auth::bearer(server) {
        req = req.header("Authorization", &b);
    }
    let mut r = req.call().with_context(|| format!("GET {url}"))?;
    if r.status() == 404 {
        return Ok(None);
    }
    if r.status() != 200 {
        bail!("{url} answered HTTP {}", r.status());
    }
    let v: serde_json::Value = serde_json::from_slice(&r.body_mut().with_config().limit(64 << 20).read_to_vec()?)
        .with_context(|| format!("{url} is not JSON"))?;
    let hashes = v.get("hashes").and_then(|h| h.as_array()).context("/api/files without hashes")?;
    Ok(Some(hashes.iter().filter_map(|h| h.as_str().map(String::from)).collect()))
}

/// `PUT {server}/api/formats/{id}`: install a format definition (TOML) on
/// the Rust server, which validates it and ingests what it now reads.
pub fn put_format(agent: &ureq::Agent, server: &str, id: &str, toml: &str) -> Result<()> {
    crate::demo::refuse(server)?;
    let url = format!("{}/api/formats/{id}", server.trim_end_matches('/'));
    // a write: the server wants this device's token (FINANCE_AUTH)
    let mut req = agent.put(&url).header("X-Finance-Format", "1").header("Content-Type", "application/toml");
    if let Some(b) = crate::auth::bearer(server) {
        req = req.header("Authorization", &b);
    }
    let mut r = req
        .send(toml.as_bytes())
        .with_context(|| format!("PUT {url}"))?;
    match r.status().as_u16() {
        200 => Ok(()),
        404 | 405 => bail!("this server has no /api/formats (an older anser-server): update it first"),
        401 => bail!("this device isn't paired with {server}: pair it in Settings first"),
        s => {
            let why = r.body_mut().read_to_string().unwrap_or_default();
            bail!("{url} answered HTTP {s}: {}", why.chars().take(200).collect::<String>())
        }
    }
}

/// One definition from `GET {server}/api/formats`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFormat {
    pub id: String,
    pub builtin: bool,
    pub toml: String,
}

/// The server's format definitions. `Ok(None)` when it has no such route.
pub fn server_formats(agent: &ureq::Agent, server: &str) -> Result<Option<Vec<RemoteFormat>>> {
    crate::demo::refuse(server)?;
    let url = format!("{}/api/formats", server.trim_end_matches('/'));
    // a read: a token only matters under FINANCE_AUTH=all
    let mut req = agent.get(&url);
    if let Some(b) = crate::auth::bearer(server) {
        req = req.header("Authorization", &b);
    }
    let mut r = req.call().with_context(|| format!("GET {url}"))?;
    if r.status() == 404 {
        return Ok(None);
    }
    if r.status() != 200 {
        bail!("{url} answered HTTP {}", r.status());
    }
    let v: serde_json::Value = serde_json::from_slice(&r.body_mut().with_config().limit(8 << 20).read_to_vec()?)
        .with_context(|| format!("{url} is not JSON"))?;
    let list = v.get("formats").and_then(|f| f.as_array()).context("/api/formats without formats")?;
    Ok(Some(
        list.iter()
            .filter_map(|f| {
                Some(RemoteFormat {
                    id: f.get("id")?.as_str()?.to_string(),
                    builtin: f.get("builtin").and_then(|b| b.as_bool()).unwrap_or(false),
                    toml: f.get("toml")?.as_str()?.to_string(),
                })
            })
            .collect(),
    ))
}

fn open_ro(db: &Path) -> Result<Connection> {
    crate::replica::open_db(db)
}

/// Every file hash the replica's `ingested_files` records (the server's,
/// as of the last sync). Empty when there is no replica yet.
pub fn replica_hashes(db: &Path) -> BTreeSet<String> {
    let Ok(conn) = open_ro(db) else { return BTreeSet::new() };
    let Ok(mut st) = conn.prepare("SELECT file_hash FROM ingested_files") else { return BTreeSet::new() };
    st.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
}

/// One row of the server's ingest history (the replica's `ingested_files`).
#[derive(Debug, Clone, PartialEq)]
pub struct Ingested {
    pub name: String,
    pub source: String,
    pub rows: i64,
    /// rows that were new on ingest; None for rows from before it was tracked
    pub added: Option<i64>,
    pub at_ms: i64,
}

/// The newest `limit` ingested files, newest first.
pub fn replica_history(db: &Path, limit: usize) -> Vec<Ingested> {
    let Ok(conn) = open_ro(db) else { return vec![] };
    let sql = "SELECT path, source, row_count, rows_added, ingested_at FROM ingested_files ORDER BY ingested_at DESC, path LIMIT ?";
    let Ok(mut st) = conn.prepare(sql) else { return vec![] };
    st.query_map([limit as i64], |r| {
        let path: String = r.get(0)?;
        Ok(Ingested {
            name: path.rsplit('/').next().unwrap_or(&path).to_string(),
            source: r.get(1)?,
            rows: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            added: r.get(3)?,
            at_ms: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_from_location() {
        let o = Outcome::from_location("/upload?saved=a.csv%2C+b+%282%29.csv&skipped=c.pdf&err=x.txt%3A+only");
        assert_eq!(o.saved, vec!["a.csv", "b (2).csv"]);
        assert_eq!(o.skipped, vec!["c.pdf"]);
        assert_eq!(o.errors, vec!["x.txt: only"]);
        assert_eq!(Outcome::from_location("/upload?err=no+files+received").errors, vec!["no files received"]);
        assert_eq!(Outcome::from_location("/upload"), Outcome::default());
        assert_eq!(decode_form("100%"), "100%");
        assert_eq!(decode_form("%e2%80%93"), "\u{2013}");
    }

    #[test]
    fn multipart_framing() {
        let body = encode_multipart("B", &[("a\".csv".into(), b"x,y\n".to_vec()), ("b.pdf".into(), vec![0, 1])]);
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("--B\r\nContent-Disposition: form-data; name=\"files\"; filename=\"a_.csv\"\r\n"));
        assert!(text.contains("\r\n\r\nx,y\n\r\n--B\r\n"));
        assert!(body.ends_with(b"\x00\x01\r\n--B--\r\n"));
        assert!(fits(MAX_BYTES - 2048) && !fits(MAX_BYTES));
    }

    #[test]
    fn names_like_the_server() {
        assert_eq!(sanitize("C:\\x\\y.csv"), "C__x_y.csv");
        assert_eq!(sanitize(" .hidden.csv. "), "hidden.csv");
        assert_eq!(sanitize("a/b/c (1).csv"), "c (1).csv");
        assert_eq!(upload_name("Revolut \u{2013} Apr.CSV"), "Revolut _ Apr.csv");
        assert_eq!(upload_name("README"), "README");
        assert_eq!(md5_hex(b"x"), "9dd4e461268c8034f5c8564e155c67a6");
    }

    #[test]
    fn replica_reads() {
        let dir = std::env::temp_dir().join(format!("finance-core-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("finance.db");
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE ingested_files(path TEXT PRIMARY KEY, source TEXT, file_hash TEXT, row_count INTEGER,
                   ingested_at INTEGER, size INTEGER, mtime_ns INTEGER, rows_added INTEGER);
                 INSERT INTO ingested_files VALUES('/srv/t/old.csv','n26','h1',10,1,1,1,NULL);
                 INSERT INTO ingested_files VALUES('/srv/t/new.csv','hsbc','h2',5,2,1,1,3);",
            )
            .unwrap();
        assert_eq!(replica_hashes(&db), ["h1".to_string(), "h2".to_string()].into());
        let h = replica_history(&db, 10);
        assert_eq!(h[0], Ingested { name: "new.csv".into(), source: "hsbc".into(), rows: 5, added: Some(3), at_ms: 2 });
        assert_eq!(h[1].added, None);
        assert!(replica_hashes(&dir.join("missing.db")).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
