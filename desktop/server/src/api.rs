//! The account and setup routes (gate.rs has already let the request in):
//!
//! * pairing: `POST /api/pair` {code, device_name, resume_key?} -> {token}
//!   (a device: desktop, phone, CLI), `POST /api/login` {code, resume_key?}
//!   -> the session cookie (a browser), `POST /api/logout` {resume_key?} ->
//!   {reconnect_code} with a resume key (the code to jump back in on that
//!   device), `GET /api/whoami`, `GET /login`
//! * admin: `POST /api/admin/pair-code` -> a new code, `POST
//!   /api/admin/auth-mode` {mode}, `GET /api/admin/devices`, `DELETE
//!   /api/admin/devices/{id}`, `POST /api/secrets/import-env`
//! * sources: `GET /api/sources/catalog`, `PUT|DELETE
//!   /api/sources/{key}/secret`, `POST /api/sources/{key}/test`
//! * profile and settings: `GET|PUT /api/profile`, `PUT /api/settings/{key}`
//! * mcp.rs: `POST /mcp` and the MCP keys (`/api/mcp`)
//! * accounts.rs: signup, invites, the profile picture
//! * `POST /api/preview`: what an upload would add, without writing
//!
//! Secrets go in and never come out: no reply, log line or error carries a
//! value (errors from the test are scrubbed of the candidate values).

use std::collections::BTreeMap;
use std::sync::RwLock;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tiny_http::Request;

use finance_engine::fetch::verify::{self, Verdict};
use finance_engine::{catalog, config, db, preview, profile, secrets};

use crate::auth::{self, Mode};
use crate::gate::{self, Who};
use crate::log::log;
use crate::server::{header, read_body, Reply};
use crate::upload;

/// The source test (verify::test; tests swap in a stand-in).
type Tester = fn(&str, &BTreeMap<String, String>) -> Verdict;
static TESTER: RwLock<Tester> = RwLock::new(verify::test);

pub fn set_tester(t: Tester) {
    if let Ok(mut g) = TESTER.write() {
        *g = t;
    }
}

fn run_test(key: &str, fields: &BTreeMap<String, String>) -> Verdict {
    let t = TESTER.read().map(|g| *g).unwrap_or(verify::test);
    t(key, fields)
}

fn json_body(req: &mut Request, limit: usize) -> Result<Value> {
    let body = read_body(req, limit)?;
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).context("the body is not JSON")
}

fn bad(msg: impl Into<String>) -> Reply {
    Reply::json_status(400, &json!({"ok": false, "error": msg.into()}))
}

