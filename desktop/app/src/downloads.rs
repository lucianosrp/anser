//! The Downloads watcher's model, UI-free: an opt-in (Settings) scan of the
//! downloads folder for bank exports the server doesn't have yet.
//!
//! A file is suggested when it was modified in the last `days` days, a
//! source or a format definition claims it (the Upload page's own
//! detection), its md5 is not among the server's (`GET /api/files` and the
//! replica's ingested_files, as the Upload page checks) and it wasn't
//! dismissed. Settings and dismissals live in
//!
//!   ~/.config/finance-desktop/downloads.toml
//!   enabled = true
//!   dir = "~/Downloads"          # default: the platform's downloads folder
//!   days = 30
//!   dismissed = ["<md5>", ...]   # files not to suggest again
//!
//! `FINANCE_DESKTOP_DOWNLOADS_FILE` uses another file; `FINANCE_DESKTOP_DOWNLOADS=/dir`
//! (dev and screenshots) scans that folder and counts as opted in.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

pub const DEFAULT_DAYS: u32 = 30;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<u32>,
    pub dismissed: Vec<String>,
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("FINANCE_DESKTOP_DOWNLOADS_FILE") {
        return PathBuf::from(p);
    }
    dirs::config_dir().unwrap_or_default().join("finance-desktop").join("downloads.toml")
}

fn env_dir() -> Option<PathBuf> {
    std::env::var_os("FINANCE_DESKTOP_DOWNLOADS").filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

const HEADER: &str = "# anser: suggest new bank exports from the downloads folder (Settings).\n\
# enabled, dir (default: the downloads folder), days (default 30), dismissed: md5s not to suggest again.\n\n";

impl Config {
    pub fn load() -> Config {
        finance_core::replica::read_text(&path()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        // sealed (.enc) while the local copy is encrypted
        finance_core::replica::write_text(&path(), &format!("{HEADER}{}", toml::to_string(self)?))
    }

    pub fn enabled(&self) -> bool {
        self.enabled || env_dir().is_some()
    }

    /// The folder to scan: the dev override, the configured one, else the
    /// platform's downloads folder.
    pub fn dir(&self) -> Option<PathBuf> {
        env_dir().or_else(|| self.dir.as_deref().map(expand)).or_else(dirs::download_dir)
    }

    pub fn days(&self) -> u32 {
        self.days.unwrap_or(DEFAULT_DAYS).max(1)
    }
}

/// A file worth importing.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    pub name: String,
    pub md5: String,
    /// the source or format that claims it ("HSBC", "Monzo", ...)
    pub label: String,
}

type Key = (PathBuf, u64, SystemTime);

/// Remembers what each file (by path, size and mtime) is, so a rescan on
/// every window focus reads only new or changed files.
#[derive(Debug, Default)]
pub struct Scanner {
    seen: HashMap<Key, Option<(String, String)>>,
}

const EXTS: [&str; 5] = ["csv", "xlsx", "xls", "xml", "pdf"];

impl Scanner {
    /// The files in `dir` (not recursive, hidden ones skipped) modified
    /// within `days` of `now` that a source claims, newest first, minus
    /// `known` and `dismissed` md5s.
    pub fn scan(&mut self, dir: &Path, days: u32, now: SystemTime, known: &BTreeSet<String>, dismissed: &[String]) -> Vec<Found> {
        let since = now.checked_sub(Duration::from_secs(u64::from(days) * 86_400)).unwrap_or(SystemTime::UNIX_EPOCH);
        let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
        let mut files: Vec<(SystemTime, Found)> = Vec::new();
        let mut live = Vec::new();
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let ext = path.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if name.starts_with('.') || !EXTS.contains(&ext.as_str()) {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            let Ok(mtime) = md.modified() else { continue };
            if !md.is_file() || mtime < since || md.len() == 0 || !finance_core::upload::fits(md.len() as usize) {
                continue;
            }
            let key: Key = (path.clone(), md.len(), mtime);
            live.push(key.clone());
            let what = self.seen.entry(key).or_insert_with(|| classify(&path)).clone();
            let Some((md5, label)) = what else { continue };
            if known.contains(&md5) || dismissed.contains(&md5) {
                continue;
            }
            files.push((mtime, Found { path, name, md5, label }));
        }
        // forget files that are gone or changed
        self.seen.retain(|k, _| live.contains(k));
        files.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
        // one copy per content (the same export downloaded twice)
        let mut seen_md5 = BTreeSet::new();
        files.into_iter().map(|(_, f)| f).filter(|f| seen_md5.insert(f.md5.clone())).collect()
    }
}

/// (md5, label) when a source or format claims the file.
fn classify(path: &Path) -> Option<(String, String)> {
    let src = finance_engine::preview::detect(path).ok()?;
    let md5 = finance_core::upload::md5_file(path).ok()?;
    Some((md5, src.spec().label.to_string()))
}

/// "3 new exports found in Downloads".
pub fn headline(n: usize, dir: &Path) -> String {
    let folder = dir.file_name().map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string());
    format!("{n} new export{} found in {folder}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const N26: &str = "\"Booking Date\",\"Value Date\",\"Partner Name\",\"Partner Iban\",Type,\"Payment Reference\",\"Account Name\",\"Amount (EUR)\",\"Original Amount\",\"Original Currency\",\"Exchange Rate\"\n\
2026-03-01,2026-03-01,Coffee,,Presentment,,Main Account,-3.5,,,\n";

    #[test]
    fn suggests_only_new_supported_recent_files() {
        let dir = std::env::temp_dir().join(format!("anser-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("n26-march.csv"), N26).unwrap();
        std::fs::write(dir.join("n26-march (1).csv"), N26).unwrap(); // the same export twice
        std::fs::write(dir.join("photo.jpg"), b"x").unwrap();
        std::fs::write(dir.join("notes.csv"), "a,b\n1,2\n").unwrap(); // no source claims it
        std::fs::write(dir.join(".hidden.csv"), N26).unwrap();
        let now = SystemTime::now();
        let mut s = Scanner::default();
        let found = s.scan(&dir, 30, now, &BTreeSet::new(), &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].label, "N26");
        let md5 = found[0].md5.clone();
        // known on the server, or dismissed: not suggested
        assert!(s.scan(&dir, 30, now, &BTreeSet::from([md5.clone()]), &[]).is_empty());
        assert!(s.scan(&dir, 30, now, &BTreeSet::new(), std::slice::from_ref(&md5)).is_empty());
        // too old: a scan "40 days from now" with a 30-day window
        assert!(s.scan(&dir, 30, now + Duration::from_secs(40 * 86_400), &BTreeSet::new(), &[]).is_empty());
        assert_eq!(headline(3, &dir).split(" found in ").next(), Some("3 new exports"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_defaults() {
        let c: Config = toml::from_str("").unwrap();
        assert!(!c.enabled && c.days() == 30 && c.dismissed.is_empty());
        let c: Config = toml::from_str("enabled = true\ndays = 7\ndir = \"~/Dl\"").unwrap();
        assert_eq!(c.days(), 7);
        assert!(toml::to_string(&c).unwrap().contains("days = 7"));
    }
}
