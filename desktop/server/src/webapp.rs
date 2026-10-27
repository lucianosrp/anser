//! The web app: the desktop app's wasm build (desktop/web), served from the
//! data tree so installing it is copying files, no rebuild of the server.
//!
//! * `GET /app/` (and `/` once installed) -> `data/webapp/index.html`;
//!   `GET /app/<file>` -> that file. A `<file>.gz` sibling is sent instead
//!   when the client accepts gzip (the build script writes one for the
//!   wasm). Files carry an ETag and `Cache-Control: no-cache`, so a phone
//!   revalidates instead of downloading the wasm again.
//! * `GET /sw.js` -> the bundle's service worker, from the root so it may
//!   control `/` as well as `/app/` (an installable app). no-cache like the
//!   rest: the browser checks it for a new build on every navigation.
//! * `GET /api/ledger.json` -> every ledger row (`finance_core::Ledger`,
//!   the same rows the desktop app reads from its SQLite replica), gzipped
//!   when accepted. The browser can't open SQLite.
//! * `POST /api/webapp` installs a bundle: `multipart/form-data` with the
//!   files in the `files` field (the /upload framing), plus the header
//!   `X-Finance-Install: 1`. Like /upload it is reachable from the tailnet
//!   only (tailscale serve); the custom header forces a CORS preflight this
//!   server never grants, so a web page elsewhere can't post a bundle
//!   through the owner's browser. The files land in `data/webapp.new/`,
//!   which then replaces `data/webapp/` in two renames.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};

use finance_engine::config;

/// Upper bound for one install request (the wasm, its .gz, glue, fonts).
pub const MAX_INSTALL_BYTES: usize = 128 * 1024 * 1024;
/// The files a bundle must have.
pub const REQUIRED: [&str; 3] = ["index.html", "finance_web.js", "finance_web_bg.wasm"];

pub fn dir() -> PathBuf {
    config::root_data_dir().join("webapp")
}

pub fn installed() -> bool {
    dir().join("index.html").is_file()
}

/// A file served from the bundle (or a generated body), with its headers.
pub struct Served {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub headers: Vec<(&'static str, String)>,
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "json" => "application/json; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ttf" => "font/ttf",
        "woff2" => "font/woff2",
        "css" => "text/css; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Bundle file names: one path segment of `[A-Za-z0-9._-]`, no leading dot.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn accepts_gzip(accept_encoding: Option<&str>) -> bool {
    accept_encoding.is_some_and(|v| v.split(',').any(|e| e.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("gzip")))
}

fn etag(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    Some(format!("\"{:x}-{:x}\"", m.len(), mtime))
}

/// `name` from the installed bundle ("" = index.html). None = no such file.
pub fn file(name: &str, accept_encoding: Option<&str>, if_none_match: Option<&str>) -> Option<Served> {
    let name = if name.is_empty() { "index.html" } else { name };
    if !valid_name(name) {
        return None;
    }
    let plain = dir().join(name);
    if !plain.is_file() {
        return None;
    }
    let gz = dir().join(format!("{name}.gz"));
    let use_gz = accepts_gzip(accept_encoding) && gz.is_file();
    let path = if use_gz { gz } else { plain };
    let tag = etag(&path);
    let mut headers = vec![("Cache-Control", "no-cache".to_string()), ("Vary", "Accept-Encoding".to_string())];
    if let Some(t) = &tag {
        headers.push(("ETag", t.clone()));
    }
    if tag.is_some() && if_none_match.is_some_and(|v| v.split(',').any(|x| x.trim() == tag.as_deref().unwrap_or(""))) {
        return Some(Served { status: 304, content_type: content_type(name), body: vec![], headers });
    }
    let body = std::fs::read(&path).ok()?;
    if use_gz {
        headers.push(("Content-Encoding", "gzip".to_string()));
    }
    Some(Served { status: 200, content_type: content_type(name), body, headers })
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut enc = flate2::write::GzEncoder::new(Vec::with_capacity(bytes.len() / 6), flate2::Compression::fast());
    enc.write_all(bytes)?;
    Ok(enc.finish()?)
}

/// JSON, gzipped when the client takes it (the ledger is a few MB of
/// repetitive text: ~10x smaller over the phone's link).
pub fn json_bytes(body: Vec<u8>, accept_encoding: Option<&str>) -> Served {
    let ct = "application/json; charset=utf-8";
    if accepts_gzip(accept_encoding) {
        if let Ok(z) = gzip(&body) {
            return Served { status: 200, content_type: ct, body: z, headers: vec![("Content-Encoding", "gzip".into()), ("Vary", "Accept-Encoding".into())] };
        }
    }
    Served { status: 200, content_type: ct, body, headers: vec![] }
}

/// Every ledger row of the server's database, as `finance_core::Ledger` JSON.
pub fn ledger_json() -> Result<Vec<u8>> {
    finance_core::Ledger::load(&config::db_path())?.to_json()
}

/// Install a bundle from (file name, bytes) parts. Every name must be
/// valid and the required files present; then the new tree replaces the
/// old one. Returns what was installed.
pub fn install(parts: &[(String, Vec<u8>)]) -> Result<Value> {
    ensure!(!parts.is_empty(), "no files in the request");
    for (name, _) in parts {
        ensure!(valid_name(name), "bad file name {name:?}");
    }
    for req in REQUIRED {
        ensure!(parts.iter().any(|(n, d)| n == req && !d.is_empty()), "the bundle has no {req}");
    }
    let wasm = &parts.iter().find(|(n, _)| n == "finance_web_bg.wasm").context("no wasm")?.1;
    if !wasm.starts_with(b"\0asm") {
        bail!("finance_web_bg.wasm is not WebAssembly");
    }
    let root = config::root_data_dir();
    std::fs::create_dir_all(&root)?;
    let new = root.join("webapp.new");
    let old = root.join("webapp.old");
    let _ = std::fs::remove_dir_all(&new);
    let _ = std::fs::remove_dir_all(&old);
    std::fs::create_dir_all(&new).with_context(|| format!("creating {}", new.display()))?;
    let mut bytes = 0usize;
    for (name, data) in parts {
        std::fs::write(new.join(name), data).with_context(|| format!("writing {name}"))?;
        bytes += data.len();
    }
    let cur = dir();
    if cur.exists() {
        std::fs::rename(&cur, &old).context("moving the old bundle aside")?;
    }
    std::fs::rename(&new, &cur).context("putting the new bundle in place")?;
    let _ = std::fs::remove_dir_all(&old);
    let mut names: Vec<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    Ok(json!({"installed": names, "bytes": bytes, "dir": cur.display().to_string()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("finance_web_bg.wasm.gz"));
        assert!(valid_name("index.html"));
        assert!(!valid_name("../x"));
        assert!(!valid_name(".env"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(""));
    }

    #[test]
    fn gzip_negotiation() {
        assert!(accepts_gzip(Some("gzip, deflate, br")));
        assert!(accepts_gzip(Some("br;q=1.0, gzip;q=0.8")));
        assert!(!accepts_gzip(Some("br")));
        assert!(!accepts_gzip(None));
    }
}
