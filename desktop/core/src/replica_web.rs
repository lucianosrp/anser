//! Web build stand-in for `replica.rs`: the browser keeps no replica on
//! disk, so nothing is encrypted and the config helpers are plain (and,
//! without a filesystem, read nothing).

use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub encrypted: bool,
    pub unlocked: bool,
}

impl Status {
    pub fn label(&self) -> String {
        "kept in the browser's memory only".into()
    }
}

pub fn status() -> Status {
    Status { encrypted: false, unlocked: false }
}

pub fn is_encrypted() -> bool {
    false
}

pub fn locked() -> bool {
    false
}

pub fn auto_unlock() -> bool {
    true
}

pub fn stored_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

pub fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

pub fn write_text(path: &Path, text: &str) -> Result<()> {
    Ok(std::fs::write(path, text)?)
}
