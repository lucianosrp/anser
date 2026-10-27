//! The format definitions over HTTP (engine `sources::format`):
//!
//! * `GET /api/formats`: every definition in effect (the server's
//!   `$FINANCE_HOME/formats/*.toml`, then the built-in ones), each with its
//!   TOML, plus the files that failed to load and why
//! * `PUT /api/formats/{id}`: install or replace one. The body is the TOML
//!   (at most 64 KB), parsed and validated before it is written, and its
//!   `id` must be the path's. Guarded like `/api/webapp`: the request must
//!   carry `X-Finance-Format: 1`, which a cross-site form can't send without
//!   a preflight this server never answers; and like `/upload` it is
//!   reachable only on the tailnet. Once saved, transactions/ is ingested
//!   again, so files only this definition reads are picked up at once.

use anyhow::{ensure, Result};
use serde_json::{json, Value};
use tiny_http::Request;

use finance_engine::sources::format::{self, FormatKind};

use crate::log::log;
use crate::server::{header, read_body};

pub const MAX_FORMAT_BYTES: usize = 64 * 1024;

fn kind(k: FormatKind) -> &'static str {
    match k {
        FormatKind::Bank => "bank",
        FormatKind::Card => "card",
        FormatKind::Wallet => "wallet",
    }
}

/// GET /api/formats.
pub fn list() -> Value {
    let formats: Vec<Value> = format::formats()
        .into_iter()
        .map(|f| {
            json!({
                "id": f.def.id,
                "label": f.def.label,
                "institution": f.def.institution,
                "country": f.def.country,
                "kind": kind(f.def.kind),
                "builtin": f.builtin,
                "toml": f.toml,
            })
        })
        .collect();
    json!({"formats": formats, "errors": format::load_errors()})
}

/// PUT /api/formats/{id}: validate and save. Returns what was saved.
pub fn handle_put(req: &mut Request, id: &str) -> Result<Value> {
    ensure!(header(req, "X-Finance-Format").as_deref() == Some("1"), "missing X-Finance-Format: 1");
    let body = read_body(req, MAX_FORMAT_BYTES)?;
    let text = String::from_utf8(body).map_err(|_| anyhow::anyhow!("the definition isn't UTF-8 text"))?;
    let dir = format::default_dir();
    let path = format::save_text(&dir, id, &text)?;
    log(&format!("[formats] saved {}", path.display()));
    Ok(json!({"id": id, "saved": true}))
}