/// The routes here; None = not one of them (server.rs carries on).
pub fn route(req: &mut Request, method: &str, path: &str, who: &Who) -> Option<Result<Reply>> {
    if let Some(r) = crate::mcp::route(req, method, path, who) {
        return Some(r);
    }
    if let Some(r) = crate::backups::route(req, method, path) {
        return Some(r);
    }
    if let Some(r) = crate::categories::route(req, method, path) {
        return Some(r);
    }
    if let Some(r) = crate::investments::route(req, method, path) {
        return Some(r);
    }
    if let Some(r) = crate::accounts::route(req, method, path, who) {
        return Some(r);
    }
    let r = match (method, path) {
        ("GET", "/api/whoami") => Ok(whoami(who)),
        ("GET", "/login") => Ok(Reply::html(200, LOGIN_PAGE)),
        ("POST", "/api/pair") => pair(req, false),
        ("POST", "/api/login") => pair(req, true),
        ("POST", "/api/logout") => logout(req, who),
        ("POST", "/api/admin/pair-code") => admin_code(who),
        // a code for another device of the caller's own account
        ("POST", "/api/me/pair-code") => admin_code(who),
        // the danger zone: a member's own account; an admin's Delete all
        ("POST", "/api/me/delete") => delete_me(req, who),
        ("POST", "/api/admin/wipe") => wipe(req, who),
        ("GET", "/api/admin/users") => Ok(Reply::json(&json!({"users": users_json()}))),
        ("POST", "/api/admin/auth-mode") => auth_mode(req),
        ("GET", "/api/admin/devices") => Ok(Reply::json(&json!({"devices": auth::devices().unwrap_or_default(),
            "signed_out": auth::signed_out().unwrap_or_default()}))),
        ("POST", "/api/secrets/import-env") => import_env(),
        ("GET", "/api/sources/catalog") => Ok(Reply::json(&serde_json::to_value(catalog::catalog()).unwrap_or_default())),
        ("GET", "/api/profile") => db::open().map(|c| Reply::json(&profile::bundle_profile(&c))),
        ("PUT", "/api/profile") => put_profile(req),
        ("POST", "/api/preview") => preview_upload(req),
        _ => {
            if let Some(id) = path.strip_prefix("/api/admin/devices/").filter(|_| method == "DELETE") {
                return Some(revoke(id, who));
            }
            if let Some(id) = path.strip_prefix("/api/admin/users/") {
                return match method {
                    "DELETE" => Some(remove_user(id, who)),
                    "PATCH" => Some(rename_user(req, id)),
                    _ => None,
                };
            }
            if let Some(key) = path.strip_prefix("/api/settings/").filter(|_| method == "PUT") {
                return Some(put_setting(req, key));
            }
            let rest = path.strip_prefix("/api/sources/")?;
            let (key, what) = rest.split_once('/')?;
            match (method, what) {
                ("PUT", "secret") => put_secret(req, key),
                ("DELETE", "secret") => delete_secret(key),
                ("POST", "test") => test_source(req, key),
                _ => return None,
            }
        }
    };
    Some(r)
}

fn whoami(who: &Who) -> Reply {
    // what the gate applies (a household server is always `all`)
    let mode = auth::effective_mode();
    let via = match who {
        Who::Nobody => Value::Null,
        Who::Bearer(_) => json!("bearer"),
        Who::Cookie(_) => json!("cookie"),
    };
    Reply::json(&json!({
        "paired": who.device().is_some(),
        "via": via,
        "device": who.device().map(|d| json!({"id": d.id, "name": d.name, "created": d.created, "last_seen": d.last_seen})),
        "user": who.device().map(|d| json!({"id": d.user_id, "name": d.user_name, "role": d.role, "data": d.has_data})),
        "auth_mode": mode.as_str(),
        "auth_mode_from_env": auth::mode_from_env(),
        "household": auth::household(),
        // how a new account is made here: first (no users yet), open, invite
        "signup": auth::signup_mode(),
    }))
}

fn pair(req: &mut Request, browser: bool) -> Result<Reply> {
    let who_is = crate::accounts::client(req);
    if let Some(wait) = auth::throttled(&who_is) {
        return Ok(crate::accounts::throttled_reply(wait));
    }
    let body = json_body(req, 4096)?;
    let code = body.get("code").and_then(Value::as_str).unwrap_or("");
    // the device's resume key, when it signed out with a code to jump back in
    let resume = body.get("resume_key").and_then(Value::as_str);
    let name = match body.get("device_name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) {
        Some(n) => n.to_string(),
        None if browser => crate::accounts::browser_name(req),
        None => "device".into(),
    };
    match auth::pair_with(code, &name, resume) {
        Ok((token, dev)) => {
            log(&format!("[auth] paired device {} ({}) for user {}", dev.id, dev.name, dev.user_id));
            let device = json!({"id": dev.id, "name": dev.name});
            if browser {
                let mut r = Reply::json(&json!({"ok": true, "device": device}));
                r.headers.push(("Set-Cookie", gate::set_cookie(&token)));
                Ok(r)
            } else {
                Ok(Reply::json(&json!({"ok": true, "token": token, "device": device})))
            }
        }
        Err(e) => {
            auth::note_failure(&who_is);
            log("[auth] a pairing attempt failed");
            let reason = e.downcast_ref::<auth::CodeError>().map_or("error", |c| c.reason());
            Ok(Reply::json_status(400, &json!({"ok": false, "error": e.to_string(), "reason": reason})))
        }
    }
}

