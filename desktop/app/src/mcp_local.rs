//! `anser mcp [--account NAME]`: the read-only MCP server (the
//! engine's, the same tools as the server's /mcp) on stdio, over this
//! device's local copy of one account. No window, no network, no key: an
//! assistant that starts it as a child process reads the copy as of the
//! last sync.
//!
//! The account is the active one unless `--account` names another (its
//! label or id, any case). The copy opens read-only; an encrypted one is
//! unlocked as the app does at startup (the keyring's key) or, for a
//! passphrase copy, from `$FINANCE_DESKTOP_PASSPHRASE`. stdout is the
//! protocol, so every message to the human goes to stderr, and a copy that
//! can't be opened ends the process with a non-zero status before anything
//! is read from stdin.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use finance_core::{cache, profiles, replica, Ledger};
use finance_engine::mcp::{self, Source};
use finance_engine::views::{MarketFeed, Offline};
use rusqlite::Connection;

/// The engine's tools over the copy: the offline feed, the ledger read once.
struct ReplicaSource {
    conn: Connection,
    ledger: Arc<Ledger>,
}

impl Source for ReplicaSource {
    fn conn(&self) -> &Connection {
        &self.conn
    }
    fn feed(&self) -> &dyn MarketFeed {
        &Offline
    }
    fn ledger(&self) -> Result<Arc<Ledger>> {
        Ok(self.ledger.clone())
    }
}

/// `anser mcp` (args after the subcommand). Exit code on error is
/// the caller's (main.rs).
pub fn run(args: &[String]) -> Result<()> {
    let account = match args {
        [] => None,
        [flag, name] if flag == "--account" => Some(name.as_str()),
        _ => bail!("usage: anser mcp [--account NAME]"),
    };
    // never make the profiles here: no file, no app ever ran
    if !profiles::file().exists() {
        bail!("no local copy: open anser, sign in and sync first");
    }
    let all = profiles::list();
    let profile = match account {
        None => profiles::active(),
        Some(name) => all
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(name) || (!p.label.is_empty() && p.label.eq_ignore_ascii_case(name)))
            .cloned()
            .with_context(|| format!("no account \"{name}\" (have: {})", names(&all)))?,
    };
    let dir = profiles::dir_of(&profile.id);
    let db = dir.join(cache::DB_FILE);
    if !replica::stored_path(&db).exists() {
        bail!("account {} has no local copy yet: open anser and sync first", names(std::slice::from_ref(&profile)));
    }
    replica::unlock_dir(&dir)?;
    let conn = replica::open_db(&db)?;
    let ledger = Arc::new(Ledger::load(&db)?);
    eprintln!("anser mcp: serving the local copy of {} (read-only)", names(std::slice::from_ref(&profile)));
    mcp::run_stdio(&ReplicaSource { conn, ledger })?;
    Ok(())
}

/// "label (id)" for each, comma-separated.
fn names(ps: &[profiles::Profile]) -> String {
    ps.iter()
        .map(|p| if p.label.is_empty() { p.id.clone() } else { format!("{} ({})", p.label, p.id) })
        .collect::<Vec<_>>()
        .join(", ")
}
