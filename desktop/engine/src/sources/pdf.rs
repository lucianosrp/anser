//! `pdftotext -layout` for statement PDFs (HSBC card, Mox, …).
//!
//! Always the same flags, so a statement's rows and keys never move.
//! Present on the server and the laptop.

use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use super::textfmt;

/// `pdftotext -layout <path> -` as text: strict UTF-8, newlines normalized
/// to `\n`, 60 s timeout. The last file's text is kept (by path, size and
/// mtime): every PDF source's `matches` and then the winner's `parse` read
/// the same file, and one run of pdftotext is enough.
pub fn text(path: &Path) -> Result<String> {
    static LAST: Mutex<Option<(FileKey, String)>> = Mutex::new(None);
    let key = file_key(path);
    if let (Some(k), Ok(last)) = (&key, LAST.lock()) {
        if let Some((_, text)) = last.as_ref().filter(|(lk, _)| lk == k) {
            return Ok(text.clone());
        }
    }
    let text = run_pdftotext(path)?;
    if let (Some(k), Ok(mut last)) = (key, LAST.lock()) {
        *last = Some((k, text.clone()));
    }
    Ok(text)
}

type FileKey = (std::path::PathBuf, u64, Option<std::time::SystemTime>);

fn file_key(path: &Path) -> Option<FileKey> {
    let m = std::fs::metadata(path).ok()?;
    Some((path.to_path_buf(), m.len(), m.modified().ok()))
}

fn run_pdftotext(path: &Path) -> Result<String> {
    let mut child = Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        Read::read_to_end(&mut stdout, &mut buf).map(|_| buf)
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("pdftotext timed out on {}", path.display());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let out = reader
        .join()
        .map_err(|_| anyhow!("pdftotext reader panicked"))??;
    if !status.success() {
        bail!("pdftotext failed on {}: {status}", path.display());
    }
    Ok(textfmt::universal_newlines(&String::from_utf8(out)?))
}

/// True when `path` is a PDF whose `pdftotext` output starts with every needle.
/// `n` is how many leading chars to look at (HSBC card: 400).
pub fn head_contains(path: &Path, n: usize, needles: &[&str]) -> bool {
    match text(path) {
        Ok(text) => {
            let head = textfmt::prefix_chars(&text, n);
            needles.iter().all(|n| head.contains(n))
        }
        Err(_) => false,
    }
}
