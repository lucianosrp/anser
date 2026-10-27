//! `anser-server push [--remote URL]`: send the remote every local
//! transactions file and txn_override it lacks (see finance_server::push).
//! Unreachable remote = one line and exit 0: being offline is normal.

use anyhow::{bail, Result};

use finance_server::push;

use super::args::Args;

pub fn run(rest: &[String]) -> Result<()> {
    let a = Args::parse(rest, &["--remote"], &[])?;
    let Some(remote) = push::default_remote(a.value("--remote").as_deref()) else {
        bail!("no remote: pass --remote URL, set FINANCE_REMOTE in .env, or save a server in the desktop app");
    };
    if let Err(e) = push::probe(&remote) {
        println!("{remote} not reachable ({e}); nothing pushed.");
        return Ok(());
    }
    let rep = push::push(&remote, &|l| println!("{l}"))?;
    println!("Pushed to {remote}: {}", rep.summary());
    Ok(())
}