/// Sign out this device. With the resume key it keeps (`resume_key`), the
/// reply carries `reconnect_code`: the code to jump back in on that device
/// (auth::sign_out). Without one (*Sign out for good*, an older client) it
/// just ends.
fn logout(req: &mut Request, who: &Who) -> Result<Reply> {
    // an unreadable body is no resume key: signing out still works
    let body = json_body(req, 4096).unwrap_or(Value::Null);
    let mut out = json!({"ok": true});
    if let Some(d) = who.device() {
        match auth::sign_out(d, body.get("resume_key").and_then(Value::as_str))? {
            Some(code) => {
                out["reconnect_code"] = json!(code);
                log(&format!("[auth] device {} signed out with a code to jump back in", d.id));
            }
            None => log(&format!("[auth] device {} signed out", d.id)),
        }
    }
    let mut r = Reply::json(&out);
    r.headers.push(("Set-Cookie", gate::clear_cookie()));
    Ok(r)
}

fn admin_code(who: &Who) -> Result<Reply> {
    let uid = who.device().map(|d| d.user_id).unwrap_or(secrets::active_user());
    let (code, expires) = auth::new_pair_code(uid)?;
    Ok(Reply::json(&json!({"code": code, "expires": expires})))
}

fn auth_mode(req: &mut Request) -> Result<Reply> {
    let body = json_body(req, 1024)?;
    let Some(m) = body.get("mode").and_then(Value::as_str).and_then(Mode::parse) else {
        return Ok(bad("mode must be off, writes or all"));
    };
    if auth::mode_from_env() {
        return Ok(bad("FINANCE_AUTH is set in the server's environment and wins: change it there"));
    }
    auth::set_mode(m)?;
    log(&format!("[auth] mode set to {}", m.as_str()));
    Ok(Reply::json(&json!({"ok": true, "auth_mode": m.as_str()})))
}

fn revoke(id: &str, who: &Who) -> Result<Reply> {
    let Ok(id) = id.parse::<i64>() else { return Ok(bad("not a device id")) };
    if who.device().is_some_and(|d| d.id == id) {
        return Ok(bad("this is the device you are using"));
    }
    Ok(Reply::json(&json!({"ok": auth::revoke(id)?})))
}

/// The accounts for Settings, Admin, Users: names, roles, devices; never
/// a path or anything from a user's tree (an admin manages accounts, and
/// reads no one else's data through the API).
fn users_json() -> Vec<Value> {
    auth::users()
        .unwrap_or_default()
        .into_iter()
        .map(|u| {
            json!({"id": u.id, "name": u.name, "role": u.role, "email": u.email, "created": u.created,
                "devices": u.devices, "data": !u.home.is_empty(), "owner": u.id == finance_engine::tenant::OWNER_UID})
        })
        .collect()
}

/// DELETE /api/admin/users/{id}: the account and its devices go; the
/// tree stays on disk (purging is `anser-server user remove --purge`, on the
/// server's shell).
fn remove_user(id: &str, who: &Who) -> Result<Reply> {
    let Ok(id) = id.parse::<i64>() else { return Ok(bad("not a user id")) };
    if who.device().is_some_and(|d| d.user_id == id) {
        return Ok(bad("this is your own account"));
    }
    match auth::remove_user(id, false) {
        Ok(r) => {
            log(&format!("[users] user {id} removed ({} device(s)); the tree is kept", r.devices));
            Ok(Reply::json(&json!({"ok": true, "devices": r.devices, "kept": true})))
        }
        Err(e) => Ok(bad(e.to_string())),
    }
}

