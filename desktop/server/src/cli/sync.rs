//! `anser-server sync`: every fetcher in the foreground (Binance, OKX, IBKR,
//! then the current month's FX), under the same cross-process lock as the
//! server's background sync, so the two never walk the exchanges at once.

use anyhow::Result;

use finance_engine::{config, fetch};
use crate::syncer;

use super::args::Args;

pub fn run(rest: &[String]) -> Result<()> {
    Args::parse(rest, &[], &[])?;
    let ledger = syncer::run_foreground(&|l| println!("{l}"));
    // FX outside the lock: frankfurter is cheap and the current month is
    // refetched on every call anyway
    let conn = finance_engine::db::open()?;
    let today = config::utc_today();
    let rows = fetch::fx::get_monthly_rates(&conn, today, today)?;
    println!("FX current month: {:?}", rows.first().map(|r| &r.rates));
    ledger?;
    println!("Sync complete.");
    Ok(())
}
