//! What the server has (the bundle's `capabilities` section, also GET
//! /api/capabilities): the data sources and the features they make
//! possible. Pages and sections ask it before they show themselves.
//!
//! A bundle from an older server has no such section: then every feature
//! and every source counts as present, so an upgrade of the app never
//! hides anything the old server had.

use serde::Deserialize;
use serde_json::Value;

use crate::Bundle;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct CapSource {
    pub key: String,
    /// bank, card, fund_units, crypto_wallet, exchange, broker
    pub kind: String,
    /// banks, cards, funds, crypto, broker
    pub group: String,
    pub label: String,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub has_data: bool,
    #[serde(default)]
    pub rows: i64,
    pub first: Option<String>,
    pub last: Option<String>,
    /// unix ms
    pub last_sync: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    /// false: the server sent none (everything on)
    pub known: bool,
    pub features: std::collections::BTreeMap<String, bool>,
    pub sources: Vec<CapSource>,
    /// "anser-server 0.1.0"
    pub server: Option<String>,
}

impl Default for Capabilities {
    /// Nothing known: everything on.
    fn default() -> Self {
        Capabilities { known: false, features: Default::default(), sources: Vec::new(), server: None }
    }
}

/// The features a server reports (engine views::capabilities::FEATURES).
pub const FEATURES: [&str; 8] = ["banks", "spending", "crypto", "broker", "fund_proxy", "receivables", "travel", "cashflow"];

impl Capabilities {
    pub fn from_value(v: &Value) -> Self {
        let Some(obj) = v.as_object().filter(|o| o.contains_key("features")) else { return Self::default() };
        let features = obj.get("features").and_then(Value::as_object)
            .map(|f| f.iter().filter_map(|(k, v)| Some((k.clone(), v.as_bool()?))).collect())
            .unwrap_or_default();
        let sources = obj.get("sources").and_then(|s| serde_json::from_value::<Vec<CapSource>>(s.clone()).ok()).unwrap_or_default();
        let server = obj.get("server").map(|s| format!("{} {}", s["kind"].as_str().unwrap_or("?"), s["version"].as_str().unwrap_or("?")));
        Capabilities { known: true, features, sources, server }
    }

    /// Just the version of `server` ("0.1.0" of "anser-server 0.1.0"); None
    /// when the server sent no capabilities (an older build) or no version.
    pub fn server_version(&self) -> Option<&str> {
        self.server.as_deref()?.rsplit_once(' ').map(|(_, v)| v).filter(|v| *v != "?")
    }

    pub fn of(b: &Bundle) -> Self {
        Self::from_value(b.at(&["capabilities"]))
    }

    /// Is `feature` on? Unknown capabilities, or a feature the server
    /// doesn't report: yes.
    pub fn has(&self, feature: &str) -> bool {
        !self.known || self.features.get(feature).copied().unwrap_or(true)
    }

    /// Does the source `key` have data? Unknown, or a source the server
    /// doesn't list: yes.
    pub fn source(&self, key: &str) -> bool {
        !self.known || self.sources.iter().find(|s| s.key == key).is_none_or(|s| s.has_data)
    }

    /// Any investment at all: a broker, a fund, crypto.
    pub fn investments(&self) -> bool {
        self.has("broker") || self.has("fund_proxy") || self.has("crypto")
    }

    /// The Upload page's *Paste a message* (an HSBC FlexInvest confirmation
    /// SMS): for a user whose FlexInvest SMS source has data, or who has an
    /// HSBC account or card with data (FlexInvest is HSBC Hong Kong's, so
    /// its customer may start one). Unknown capabilities: shown.
    pub fn paste_sms(&self) -> bool {
        !self.known || self.sources.iter().any(|s| s.has_data && matches!(s.key.as_str(), "hsbc_sms" | "hsbc" | "hsbc_card"))
    }

    /// The `acct:` example in the search box's placeholder: the key of one
    /// of the user's own bank, card or wallet sources with data, else a
    /// neutral one (nothing loaded yet, an older server).
    pub fn search_example(&self) -> String {
        let own = self.known.then(|| self.sources.iter()
            .find(|s| s.has_data && matches!(s.kind.as_str(), "bank" | "card" | "crypto_wallet") && !s.key.is_empty()))
            .flatten();
        format!("acct:{}", own.map_or("savings", |s| s.key.as_str()))
    }

