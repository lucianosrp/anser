//! Format definitions on this machine (engine `sources::format`): where the
//! Upload page's preview finds them, and how a definition made with the
//! column mapper reaches the server.
//!
//! * `~/.config/finance-desktop/formats/*.toml`: the user's own
//!   (`$FINANCE_DESKTOP_FORMATS` for another directory). The mapper's
//!   *Save as format* writes here; drop a file here to install one by hand.
//! * `<cache>/formats/`: a copy of the server's (`GET /api/formats`), so a
//!   definition saved on another machine previews here too. The user's own
//!   win over the server's with the same id.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use finance_core::{cache, upload};
use finance_engine::sources::format;

pub fn local_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("FINANCE_DESKTOP_FORMATS") {
        return PathBuf::from(p);
    }
    dirs::config_dir().unwrap_or_default().join("finance-desktop").join("formats")
}

pub fn server_dir() -> PathBuf {
    cache::dir().join("formats")
}

/// Point the engine at this machine's directories. Once, at start.
pub fn init() {
    format::set_dirs(vec![local_dir(), server_dir()]);
}

/// Copy the server's own definitions (not its built-in ones) into the
/// cache, replacing the last copy. Blocking. Returns how many it has.
pub fn pull(server: &str) -> Result<usize> {
    let agent = upload::agent(Duration::from_secs(8));
    let Some(list) = upload::server_formats(&agent, server)? else { return Ok(0) };
    let dir = server_dir();
    let mine: Vec<_> = list.into_iter().filter(|f| !f.builtin).collect();
    sync_dir(&dir, mine.iter().map(|f| (f.id.as_str(), f.toml.as_str())))?;
    format::invalidate();
    Ok(mine.len())
}

/// Make `dir` hold exactly these (id, toml) files; invalid ones are skipped.
fn sync_dir<'a>(dir: &Path, files: impl Iterator<Item = (&'a str, &'a str)>) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut keep = Vec::new();
    for (id, text) in files {
        if format::valid_id(id) && format::save_text(dir, id, text).is_ok() {
            keep.push(format!("{id}.toml"));
        }
    }
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".toml") && !keep.contains(&name) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(())
}

/// Send a definition saved here (`<local_dir>/<id>.toml`) to the server
/// (blocking): its ingest needs it to read the files it describes.
pub fn push(id: &str, server: &str) -> Result<()> {
    let text = std::fs::read_to_string(local_dir().join(format!("{id}.toml")))?;
    upload::put_format(&upload::agent(Duration::from_secs(20)), server, id, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_copy_replaces_the_last_one() {
        let dir = std::env::temp_dir().join(format!("anser-formats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let def = |id: &str| {
            format!("id = \"{id}\"\nlabel = \"X\"\ncurrency = \"EUR\"\n[detect]\ncolumns = [\"D\"]\n[columns]\ndate = \"D\"\namount = \"A\"\ndescription = \"T\"\n")
        };
        let (a, b) = (def("aaa"), def("bbb"));
        sync_dir(&dir, [("aaa", a.as_str()), ("bbb", b.as_str()), ("bad id", "x")].into_iter()).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, vec!["aaa.toml", "bbb.toml"]);
        sync_dir(&dir, [("bbb", b.as_str())].into_iter()).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["bbb.toml"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
