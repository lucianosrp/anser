//! MCP on the server: the tools are the engine's (finance_engine::mcp);
//! here are the live [`Source`] they read, the Streamable HTTP endpoint and
//! the key management behind Settings.
//!
//! * `anser-server mcp [--user NAME|ID]`: MCP over stdio for one user's tree
//!   (cli/mcp.rs). Local shell access already, so no switch and no key.
//! * `POST /mcp`: JSON-RPC messages (one, or a batch) answered as
//!   `application/json` (the spec's JSON response mode: no SSE stream, so
//!   `GET /mcp` is 405), a notifications-only POST as 202 with no body.
//!   Stateless: no `Mcp-Session-Id`, and the `MCP-Protocol-Version` header
//!   is accepted and ignored. gate.rs lets in a bearer key only (no
//!   cookie), from a known Host and Origin; the request then runs as the
//!   key's user. A user who has not switched MCP on (`PUT /api/mcp`,
//!   `anser-server mcp-enable`) gets the same 404 as for an unknown route.
//! * MCP keys: a device token with scope `mcp` (auth::add_mcp_key), good for
//!   `/mcp` and refused everywhere else. `GET /api/mcp` {enabled, path,
//!   keys: [{id, name, created, last_used}]} (never a secret), `PUT
//!   /api/mcp` {enabled}, `POST /api/mcp/keys` {name} -> the token, once,
//!   `DELETE /api/mcp/keys/{id}`. Every one acts on the caller's own user:
//!   a key of another user is a 404.
//!
//! The server is published on the tailnet only (DEPLOY.md); nothing here
//! opens it further, and nothing writes: the tools are read-only.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use tiny_http::Request;

use finance_core::Ledger;
use finance_engine::mcp::{self, Source};
use finance_engine::views::{api, cache, Ctx, MarketFeed};
use finance_engine::{config, db, profile, secrets};

use crate::feed::{FEED, REQUEST_FEED};
use crate::gate::Who;
use crate::log::log;
use crate::server::{read_body, Reply};
use crate::{auth, live};

/// The engine tools over one connection of this process's tenant.
pub struct ServerSource<'a> {
    conn: &'a Connection,
    feed: &'static crate::feed::LiveFeed,
    /// stdio: wait for the live valuation. HTTP: the cached one, refreshed
    /// behind (live.rs), so a call never waits on an exchange.
    blocking: bool,
}

impl<'a> ServerSource<'a> {
    /// For `anser-server mcp`: fetches through the blocking feed.
    pub fn stdio(conn: &'a Connection) -> Self {
        ServerSource { conn, feed: &FEED, blocking: true }
    }

    /// For `/mcp`: the stale-while-revalidate feed, like every page.
    pub fn http(conn: &'a Connection) -> Self {
        ServerSource { conn, feed: &REQUEST_FEED, blocking: false }
    }
}

impl Source for ServerSource<'_> {
    fn conn(&self) -> &Connection {
        self.conn
    }

    fn feed(&self) -> &dyn MarketFeed {
        self.feed
    }

    /// Kept per db fingerprint with the other shared inputs, so repeated
    /// calls don't read every row again.
    fn ledger(&self) -> Result<Arc<Ledger>> {
        let ctx = Ctx::new(self.conn, self.feed);
        let load = || Ledger::load(&config::db_path());
        if let Some(l) = &*cache::shared(&ctx, "mcp_ledger", "", || load().ok().map(Arc::new)) {
            return Ok(l.clone());
        }
        Ok(Arc::new(load()?))
    }

    fn net_worth(&self) -> Value {
        if self.blocking {
            let ctx = Ctx::new(self.conn, self.feed);
            api::get_net_worth_now(&ctx, true).unwrap_or_else(|_| ctx.net_worth_offline().clone())
        } else {
            live::net_worth()
        }
    }
}

fn bad(status: u16, msg: &str) -> Reply {
    Reply::json_status(status, &json!({"ok": false, "error": msg}))
}

