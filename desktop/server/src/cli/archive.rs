//! The portable archive from the shell (finance_engine::archive):
//!
//! * `anser-server export [--out FILE] (--passphrase-file F | --recipient age1…)
//!   [--from YYYY-MM-DD] [--to YYYY-MM-DD] [--sources a,b] [--with-secrets] [--user NAME|ID]`
//!   (no key given: the configured backup recipient)
//! * `anser-server import FILE --mode fresh|merge|replace [--user NAME|ID]
//!   [--dry-run] [--prefer-archive] (--passphrase-file F | --identity FILE)`
//! * `anser-server verify FILE (--passphrase-file F | --identity FILE)`
//!
//! `--passphrase-file -` reads the passphrase from stdin (its first line).

use std::io::Read;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use finance_engine::archive::{self, ExportOpts, Mode, Scope, Seal, Tree, Unseal};
use finance_engine::secrets;
use crate::backups;

use super::args::Args;

fn read_passphrase(file: &str) -> Result<String> {
    let text = if file == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?
    };
    let p = text.lines().next().unwrap_or("").to_string();
    anyhow::ensure!(!p.is_empty(), "the passphrase file is empty");
    Ok(p)
}

fn unseal(a: &Args, file: &PathBuf) -> Result<Unseal> {
    if let Some(f) = a.value("--passphrase-file") {
        return Ok(Unseal::Passphrase(read_passphrase(&f)?));
    }
    if let Some(f) = a.value("--identity") {
        return Ok(Unseal::Identity(std::fs::read_to_string(&f).with_context(|| format!("reading {f}"))?));
    }
    let pass = archive::is_passphrase(std::fs::File::open(file)?)?;
    bail!("{} is sealed with {}: pass {}", file.display(),
        if pass { "a passphrase" } else { "a backup key" },
        if pass { "--passphrase-file FILE (or - for stdin)" } else { "--identity FILE (the key the desktop app saved)" })
}

pub fn export(rest: &[String]) -> Result<()> {
    let a = Args::parse(rest, &["--out", "--passphrase-file", "--recipient", "--from", "--to", "--sources"], &["--with-secrets"])?;
    super::serve::private_umask();
    let seal = match (a.value("--passphrase-file"), a.value("--recipient")) {
        (Some(f), None) => Seal::Passphrase { passphrase: read_passphrase(&f)?, work_factor: None },
        (None, Some(r)) => Seal::Recipients(vec![r]),
        (Some(_), Some(_)) => bail!("--passphrase-file or --recipient, not both"),
        (None, None) => match backups::recipient() {
            Some(r) => Seal::Recipients(r.split(',').map(String::from).collect()),
            None => bail!("give --passphrase-file FILE or --recipient age1… (no backup recipient is configured)"),
        },
    };
    let scope = Scope {
        from: a.value("--from"),
        to: a.value("--to"),
        sources: a.value("--sources").map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default(),
    };
    let held = if a.has("--with-secrets") { Some(secrets::load(secrets::active_user())?) } else { None };
    let out = PathBuf::from(a.value("--out").unwrap_or_else(|| format!("finance-{}.finance", chrono::Local::now().format("%Y%m%d-%H%M%S"))));
    let m = archive::export_file(&Tree::configured(), &ExportOpts { scope, secrets: held.as_ref(), created_at: None }, &seal, &out)?;
    let size = std::fs::metadata(&out)?.len();
    println!("Wrote {} ({:.2} MB): {} bank rows, {} crypto events, {} originals{}{}.", out.display(), size as f64 / 1e6,
        m.row_counts.get("bank_transactions").unwrap_or(&0), m.row_counts.get("crypto_events").unwrap_or(&0),
        m.entries.keys().filter(|k| k.starts_with("originals/")).count(),
        if m.partial { ", partial" } else { "" }, if m.includes_secrets { ", WITH SECRETS (keep it safe)" } else { "" });
    Ok(())
}

pub fn verify(rest: &[String]) -> Result<()> {
    let Some(file) = rest.first().filter(|f| !f.starts_with("--")).map(PathBuf::from) else {
        bail!("usage: {} verify FILE [--passphrase-file F | --identity FILE]", super::prog());
    };
    let a = Args::parse(&rest[1..], &["--passphrase-file", "--identity"], &[])?;
    let u = unseal(&a, &file)?;
    let r = archive::verify(std::fs::File::open(&file)?, &u)?;
    println!("{}", r.summary());
    Ok(())
}

pub fn import(rest: &[String]) -> Result<()> {
    let Some(file) = rest.first().filter(|f| !f.starts_with("--")).map(PathBuf::from) else {
        bail!("usage: {} import FILE --mode fresh|merge|replace [--user NAME|ID] [--dry-run] [--prefer-archive] [--passphrase-file F | --identity FILE]", super::prog());
    };
    // --user NAME|ID was taken by cli::dispatch: this runs on that tree
    let a = Args::parse(&rest[1..], &["--mode", "--passphrase-file", "--identity"], &["--dry-run", "--prefer-archive"])?;
    let mode = a.value("--mode").as_deref().and_then(Mode::parse).context("--mode fresh, merge or replace")?;
    super::serve::private_umask();
    let u = unseal(&a, &file)?;
    let arc = archive::open(std::fs::File::open(&file)?, &u)?;
    let (rep, stored) = backups::run_import(&arc, mode, a.has("--dry-run"), a.has("--prefer-archive"))?;
    print!("{}", rep.text());
    if !stored.is_empty() {
        println!("  secrets stored: {}", stored.join(", "));
    } else if arc.manifest.includes_secrets && rep.dry_run {
        println!("  the archive carries secrets: a real run stores them in the encrypted store");
    }
    if rep.dry_run {
        println!("Dry run: run again without --dry-run to apply.");
    }
    Ok(())
}
