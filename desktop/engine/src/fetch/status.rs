//! Per-source fetch outcomes, for the Doctor (GET /api/sources): when each
//! API source last fetched, the last error (scrubbed), and for IBKR which
//! Flex sections the last statement carried. Kept in
//! `data/fetch_status.json`, beside the db rather than in it, so the schema
//! stays SCHEMA's.
//!
//! [`super::fetch_source`] records every attempt, so the CLI, the cron sync
//! and the server's background sync all land here. Writes go through a temp
//! file and a rename; a lost race between two writers loses one outcome,
//! never the file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config;

/// Which Flex sections a statement carried (counts of rows).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlexSections {
    pub trades: usize,
    pub open_positions: usize,
    pub cash_report: usize,
    /// daily statements with NAV (EquitySummary or CashReport + positions)
    pub nav: usize,
    pub cash_transactions: usize,
    pub dividends: usize,
    pub withholding: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceStatus {
    /// unix ms of the last attempt, and of the last success
    pub last_attempt_ms: Option<i64>,
    pub last_ok_ms: Option<i64>,
    /// the last attempt's error, scrubbed; None after a success
    pub last_error: Option<String>,
    /// IBKR: the sections of the last statement
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<FlexSections>,
    /// the last [`RUNS_KEPT`] attempts, oldest first: one record per source
    /// per sync
    #[serde(default)]
    pub runs: Vec<Run>,
}

/// One fetch attempt of one source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub started_ms: i64,
    pub ms: u64,
    pub ok: bool,
}

pub const RUNS_KEPT: usize = 20;

pub fn path() -> PathBuf {
    config::data_dir().join("fetch_status.json")
}

pub fn load() -> BTreeMap<String, SourceStatus> {
    load_from(&path())
}

fn load_from(p: &Path) -> BTreeMap<String, SourceStatus> {
    std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn update(p: &Path, key: &str, f: impl FnOnce(&mut SourceStatus)) {
    let mut all = load_from(p);
    f(all.entry(key.to_string()).or_default());
    let tmp = p.with_extension("json.tmp");
    if let Ok(body) = serde_json::to_vec_pretty(&all) {
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, p);
        }
    }
}

/// One fetch attempt of `key`, started at `started_ms`, ended with `result`.
pub fn record(key: &str, started_ms: i64, result: &anyhow::Result<()>) {
    record_at(&path(), key, started_ms, result, &secrets());
}

fn record_at(p: &Path, key: &str, started_ms: i64, result: &anyhow::Result<()>, secrets: &[String]) {
    let now = chrono::Utc::now().timestamp_millis();
    update(p, key, |s| {
        s.last_attempt_ms = Some(now);
        s.runs.push(Run { started_ms, ms: (now - started_ms).max(0) as u64, ok: result.is_ok() });
        if s.runs.len() > RUNS_KEPT {
            s.runs.drain(..s.runs.len() - RUNS_KEPT);
        }
        match result {
            Ok(()) => {
                s.last_ok_ms = Some(now);
                s.last_error = None;
            }
            Err(e) => s.last_error = Some(scrub(&format!("{e:#}"), secrets)),
        }
    });
}

pub fn record_sections(key: &str, sections: FlexSections) {
    update(&path(), key, |s| s.sections = Some(sections));
}

/// Every configured secret value (to blank out of error text).
fn secrets() -> Vec<String> {
    ["IBKR_FLEX_TOKEN", "IBKR_FLEX_QUERY_ID", "BINANCE_API_KEY", "BINANCE_API_SECRET", "OKX_API_KEY", "OKX_API_SECRET", "OKX_PASSPHRASE"]
        .iter()
        .filter_map(|k| config::env(k))
        .filter(|v| v.len() >= 4)
        .collect()
}

/// Error text safe to show anywhere: no URLs (they carry tokens and signed
/// queries), no known secret, no long key-like runs; one line, 200 chars.
pub fn scrub(msg: &str, secrets: &[String]) -> String {
    let mut s = msg.replace(['\n', '\r'], " ");
    for v in secrets {
        s = s.replace(v.as_str(), "***");
    }
    let url = Regex::new(r"(?i)\b[a-z][a-z0-9+.-]*://\S+").unwrap();
    s = url.replace_all(&s, "<url>").into_owned();
    let query = Regex::new(r"(?i)\b(t|q|token|key|secret|signature|sign|passphrase|apikey)=[^\s&]+").unwrap();
    s = query.replace_all(&s, "$1=***").into_owned();
    let long = Regex::new(r"\b[A-Za-z0-9_\-]{24,}\b").unwrap();
    s = long.replace_all(&s, "***").into_owned();
    s.chars().take(200).collect()
}

