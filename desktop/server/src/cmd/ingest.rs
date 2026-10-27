//! `anser-server ingest`: load new transactions/*.{csv,xml,pdf,xlsx,xls} into
//! sqlite (no network).

use finance_engine::{db, ingest};

pub fn run(args: &[String]) -> anyhow::Result<()> {
    super::args::no_args("ingest", args);
    let conn = db::open()?;
    let mut summary = ingest::run_ingest(&conn)?;
    drop(conn);
    let total_files: usize = summary.iter().map(|(_, s)| s.files_new).sum();
    let total_rows: usize = summary.iter().map(|(_, s)| s.rows_added).sum();
    let failed: usize = summary.iter().map(|(_, s)| s.files_failed).sum();
    if failed > 0 {
        eprintln!("{failed} file(s) could not be read or parsed (see above); they are tried again next run.");
    }
    if total_files == 0 {
        if failed == 0 {
            println!("Nothing new — all files already ingested.");
        }
        return Ok(());
    }
    summary.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, s) in &summary {
        if s.files_new > 0 {
            // near_dupes: same transaction re-exported with a drifted
            // balance, renamed merchant or shifted date -- shown so a big
            // number reads as "overlapping export", not as rows going missing
            let mut extra = if s.near_dupes > 0 { format!(", {} near-duplicate(s) skipped", s.near_dupes) } else { String::new() };
            if s.legacy_twins > 0 {
                extra += &format!(", {} already in the legacy export skipped", s.legacy_twins);
            }
            println!("  {key:16} +{} file(s), +{} row(s){extra}", s.files_new, s.rows_added);
        }
    }
    println!("Ingested {total_files} file(s), {total_rows} row(s).");
    Ok(())
}