    /// The Upload drop zone's line on what to drop: the export formats of
    /// the user's own file sources, then the way in for any other bank.
    /// No file source with data yet: a generic line. Unknown capabilities
    /// (an older server): every built-in format.
    pub fn upload_hint(&self) -> String {
        const MAP: &str = "once its columns are mapped";
        if !self.known {
            return format!("N26, HSBC account and credit card (CSV or PDF statement), Mox (monthly PDF), Revolut, Trezor, HSBC SMS backup (.xml), \
                            and any bank's CSV or Excel export {MAP}.");
        }
        let mut have: Vec<String> = Vec::new();
        for s in self.sources.iter().filter(|s| s.has_data) {
            let f = match s.key.as_str() {
                "n26" => "N26 (CSV)".to_string(),
                "hsbc" => "HSBC account (CSV)".into(),
                "hsbc_card" => "HSBC credit card (CSV or PDF statement)".into(),
                "hsbc_sms" => "HSBC SMS backup (.xml)".into(),
                "mox" => "Mox (monthly PDF)".into(),
                k if k.starts_with("revolut") => "Revolut (CSV)".into(),
                "trezor" => "Trezor (CSV)".into(),
                // API sources (exchanges, the broker) aren't uploaded
                _ if matches!(s.kind.as_str(), "exchange" | "broker") => continue,
                // a format mapped from a bank's own export
                _ => s.label.clone(),
            };
            if !have.contains(&f) {
                have.push(f);
            }
        }
        if have.is_empty() {
            return format!("Bank, card or crypto wallet exports: CSV, Excel or a PDF statement. A bank the app doesn't know yet is read {MAP}.");
        }
        format!("Your sources: {}. Any other bank's CSV or Excel export is read {MAP}.", have.join(", "))
    }
}

/// The Doctor's Sources row, shared by the desktop Doctor and the web
/// build's stand-in.
#[derive(Debug, Clone, PartialEq)]
pub struct SourcesReport {
    /// 0 ok, 1 warn, 2 nothing yet, 3 unknown (an older server)
    pub level: u8,
    pub summary: String,
    /// (label, state) per source
    pub rows: Vec<(String, String)>,
}

fn ago_days(now_ms: i64, ms: i64) -> String {
    let d = (now_ms - ms).max(0) / 86_400_000;
    match d {
        0 => "today".into(),
        1 => "yesterday".into(),
        d => format!("{d} d ago"),
    }
}

impl Capabilities {
    /// Which sources have data, which API source is configured but empty.
    pub fn report(&self, now_ms: i64) -> SourcesReport {
        if !self.known {
            return SourcesReport { level: 3, summary: "The server doesn't report its sources (an older build): deploy it to see them here.".into(),
                                   rows: Vec::new() };
        }
        let mut rows = Vec::new();
        let mut empty_api = Vec::new();
        for s in &self.sources {
            let api = matches!(s.kind.as_str(), "exchange" | "broker");
            let state = match (s.has_data, api, s.configured) {
                (true, _, _) => format!("{} rows{}{}", s.rows,
                    s.last.as_deref().map(|l| format!(", newest {l}")).unwrap_or_default(),
                    s.last_sync.map(|t| format!(", synced {}", ago_days(now_ms, t))).unwrap_or_default()),
                (false, true, true) => {
                    empty_api.push(s.label.clone());
                    "keys set, no data yet".into()
                }
                (false, true, false) => "not configured".into(),
                (false, false, _) => "no files uploaded".into(),
            };
            // a source never used isn't worth a line, unless it's an API one
            // whose keys are set (an unused exchange or broker: none)
            if s.has_data || (api && s.configured) {
                rows.push((s.label.clone(), state));
            }
        }
        let with_data = self.sources.iter().filter(|s| s.has_data).count();
        let (level, summary) = if with_data == 0 {
            (2, "No data yet: upload bank exports, or set API keys in the server's .env.".to_string())
        } else if !empty_api.is_empty() {
            (1, format!("{} configured but without data: run a sync, or see its row above.", empty_api.join(", ")))
        } else {
            (0, format!("{with_data} source{} with data.", if with_data == 1 { "" } else { "s" }))
        };
        SourcesReport { level, summary, rows }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_means_everything_on() {
        let c = Capabilities::of(&Bundle(json!({"version": 1})));
        assert!(!c.known && c.has("broker") && c.source("trezor") && c.investments());
    }

    #[test]
    fn reads_the_section() {
        let b = Bundle(json!({"capabilities": {
            "server": {"kind": "anser-server", "version": "0.1.0"},
            "sources": [{"key": "ibkr", "kind": "broker", "group": "broker", "label": "Interactive Brokers", "configured": false,
                         "has_data": false, "rows": 0, "first": null, "last": null, "last_sync": null},
                        {"key": "hsbc", "kind": "bank", "group": "banks", "label": "HSBC", "configured": true,
                         "has_data": true, "rows": 3, "first": "2026-01-01", "last": "2026-02-01", "last_sync": 5}],
            "features": {"banks": true, "broker": false, "crypto": false, "fund_proxy": false}}}));
        let c = Capabilities::of(&b);
        assert!(c.known && c.has("banks") && !c.has("broker") && !c.investments());
        assert!(c.has("travel"), "a feature the server doesn't report stays on");
        assert!(!c.source("ibkr") && c.source("hsbc") && c.source("nonexistent"));
        assert_eq!(c.server.as_deref(), Some("anser-server 0.1.0"));
    assert_eq!(c.server_version(), Some("0.1.0"));
    assert_eq!(Capabilities::default().server_version(), None);
        assert_eq!(c.sources[1].last.as_deref(), Some("2026-02-01"));
    }

    #[test]
    fn the_sources_report() {
        assert_eq!(Capabilities::default().report(0).level, 3);
        let src = |key: &str, kind: &str, configured: bool, rows: i64| CapSource {
            key: key.into(), kind: kind.into(), label: key.into(), configured, has_data: rows > 0, rows,
            last: (rows > 0).then(|| "2026-10-01".into()), last_sync: (rows > 0).then_some(0), ..Default::default() };
        let mut c = Capabilities { known: true, features: Default::default(), server: None,
            sources: vec![src("hsbc", "bank", true, 10), src("n26", "bank", true, 0), src("okx", "exchange", true, 0), src("ibkr", "broker", false, 0)] };
        let r = c.report(86_400_000 * 2);
        assert_eq!(r.level, 1);
        assert!(r.summary.starts_with("okx configured"));
        let labels: Vec<&str> = r.rows.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["hsbc", "okx"], "an unused file source, and an API one without keys, are left out");
        assert!(r.rows[0].1.contains("10 rows") && r.rows[0].1.contains("2 d ago"));
        c.sources.truncate(2);
        assert_eq!(c.report(0).level, 0);
        c.sources[0].has_data = false;
        assert_eq!(c.report(0).level, 2);
    }

