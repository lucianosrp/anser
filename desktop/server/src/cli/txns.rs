//! `anser-server txns TEXT [--limit N]`: find bank
//! transactions by description, with the ids `override` takes.

use finance_engine::db;
use finance_engine::sources::textfmt;

const USAGE: &str = "txns [-h] [--limit LIMIT] text";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let mut text: Option<String> = None;
    let mut limit: i64 = 20;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--limit=") {
            limit = super::args::int(USAGE, "--limit", v);
        } else if a == "--limit" {
            let v = it.next().unwrap_or_else(|| super::args::usage_error(USAGE, "argument --limit: expected one argument"));
            limit = super::args::int(USAGE, "--limit", v);
        } else if text.is_none() && !(a.starts_with('-') && a.len() > 1) {
            text = Some(a.clone());
        } else {
            super::args::usage_error(USAGE, &format!("unrecognized arguments: {a}"));
        }
    }
    let Some(text) = text else {
        super::args::usage_error(USAGE, "the following arguments are required: text")
    };
    let conn = db::open()?;
    let mut st = conn.prepare(
        "SELECT id, txn_date, source, amount, currency, category, share, description \
         FROM bank_transactions WHERE lower(description) LIKE ? ORDER BY txn_date DESC LIMIT ?",
    )?;
    let rows = st.query_map((format!("%{}%", text.to_lowercase()), limit), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<f64>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<f64>>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    for r in rows {
        let (id, date, source, amount, ccy, cat, share, desc) = r?;
        let share = share.map(|s| format!(" x{}", textfmt::format_g(s))).unwrap_or_default();
        let amount = textfmt::format_thousands(amount.unwrap_or(0.0), 2);
        println!(
            "{id:6} {} {source:11} {amount:>11} {ccy}  {}{share}  {}",
            textfmt::prefix_chars(&date, 10),
            cat.filter(|c| !c.is_empty()).as_deref().unwrap_or("Other"),
            textfmt::prefix_chars(&desc, 50)
        );
    }
    Ok(())
}
