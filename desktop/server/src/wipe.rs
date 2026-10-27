//! Delete all, on the server: every account, device, key, upload and
//! database, so the next start is a fresh install. An admin asks for it in
//! Settings, Admin, Danger zone (`POST /api/admin/wipe`); the server then
//! exits and its service manager starts it again, empty.
//!
//! Only the data goes, never the program: the repo checkout the server builds
//! from and the installed web app (`data/webapp`) stay. The owner may keep
//! the nightly backups (`data/backups`, so a mistake can be undone with
//! Import) and the API keys (`secrets/`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use finance_engine::config;

#[derive(Debug, Clone, Copy, Default)]
pub struct Keep {
    pub backups: bool,
    pub keys: bool,
}

/// What a wipe of the tree at `home` (its data dir `data`) removes.
pub fn plan(home: &Path, data: &Path, keep: Keep) -> Result<Vec<PathBuf>> {
    let home_c = home.canonicalize().with_context(|| format!("{}", home.display()))?;
    if home_c.parent().is_none() || dirs_home().is_some_and(|h| h == home_c) {
        bail!("refusing to wipe {}: not a finance tree", home_c.display());
    }
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(data) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let kept = name == "webapp" || (keep.backups && name == "backups");
            if !kept {
                out.push(e.path());
            }
        }
    }
    let mut also = vec![home.join("transactions"), home.join("users")];
    if !keep.keys {
        also.push(home.join("secrets"));
    }
    out.extend(also.into_iter().filter(|p| p.exists()));
    out.sort();
    Ok(out)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).and_then(|h| h.canonicalize().ok())
}

/// Remove what [`plan`] lists. Returns the paths removed.
pub fn run(home: &Path, data: &Path, keep: Keep) -> Result<Vec<PathBuf>> {
    let gone = plan(home, data, keep)?;
    for p in &gone {
        let r = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
        match r {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
        }
    }
    Ok(gone)
}

/// The server's own tree.
pub fn run_here(keep: Keep) -> Result<Vec<PathBuf>> {
    run(&config::root_home(), &config::root_data_dir(), keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("finance-wipe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        let h = h.as_path();
        for d in ["data/backups", "data/webapp", "data/avatar", "transactions/n26", "users/2/data", "secrets", "desktop/src"] {
            std::fs::create_dir_all(h.join(d)).unwrap();
        }
        for f in ["data/finance.db", "data/server.db", "data/backups/a.finance", "data/webapp/index.html",
                  "transactions/n26/x.csv", "secrets/master.key", "desktop/src/main.rs", "README.md"] {
            std::fs::write(h.join(f), "x").unwrap();
        }
        h.to_path_buf()
    }

    #[test]
    fn wipes_the_data_and_keeps_the_program() {
        let t = tree("all");
        let h = t.as_path();
        run(h, &h.join("data"), Keep::default()).unwrap();
        for gone in ["data/finance.db", "data/server.db", "data/avatar", "data/backups", "transactions", "users", "secrets"] {
            assert!(!h.join(gone).exists(), "{gone} should be gone");
        }
        for kept in ["data/webapp/index.html", "desktop/src/main.rs", "README.md"] {
            assert!(h.join(kept).exists(), "{kept} should stay");
        }
        let _ = std::fs::remove_dir_all(h);
    }

    #[test]
    fn keeps_backups_and_keys_when_asked() {
        let t = tree("keep");
        let h = t.as_path();
        run(h, &h.join("data"), Keep { backups: true, keys: true }).unwrap();
        assert!(h.join("data/backups/a.finance").exists());
        assert!(h.join("secrets/master.key").exists());
        assert!(!h.join("data/finance.db").exists());
        let _ = std::fs::remove_dir_all(h);
    }
}
