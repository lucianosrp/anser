//! `anser-server serve [--bind HOST:PORT] [--push-to URL] [--push-every SECS]`
//! With --push-to, a background thread pushes the
//! local data tree to that remote whenever it is reachable (every 5 minutes
//! by default), so a laptop server catches the remote server up on its own.

use std::time::Duration;

use anyhow::Result;

use finance_server::log::log;
use finance_server::{push, server};

use super::args::Args;

/// Everything this process creates is private to its user: the secrets,
/// server.db, backups, uploads.
pub fn private_umask() {
    #[cfg(unix)]
    unsafe {
        libc::umask(0o077);
    }
}

pub fn run(rest: &[String]) -> Result<()> {
    let a = Args::parse(rest, &["--bind", "--push-to", "--push-every"], &[])?;
    private_umask();
    // users, devices and the owner's profile (a backup of finance.db first)
    let boot = finance_server::auth::bootstrap()?;
    if let Some(b) = &boot.backup {
        log(&format!("[auth] before a migration: finance.db backed up to {}", b.display()));
    }
    if boot.categories_migrated {
        log("[categories] finance.db gained the categories table; every existing rule is a user rule (same order)");
    }
    for uid in &boot.homes_made {
        log(&format!("[users] user {uid} got a data tree (users/{uid}/)"));
    }
    if let Some(m) = boot.default_mode {
        log(&format!("[auth] default auth mode: {}", m.as_str()));
    }
    log(&format!("[auth] mode {} (FINANCE_AUTH overrides){}", finance_server::auth::mode().as_str(),
        if finance_server::auth::household() { "; a household server: reads need a device too (all)" } else { "" }));
    if let Some(remote) = a.value("--push-to") {
        let remote = remote.trim_end_matches('/').to_string();
        let every = a.value("--push-every").and_then(|s| s.parse().ok()).unwrap_or(300u64).max(10);
        std::thread::Builder::new().name("push".into()).spawn(move || loop {
            // a laptop server pushes the owner's tree (its only one)
            match finance_engine::tenant::Tenant::owner().enter(|| push::push_if_reachable(&remote, &log)) {
                Ok(Some(rep)) => log(&format!("[push] {remote}: {}", rep.summary())),
                Ok(None) => {}
                Err(e) => log(&format!("[push] {remote} failed: {e}")),
            }
            std::thread::sleep(Duration::from_secs(every));
        })?;
    }
    server::serve(&server::bind_addr(a.value("--bind").as_deref()))
}
