//! MCP from the shell:
//!
//! * `anser-server mcp [--user NAME|ID]`: the read-only MCP server on stdio,
//!   for one user's tree (the owner's unless --user names another,
//!   cmd::dispatch). Local shell access already, so it needs neither the
//!   `/mcp` switch nor a key.
//! * `anser-server mcp-key create NAME | list | revoke ID [--user NAME|ID]`:
//!   the MCP keys of one user: tokens only `/mcp` accepts. `create` prints
//!   the key once.
//! * `anser-server mcp-enable` / `mcp-disable [--user NAME|ID]`: the switch
//!   for `/mcp` over HTTP (off by default).

use anyhow::{bail, Result};

use finance_engine::{db, profile, secrets};
use finance_server::auth;
use finance_server::mcp::ServerSource;

pub fn run(args: &[String]) -> Result<()> {
    super::args::no_args("mcp", args);
    let conn = db::open()?;
    finance_engine::mcp::run_stdio(&ServerSource::stdio(&conn))?;
    Ok(())
}

fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn state() -> Result<()> {
    let on = profile::mcp_enabled(&db::open()?);
    println!("MCP over HTTP (/mcp) is {} for user {}", if on { "on" } else { "off" }, secrets::active_user());
    Ok(())
}

pub fn enable(args: &[String]) -> Result<()> {
    super::args::no_args("mcp-enable", args);
    profile::set_mcp_enabled(&db::open()?, true)?;
    state()
}

pub fn disable(args: &[String]) -> Result<()> {
    super::args::no_args("mcp-disable", args);
    profile::set_mcp_enabled(&db::open()?, false)?;
    state()
}

const KEY_USAGE: &str = "usage: anser-server mcp-key create NAME | list | revoke ID  [--user NAME|ID]";

pub fn key(args: &[String]) -> Result<()> {
    auth::bootstrap()?;
    let uid = secrets::active_user();
    match args.first().map(String::as_str) {
        Some("create") => {
            let Some(name) = args.get(1).filter(|n| args.len() == 2 && !n.trim().is_empty()) else { bail!(KEY_USAGE) };
            let (token, key) = auth::add_mcp_key(uid, name)?;
            println!("MCP key {} \"{}\" for user {uid}. It is shown once; keep it like a password:", key.id, key.name);
            println!("{token}");
            if !profile::mcp_enabled(&db::open()?) {
                println!("MCP is still off for this user: run anser-server mcp-enable{}", if uid == finance_engine::tenant::OWNER_UID { String::new() } else { format!(" --user {uid}") });
            }
            Ok(())
        }
        Some("list") | None => {
            state()?;
            for k in auth::mcp_keys(uid)? {
                println!("  key {} {:<28} made {}  last used {}", k.id, k.name, when(k.created), k.last_used.map(when).unwrap_or_else(|| "never".into()));
            }
            Ok(())
        }
        Some("revoke") => {
            let Some(id) = args.get(1).filter(|_| args.len() == 2).and_then(|s| s.parse::<i64>().ok()) else { bail!(KEY_USAGE) };
            println!("{}", if auth::revoke_mcp_key(uid, id)? { "revoked" } else { "no such key for this user" });
            Ok(())
        }
        Some(other) => bail!("{KEY_USAGE}  (unknown: {other})"),
    }
}
