//! Tiny flag parsing for the subcommands (no clap: a dozen flags total).

use anyhow::{bail, Result};

pub struct Args<'a> {
    rest: &'a [String],
}

impl<'a> Args<'a> {
    /// Every token must be a known flag: `valued` take a value
    /// (`--x V` or `--x=V`), `switches` don't.
    pub fn parse(rest: &'a [String], valued: &[&str], switches: &[&str]) -> Result<Self> {
        let mut i = 0;
        while i < rest.len() {
            let tok = rest[i].split_once('=').map(|x| x.0).unwrap_or(&rest[i]);
            if valued.contains(&tok) {
                if !rest[i].contains('=') {
                    if i + 1 >= rest.len() {
                        bail!("{tok} needs a value");
                    }
                    i += 1;
                }
            } else if !switches.contains(&tok) {
                bail!("unknown argument {}", rest[i]);
            }
            i += 1;
        }
        Ok(Args { rest })
    }

    pub fn value(&self, flag: &str) -> Option<String> {
        let mut it = self.rest.iter();
        while let Some(t) = it.next() {
            if t == flag {
                return it.next().cloned();
            }
            if let Some(v) = t.strip_prefix(&format!("{flag}=")) {
                return Some(v.to_string());
            }
        }
        None
    }

    pub fn has(&self, flag: &str) -> bool {
        self.rest.iter().any(|t| t == flag)
    }
}

/// "YYYY-MM" -> the first of that month.
pub fn month(s: &str) -> Result<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(&format!("{}-01", s.get(..7).unwrap_or(s)), "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("expected YYYY-MM, got {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags() {
        let v: Vec<String> = ["--exchange", "okx", "--full", "--to=2026-01"].iter().map(|s| s.to_string()).collect();
        let a = Args::parse(&v, &["--exchange", "--to"], &["--full"]).unwrap();
        assert_eq!(a.value("--exchange").as_deref(), Some("okx"));
        assert_eq!(a.value("--to").as_deref(), Some("2026-01"));
        assert!(a.has("--full"));
        assert!(Args::parse(&v, &["--exchange"], &["--full"]).is_err());
        assert_eq!(month("2026-02").unwrap().to_string(), "2026-02-01");
    }
}

// argparse-shaped errors for the subcommands: a bad argument prints usage +
// the error to stderr and exits 2.

pub fn usage_error(usage: &str, msg: &str) -> ! {
    eprintln!("usage: anser-server {usage}");
    eprintln!("anser-server: error: {msg}");
    std::process::exit(2)
}

/// A subcommand that takes no arguments.
pub fn no_args(cmd: &str, args: &[String]) {
    if !args.is_empty() {
        usage_error(&format!("{cmd} [-h]"), &format!("unrecognized arguments: {}", args.join(" ")));
    }
}

pub fn int(usage: &str, name: &str, v: &str) -> i64 {
    v.trim()
        .parse()
        .unwrap_or_else(|_| usage_error(usage, &format!("argument {name}: invalid int value: '{v}'")))
}

pub fn float(usage: &str, name: &str, v: &str) -> f64 {
    v.trim()
        .parse()
        .unwrap_or_else(|_| usage_error(usage, &format!("argument {name}: invalid float value: '{v}'")))
}
