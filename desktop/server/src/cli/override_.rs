//! `anser-server override ID [CATEGORY] [--share F] [--note TEXT] [--clear]`
//! pin one transaction's category, beating the rules.

use finance_engine::sources::textfmt;
use finance_engine::{categorize, db};

const USAGE: &str = "override [-h] [--share SHARE] [--note NOTE] [--clear] id [category]";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let mut pos: Vec<String> = Vec::new();
    let (mut share, mut note, mut clear) = (None::<f64>, None::<String>, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = |name: &str| -> String {
            inline.clone().or_else(|| it.next().cloned()).unwrap_or_else(|| {
                super::args::usage_error(USAGE, &format!("argument {name}: expected one argument"))
            })
        };
        match flag {
            "--share" => {
                let v = value("--share");
                share = Some(super::args::float(USAGE, "--share", &v));
            }
            "--note" => note = Some(value("--note")),
            "--clear" => clear = true,
            _ if a.starts_with("--") => super::args::usage_error(USAGE, &format!("unrecognized arguments: {a}")),
            _ => pos.push(a.clone()),
        }
    }
    if pos.is_empty() {
        super::args::usage_error(USAGE, "the following arguments are required: id");
    }
    if pos.len() > 2 {
        super::args::usage_error(USAGE, &format!("unrecognized arguments: {}", pos[2..].join(" ")));
    }
    let id = super::args::int(USAGE, "id", &pos[0]);
    let category = pos.get(1).cloned();
    if !clear && category.as_deref().unwrap_or("").is_empty() {
        eprintln!("give a CATEGORY, or --clear to remove the override");
        std::process::exit(1);
    }
    let conn = db::open()?;
    let row = categorize::set_override(
        &conn,
        id,
        if clear { None } else { category.as_deref() },
        share,
        note.as_deref(),
    )?;
    drop(conn);
    let share = row.share.map(|s| format!(" x{}", textfmt::format_g(s))).unwrap_or_default();
    println!(
        "{} {} {} {} {} -> {}{share}",
        row.id,
        textfmt::prefix_chars(&row.txn_date, 10),
        textfmt::format_thousands(row.amount.unwrap_or(0.0), 2),
        row.currency,
        textfmt::prefix_chars(&row.description, 40),
        row.category.as_deref().unwrap_or("None"),
    );
    Ok(())
}
