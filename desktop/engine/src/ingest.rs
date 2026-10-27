//! Discover transactions/*.{csv,xml,pdf,xlsx,xls}, classify by source, parse only new
//! files, load into sqlite.
//!
//! Idempotent: a file already recorded in ingested_files (by path+hash) is
//! skipped entirely on re-runs. Below that, row-level dedup happens via the
//! UNIQUE constraints (bank_transactions.raw_hash, crypto_events' natural
//! key) plus the near-duplicate guards, so overlapping/duplicate export files
//! are harmless even on first ingest.
//!
//! Each new file is parsed on its own rather than batching all of a source's
//! new files into one call, so every resulting row can be tagged with its
//! real source_file.
//!
//! CARDINAL RULE: `natural_key` and `raw_hash` never change for a row already
//! stored (sources::textfmt holds the exact forms).
//! txn_overrides are keyed on (source, raw_hash); a hash that drifts by one
//! character silently orphans the owner's hand overrides and re-inserts
//! history on the next overlapping export. Never change the key format.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use md5::{Digest, Md5};
use rusqlite::Connection;

use crate::db::{self, NewBankTxn};
use crate::sources::{self, textfmt, BankRow, Kind, Parsed, SourceSpec};

/// Per-source result of one ingest run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceSummary {
    pub files_new: usize,
    pub rows_added: usize,
    /// rows dropped as the same transaction as a stored one (drifted
    /// balance, renamed merchant, shifted date)
    pub near_dupes: usize,
    /// rows dropped as the twin of a legacy-export row (see LEGACY_TWINS)
    pub legacy_twins: usize,
    /// files that could not be read or parsed: logged, left out of
    /// ingested_files (tried again next run), the rest still ingested
    pub files_failed: usize,
}

/// None -> "", else the float's shortest round-trip text (textfmt::float_repr).
fn s(v: Option<f64>) -> String {
    v.map(textfmt::float_repr).unwrap_or_default()
}

/// The row's identity: `source|account|currency|txn_date|amount|balance|
/// description[|dedup]`, floats as textfmt::float_repr writes them.
pub fn natural_key(source: &str, row: &BankRow) -> String {
    let mut parts = vec![
        source.to_string(),
        row.account.clone(),
        row.currency.clone(),
        row.txn_date.clone(),
        s(row.amount),
        s(row.balance),
        row.description.clone(),
    ];
    // Optional per-source discriminator (a parser's `dedup` column, e.g.
    // N26's Value Date + Payment Reference for balance-less exports).
    // Appended ONLY when present so every source without one keeps its
    // historical hashes -- changing the key format for existing sources would
    // make the next overlapping export re-insert its entire history.
    if !row.dedup.trim_matches('|').is_empty() {
        parts.push(row.dedup.clone());
    }
    parts.join("|")
}

pub fn raw_hash(source: &str, row: &BankRow) -> String {
    hex::encode(Md5::digest(natural_key(source, row).as_bytes()))
}

const NEAR_DUP_DAYS: i64 = 5;

/// Sources whose older export format is also stored under another source
/// key: (source, the legacy sources it overlaps). Revolut's current export
/// reaches back over the months the legacy GBP statement already holds.
const LEGACY_TWINS: &[(&str, &[&str])] = &[("revolut_new", &["revolut_old_gbp"])];

fn cents(x: f64) -> i64 {
    (x * 100.0).round() as i64
}

