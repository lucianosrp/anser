//! The investment setup routes: the accounts money is sent to and the
//! instruments' fees (engine invest_setup), behind Settings → Investments.
//! Reading is a read; changing it is a write on the caller's own tree
//! (gate.rs: not an admin route, though the path is under `/api/admin/`).
//!
//! * `GET /api/investments/setup`: {accounts, instruments, benchmark,
//!   default_benchmark, valued_by, kinds}.
//! * `POST /api/admin/investments/accounts` (an account), `PUT
//!   /api/admin/investments/accounts/{name}` (replace it; a different name
//!   in the body renames it), `DELETE /api/admin/investments/accounts/{name}`.
//! * `PUT /api/admin/investments/instruments/{symbol}` {name?, kind,
//!   ter_pct?} (adds or changes), `DELETE
//!   /api/admin/investments/instruments/{symbol}`.
//!
//! The benchmark is the profile's (`PUT /api/profile` {benchmark}). Every
//! write moves data_rev (a device sees its replica is behind) and re-warms
//! the views (the view cache's fingerprint covers these tables).

use anyhow::Result;
use serde_json::{json, Value};
use tiny_http::Request;

use finance_engine::db;
use finance_engine::invest_setup::{self as setup, Account, Instrument};

use crate::categories::decode;
use crate::log::log;
use crate::server::{read_body, Reply};

fn bad(msg: impl Into<String>) -> Reply {
    Reply::json_status(400, &json!({"ok": false, "error": msg.into()}))
}

fn body(req: &mut Request) -> Result<Value> {
    let b = read_body(req, 64 * 1024)?;
    Ok(if b.is_empty() { Value::Null } else { serde_json::from_slice(&b).unwrap_or(Value::Null) })
}

/// The setup as a client shows it.
pub fn setup_json(conn: &rusqlite::Connection) -> Value {
    let s = setup::Setup::load(conn);
    json!({
        "accounts": s.accounts,
        "instruments": s.instruments,
        "benchmark": setup::benchmark(conn),
        "default_benchmark": setup::DEFAULT_BENCHMARK,
        "valued_by": setup::VALUED_BY,
        "kinds": setup::INSTRUMENT_KINDS,
    })
}

/// Run a write in a transaction, move data_rev, answer with the setup.
fn written(what: &str, f: impl FnOnce(&rusqlite::Connection) -> Result<()>) -> Result<Reply> {
    let conn = db::open()?;
    let r = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        f(&tx)?;
        finance_engine::categories::bump(&tx)?;
        tx.commit()?;
        Ok(())
    })();
    match r {
        Ok(()) => {
            log(&format!("[investments] {what}"));
            crate::server::warm_async();
            let mut v = setup_json(&conn);
            v["ok"] = json!(true);
            v["data_rev"] = json!(finance_engine::categories::data_rev(&conn));
            Ok(Reply::json(&v))
        }
        Err(e) => {
            log(&format!("[investments] {what} refused: {e:#}"));
            Ok(bad(format!("{e:#}")))
        }
    }
}

/// The routes here; None = not one of them.
pub fn route(req: &mut Request, method: &str, path: &str) -> Option<Result<Reply>> {
    if (method, path) == ("GET", "/api/investments/setup") {
        return Some(db::open().map(|c| Reply::json(&setup_json(&c))));
    }
    let rest = path.strip_prefix("/api/admin/investments/")?;
    let b = match body(req) {
        Ok(b) => b,
        Err(e) => return Some(Ok(bad(e.to_string()))),
    };
    let account = || serde_json::from_value::<Account>(b.clone()).map_err(|e| bad(format!("not an account: {e}")));
    let r = match (method, rest.split_once('/')) {
        ("POST", None) if rest == "accounts" => match account() {
            Ok(a) => written("new account", move |c| setup::put_account(c, None, &a).map(|_| ())),
            Err(r) => Ok(r),
        },
        ("PUT", Some(("accounts", name))) => {
            let old = decode(name);
            match account() {
                Ok(a) => written("account", move |c| setup::put_account(c, Some(&old), &a).map(|_| ())),
                Err(r) => Ok(r),
            }
        }
        ("DELETE", Some(("accounts", name))) => {
            let name = decode(name);
            written("delete account", move |c| setup::delete_account(c, &name))
        }
        ("PUT", Some(("instruments", sym))) => {
            let mut i: Instrument = match serde_json::from_value(b.clone()) {
                Ok(i) => i,
                Err(e) => return Some(Ok(bad(format!("not an instrument: {e}")))),
            };
            i.symbol = decode(sym);
            written("instrument", move |c| setup::put_instrument(c, &i).map(|_| ()))
        }
        ("DELETE", Some(("instruments", sym))) => {
            let sym = decode(sym);
            written("delete instrument", move |c| setup::delete_instrument(c, &sym))
        }
        _ => return None,
    };
    Some(r)
}