/// POST /api/me/delete {"purge": bool}: the caller's own account and
/// devices go; with purge, its data and keys too. The owner (user 1) can't:
/// the server is theirs, and Delete all is how it goes.
fn delete_me(req: &mut Request, who: &Who) -> Result<Reply> {
    let Some(uid) = who.device().map(|d| d.user_id) else { return Ok(bad("sign in first")) };
    if uid == finance_engine::tenant::OWNER_UID {
        return Ok(bad("you own this server: your account goes only with Delete all (Settings, Admin)"));
    }
    let purge = json_body(req, 1024)?.get("purge").and_then(Value::as_bool).unwrap_or(false);
    match auth::remove_user(uid, purge) {
        Ok(r) => {
            log(&format!("[users] user {uid} deleted their account ({} device(s)); data {}", r.devices, if r.purged { "deleted" } else { "kept" }));
            let mut reply = Reply::json(&json!({"ok": true, "purged": r.purged}));
            reply.headers.push(("Set-Cookie", gate::clear_cookie()));
            Ok(reply)
        }
        Err(e) => Ok(bad(e.to_string())),
    }
}

/// POST /api/admin/wipe {"confirm": "DELETE ALL", "keep_backups", "keep_keys"}:
/// Delete all on the server (wipe.rs). The reply goes out first; then the
/// process exits and the service manager starts a fresh, empty server.
fn wipe(req: &mut Request, who: &Who) -> Result<Reply> {
    let body = json_body(req, 1024)?;
    if body.get("confirm").and_then(Value::as_str) != Some("DELETE ALL") {
        return Ok(bad("type DELETE ALL to confirm"));
    }
    let keep = crate::wipe::Keep {
        backups: body.get("keep_backups").and_then(Value::as_bool).unwrap_or(true),
        keys: body.get("keep_keys").and_then(Value::as_bool).unwrap_or(false),
    };
    let by = who.device().map(|d| d.user_id).unwrap_or(0);
    let gone = crate::wipe::run_here(keep)?;
    log(&format!("[wipe] Delete all by user {by}: {} path(s) removed (backups {}, keys {}); restarting empty",
        gone.len(), if keep.backups { "kept" } else { "deleted" }, if keep.keys { "kept" } else { "deleted" }));
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(800));
        std::process::exit(0);
    });
    Ok(Reply::json(&json!({"ok": true, "removed": gone.len(), "restarting": true})))
}

fn rename_user(req: &mut Request, id: &str) -> Result<Reply> {
    let Ok(id) = id.parse::<i64>() else { return Ok(bad("not a user id")) };
    let body = json_body(req, 4096)?;
    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
    match auth::rename_user(id, name) {
        Ok(true) => Ok(Reply::json(&json!({"ok": true}))),
        Ok(false) => Ok(Reply::json_status(404, &json!({"ok": false, "error": "no such user"}))),
        Err(e) => Ok(bad(e.to_string())),
    }
}

fn import_env() -> Result<Reply> {
    let keys = match secrets::import_env(secrets::active_user()) {
        Ok(k) => k,
        Err(e) => return Ok(bad(e.to_string())),
    };
    log(&format!("[secrets] imported {} key(s) from .env", keys.len()));
    Ok(Reply::json(&json!({"ok": true, "imported": keys, "backup": if keys.is_empty() { Value::Null } else { json!(".env.bak") }})))
}

