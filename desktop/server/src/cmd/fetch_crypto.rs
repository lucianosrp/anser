//! `anser-server fetch-crypto [--exchange binance|okx|ibkr|all] [--full]`
//! --full re-walks Binance from 2020.
//! Takes the sync lock like `sync`, so it can't overlap the server's sync.

use anyhow::{bail, Result};

use finance_engine::fetch;
use finance_server::syncer;

use super::args::Args;

pub fn run(rest: &[String]) -> Result<()> {
    let a = Args::parse(rest, &["--exchange"], &["--full"])?;
    let want = a.value("--exchange").unwrap_or_else(|| "all".into());
    if want != "all" && !fetch::API_SOURCES.contains(&want.as_str()) {
        bail!("--exchange must be one of all, binance, okx, ibkr");
    }
    let _lock = syncer::try_lock()?;
    let conn = finance_engine::db::open()?;
    for key in fetch::API_SOURCES.iter().filter(|k| want == "all" || want == **k) {
        println!("Fetching {key} …");
        fetch::fetch_source(&conn, key, a.has("--full"), &|l| println!("{l}"))?;
    }
    println!("Crypto refresh complete.");
    Ok(())
}