/// Drop incoming rows that a legacy source already holds: same currency,
/// same amount to the cent, date within a day, one incoming row per legacy
/// row (exact dates claim first, then +/-1 day). Only dates up to the legacy
/// source's last row in that currency are considered, so everything after
/// it is kept, as is every row with no twin. Needs the legacy rows stored
/// first: a legacy file ingested after the new export cannot retract rows.
pub fn skip_legacy_twins(conn: &Connection, source: &str, rows: Vec<NewBankTxn>) -> Result<(Vec<NewBankTxn>, usize)> {
    let Some((_, legacy)) = LEGACY_TWINS.iter().find(|(k, _)| *k == source) else {
        return Ok((rows, 0));
    };
    // per currency: (date, cents, claimed), and the last legacy date
    let mut held: HashMap<String, Vec<(NaiveDate, i64, bool)>> = HashMap::new();
    let mut st = conn.prepare("SELECT currency, txn_date, amount FROM bank_transactions WHERE source=? ORDER BY id")?;
    for key in *legacy {
        let mut q = st.query([key])?;
        while let Some(r) = q.next()? {
            let (ccy, date, amt): (String, String, Option<f64>) = (r.get(0)?, r.get(1)?, r.get(2)?);
            if let (Some(d), Some(a)) = (day(&date), amt) {
                held.entry(ccy).or_default().push((d, cents(a), false));
            }
        }
    }
    if held.is_empty() {
        return Ok((rows, 0));
    }
    let last: HashMap<String, NaiveDate> =
        held.iter().filter_map(|(c, v)| v.iter().map(|x| x.0).max().map(|d| (c.clone(), d))).collect();
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| (rows[i].txn_date.clone(), i));
    let mut twin = vec![false; rows.len()];
    for max_gap in [0, 1] {
        for &i in &order {
            let r = &rows[i];
            let (Some(amt), Some(d), Some(l), Some(pool)) =
                (r.amount, day(&r.txn_date), last.get(&r.currency), held.get_mut(&r.currency))
            else {
                continue;
            };
            if twin[i] || d > *l {
                continue;
            }
            let c = cents(amt);
            let hit = pool
                .iter_mut()
                .filter(|x| !x.2 && x.1 == c && (x.0 - d).num_days().abs() <= max_gap)
                .min_by_key(|x| ((x.0 - d).num_days().abs(), x.0));
            if let Some(x) = hit {
                x.2 = true;
                twin[i] = true;
            }
        }
    }
    let skipped = twin.iter().filter(|t| **t).count();
    let kept = rows.into_iter().zip(twin).filter(|(_, t)| !t).map(|(r, _)| r).collect();
    Ok((kept, skipped))
}

/// Round to cents for the guard keys: correctly rounded half-even on
/// the exact binary value, via the shortest repr of the formatted result.
fn round2(x: f64) -> u64 {
    let r: f64 = format!("{x:.2}").parse().unwrap_or(x);
    let r = if r == 0.0 { 0.0 } else { r };
    r.to_bits()
}

fn day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(textfmt::prefix_chars(s, 10), "%Y-%m-%d").ok()
}

/// Drop rows that are the SAME transaction as one already stored, in the
/// cases where raw_hash cannot tell -- because two exports of one account
/// disagree on a field that feeds the hash:
///
///   * balance drift -- same date/amount/description, different running
///     balance, because the exports ordered a same-day group differently
///     (HSBC Feb-2025: a 0.09 rebate landing before vs after a purchase
///     shifted every subsequent balance).
///   * merchant rename -- Revolut rewrites payee strings between exports.
///   * date shift -- an export pulled in another timezone moves a
///     near-midnight transaction by a day, sometimes into another month.
///
/// Deliberately NOT fixed by widening natural_key: that rewrites every
/// stored hash.
///
/// Every stored row can vouch for at most ONE incoming row, whichever check
/// matches it. Exact raw_hash matches claim their stored row first: they ARE
/// that row (INSERT OR IGNORE drops them anyway), so the near-duplicate
/// guards must not spend that stored row on a different, genuinely new row.
/// (Without this, an HSBC export -- newest first -- that adds a second
/// identical same-day fare lost it: the new fare matched guard 1 against the
/// stored first fare, then the re-exported first fare matched guard 2
/// against the same stored row.)
///
/// Guard 1 (same account/currency/date/amount/description) is
/// multiplicity-aware, because genuinely repeated charges exist and differ
/// ONLY by balance -- three OVpay -3.00 fares on one day are three real rows.
/// Guard 2 keys on the running balance, which pins a row's position in the
/// chain and so survives a rename or a date shift; it is fenced to
/// +/-NEAR_DUP_DAYS so an unrelated old transaction that happens to share an
/// amount and balance can never suppress a genuinely new one.
///
/// Balance-less sources (N26, the card) are left entirely to raw_hash and
/// their dedup discriminator: without a balance neither guard has the
/// evidence it needs, and guard 1 alone would collapse distinct references.
pub fn reject_known_duplicates(
    conn: &Connection,
    source: &str,
    rows: Vec<NewBankTxn>,
) -> Result<(Vec<NewBankTxn>, usize)> {
    type K1 = (String, String, String, u64, String);
    type K2 = (String, String, u64, u64);
    let mut st = conn.prepare(
        "SELECT account, currency, txn_date, amount, balance, description, raw_hash \
         FROM bank_transactions WHERE source=? ORDER BY id",
    )?;
    let mut stored_date: Vec<String> = Vec::new();
    let mut by_hash: HashMap<String, usize> = HashMap::new();
    let mut by_k1: HashMap<K1, Vec<usize>> = HashMap::new();
    let mut by_k2: HashMap<K2, Vec<usize>> = HashMap::new();
    let mut q = st.query([source])?;
    while let Some(r) = q.next()? {
        let i = stored_date.len();
        let (account, currency, date): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
        let amt = round2(r.get::<_, Option<f64>>(3)?.unwrap_or(0.0));
        let bal: Option<f64> = r.get(4)?;
        let desc: String = r.get(5)?;
        by_hash.entry(r.get(6)?).or_insert(i);
        by_k1.entry((account.clone(), currency.clone(), date.clone(), amt, desc)).or_default().push(i);
        if let Some(b) = bal {
            by_k2.entry((account, currency, amt, round2(b))).or_default().push(i);
        }
        stored_date.push(date);
    }
    let mut used = vec![false; stored_date.len()];

    // pass 1: exact matches claim their own stored row
    let mut exact = vec![false; rows.len()];
    for (n, row) in rows.iter().enumerate() {
        if row.balance.is_none() {
            continue;
        }
        if let Some(&i) = by_hash.get(&row.raw_hash) {
            if !used[i] {
                used[i] = true;
                exact[n] = true;
            }
        }
    }

    let mut kept = Vec::with_capacity(rows.len());
    let mut skipped = 0;
    for (n, row) in rows.into_iter().enumerate() {
        let Some(bal) = row.balance else {
            kept.push(row);
            continue;
        };
        if exact[n] {
            kept.push(row); // INSERT OR IGNORE drops it; not a near-duplicate
            continue;
        }
        let amt = round2(row.amount.unwrap_or(0.0));
        let k1: K1 = (row.account.clone(), row.currency.clone(), row.txn_date.clone(), amt, row.description.clone());
        if let Some(&i) = by_k1.get(&k1).and_then(|v| v.iter().find(|&&i| !used[i])) {
            used[i] = true; // this stored row is now accounted for
            skipped += 1;
            continue;
        }
        let k2: K2 = (row.account.clone(), row.currency.clone(), amt, round2(bal));
        let mine = day(&row.txn_date);
        let hit = by_k2.get(&k2).and_then(|v| {
            v.iter().copied().find(|&i| {
                !used[i]
                    && match (mine, day(&stored_date[i])) {
                        (Some(a), Some(b)) => (b - a).num_days().abs() <= NEAR_DUP_DAYS,
                        _ => false,
                    }
            })
        });
        if let Some(i) = hit {
            used[i] = true;
            skipped += 1;
            continue;
        }
        kept.push(row);
    }
    Ok((kept, skipped))
}