/// The candidate fields of a PUT or a test: {"fields": {KEY: value}}, only
/// this source's keys.
fn fields_of(body: &Value, entry: &catalog::ApiEntry) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    if let Some(obj) = body.get("fields").and_then(Value::as_object) {
        for (k, v) in obj {
            if !entry.fields.iter().any(|f| f.key == k) {
                bail!("{k} is not a field of {}", entry.label);
            }
            if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                if s.len() > 512 {
                    bail!("{k} is too long");
                }
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    Ok(out)
}

/// The candidates over what is configured now (an edit that changes only
/// the secret keeps the stored key).
fn merged(entry: &catalog::ApiEntry, given: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut all = BTreeMap::new();
    for f in &entry.fields {
        if let Some(v) = given.get(f.key).cloned().or_else(|| secrets::lookup(f.key).map(|(v, _)| v)) {
            all.insert(f.key.to_string(), v);
        }
    }
    all
}

fn verdict_reply(status: u16, saved: bool, v: &Verdict) -> Reply {
    Reply::json_status(status, &json!({"ok": v.ok && (saved || status == 200), "saved": saved, "test": v}))
}

fn put_secret(req: &mut Request, key: &str) -> Result<Reply> {
    let Some(entry) = catalog::api_entry(key) else { return Ok(Reply::text(404, "no such API source")) };
    let body = json_body(req, 16 * 1024)?;
    let given = match fields_of(&body, &entry) {
        Ok(g) => g,
        Err(e) => return Ok(bad(e.to_string())),
    };
    let all = merged(&entry, &given);
    if let Some(f) = entry.fields.iter().find(|f| !all.contains_key(f.key)) {
        return Ok(bad(format!("{} is missing", f.label)));
    }
    let v = run_test(key, &all);
    if !v.ok {
        log(&format!("[secrets] {key}: refused, the test failed"));
        return Ok(verdict_reply(422, false, &v));
    }
    secrets::put(secrets::active_user(), &all)?;
    log(&format!("[secrets] {key}: saved ({} field(s))", all.len()));
    Ok(verdict_reply(200, true, &v))
}

fn delete_secret(key: &str) -> Result<Reply> {
    let Some(entry) = catalog::api_entry(key) else { return Ok(Reply::text(404, "no such API source")) };
    let keys: Vec<&str> = entry.fields.iter().map(|f| f.key).collect();
    secrets::remove(secrets::active_user(), &keys)?;
    log(&format!("[secrets] {key}: removed from the store"));
    // what is left: a key may still come from .env or the environment
    let still: Vec<&str> = keys.iter().copied().filter(|k| secrets::lookup(k).is_some()).collect();
    Ok(Reply::json(&json!({"ok": true, "still_configured_elsewhere": still})))
}

fn test_source(req: &mut Request, key: &str) -> Result<Reply> {
    let Some(entry) = catalog::api_entry(key) else { return Ok(Reply::text(404, "no such API source")) };
    let body = json_body(req, 16 * 1024)?;
    let given = match fields_of(&body, &entry) {
        Ok(g) => g,
        Err(e) => return Ok(bad(e.to_string())),
    };
    let v = run_test(key, &merged(&entry, &given));
    Ok(Reply::json(&json!({"ok": v.ok, "test": v})))
}

fn put_profile(req: &mut Request) -> Result<Reply> {
    let body = json_body(req, 16 * 1024)?;
    let p: profile::Profile = match serde_json::from_value(body) {
        Ok(p) => p,
        Err(e) => return Ok(bad(format!("not a profile: {e}"))),
    };
    let conn = db::open()?;
    match profile::put(&conn, &p) {
        Ok(()) => Ok(Reply::json(&json!({"ok": true, "profile": profile::bundle_profile(&conn)}))),
        Err(e) => Ok(bad(e.to_string())),
    }
}

fn put_setting(req: &mut Request, key: &str) -> Result<Reply> {
    if !profile::SETTING_KEYS.contains(&key) {
        return Ok(Reply::text(404, "no such setting"));
    }
    let body = json_body(req, 1 << 20)?;
    let conn = db::open()?;
    match profile::put_setting(&conn, key, &body) {
        Ok(()) => Ok(Reply::json(&json!({"ok": true}))),
        Err(e) => Ok(bad(e.to_string())),
    }
}

/// POST /api/preview: the upload page's dry run, on the server. Multipart
/// like /upload; nothing is written but a scratch copy under data/, removed
/// before answering.
fn preview_upload(req: &mut Request) -> Result<Reply> {
    let ctype = header(req, "Content-Type").unwrap_or_default();
    if !ctype.starts_with("multipart/form-data") {
        return Ok(bad("not a file upload"));
    }
    let body = read_body(req, upload::MAX_BYTES)?;
    let parts = upload::parse_multipart(&ctype, &body)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let scratch = config::data_dir().join(format!(".preview-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let result = (|| -> Result<Vec<Value>> {
        let store = preview::replica_copy(&config::db_path())?;
        let mut out = Vec::new();
        for p in &parts {
            let name = finance_core::upload::upload_name(&p.filename);
            let path = scratch.join(&name);
            std::fs::write(&path, &p.data)?;
            let v = match preview::parse_file(&path) {
                Ok(Ok(f)) => match preview::dedupe(&store, &f, &name) {
                    Ok(load) => json!({"name": name, "ok": true, "source": f.source.key, "label": f.source.label,
                        "rows": load.rows, "new": load.added, "near_dupes": load.near_dupes, "legacy_twins": load.legacy_twins,
                        "range": f.range.map(|(a, b)| json!([a, b]))}),
                    Err(e) => json!({"name": name, "ok": false, "error": format!("{e:#}")}),
                },
                Ok(Err(u)) => json!({"name": name, "ok": false, "error": u.to_string()}),
                Err(e) => json!({"name": name, "ok": false, "error": format!("{e:#}")}),
            };
            out.push(v);
        }
        Ok(out)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(Reply::json(&json!({"files": result?})))
}

/// The plain browser login, a fallback only: the web app signs in on its
/// own first screen (the onboarding wizard's sign-in step) and never comes
/// here. Kept for a browser the web app can't run in (no WebGPU or WebGL2).
/// Plain HTML; the one script posts the code with the X-Finance-Request
/// header the server wants, and the resume key the web app keeps in this
/// origin's localStorage (`finance-resume-key`), if any.
const LOGIN_PAGE: &str = r#"<!doctype html><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1">
<title>Anser: pair this browser</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:28em;margin:3em auto;padding:0 1em;color:#1f2328;background:#fff}
@media(prefers-color-scheme:dark){body{color:#e6e6e6;background:#16171a}input{background:#222;color:#eee;border-color:#444}}
input{font:inherit;font-size:1.4em;letter-spacing:.15em;text-transform:uppercase;width:10em;padding:.3em .5em;border:1px solid #bbb;border-radius:6px}
button{font:inherit;padding:.5em 1.2em;margin-left:.5em;border-radius:6px;border:0;background:#3b6fd8;color:#fff;cursor:pointer}
#msg{min-height:1.5em;margin-top:1em}</style>
<h1>Pair this browser</h1>
<p>Enter a pairing code. Make one on the server with <code>anser-server pair</code>, or from a paired device: Settings, then Device, then <b>Pair another device</b>. If this browser signed out with a code to jump back in, that code works here too, once.</p>
<form id=f><input id=c autocomplete=one-time-code maxlength=9 autofocus required><button>Pair</button></form>
<div id=msg></div>
<script>
function rk(){try{return localStorage.getItem('finance-resume-key')||undefined}catch(x){}}
document.getElementById('f').onsubmit=async e=>{e.preventDefault();const m=document.getElementById('msg');m.textContent='Pairing…';
try{const r=await fetch('/api/login',{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json','X-Finance-Request':'1'},
body:JSON.stringify({code:document.getElementById('c').value,resume_key:rk()})});const j=await r.json().catch(()=>({}));
if(r.ok){m.textContent='Paired. Opening…';location.replace('/');}else{m.textContent=j.error||('HTTP '+r.status);}}catch(x){m.textContent=String(x);}};
</script>"#;
