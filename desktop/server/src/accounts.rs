//! Making accounts, and the profile picture (gate.rs has already let the
//! request in; api.rs hands these over):
//!
//! * `POST /api/signup` {name, email?, invite?, device_name?} -> a device
//!   token (the desktop); `POST /api/signup/session` the same for a browser:
//!   a session cookie instead (with `X-Finance-Request`). The first account
//!   on a server with no users becomes the admin; any other needs an invite
//!   (or FINANCE_ALLOW_SIGNUP) and has no data yet (auth.rs). Wrong invites
//!   count against the same throttle as pairing codes.
//! * `POST /api/admin/invite` -> {code, expires}: one account, 24 hours.
//! * `GET /api/profile/avatar[?refresh=1]`: the picture (PNG, 256×256), 404
//!   when the avatar is initials; `PUT /api/profile/avatar` (the picture's
//!   bytes, raw or multipart): resized, kept, the kind set to upload;
//!   `POST /api/profile/avatar/gravatar` {email}: fetched by the server, the
//!   kind set to gravatar (404 "no Gravatar for this email");
//!   `DELETE /api/profile/avatar`: back to initials.

use anyhow::Result;
use serde_json::{json, Value};
use tiny_http::Request;

use finance_engine::{db, profile};

use crate::auth::{self, CodeError, SignupError};
use crate::avatar::{self, Gravatar, PictureError};
use crate::gate::{self, Who};
use crate::log::log;
use crate::server::{header, read_body, Reply};

pub fn route(req: &mut Request, method: &str, path: &str, who: &Who) -> Option<Result<Reply>> {
    Some(match (method, path) {
        ("POST", "/api/signup") => signup(req, false),
        ("POST", "/api/signup/session") => signup(req, true),
        ("POST", "/api/admin/invite") => invite(who),
        ("GET", "/api/profile/avatar") => {
            let refresh = req.url().split_once('?').is_some_and(|(_, q)| q.split('&').any(|kv| kv == "refresh=1"));
            get_avatar(refresh)
        }
        // POST too: the web app's file input sends its picks with POST
        ("PUT" | "POST", "/api/profile/avatar") => put_avatar(req),
        ("DELETE", "/api/profile/avatar") => clear_avatar(),
        ("POST", "/api/profile/avatar/gravatar") => use_gravatar(req),
        _ => return None,
    })
}

fn refusal(status: u16, error: impl std::fmt::Display, reason: &str) -> Reply {
    Reply::json_status(status, &json!({"ok": false, "error": error.to_string(), "reason": reason}))
}

/// Who a request is from, for counting wrong codes ([`auth::client_id`]).
pub fn client(req: &Request) -> String {
    auth::client_id(req.remote_addr().map(|a| a.ip()), header(req, "X-Forwarded-For").as_deref())
}

/// The 429 for a client with too many wrong codes, with the real time left.
pub fn throttled_reply(wait_ms: i64) -> Reply {
    let mins = ((wait_ms.max(0) + 59_999) / 60_000).max(1);
    refusal(429, format!("Too many wrong codes: try again in {mins} minute{}.", if mins == 1 { "" } else { "s" }), "throttled")
}

fn body_json(req: &mut Request) -> Result<Value> {
    let b = read_body(req, 8192)?;
    Ok(if b.is_empty() { Value::Null } else { serde_json::from_slice(&b).unwrap_or(Value::Null) })
}

/// A browser's device name from its User-Agent ("Chrome (web)").
pub fn browser_name(req: &Request) -> String {
    let ua = header(req, "User-Agent").unwrap_or_default();
    // Chrome's User-Agent says Safari too: Chrome first
    const KINDS: [(&str, &str); 7] =
        [("iPhone", "iPhone"), ("iPad", "iPad"), ("Android", "Android"), ("Firefox", "Firefox"), ("Edg/", "Edge"), ("Chrome", "Chrome"), ("Safari", "Safari")];
    let kind = KINDS.iter().find(|(k, _)| ua.contains(k)).map_or("browser", |(_, name)| name);
    format!("{kind} (web)")
}

