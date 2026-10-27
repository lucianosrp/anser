//! The Upload page's model, UI-free: one row per dropped file and the stage
//! it is in, plus the blocking work each stage does (run off the UI thread
//! by `pages::upload`).
//!
//! ```text
//! queued -> parsing -> parsed ------> uploading -> done (saved / same content / failed)
//!                  \-> unsupported      \-> done (already on the server: not sent)
//!                  \-> unmatched (a CSV / spreadsheet no format reads: "Map columns…")
//!                  \-> error
//! ```
//!
//! A held batch (the Downloads banner's *Review*) stops at parsed until
//! the user sends it.
//!
//! ```text
//! ```
//!
//! Parsing is local: finance-engine's own source detection and parsers, and
//! its real ingest run against a private in-memory copy of the replica, so
//! "new" is exactly what ingest on the server would add. Nothing here writes
//! to the replica.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use finance_core::upload::{self, Outcome};
use finance_engine::preview::{self, ParsedFile};
use finance_engine::sources::{BalanceMethod, Kind};
use rusqlite::Connection;

/// What a parsed file holds, and what ingest would make of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    /// source key ('n26', 'hsbc_card', ...) and its label
    pub source: String,
    pub label: String,
    /// "bank", "card", "crypto" or "fund units"
    pub kind: &'static str,
    pub rows: usize,
    /// rows ingest would add / of those known, the near-duplicates
    pub new: usize,
    pub near_dupes: usize,
    pub range: Option<(String, String)>,
}

impl Preview {
    pub fn known(&self) -> usize {
        self.rows.saturating_sub(self.new)
    }
}

/// What the server made of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    /// written to transactions/ under this name
    Saved(String),
    /// the server had a file of this name with the same content
    SameContent,
    /// the content hash was known: never sent
    AlreadyOnServer,
    /// refused, or unreachable
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    Queued,
    Parsing,
    Parsed(Preview),
    Unsupported(String),
    /// a CSV or spreadsheet no source or format reads: the column mapper
    /// can describe it
    Unmatched(String),
    Error(String),
    Uploading(Preview),
    Done(Preview, Sent),
}

impl Stage {
    /// Short label for the status chip.
    pub fn label(&self) -> &'static str {
        match self {
            Stage::Queued => "queued",
            Stage::Parsing => "parsing",
            Stage::Parsed(_) => "parsed",
            Stage::Unsupported(_) => "unsupported",
            Stage::Unmatched(_) => "unknown format",
            Stage::Error(_) => "error",
            Stage::Uploading(_) => "uploading",
            Stage::Done(_, Sent::Saved(_)) => "uploaded",
            Stage::Done(_, Sent::SameContent) => "already there",
            Stage::Done(_, Sent::AlreadyOnServer) => "on the server",
            Stage::Done(_, Sent::Failed(_)) => "upload failed",
        }
    }

    /// Still moving (a spinner, not a verdict).
    pub fn busy(&self) -> bool {
        matches!(self, Stage::Queued | Stage::Parsing | Stage::Parsed(_) | Stage::Uploading(_))
    }

    pub fn preview(&self) -> Option<&Preview> {
        match self {
            Stage::Parsed(p) | Stage::Uploading(p) | Stage::Done(p, _) => Some(p),
            _ => None,
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self, Stage::Unsupported(_) | Stage::Unmatched(_) | Stage::Error(_) | Stage::Done(_, Sent::Failed(_)))
    }
}

#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: usize,
    /// which drop it came with (a batch uploads once all its files parsed)
    pub batch: usize,
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub md5: Option<String>,
    pub stage: Stage,
}

/// Totals over the rows on the page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub files: usize,
    pub busy: usize,
    pub rows: usize,
    pub new: usize,
    pub uploaded: usize,
    pub failed: usize,
}

#[derive(Debug, Default)]
pub struct Queue {
    pub rows: Vec<FileRow>,
    next_id: usize,
    next_batch: usize,
    /// batches that wait for the user's go once parsed
    held: BTreeSet<usize>,
}

