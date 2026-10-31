//! `anser-server fetch-fx [--from YYYY-MM --to YYYY-MM]`: pre-warm the FX
//! cache. Default span 2020-01 .. this month.

use anyhow::Result;
use chrono::NaiveDate;

use finance_engine::{config, fetch};

use super::args::{month, Args};

pub fn run(rest: &[String]) -> Result<()> {
    let a = Args::parse(rest, &["--from", "--to"], &[])?;
    let lo = match a.value("--from") {
        Some(s) => month(&s)?,
        None => NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
    };
    let hi = match a.value("--to") {
        Some(s) => month(&s)?,
        None => config::utc_today(),
    };
    let conn = finance_engine::db::open()?;
    let rows = fetch::fx::get_monthly_rates(&conn, lo, hi)?;
    println!("FX cache warmed: {} month(s) {}..{}.", rows.len(), lo.format("%Y-%m"), hi.format("%Y-%m"));
    Ok(())
}