/// The routes here; None = not one of them (server.rs carries on).
pub fn route(req: &mut Request, method: &str, path: &str, who: &Who) -> Option<Result<Reply>> {
    match path {
        "/mcp" => Some(endpoint(req, method, who)),
        "/api/mcp" => match method {
            "GET" => Some(status(who)),
            "PUT" => Some(set_enabled(req, who)),
            _ => None,
        },
        "/api/mcp/keys" if method == "POST" => Some(new_key(req, who)),
        _ => path.strip_prefix("/api/mcp/keys/").filter(|_| method == "DELETE").map(|id| revoke(id, who)),
    }
}

/// The caller's user (without a device, in an open mode: the owner's tree).
fn user_of(who: &Who) -> i64 {
    who.device().map(|d| d.user_id).unwrap_or_else(secrets::active_user)
}

fn endpoint(req: &mut Request, method: &str, who: &Who) -> Result<Reply> {
    // the gate only lets a bearer key in; once more here, since this is the door to the data
    if !matches!(who, Who::Bearer(_)) {
        return Ok(bad(401, "send an MCP key: Authorization: Bearer <key>"));
    }
    let conn = db::open()?;
    if !profile::mcp_enabled(&conn) {
        return Ok(Reply::text(404, "404: no route for /mcp"));
    }
    if method != "POST" {
        // no server-initiated stream, so no GET; no sessions, so no DELETE
        let mut r = Reply::text(405, "method not allowed: POST JSON-RPC messages to /mcp");
        r.headers.push(("Allow", "POST".into()));
        return Ok(r);
    }
    let Ok(body) = read_body(req, 4 << 20) else { return Ok(bad(413, "body too large")) };
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return Ok(Reply::json_status(400, &mcp::parse_error()));
    };
    Ok(match mcp::handle_payload(&ServerSource::http(&conn), &payload) {
        Some(reply) => Reply::json(&reply),
        // only notifications (or responses): accepted, nothing to say
        None => Reply { status: 202, content_type: None, location: None, body: vec![], headers: vec![], stream: None },
    })
}

fn status(who: &Who) -> Result<Reply> {
    let enabled = profile::mcp_enabled(&db::open()?);
    Ok(Reply::json(&json!({"enabled": enabled, "path": "/mcp", "keys": auth::mcp_keys(user_of(who))?})))
}

fn json_body(req: &mut Request) -> Result<Value> {
    let body = read_body(req, 4096)?;
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).map_err(|_| anyhow!("the body is not JSON"))
}

fn set_enabled(req: &mut Request, who: &Who) -> Result<Reply> {
    let Some(on) = json_body(req).ok().and_then(|b| b.get("enabled").and_then(Value::as_bool)) else {
        return Ok(bad(400, "send {\"enabled\": true} or {\"enabled\": false}"));
    };
    profile::set_mcp_enabled(&db::open()?, on)?;
    log(&format!("[mcp] {} for user {}", if on { "enabled" } else { "disabled" }, user_of(who)));
    Ok(Reply::json(&json!({"ok": true, "enabled": on})))
}

fn new_key(req: &mut Request, who: &Who) -> Result<Reply> {
    let name = json_body(req).ok().and_then(|b| b.get("name").and_then(Value::as_str).map(|n| n.trim().to_string())).unwrap_or_default();
    if name.is_empty() || name.chars().count() > 60 {
        return Ok(bad(400, "name the key (1 to 60 characters): which client will use it"));
    }
    let uid = user_of(who);
    let (token, key) = match auth::add_mcp_key(uid, &name) {
        Ok(k) => k,
        Err(e) => return Ok(bad(400, &e.to_string())),
    };
    log(&format!("[mcp] key {} made for user {uid}", key.id));
    Ok(Reply::json(&json!({"ok": true, "id": key.id, "name": key.name, "created": key.created, "token": token})))
}

fn revoke(id: &str, who: &Who) -> Result<Reply> {
    let Ok(id) = id.parse::<i64>() else { return Ok(bad(400, "not a key id")) };
    let uid = user_of(who);
    if !auth::revoke_mcp_key(uid, id)? {
        return Ok(bad(404, "no such key"));
    }
    log(&format!("[mcp] key {id} of user {uid} revoked"));
    Ok(Reply::json(&json!({"ok": true})))
}