/// IBKR Flex Web Service error codes (from IBKR's Flex Web Service guide):
/// the code in "error 1012: ..." -> (kind, plain words).
pub fn flex_error(msg: &str) -> Option<(i64, &'static str, &'static str)> {
    let code: i64 = Regex::new(r"error (\d{4})").unwrap().captures(msg)?.get(1)?.as_str().parse().ok()?;
    let (kind, words) = match code {
        1012 => ("token_expired", "The Flex token has expired (they last a year at most)."),
        1015 => ("token_invalid", "The Flex token is invalid: it was regenerated or mistyped."),
        1011 => ("token_invalid", "The Flex Web Service account is inactive."),
        1014 => ("query_invalid", "The Flex query ID is invalid (deleted, or not an Activity query)."),
        1010 => ("query_invalid", "The Flex query is a legacy one IBKR no longer serves; create a new Activity Flex query."),
        1016 => ("query_invalid", "The account in the query is invalid."),
        1013 => ("ip_restricted", "The token is restricted to other IP addresses."),
        1018 => ("throttled", "Too many requests with this token; it clears by itself."),
        1001 | 1003..=1009 | 1019 | 1021 => ("transient", "IBKR couldn't generate the statement right then; the next sync retries."),
        1017 | 1020 => ("transient", "IBKR rejected the request; the next sync retries."),
        _ => ("other", "IBKR Flex returned an error."),
    };
    Some((code, kind, words))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_urls_secrets_and_keys() {
        let secret = "123456789012345678901234".to_string();
        let msg = "Flex SendRequest: GET https://ndcdyn.interactivebrokers.com/AccountManagement/FlexWebService/SendRequest?t=123456789012345678901234&q=99 failed\nline2";
        let s = scrub(msg, &[secret.clone()]);
        assert!(!s.contains(&secret) && !s.contains("https://") && !s.contains('\n'), "{s}");
        assert!(s.contains("<url>"));
        let s = scrub("binance: bad signature=abcdef&apikey=XYZ timestamp", &[]);
        assert!(!s.contains("abcdef") && !s.contains("XYZ"), "{s}");
        assert!(scrub("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123 leaked", &[]).starts_with("***"));
        assert_eq!(scrub("okx: HTTP 401", &[]), "okx: HTTP 401");
    }

    #[test]
    fn flex_codes_in_plain_words() {
        assert_eq!(flex_error("ibkr: Flex SendRequest: error 1012: Token has expired.").map(|e| e.1), Some("token_expired"));
        assert_eq!(flex_error("Flex SendRequest: error 1015: Token is invalid.").map(|e| e.1), Some("token_invalid"));
        assert_eq!(flex_error("Flex GetStatement: error 1019: in progress").map(|e| e.1), Some("transient"));
        assert_eq!(flex_error("Flex SendRequest: error 1014: Query is invalid.").map(|e| e.0), Some(1014));
        assert_eq!(flex_error("HTTP 500"), None);
    }

    #[test]
    fn record_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("finance-fetch-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("fetch_status.json");
        let token = "tok_0123456789abcdef0123456789".to_string();
        record_at(&p, "okx", 1, &Err(anyhow::anyhow!("okx: HTTP 401")), &[]);
        record_at(&p, "ibkr", 1, &Err(anyhow::anyhow!("GET ?t={token} failed with {token}")), &[token.clone()]);
        assert!(!std::fs::read_to_string(&p).unwrap().contains(&token), "a secret reached the file");
        record_at(&p, "ibkr", 1, &Ok(()), &[]);
        for _ in 0..30 {
            record_at(&p, "okx", 2, &Ok(()), &[]);
        }
        update(&p, "ibkr", |s| s.sections = Some(FlexSections { trades: 3, ..Default::default() }));
        let all = load_from(&p);
        assert!(all["okx"].last_error.is_none(), "a later success clears it");
        assert_eq!(all["okx"].runs.len(), RUNS_KEPT);
        assert!(all["okx"].runs.iter().all(|r| r.ok && r.started_ms == 2), "the oldest runs dropped first");
        assert_eq!((all["ibkr"].runs.len(), all["ibkr"].runs[0].ok, all["ibkr"].runs[1].ok), (2, false, true));
        assert!(all["ibkr"].last_ok_ms.is_some() && all["ibkr"].last_error.is_none());
        assert_eq!(all["ibkr"].sections.as_ref().unwrap().trades, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
