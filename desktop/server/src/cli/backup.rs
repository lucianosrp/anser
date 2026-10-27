//! `anser-server backup [--keep N] [--user NAME|ID]` : a consistent
//! db copy into data/backups/, pruned to the newest N (default 14). With a
//! backup recipient set (server.db; Settings, Backups in the desktop app), also
//! an age archive into data/backups/archives/ (backups.rs).

use finance_engine::db;

const USAGE: &str = "backup [-h] [--keep KEEP]";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let mut keep: i64 = 14;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--keep=") {
            keep = super::args::int(USAGE, "--keep", v);
        } else if a == "--keep" {
            let v = it.next().unwrap_or_else(|| super::args::usage_error(USAGE, "argument --keep: expected one argument"));
            keep = super::args::int(USAGE, "--keep", v);
        } else {
            super::args::usage_error(USAGE, &format!("unrecognized arguments: {a}"));
        }
    }
    super::serve::private_umask();
    // --user: that tree only; else the owner's (as always), then every
    // other user's, each into its own data/backups/ and sealed to its own
    // backup key
    if finance_engine::tenant::current().is_some() {
        return one(keep);
    }
    let mut result = one(keep);
    let others: Vec<_> = if crate::auth::path().exists() {
        crate::syncer::tenants().into_iter().filter(|t| !t.legacy).collect()
    } else {
        vec![]
    };
    for t in others {
        println!("User {}:", t.uid);
        if let Err(e) = t.enter(|| one(keep)) {
            eprintln!("user {}: backup failed: {e:#}", t.uid);
            result = result.and(Err(e));
        }
    }
    result
}

/// The active tree's backup and archive.
fn one(keep: i64) -> anyhow::Result<()> {
    let dest = db::backup(keep.max(1) as usize)?;
    let size = std::fs::metadata(&dest)?.len() as f64;
    println!("Backed up to {} ({:.1} MB); keeping newest {keep}.", dest.display(), size / 1e6);
    // the encrypted off-device archive, when a backup key is configured
    match crate::backups::nightly() {
        Ok(Some((path, size))) => println!("Archive {} ({:.1} MB, sealed to the backup key); keeping newest {}.",
            path.display(), size as f64 / 1e6, crate::backups::keep()),
        Ok(None) => {}
        // the plain backup above stands; the timer's log shows this
        Err(e) => eprintln!("archive failed: {e:#}"),
    }
    Ok(())
}