    fn with(sources: &[(&str, &str, bool)]) -> Capabilities {
        Capabilities { known: true, features: Default::default(), server: None,
            sources: sources.iter().map(|(key, kind, has_data)| CapSource {
                key: key.to_string(), kind: kind.to_string(), label: key.to_uppercase(), has_data: *has_data, ..Default::default() }).collect() }
    }

    #[test]
    fn the_paste_card_needs_an_hsbc_source() {
        assert!(Capabilities::default().paste_sms(), "an older server: shown");
        assert!(!with(&[]).paste_sms(), "nothing yet: hidden");
        assert!(!with(&[("n26", "bank", true), ("hsbc_sms", "fund_units", false), ("binance", "exchange", true)]).paste_sms());
        assert!(with(&[("hsbc_sms", "fund_units", true)]).paste_sms());
        assert!(with(&[("n26", "bank", true), ("hsbc", "bank", true)]).paste_sms(), "an HSBC account may start FlexInvest");
        assert!(with(&[("hsbc_card", "card", true)]).paste_sms());
    }

    #[test]
    fn the_search_example_is_one_of_the_users_accounts() {
        assert_eq!(Capabilities::default().search_example(), "acct:savings", "unknown: neutral");
        assert_eq!(with(&[]).search_example(), "acct:savings", "no sources: neutral");
        assert_eq!(with(&[("binance", "exchange", true), ("n26", "bank", false), ("revolut", "bank", true)]).search_example(), "acct:revolut",
            "the first bank with data; an exchange or an empty bank is skipped");
    }

    #[test]
    fn the_upload_hint_lists_only_the_users_formats() {
        let all = Capabilities::default().upload_hint();
        assert!(all.contains("N26") && all.contains("SMS backup") && all.contains("Mox"), "{all}");
        let none = with(&[("n26", "bank", false), ("hsbc_sms", "fund_units", false)]).upload_hint();
        assert!(!none.contains("N26") && !none.contains("HSBC") && none.contains("columns are mapped"), "{none}");
        let some = with(&[("n26", "bank", true), ("revolut_new", "bank", true), ("revolut", "crypto_wallet", true),
                          ("hsbc_sms", "fund_units", false), ("okx", "exchange", true), ("dbs", "bank", true)]).upload_hint();
        assert_eq!(some, "Your sources: N26 (CSV), Revolut (CSV), DBS. Any other bank's CSV or Excel export is read once its columns are mapped.");
    }
}