/// What loading one file's rows did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileLoad {
    /// rows parsed from the file
    pub rows: usize,
    /// rows that were new to the store
    pub added: usize,
    /// rows dropped by the near-duplicate guards (counted in rows - added)
    pub near_dupes: usize,
    /// rows dropped as the twin of a legacy-export row (not in `added`)
    pub legacy_twins: usize,
}

/// Load one file's parsed rows through every dedupe layer: raw_hash /
/// natural-key UNIQUEs, plus the near-duplicate guards for bank rows. This
/// is exactly what ingest does per file (minus ingested_files bookkeeping),
/// so a dry run on a copy of the store (the desktop app's upload preview)
/// counts the same new rows a real ingest would add.
pub fn load_parsed(conn: &Connection, spec: &SourceSpec, parsed: Parsed, file_name: &str, now_ms: i64) -> Result<FileLoad> {
    let key = spec.key;
    let mut load = FileLoad { rows: parsed.len(), ..FileLoad::default() };
    load.added = match (spec.kind, parsed) {
        (Kind::Bank, Parsed::Bank(rows)) => {
            let rows: Vec<NewBankTxn> = rows
                .iter()
                .map(|r| NewBankTxn {
                    source: key.to_string(),
                    account: r.account.clone(),
                    currency: r.currency.clone(),
                    txn_date: r.txn_date.clone(),
                    amount: r.amount,
                    balance: r.balance,
                    description: r.description.clone(),
                    raw_hash: raw_hash(key, r),
                    source_file: file_name.to_string(),
                    ingested_at: now_ms,
                })
                .collect();
            let (rows, twins) = skip_legacy_twins(conn, key, rows)?;
            load.legacy_twins = twins;
            if rows.is_empty() {
                0
            } else {
                // second dedup layer, above the raw_hash UNIQUE constraint:
                // the same transaction re-exported with a drifted balance, a
                // renamed merchant, or a timezone-shifted date
                let (rows, near) = reject_known_duplicates(conn, key, rows)?;
                load.near_dupes = near;
                if rows.is_empty() {
                    0
                } else {
                    db::insert_bank_transactions(conn, &rows)?
                }
            }
        }
        // real unit-trust executions (units + NAV) -- deliberately NOT bank
        // transactions: the cash legs of these purchases already arrive via
        // the HSBC CSV export. PK on the confirmation ref dedups overlapping
        // SMS backups.
        (Kind::FundUnits, Parsed::FundUnits(rows)) => db::upsert_fund_subscriptions(conn, &rows)?,
        (Kind::CryptoWallet, Parsed::Crypto(rows)) => {
            let events: Vec<db::Event> = rows
                .into_iter()
                .map(|r| db::Event { ts: r.ts, asset: r.asset, delta: r.delta, usd: r.usd, kind: r.kind, detail: r.detail })
                .collect();
            db::insert_crypto_events(conn, key, &events)?
        }
        (kind, _) => anyhow::bail!("{key}: parser output does not match kind {kind:?}"),
    };
    Ok(load)
}

