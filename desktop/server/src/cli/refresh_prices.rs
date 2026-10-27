//! `anser-server refresh-prices [--range 5y]`: refetch every cached Yahoo
//! symbol with full daily OHLC.

use anyhow::Result;

use finance_engine::fetch;

use super::args::Args;

pub fn run(rest: &[String]) -> Result<()> {
    let a = Args::parse(rest, &["--range"], &[])?;
    let conn = finance_engine::db::open()?;
    for line in fetch::prices::refresh_prices(&conn, &a.value("--range").unwrap_or_else(|| "5y".into()))? {
        println!("{line}");
    }
    Ok(())
}
