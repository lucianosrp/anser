//! The category and rule routes (Phase 3). gate.rs classifies them: the
//! GETs are reads, everything else a write (a device token, under
//! FINANCE_AUTH=writes or all).
//!
//! * `GET /api/categories`, `POST /api/categories` {name, icon?, kind?,
//!   fixed?, reimbursable?, hidden?}, `PATCH /api/categories` {order:
//!   [names]}, `PATCH /api/categories/{name}` {any of those, name: a
//!   rename}, `POST /api/categories/{name}/merge` {into}, `DELETE
//!   /api/categories/{name}?into=` (a body {into} works too; into Other =
//!   back to uncategorized). Renames, merges and deletes rewrite the rules,
//!   the overrides and the budgets in the same transaction.
//! * `GET /api/rules`, `POST /api/rules` {pattern, direction?, category,
//!   share?, priority?, note?}, `PATCH /api/rules/{id}`, `DELETE
//!   /api/rules/{id}`, `POST /api/rules/preview` (a dry run), `POST
//!   /api/rules/test` {description, amount}, `POST /api/rules/starter` (the
//!   starter pack).
//! * `PUT /api/txn/{source}/{raw_hash}/category` {category (null: back to
//!   the rules), share?, note?}.
//! * `GET /api/categorize/suggest` (suggest.rs).
//!
//! Every write recategorizes in its transaction and replies with the rows
//! that moved, the new data_rev, and the categories and rules as they are
//! now (engine rules::reply), then re-warms the views.

use anyhow::Result;
use serde_json::{json, Value};
use tiny_http::Request;

use finance_engine::categories::{self, Patch};
use finance_engine::rules::{self, Input};
use finance_engine::db;

use crate::log::log;
use crate::server::{read_body, Reply};

fn json_body(req: &mut Request) -> Result<Value> {
    let body = read_body(req, 256 * 1024)?;
    if body.is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn bad(msg: impl Into<String>) -> Reply {
    Reply::json_status(400, &json!({"ok": false, "error": msg.into()}))
}

/// One %-encoded path segment, decoded ("Utilities%20%2F%20Telecom").
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A query parameter of the request's URL.
fn query(req: &Request, key: &str) -> Option<String> {
    let q = req.url().split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (decode(k) == key).then(|| decode(v))
    })
}

/// Run a write and answer with its reply (a refusal is a 400 with the reason).
fn written<T: serde::Serialize>(what: &str, f: impl FnOnce(&rusqlite::Connection) -> Result<T>) -> Result<Reply> {
    let conn = db::open()?;
    match rules::write(&conn, f) {
        Ok(w) => {
            log(&format!("[categories] {what}: {} row(s) moved, data_rev {}", w.changed.len(), w.data_rev));
            let reply = rules::reply(&conn, &w);
            crate::server::warm_async();
            Ok(Reply::json(&reply))
        }
        Err(e) => {
            log(&format!("[categories] {what} refused: {e:#}"));
            Ok(bad(format!("{e:#}")))
        }
    }
}

fn input(body: &Value) -> Result<Input, Reply> {
    serde_json::from_value(body.clone()).map_err(|e| bad(format!("not a rule: {e}")))
}

fn patch(body: &Value) -> Result<Patch, Reply> {
    serde_json::from_value(body.clone()).map_err(|e| bad(format!("not a category: {e}")))
}

/// The routes here; None = not one of them.
pub fn route(req: &mut Request, method: &str, path: &str) -> Option<Result<Reply>> {
    let r = match (method, path) {
        ("GET", "/api/categories") => list_categories(),
        ("POST", "/api/categories") => {
            let body = match json_body(req) {
                Ok(b) => b,
                Err(e) => return Some(Ok(bad(e.to_string()))),
            };
            match patch(&body) {
                Ok(p) => written("new category", move |c| categories::create(c, &p)),
                Err(r) => Ok(r),
            }
        }
        ("PATCH", "/api/categories") => {
            let body = json_body(req).unwrap_or(Value::Null);
            let order: Vec<String> = body.get("order").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
            if order.is_empty() {
                Ok(bad("send {\"order\": [names]}"))
            } else {
                written("reorder", move |c| categories::reorder(c, &order))
            }
        }
        ("GET", "/api/rules") => list_rules(),
        ("POST", "/api/rules") => {
            let body = json_body(req).unwrap_or(Value::Null);
            match input(&body) {
                Ok(i) => written("new rule", move |c| rules::create(c, &i)),
                Err(r) => Ok(r),
            }
        }
        ("POST", "/api/rules/preview") => {
            let body = json_body(req).unwrap_or(Value::Null);
            match input(&body) {
                Ok(i) => db::open().map(|c| {
                    let _ = categories::ensure(&c);
                    match rules::preview(&c, &i) {
                        Ok(v) => Reply::json(&v),
                        Err(e) => bad(format!("{e:#}")),
                    }
                }),
                Err(r) => Ok(r),
            }
        }
        ("POST", "/api/rules/test") => {
            let body = json_body(req).unwrap_or(Value::Null);
            let desc = body.get("description").and_then(Value::as_str).unwrap_or("").to_string();
            let amount = body.get("amount").and_then(|a| a.as_f64().or_else(|| a.as_str().and_then(|s| s.trim().parse().ok()))).unwrap_or(-1.0);
            db::open().map(|c| match rules::test(&c, &desc, amount) {
                Ok(v) => Reply::json(&v),
                Err(e) => bad(format!("{e:#}")),
            })
        }
        ("POST", "/api/rules/starter") => written("starter pack", rules::install_starter),
        ("GET", "/api/categorize/suggest") => db::open().and_then(|c| {
            let _ = categories::ensure(&c);
            crate::suggest::suggest(&c).map(|v| Reply::json(&v))
        }),
        _ => return dynamic(req, method, path),
    };
    Some(r)
}