fn md5_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(hex::encode(Md5::digest(&bytes)))
}

/// The candidate files: `*.csv`, `*.xml`, `*.pdf`, `*.xlsx`, `*.xls`
/// (case-sensitive extensions; hidden files included), sorted.
pub fn candidate_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            [".csv", ".xml", ".pdf", ".xlsx", ".xls"].iter().any(|e| n.ends_with(e))
                && e.path().is_file()
        })
        .map(|e| dir.join(e.file_name()))
        .collect();
    out.sort();
    out
}

/// Ingest every new/changed file in the configured transactions dir.
pub fn run_ingest(conn: &Connection) -> Result<Vec<(String, SourceSummary)>> {
    run_ingest_dir(conn, &crate::config::transactions_dir())
}

/// Ingest every new/changed file in `dir`. Returns per-source summaries in
/// processing order (sources with no fresh files are absent).
pub fn run_ingest_dir(conn: &Connection, dir: &Path) -> Result<Vec<(String, SourceSummary)>> {
    let paths = candidate_files(dir);

    // Stat fast path: a file whose exact (path, size, mtime) is already in
    // ingested_files is skipped WITHOUT being opened. Auto-ingest runs on
    // every dashboard request, and everything below reads file contents --
    // classify() sniffs each header (pdftotext for PDFs), hashing reads the
    // whole file. On a warm tree this loop is one stat + one indexed SELECT
    // per file.
    let mut stats: HashMap<PathBuf, (i64, i64)> = HashMap::new();
    let mut fresh = Vec::new();
    for f in paths {
        let md = match std::fs::metadata(&f) {
            Ok(md) => md,
            Err(e) => {
                eprintln!("ingest: skipping {}: {e}", f.display());
                continue;
            }
        };
        let st = (md.size() as i64, md.mtime() * 1_000_000_000 + md.mtime_nsec());
        if !db::file_stat_known(conn, &textfmt::path_str(&f), st.0, st.1)? {
            fresh.push(f.clone());
        }
        stats.insert(f, st);
    }
    if fresh.is_empty() {
        return Ok(vec![]);
    }

    let buckets = sources::classify(&fresh);
    let mut summary: Vec<(String, SourceSummary)> = Vec::new();
    let now_ms = db::now_ms();

    for (source, files) in buckets {
        let key = source.spec().key;
        let mut sum = SourceSummary::default();
        for f in files {
            let (size, mtime_ns) = stats[&f];
            let path = textfmt::path_str(&f);
            let file_hash = match md5_file(&f) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("ingest: skipping {path}: {e:#}");
                    sum.files_failed += 1;
                    continue;
                }
            };
            if db::file_already_ingested(conn, &path, &file_hash)? {
                // same content, unrecorded stat (touched file, or a row from
                // before stat tracking): store the fingerprint so the fast
                // path catches it from now on -- this hash was its last.
                db::update_ingested_stat(conn, &path, &file_hash, size, mtime_ns)?;
                continue;
            }
            let parsed = match source.parse(std::slice::from_ref(&f)).with_context(|| format!("parsing {path}")) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ingest: skipping {path}: {e:#}");
                    sum.files_failed += 1;
                    continue;
                }
            };
            sum.files_new += 1;
            let row_count = parsed.len();
            let file_name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let load = load_parsed(conn, source.spec(), parsed, &file_name, now_ms)?;
            sum.near_dupes += load.near_dupes;
            sum.legacy_twins += load.legacy_twins;
            let file_added = load.added;
            sum.rows_added += file_added;
            // rows_added per file makes dedup visible on the Upload page --
            // "parsed 100, added 0" says overlapping export; a silent drop
            // of genuinely new rows would show up here too.
            db::record_ingested_file(
                conn,
                &path,
                key,
                &file_hash,
                row_count as i64,
                Some(size),
                Some(mtime_ns),
                Some(file_added as i64),
            )?;
        }
        summary.push((key.to_string(), sum));
    }

    // keep categories in sync with whatever rows now exist (cheap, idempotent)
    if summary.iter().any(|(_, s)| s.rows_added > 0) {
        crate::categorize::categorize_all(conn)?;
    }
    Ok(summary)
}
