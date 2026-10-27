//! `anser-server categorize [--reset --yes-replace-rules]`: (re)apply spending category rules.

use finance_engine::{categorize, db};

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let (mut reset, mut confirmed) = (false, false);
    for a in args {
        match a.as_str() {
            "--reset" => reset = true,
            "--yes-replace-rules" => confirmed = true,
            _ => super::args::usage_error("categorize [-h] [--reset [--yes-replace-rules]]", &format!("unrecognized arguments: {a}")),
        }
    }
    if reset && !confirmed {
        anyhow::bail!(
            "--reset deletes every category rule (yours included) and replaces them with the generic defaults: add --yes-replace-rules to confirm"
        );
    }
    let conn = db::open()?;
    if reset {
        conn.execute("DELETE FROM category_rules", [])?;
        // ensure_rules leaves a table that was seeded once and emptied alone
        finance_engine::categories::clear_rules_seeded(&conn)?;
        categorize::ensure_rules(&conn)?;
        println!("Reloaded rules from defaults.");
    }
    let mut counts = categorize::categorize_all(&conn)?;
    drop(conn);
    // most rows first; the sort is stable, so ties keep first-seen order
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    for (cat, n) in &counts {
        println!("  {cat:26} {n:5}");
    }
    let total: usize = counts.iter().map(|c| c.1).sum();
    println!("Categorized {total} transactions into {} categories.", counts.len());
    Ok(())
}