fn signup(req: &mut Request, browser: bool) -> Result<Reply> {
    let who_is = client(req);
    if let Some(wait) = auth::throttled(&who_is) {
        return Ok(throttled_reply(wait));
    }
    let body = body_json(req)?;
    let s = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
    let name = s("name").unwrap_or_default();
    let email = s("email");
    let invite = s("invite");
    let device = match s("device_name").filter(|n| !n.trim().is_empty()) {
        Some(n) => n,
        None if browser => browser_name(req),
        None => "device".into(),
    };
    match auth::signup(&name, email.as_deref(), invite.as_deref(), &device) {
        Ok(made) => {
            log(&format!("[auth] account made: user {}{}, device {}", made.user_id, if made.first { " (first, admin)" } else { "" }, made.device.id));
            if made.first {
                // the owner's name on the profile, unless one is set
                if let Ok(conn) = db::open() {
                    let p = profile::get(&conn).ok().flatten();
                    if p.as_ref().is_none_or(|p| p.name.is_none()) {
                        let mut p = p.unwrap_or_else(|| profile::Profile { base_currency: finance_engine::config::NEW_PROFILE_CURRENCY.into(), ..Default::default() });
                        p.name = Some(name.trim().to_string());
                        let _ = profile::put(&conn, &p);
                    }
                }
            }
            let mut v = json!({
                "ok": true,
                "first": made.first,
                "data": made.device.has_data,
                "user": {"id": made.user_id, "name": made.device.user_name, "role": made.device.role},
                "device": {"id": made.device.id, "name": made.device.name},
            });
            if !made.first {
                v["note"] = json!("Your account is made, with its own data: nobody else on this server can see it.");
            }
            if browser {
                let mut r = Reply::json(&v);
                r.headers.push(("Set-Cookie", gate::set_cookie(&made.token)));
                Ok(r)
            } else {
                v["token"] = json!(made.token);
                Ok(Reply::json(&v))
            }
        }
        Err(e) => {
            if let Some(c) = e.downcast_ref::<CodeError>() {
                auth::note_failure(&who_is);
                log("[auth] a signup with a bad invite was refused");
                return Ok(refusal(400, c, c.reason()));
            }
            match e.downcast_ref::<SignupError>() {
                Some(SignupError::Closed) => Ok(refusal(403, SignupError::Closed, "closed")),
                Some(s @ SignupError::Invalid(_)) => Ok(refusal(400, s, "invalid")),
                None => Err(e),
            }
        }
    }
}

fn invite(who: &Who) -> Result<Reply> {
    let uid = who.device().map(|d| d.user_id).unwrap_or(1);
    let (code, expires) = auth::new_invite(uid)?;
    log(&format!("[auth] user {uid} made an invite"));
    Ok(Reply::json(&json!({"code": code, "expires": expires})))
}

fn profile_reply(conn: &rusqlite::Connection) -> Reply {
    Reply::json(&json!({"ok": true, "profile": profile::bundle_profile(conn)}))
}

fn get_avatar(refresh: bool) -> Result<Reply> {
    let conn = db::open()?;
    let p = profile::get(&conn)?.unwrap_or_default();
    match avatar::picture(p.avatar_kind.as_deref(), p.avatar_email.as_deref(), refresh) {
        Some(png) => Ok(Reply::new(200, "image/png", png)),
        None => Ok(refusal(404, "no picture: the avatar is initials", "none")),
    }
}

fn put_avatar(req: &mut Request) -> Result<Reply> {
    let length: usize = header(req, "Content-Length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    // multipart wraps the file in a little framing
    let cap = avatar::MAX_UPLOAD + 64 * 1024;
    if length > cap {
        return Ok(refusal(413, PictureError::TooLarge, "too_large"));
    }
    let Ok(body) = read_body(req, cap) else { return Ok(refusal(413, PictureError::TooLarge, "too_large")) };
    let ctype = header(req, "Content-Type").unwrap_or_default();
    let bytes = if ctype.starts_with("multipart/form-data") {
        match crate::upload::parse_multipart(&ctype, &body)?.into_iter().next() {
            Some(p) => p.data,
            None => return Ok(refusal(400, "no file in the upload", "invalid")),
        }
    } else {
        body
    };
    let png = match avatar::to_square_png(&bytes) {
        Ok(p) => p,
        Err(e) => return Ok(refusal(if e == PictureError::TooLarge { 413 } else { 422 }, e, "picture")),
    };
    avatar::save_upload(&png)?;
    let conn = db::open()?;
    profile::picture_changed(&conn, "upload", None)?;
    log(&format!("[avatar] new picture ({} bytes)", png.len()));
    Ok(profile_reply(&conn))
}

fn use_gravatar(req: &mut Request) -> Result<Reply> {
    let body = body_json(req)?;
    let email = body.get("email").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if email.len() > 200 || !email.split_once('@').is_some_and(|(a, b)| !a.is_empty() && b.contains('.')) {
        return Ok(refusal(400, "That email doesn't look right.", "invalid"));
    }
    match avatar::fetch_gravatar(&email) {
        Ok(Gravatar::Missing) => Ok(refusal(404, "No Gravatar for this email.", "missing")),
        Ok(Gravatar::Found) => {
            let conn = db::open()?;
            profile::picture_changed(&conn, "gravatar", Some(&email))?;
            Ok(profile_reply(&conn))
        }
        Err(e) => Ok(refusal(502, format!("{e:#}"), "unreachable")),
    }
}

fn clear_avatar() -> Result<Reply> {
    avatar::remove_upload();
    let conn = db::open()?;
    profile::picture_changed(&conn, "initials", None)?;
    Ok(profile_reply(&conn))
}
