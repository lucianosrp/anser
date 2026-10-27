//! The "Supported sources" page's data: the server's source catalog
//! (`GET /api/sources/catalog`) read into one flat list, grouped under the
//! wizard's headings, with what the user already has. UI-free.
//!
//! A catalog from an older server may lack the newer text fields
//! (`format`, `how`, `notes`): the steps then come from `instructions` and
//! the rest stays empty, so the page degrades instead of failing.

use serde_json::Value;

use crate::capabilities::Capabilities;
use crate::onboarding::Group;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub key: String,
    pub label: String,
    /// an API source (keys), else a file source (exports)
    pub api: bool,
    pub group: Group,
    /// bank, card, fund_units, crypto_wallet, exchange, broker
    pub kind: String,
    /// "CSV", "monthly PDF", "Read-only API key"
    pub format: String,
    /// the numbered steps, plain words
    pub how: Vec<String>,
    pub notes: String,
    pub docs_url: Option<String>,
    /// an API source: what its test refuses
    pub refuses: Option<String>,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

/// The sources of a catalog answer: the file sources first, then the API
/// ones, each in the catalog's order. Entries with no key are dropped.
pub fn parse(catalog: &Value) -> Vec<Source> {
    let steps = |e: &Value| {
        let how = strings(&e["how"]);
        if how.is_empty() { strings(&e["instructions"]) } else { how }
    };
    let files = catalog["files"].as_array().into_iter().flatten().map(|e| Source {
        key: text(e, "key"),
        label: text(e, "label"),
        api: false,
        group: Group::of_file(e["kind"].as_str().unwrap_or("")),
        kind: text(e, "kind"),
        format: text(e, "format"),
        how: steps(e),
        notes: text(e, "notes"),
        docs_url: e["docs_url"].as_str().filter(|u| !u.is_empty()).map(String::from),
        refuses: None,
    });
    let api = catalog["api"].as_array().into_iter().flatten().map(|e| Source {
        key: text(e, "key"),
        label: text(e, "label"),
        api: true,
        group: Group::of_api(e["group"].as_str().unwrap_or("")),
        kind: text(e, "kind"),
        format: "Read-only API key".into(),
        how: steps(e),
        notes: String::new(),
        docs_url: e["docs_url"].as_str().filter(|u| !u.is_empty()).map(String::from),
        refuses: e["refuses"].as_str().filter(|r| !r.is_empty()).map(String::from),
    });
    files.chain(api).filter(|s| !s.key.is_empty()).collect()
}

/// The sources under each heading, in the wizard's order; a heading with
/// nothing under it is left out.
pub fn grouped(all: &[Source]) -> Vec<(Group, Vec<&Source>)> {
    Group::ALL
        .into_iter()
        .map(|g| (g, all.iter().filter(|s| s.group == g).collect::<Vec<_>>()))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

/// Does the user already have this source: data for it, or (an API source)
/// a key set up? Unlike [`Capabilities::source`], unknown capabilities
/// (nothing loaded, an older server) say no: a tick has to be earned.
pub fn have(caps: &Capabilities, key: &str) -> bool {
    caps.known && caps.sources.iter().any(|s| s.key == key && (s.has_data || s.configured))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::CapSource;
    use serde_json::json;

    fn catalog() -> Value {
        json!({
            "api": [
                {"key": "binance", "label": "Binance", "kind": "exchange", "group": "crypto", "instructions": ["a", "b"], "refuses": "trading", "docs_url": "https://x"},
                {"key": "ibkr", "label": "Interactive Brokers", "kind": "broker", "group": "broker", "instructions": ["c"], "refuses": "", "docs_url": ""},
            ],
            "files": [
                {"key": "n26", "label": "N26", "kind": "bank", "format": "CSV", "how": ["one", "two"], "instructions": ["one", "two"], "notes": "n", "docs_url": null},
                {"key": "hsbc_card", "label": "HSBC credit card", "kind": "card", "format": "CSV or monthly PDF", "how": ["x"], "instructions": ["x"], "notes": ""},
                {"key": "trezor", "label": "Trezor", "kind": "crypto_wallet", "format": "CSV", "how": ["t"], "instructions": ["t"], "notes": ""},
                {"key": "hsbc_sms", "label": "HSBC SMS", "kind": "fund_units", "format": "XML backup", "how": ["s"], "instructions": ["s"], "notes": ""},
            ],
        })
    }

    #[test]
    fn groups_under_the_wizards_headings_files_before_api() {
        let all = parse(&catalog());
        let g = grouped(&all);
        let names: Vec<(Group, Vec<&str>)> = g.iter().map(|(g, v)| (*g, v.iter().map(|s| s.key.as_str()).collect())).collect();
        assert_eq!(names, vec![
            (Group::Banks, vec!["n26", "hsbc_card"]),
            (Group::Investments, vec!["hsbc_sms", "ibkr"]),
            (Group::Crypto, vec!["trezor", "binance"]),
        ]);
        let binance = all.iter().find(|s| s.key == "binance").unwrap();
        assert!(binance.api && binance.refuses.as_deref() == Some("trading") && binance.docs_url.as_deref() == Some("https://x"));
        let ibkr = all.iter().find(|s| s.key == "ibkr").unwrap();
        assert!(ibkr.refuses.is_none() && ibkr.docs_url.is_none(), "empty strings are no value");
    }

    #[test]
    fn an_empty_group_is_left_out() {
        let all = parse(&json!({"files": [{"key": "n26", "label": "N26", "kind": "bank"}]}));
        assert_eq!(grouped(&all).len(), 1);
    }

    #[test]
    fn an_older_catalog_falls_back_to_instructions() {
        let all = parse(&json!({"files": [{"key": "n26", "label": "N26", "kind": "bank", "instructions": ["old line"]}]}));
        assert_eq!(all[0].how, vec!["old line".to_string()]);
        assert!(all[0].format.is_empty() && all[0].notes.is_empty());
        assert!(parse(&json!({})).is_empty());
    }

    #[test]
    fn a_tick_is_for_data_or_a_key_never_for_unknown_capabilities() {
        let src = |key: &str, has_data, configured| CapSource { key: key.into(), has_data, configured, ..Default::default() };
        let caps = Capabilities {
            known: true,
            sources: vec![src("n26", true, false), src("binance", false, true), src("okx", false, false)],
            ..Default::default()
        };
        assert!(have(&caps, "n26") && have(&caps, "binance"));
        assert!(!have(&caps, "okx") && !have(&caps, "mox"));
        assert!(!have(&Capabilities::default(), "n26"));
    }
}
