//! `anser-server stats`: raw row counts per table, as JSON with a one-space
//! indent (one key per line).

use finance_engine::db;
use serde::Serialize;

pub fn run(args: &[String]) -> anyhow::Result<()> {
    super::args::no_args("stats", args);
    let conn = db::open()?;
    let v = db::stats(&conn)?;
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, serde_json::ser::PrettyFormatter::with_indent(b" "));
    v.serialize(&mut ser)?;
    println!("{}", String::from_utf8(buf)?);
    Ok(())
}