/// Files of `paths`, a directory standing for its files (not recursive),
/// hidden files skipped, in name order per directory.
pub fn expand(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut inner: Vec<PathBuf> = std::fs::read_dir(p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|f| f.is_file() && !f.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
                .collect();
            inner.sort();
            out.extend(inner);
        } else {
            out.push(p.clone());
        }
    }
    out
}

impl Queue {
    /// Add files as one batch that waits, once parsed, for [`Queue::release`].
    pub fn enqueue_held(&mut self, paths: &[PathBuf]) -> (usize, Vec<usize>) {
        let (batch, ids) = self.enqueue(paths);
        self.held.insert(batch);
        (batch, ids)
    }

    pub fn is_held(&self, batch: usize) -> bool {
        self.held.contains(&batch)
    }

    /// The held batches with files ready to send.
    pub fn held_ready(&self) -> Vec<(usize, usize)> {
        self.held.iter().map(|&b| (b, self.to_upload(b).len())).filter(|&(b, n)| n > 0 && self.batch_parsed(b)).collect()
    }

    /// Let a held batch go: it uploads like any other.
    pub fn release(&mut self, batch: usize) -> bool {
        self.held.remove(&batch)
    }

    /// Take a row off the page (a held file the user doesn't want).
    pub fn remove(&mut self, id: usize) {
        self.rows.retain(|r| r.id != id);
    }

    /// Add files as one batch; a file already on the page and still moving
    /// is not added twice. Returns (batch, new row ids).
    pub fn enqueue(&mut self, paths: &[PathBuf]) -> (usize, Vec<usize>) {
        let batch = self.next_batch;
        self.next_batch += 1;
        let mut ids = Vec::new();
        for path in expand(paths) {
            if self.rows.iter().any(|r| r.path == path && r.stage.busy()) {
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            self.rows.push(FileRow { id, batch, path, name, bytes, md5: None, stage: Stage::Queued });
            ids.push(id);
        }
        (batch, ids)
    }

    pub fn get(&self, id: usize) -> Option<&FileRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    pub fn set(&mut self, id: usize, stage: Stage) {
        if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
            r.stage = stage;
        }
    }

    pub fn set_md5(&mut self, id: usize, md5: Option<String>) {
        if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
            r.md5 = md5;
        }
    }

    /// Has every file of `batch` left queued / parsing?
    pub fn batch_parsed(&self, batch: usize) -> bool {
        self.rows.iter().filter(|r| r.batch == batch).all(|r| !matches!(r.stage, Stage::Queued | Stage::Parsing))
    }

    /// The batch's files ready to send, in page order.
    pub fn to_upload(&self, batch: usize) -> Vec<usize> {
        self.rows.iter().filter(|r| r.batch == batch && matches!(r.stage, Stage::Parsed(_))).map(|r| r.id).collect()
    }

    /// Drop the rows that are finished (done, unsupported, error).
    pub fn clear_finished(&mut self) {
        self.rows.retain(|r| r.stage.busy());
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary { files: self.rows.len(), ..Summary::default() };
        for r in &self.rows {
            if r.stage.busy() {
                s.busy += 1;
            }
            if r.stage.failed() {
                s.failed += 1;
            }
            if let Some(p) = r.stage.preview() {
                s.rows += p.rows;
                s.new += p.new;
            }
            if matches!(r.stage, Stage::Done(_, Sent::Saved(_))) {
                s.uploaded += 1;
            }
        }
        s
    }
}

/// "bank" / "card" / "crypto" / "fund units" for a source.
pub fn kind_label(spec: &finance_engine::sources::SourceSpec) -> &'static str {
    match (spec.kind, spec.balance_method) {
        (Kind::Bank, BalanceMethod::None) => "card",
        (Kind::Bank, _) => "bank",
        (Kind::CryptoWallet, _) => "crypto",
        (Kind::FundUnits, _) => "fund units",
    }
}