/// The routes with a name or an id in the path.
fn dynamic(req: &mut Request, method: &str, path: &str) -> Option<Result<Reply>> {
    if let Some(rest) = path.strip_prefix("/api/categories/") {
        let (name, tail) = match rest.split_once('/') {
            Some((n, t)) => (decode(n), Some(t)),
            None => (decode(rest), None),
        };
        let body = json_body(req).unwrap_or(Value::Null);
        let r = match (method, tail) {
            ("PATCH", None) => match patch(&body) {
                Ok(p) => written("category", move |c| categories::update(c, &name, &p)),
                Err(r) => Ok(r),
            },
            ("POST", Some("merge")) => {
                let into = body.get("into").and_then(Value::as_str).unwrap_or("").to_string();
                written("merge", move |c| categories::merge(c, &name, &into))
            }
            ("DELETE", None) => {
                let into = query(req, "into")
                    .or_else(|| body.get("into").and_then(Value::as_str).map(String::from))
                    .unwrap_or_else(|| categories::OTHER.to_string());
                written("delete category", move |c| categories::delete(c, &name, &into))
            }
            _ => return None,
        };
        return Some(r);
    }
    if let Some(id) = path.strip_prefix("/api/rules/") {
        let Ok(id) = id.parse::<i64>() else { return None };
        let r = match method {
            "PATCH" => {
                let body = json_body(req).unwrap_or(Value::Null);
                match input(&body) {
                    Ok(i) => written("rule", move |c| rules::update(c, id, &i)),
                    Err(r) => Ok(r),
                }
            }
            "DELETE" => written("delete rule", move |c| rules::delete(c, id)),
            _ => return None,
        };
        return Some(r);
    }
    if let Some(rest) = path.strip_prefix("/api/txn/") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() != 3 || parts[2] != "category" || method != "PUT" {
            return None;
        }
        let (source, raw_hash) = (decode(parts[0]), decode(parts[1]));
        let body = json_body(req).unwrap_or(Value::Null);
        let category = body.get("category").and_then(Value::as_str).map(String::from);
        let share = body.get("share").and_then(Value::as_f64);
        let note = body.get("note").and_then(Value::as_str).map(String::from);
        return Some(written("row category", move |c| {
            rules::set_override(c, &source, &raw_hash, category.as_deref(), share, note.as_deref())
        }));
    }
    None
}

fn list_categories() -> Result<Reply> {
    let conn = db::open()?;
    Ok(Reply::json(&json!({
        "categories": categories::list(&conn)?,
        "kinds": categories::KINDS,
        "data_rev": categories::data_rev(&conn),
    })))
}

fn list_rules() -> Result<Reply> {
    let conn = db::open()?;
    Ok(Reply::json(&json!({
        "rules": rules::list(&conn)?,
        "enabled": rules::enabled_count(&conn),
        "data_rev": categories::data_rev(&conn),
    })))
}

#[cfg(test)]
mod tests {
    use super::decode;

    /// The server seeds each category's flags from its old lists; the client
    /// falls back to its own copy of them for a category the table lacks.
    /// Both must say the same of every default category.
    #[test]
    fn the_seed_flags_match_the_clients_fallback() {
        use finance_engine::categories::default_flags;
        let mut names = finance_engine::categories::seed_names();
        names.extend(["Other", "Insurance", "Taxes", "Cash deposit"].map(String::from));
        let transfer_rule = |n: &str| finance_engine::categorize::DEFAULT_RULES.iter().any(|r| r.2 == n && r.3 == 1);
        for n in &names {
            let c = default_flags(n, transfer_rule(n));
            assert_eq!(c.moves_money(), finance_core::categories::not_spending(n), "spending {n}");
            assert_eq!(c.fixed == "always", finance_core::categories::fixed_always(n), "fixed {n}");
            assert_eq!(c.reimbursable, finance_core::categories::reimbursable(n), "reimbursable {n}");
            assert_eq!(c.kind == "investment", finance_core::categories::investment(n), "investment {n}");
        }
    }

    #[test]
    fn decodes_path_segments() {
        assert_eq!(decode("Utilities%20%2F%20Telecom"), "Utilities / Telecom");
        assert_eq!(decode("Caf%C3%A9"), "Café");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("a%zzb"), "a%zzb");
    }
}
