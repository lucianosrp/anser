//! `anser-server install-webapp --dir DIR --server URL`: POST a built web app
//! bundle (desktop/scripts/build-web.sh's output) to URL/api/webapp. Goes
//! through finance_core::webapp::install, the Doctor's own path, so a
//! `*.ts.net` server resolves via MagicDNS even when a VPN owns the system
//! DNS (curl would get tailscale's public addresses and time out).

use std::path::PathBuf;

use anyhow::{Context, Result};

use super::args::Args;

pub fn run(rest: &[String]) -> Result<()> {
    let a = Args::parse(rest, &["--dir", "--server"], &[])?;
    let dir = PathBuf::from(a.value("--dir").context("--dir DIR (the bundle) is required")?);
    let server = a.value("--server").context("--server URL is required")?;
    println!("installing {} on {server} ...", dir.display());
    let done = finance_core::webapp::install(&server, &dir)?;
    println!("installed {} files ({} bytes): open {}", done.files, done.bytes, done.url);
    Ok(())
}