/// Dev aid: FINANCE_DESKTOP_UPLOAD_STEP_MS slows every stage down, so a
/// screenshot can catch the page mid-flow.
pub fn step_pause() {
    if let Some(ms) = std::env::var("FINANCE_DESKTOP_UPLOAD_STEP_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// A private in-memory copy of the replica: the plain file, or the
/// encrypted one decrypted in memory (finance_core::replica).
fn replica_copy(replica: &Path) -> anyhow::Result<rusqlite::Connection> {
    if finance_core::replica::stored_path(replica) == replica {
        return preview::replica_copy(replica);
    }
    preview::replica_copy_from(&finance_core::replica::open_db(replica)?)
}

/// The batch's private copy of the replica, made by the first file that
/// needs it. Files are applied to it in turn, so overlap between dropped
/// files counts as known too.
pub type Scratch = Mutex<Option<Result<Connection, String>>>;

/// Parse one file and preview its dedupe against the batch's scratch copy.
/// Blocking. Returns the md5 (when the file could be read) and the stage.
pub fn parse_one(path: &Path, replica: &Path, scratch: &Scratch) -> (Option<String>, Stage) {
    step_pause();
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return (None, Stage::Error(format!("can't read it: {e}"))),
    };
    let md5 = Some(upload::md5_hex(&bytes));
    let pf: ParsedFile = match preview::parse_file(path) {
        Ok(Ok(pf)) => pf,
        Ok(Err(why @ preview::Unsupported::Unknown)) if preview::mappable(path) => {
            return (md5, Stage::Unmatched(format!("{why}: map its columns once and it reads like any other")));
        }
        Ok(Err(why)) => return (md5, Stage::Unsupported(why.to_string())),
        Err(e) => return (md5, Stage::Error(format!("{e:#}"))),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut guard = scratch.lock().unwrap_or_else(|p| p.into_inner());
    let conn = match guard.get_or_insert_with(|| replica_copy(replica).map_err(|e| format!("{e:#}"))) {
        Ok(c) => c,
        Err(e) => return (md5, Stage::Error(format!("no copy of the local replica: {e}"))),
    };
    match preview::dedupe(conn, &pf, &upload::upload_name(&name)) {
        Ok(load) => (
            md5,
            Stage::Parsed(Preview {
                source: pf.source.key.to_string(),
                label: pf.source.label.to_string(),
                kind: kind_label(&pf.source),
                rows: load.rows,
                new: load.added,
                near_dupes: load.near_dupes,
                range: pf.range,
            }),
        ),
        Err(e) => (md5, Stage::Error(format!("dedupe preview: {e:#}"))),
    }
}

/// The content hashes the server already holds: `GET /api/files` when it
/// serves it (Rust), always merged with the replica's ingested_files.
pub fn known_hashes(server: &str, replica: &Path) -> BTreeSet<String> {
    let mut known = upload::replica_hashes(replica);
    if let Ok(Some(h)) = upload::server_hashes(&upload::agent(Duration::from_secs(8)), server) {
        known.extend(h);
    }
    known
}

/// Send one file (blocking). Saved names come back as the server wrote them.
pub fn send_one(server: &str, path: &Path) -> Sent {
    step_pause();
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return Sent::Failed(format!("can't read it: {e}")),
    };
    if !upload::fits(bytes.len()) {
        return Sent::Failed(format!("too large ({} MB): the server takes 20 MB", bytes.len() >> 20));
    }
    let name = upload::upload_name(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    match upload::post_files(&upload::agent(Duration::from_secs(120)), server, &[(name, bytes)]) {
        Ok(out) => sent_from(out),
        Err(e) => Sent::Failed(format!("{e:#}")),
    }
}

/// One file's verdict from the server's redirect.
pub fn sent_from(out: Outcome) -> Sent {
    if let Some(name) = out.saved.into_iter().next() {
        Sent::Saved(name)
    } else if !out.skipped.is_empty() {
        Sent::SameContent
    } else {
        Sent::Failed(out.errors.join("; "))
    }
}

/// "3 Mar 2025 – 20 May 2025", or one day.
pub fn range_label(range: &Option<(String, String)>) -> Option<String> {
    let day = |d: &str| {
        chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_else(|_| d.to_string())
    };
    let (a, b) = range.as_ref()?;
    Some(if a == b { day(a) } else { format!("{} – {}", day(a), day(b)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(rows: usize, new: usize) -> Preview {
        Preview { source: "n26".into(), label: "N26".into(), kind: "bank", rows, new, near_dupes: 0, range: None }
    }

    #[test]
    fn stages_move_forward() {
        let dir = std::env::temp_dir().join(format!("anser-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        for f in ["a.csv", "folder/b.csv", "folder/c.pdf", "folder/.hidden"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let mut q = Queue::default();
        let (b0, ids) = q.enqueue(&[dir.join("a.csv"), dir.join("folder")]);
        assert_eq!(ids, vec![0, 1, 2], "the folder stands for its two visible files");
        assert_eq!(q.get(1).unwrap().name, "b.csv");
        assert!(q.rows.iter().all(|r| r.stage == Stage::Queued && r.batch == b0 && r.bytes == 1));
        // dropping a file that is still moving doesn't add it twice
        let (b1, again) = q.enqueue(&[dir.join("a.csv")]);
        assert!(again.is_empty() && b1 != b0);

        q.set(0, Stage::Parsing);
        q.set(1, Stage::Parsed(preview(10, 3)));
        assert!(!q.batch_parsed(b0));
        q.set(0, Stage::Parsed(preview(5, 0)));
        q.set(2, Stage::Unsupported("no known export format matches its header".into()));
        assert!(q.batch_parsed(b0));
        assert_eq!(q.to_upload(b0), vec![0, 1]);

        q.set(0, Stage::Done(preview(5, 0), Sent::AlreadyOnServer));
        q.set(1, Stage::Uploading(preview(10, 3)));
        assert_eq!(q.summary(), Summary { files: 3, busy: 1, rows: 15, new: 3, uploaded: 0, failed: 1 });
        q.set(1, Stage::Done(preview(10, 3), Sent::Saved("b.csv".into())));
        assert_eq!(q.summary().uploaded, 1);
        assert_eq!(q.get(1).unwrap().stage.label(), "uploaded");
        assert_eq!(q.get(0).unwrap().stage.preview().unwrap().known(), 5);

        // once finished, the same file can come again
        let (_, back) = q.enqueue(&[dir.join("a.csv")]);
        assert_eq!(back, vec![3]);
        q.clear_finished();
        assert_eq!(q.rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn held_batches_wait() {
        let dir = std::env::temp_dir().join(format!("anser-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["a.csv", "b.csv"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let mut q = Queue::default();
        let (b, ids) = q.enqueue_held(&[dir.join("a.csv"), dir.join("b.csv")]);
        assert!(q.is_held(b));
        assert!(q.held_ready().is_empty(), "nothing parsed yet");
        q.set(ids[0], Stage::Parsed(preview(3, 3)));
        q.set(ids[1], Stage::Unmatched("no known export format matches its header".into()));
        assert_eq!(q.held_ready(), vec![(b, 1)]);
        assert!(q.get(ids[1]).unwrap().stage.failed());
        q.remove(ids[0]);
        assert!(q.held_ready().is_empty());
        assert!(q.release(b) && !q.is_held(b));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn verdicts() {
        let o = |s: &[&str], k: &[&str], e: &[&str]| Outcome {
            saved: s.iter().map(|x| x.to_string()).collect(),
            skipped: k.iter().map(|x| x.to_string()).collect(),
            errors: e.iter().map(|x| x.to_string()).collect(),
        };
        assert_eq!(sent_from(o(&["a (2).csv"], &[], &[])), Sent::Saved("a (2).csv".into()));
        assert_eq!(sent_from(o(&[], &["a.csv"], &[])), Sent::SameContent);
        assert_eq!(sent_from(o(&[], &[], &["a.txt: only .csv / .xml / .pdf accepted"])), Sent::Failed("a.txt: only .csv / .xml / .pdf accepted".into()));
        assert!(Stage::Done(preview(1, 1), Sent::Failed("x".into())).failed());
        assert!(!Stage::Done(preview(1, 1), Sent::SameContent).busy());
    }

    #[test]
    fn labels() {
        assert_eq!(range_label(&Some(("2025-03-03".into(), "2025-05-20".into()))).unwrap(), "3 Mar 2025 – 20 May 2025");
        assert_eq!(range_label(&Some(("2025-03-03".into(), "2025-03-03".into()))).unwrap(), "3 Mar 2025");
        assert_eq!(range_label(&None), None);
        let spec = finance_engine::sources::spec("hsbc_card").unwrap();
        assert_eq!(kind_label(&spec), "card");
        assert_eq!(kind_label(&finance_engine::sources::spec("trezor").unwrap()), "crypto");
        assert_eq!(kind_label(&finance_engine::sources::spec("hsbc").unwrap()), "bank");
    }

    /// End to end against a SCRATCH server (never your real one):
    /// `FINANCE_UPLOAD_E2E_SERVER=http://127.0.0.1:PORT FINANCE_UPLOAD_E2E_REPLICA=path/finance.db
    /// FINANCE_UPLOAD_E2E_FILES=dir cargo test -p finance-desktop e2e -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn e2e_scratch_server() {
        let server = std::env::var("FINANCE_UPLOAD_E2E_SERVER").expect("FINANCE_UPLOAD_E2E_SERVER");
        assert!(server.starts_with("http://127.0.0.1:"), "scratch servers on this machine only");
        let replica = PathBuf::from(std::env::var("FINANCE_UPLOAD_E2E_REPLICA").expect("FINANCE_UPLOAD_E2E_REPLICA"));
        let files = expand(&[PathBuf::from(std::env::var("FINANCE_UPLOAD_E2E_FILES").expect("FINANCE_UPLOAD_E2E_FILES"))]);
        let before = std::fs::read(&replica).unwrap();
        let scratch: Scratch = Mutex::new(None);
        let t0 = std::time::Instant::now();
        // in parallel, as the page does
        let parsed: Vec<(PathBuf, Option<String>, Stage)> = std::thread::scope(|s| {
            let hs: Vec<_> = files.iter().map(|f| s.spawn(|| { let (m, st) = parse_one(f, &replica, &scratch); (f.clone(), m, st) })).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        eprintln!("parsed {} files in {:?}", parsed.len(), t0.elapsed());
        let mut known = known_hashes(&server, &replica);
        eprintln!("server + replica know {} hashes", known.len());
        let mut tally: std::collections::BTreeMap<&str, usize> = Default::default();
        for (path, md5, stage) in parsed {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let stage = match stage {
                Stage::Parsed(p) if md5.as_ref().is_some_and(|m| known.contains(m)) => Stage::Done(p, Sent::AlreadyOnServer),
                Stage::Parsed(p) => {
                    let sent = send_one(&server, &path);
                    if matches!(sent, Sent::Saved(_) | Sent::SameContent) {
                        known.extend(md5);
                    }
                    Stage::Done(p, sent)
                }
                other => other,
            };
            let p = stage.preview().map(|p| format!("{} {} rows, {} new", p.source, p.rows, p.new)).unwrap_or_default();
            eprintln!("{name}: {} {p} {:?}", stage.label(), match &stage { Stage::Done(_, s) => format!("{s:?}"), Stage::Unsupported(w) | Stage::Error(w) => w.clone(), _ => String::new() });
            *tally.entry(stage.label()).or_default() += 1;
        }
        eprintln!("{tally:?}");
        assert_eq!(std::fs::read(&replica).unwrap(), before, "the replica is only read");
    }
}
