//! The source catalog: what a client needs to set a source up, with no
//! state in it. `GET /api/sources/catalog` serves [`catalog`]; Settings →
//! Sources and the onboarding wizard render it.
//!
//! * API sources: the fields (labels, help, which ones are secret), how to
//!   make a read-only key, and the permissions the test refuses.
//! * File sources: every parser in `sources::file_sources()`, with export
//!   instructions where we have them (a source added later gets a generic
//!   line until it has its own).

use serde::Serialize;

use crate::sources::{self, BalanceMethod, Kind};

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    /// shown as a password field and never echoed back
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiEntry {
    pub key: &'static str,
    pub label: &'static str,
    /// exchange, broker
    pub kind: &'static str,
    /// crypto, broker
    pub group: &'static str,
    pub fields: Vec<Field>,
    pub instructions: Vec<&'static str>,
    /// what the test refuses
    pub refuses: &'static str,
    pub docs_url: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub key: &'static str,
    pub label: &'static str,
    /// bank, card, fund_units, crypto_wallet
    pub kind: &'static str,
    /// the export steps, the same list as `how` (kept for older clients)
    pub instructions: Vec<String>,
    /// what the file is: "CSV", "monthly PDF", "XML backup"
    pub format: String,
    /// the numbered export steps, in plain words
    pub how: Vec<String>,
    pub docs_url: Option<String>,
    /// one line: what a file covers and how overlaps behave
    pub notes: String,
    /// the account's country by default (ISO 3166 alpha-2), for a bank
    /// whose export only exists in one country; the user can change it
    /// (Settings → Sources: user_settings.accounts). None: its rows are
    /// placed by their currency.
    pub country: Option<&'static str>,
}

/// The default country of a file source's account (finance_core::base
/// keeps the same map for the pages; a server test checks they agree).
pub fn default_country(key: &str) -> Option<&'static str> {
    match key {
        // Hong Kong statement exports (current account, card, Mox)
        "hsbc" | "hsbc_card" | "mox" => Some("HK"),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub api: Vec<ApiEntry>,
    pub files: Vec<FileEntry>,
}

pub fn api() -> Vec<ApiEntry> {
    vec![
        ApiEntry {
            key: "binance",
            label: "Binance",
            kind: "exchange",
            group: "crypto",
            fields: vec![
                Field { key: "BINANCE_API_KEY", label: "API key", help: "The key Binance shows when you create it.", secret: false },
                Field { key: "BINANCE_API_SECRET", label: "Secret key", help: "Shown once, at creation. Make a new key if you lost it.", secret: true },
            ],
            instructions: vec![
                "Binance: Account, then API Management, then Create API (system generated).",
                "Leave only Enable Reading ticked. Spot trading, margin, futures, withdrawals and transfers must all be off.",
                "Restricting the key to your server's IP address is recommended.",
            ],
            refuses: "any trading, withdrawal, internal or universal transfer, margin, futures or options permission",
            docs_url: "https://www.binance.com/en/support/faq/360002502072",
        },
        ApiEntry {
            key: "okx",
            label: "OKX",
            kind: "exchange",
            group: "crypto",
            fields: vec![
                Field { key: "OKX_API_KEY", label: "API key", help: "From OKX's API page.", secret: false },
                Field { key: "OKX_API_SECRET", label: "Secret key", help: "Shown once, at creation.", secret: true },
                Field { key: "OKX_PASSPHRASE", label: "Passphrase", help: "The passphrase you chose for this key.", secret: true },
            ],
            instructions: vec![
                "OKX: Profile, then API keys, then Create API key (for the API trading purpose).",
                "Permissions: Read only. Do not tick Trade or Withdraw.",
                "Set a passphrase; you'll need it here.",
            ],
            refuses: "a key whose permissions are anything but read_only",
            docs_url: "https://www.okx.com/help/how-can-i-do-api-trading",
        },
        ApiEntry {
            key: "ibkr",
            label: "Interactive Brokers",
            kind: "broker",
            group: "broker",
            fields: vec![
                Field { key: "IBKR_FLEX_TOKEN", label: "Flex token", help: "Flex Web Service token (reporting only: it can't trade). Valid up to a year.", secret: true },
                Field { key: "IBKR_FLEX_QUERY_ID", label: "Query ID", help: "The number of your Activity Flex Query.", secret: false },
            ],
            instructions: vec![
                "Client Portal: Performance & Reports, then Flex Queries.",
                "Create an Activity Flex Query in XML with: Trades, Open Positions, Cash Report, Net Asset Value (NAV) in Base, Cash Transactions (dividends, withholding tax). Period: last 365 days.",
                "Flex Web Service Configuration: enable it and generate a token.",
                "Paste the token and the query's ID here.",
            ],
            refuses: "nothing: a Flex token is reporting-only by design; the test checks the statement and the sections in it",
            docs_url: "https://www.ibkrguides.com/clientportal/performanceandstatements/flex.htm",
        },
    ]
}

/// What a code source's export is and how to get it: (format, steps,
/// docs_url, notes). A format definition describes itself in its `[export]`
/// table instead (see [`files`]).
type Howto = (&'static str, &'static [&'static str], Option<&'static str>, &'static str);

fn code_howto(key: &str) -> Option<Howto> {
    Some(match key {
        "n26" => (
            "CSV",
            &[
                "Sign in to N26 on the web (app.n26.com). The CSV export is not in the phone app.",
                "Open Home, then your account (or a Space), then Downloads.",
                "Choose Account Activity (CSV) and pick the start and end dates.",
                "Download the file and upload it here.",
            ],
            Some("https://support.n26.com/en-eu/fixing-an-issue/payments-and-transfers/how-to-export-a-list-of-my-transactions"),
            "One file per account or Space. Overlapping exports are de-duplicated.",
        ),
        "hsbc" => (
            "CSV",
            &[
                "Sign in to HSBC Hong Kong online banking.",
                "Open the account, then its transaction history.",
                "Choose Download, pick the period and the CSV format.",
                "Keep the file name HSBC gives it (TransactionHistory...) and upload it here.",
            ],
            None,
            "The current account, in HKD, with its balance column. Overlapping exports are de-duplicated.",
        ),
        "hsbc_card" => (
            "CSV or monthly PDF",
            &[
                "Sign in to HSBC Hong Kong online banking and open the credit card.",
                "For a CSV: open the transaction history, then Download, and pick CSV.",
                "For a PDF: open the e-Statements, then download each month's statement.",
                "Upload the files here. Both kinds can be mixed.",
            ],
            None,
            "A card is kept out of balances and net worth. A purchase in both a CSV and a statement is counted once.",
        ),
        "hsbc_sms" => (
            "XML backup of text messages",
            &[
                "HSBC has no export for FlexInvest orders. The text messages it sends for each one are the record.",
                "On an Android phone, install SMS Backup & Restore.",
                "In it, choose Set up a backup, keep Messages ticked, and run Back up now to the phone's storage.",
                "Copy the XML file it makes (sms-YYYYMMDD....xml) to this computer and upload it here.",
            ],
            Some("https://play.google.com/store/apps/details?id=com.riteshsahu.SMSBackupRestore"),
            "Reads the messages that read like 12.345 UNITS U50004 SUBSCRIBED AT HKD 20.1234 PER UNIT. P123456 (also REDEEMED, SWITCHED IN and OUT). Other messages are ignored; overlapping backups add nothing.",
        ),
        "mox" => (
            "monthly PDF",
            &[
                "Open the Mox app and go to your account.",
                "Open Statements (or Documents).",
                "Download each month's statement PDF.",
                "Upload the PDFs here.",
            ],
            None,
            "One statement covers every pocket (HKD, USD, ...). Overlapping months are de-duplicated. Mox has no CSV export.",
        ),
        "revolut_new" => (
            "CSV, Excel or PDF",
            &[
                "Open the Revolut app and the account (the currency) you want from Home.",
                "Tap More, then Statement.",
                "Choose Excel, CSV or PDF, pick the period, and tap Get statement.",
                "Upload the file here (a CSV, an Excel workbook or a PDF; Revolut names it account-statement...).",
            ],
            None,
            "One file per currency account. Pending and reverted rows are skipped; overlapping exports (CSV, Excel or PDF) are de-duplicated (a PDF's rows match the CSV's by amount and balance, not by description), a vault statement's rows go to a separate Savings account, and rows the legacy GBP statement already holds are skipped.",
        ),
        "revolut_old_gbp" => (
            "CSV (old layout)",
            &[
                "This is the older Revolut statement layout (columns Paid Out and Paid In, dates like 3 Jan 2021), from the app's old statement export.",
                "Revolut no longer offers it: use it only for a file you already have.",
                "Put revolut and gbp in the file name (revolut-gbp-2021.csv): the file is recognised by its name.",
            ],
            None,
            "The GBP account only. Newer statements use the Revolut source.",
        ),
        "revolut" => (
            "CSV (old BTC ledger)",
            &[
                "This is Revolut's old Bitcoin ledger export (columns Paid Out (BTC), Paid In (BTC), Balance (BTC)).",
                "Revolut no longer offers it: use it only for a file you already have.",
                "Upload the file as it is, or put revolut and btc in its name.",
            ],
            None,
            "Bitcoin bought and sold in Revolut, as crypto events rather than bank rows.",
        ),
        "trezor" => (
            "CSV",
            &[
                "Open Trezor Suite and pick the Bitcoin account.",
                "On its Transactions section, open the three dots menu.",
                "Choose Export as CSV and save the file.",
                "Repeat for each account, then upload the files here.",
            ],
            None,
            "One file per account. Overlapping exports are de-duplicated.",
        ),
        _ => return None,
    })
}

const GENERIC_HOW: &[&str] = &["Export the account's transactions as CSV from the bank's website or app.", "Upload the file here."];

pub fn files() -> Vec<FileEntry> {
    sources::file_sources()
        .iter()
        .map(|s| {
            let spec = s.spec();
            let kind = match spec.kind {
                Kind::Bank if spec.balance_method == BalanceMethod::None => "card",
                Kind::Bank => "bank",
                Kind::FundUnits => "fund_units",
                Kind::CryptoWallet => "crypto_wallet",
            };
            let own = sources::format::get(spec.key).and_then(|f| f.def.export.clone());
            let (format, how, docs_url, notes) = match (own, code_howto(spec.key)) {
                (Some(e), _) => (e.format, e.how, e.docs_url, e.notes.unwrap_or_default()),
                (None, Some((f, h, d, n))) => (f.to_string(), h.iter().map(|s| s.to_string()).collect(), d.map(String::from), n.to_string()),
                (None, None) => ("CSV".to_string(), GENERIC_HOW.iter().map(|s| s.to_string()).collect(), None, String::new()),
            };
            FileEntry { key: spec.key, label: spec.label, kind, instructions: how.clone(), format, how, docs_url, notes, country: default_country(spec.key) }
        })
        .collect()
}

/// The root README's short table of sources (between its sources markers).
pub fn readme_table() -> String {
    let mut s = String::from("| Source | Kind | What you give it |\n|---|---|---|\n");
    for f in files() {
        let kind = match f.kind {
            "bank" => "bank account",
            "card" => "credit card",
            "fund_units" => "fund orders",
            _ => "crypto wallet",
        };
        s += &format!("| {} | {} | exported file: {} |\n", f.label, kind, f.format);
    }
    for a in api() {
        let what = match a.key {
            "ibkr" => "Flex Web Service (a reporting token, cannot trade): token + query id",
            "okx" => "signed read-only API: key + secret + passphrase",
            _ => "signed read-only API: key + secret",
        };
        s += &format!("| {} | {} | {} |\n", a.label, a.kind, what);
    }
    s
}

/// The long per-source section of desktop/README.md: how to get each
/// source's data in (between its sources-howto markers).
pub fn readme_howto() -> String {
    let mut s = String::new();
    let mut entry = |label: &str, format: &str, how: &[String], docs: Option<&str>, notes: &str| {
        s += &format!("### {label}\n\n");
        if !format.is_empty() {
            s += &format!("Format: {format}.\n\n");
        }
        for (i, h) in how.iter().enumerate() {
            s += &format!("{}. {}\n", i + 1, h);
        }
        if !notes.is_empty() {
            s += &format!("\n{notes}\n");
        }
        if let Some(d) = docs {
            s += &format!("\nOfficial guide: <{d}>\n");
        }
        s += "\n";
    };
    for f in files() {
        entry(f.label, &f.format, &f.how, f.docs_url.as_deref(), &f.notes);
    }
    for a in api() {
        let how: Vec<String> = a.instructions.iter().map(|h| h.to_string()).collect();
        let notes = format!("Refused: {}.", a.refuses);
        entry(a.label, "", &how, Some(a.docs_url), &notes);
    }
    s.trim_end().to_string() + "\n"
}

pub fn catalog() -> Catalog {
    Catalog { api: api(), files: files() }
}

/// The catalog entry of an API source.
pub fn api_entry(key: &str) -> Option<ApiEntry> {
    api().into_iter().find(|a| a.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_api_field_is_a_store_key_and_every_file_source_is_listed() {
        for a in api() {
            assert!(!a.fields.is_empty() && !a.instructions.is_empty());
            for f in &a.fields {
                assert!(crate::secrets::is_secret_key(f.key), "{}", f.key);
            }
        }
        let keys: Vec<&str> = api().iter().flat_map(|a| a.fields.iter().map(|f| f.key).collect::<Vec<_>>()).collect();
        assert_eq!(keys.len(), crate::secrets::SECRET_KEYS.len());
        assert_eq!(files().len(), sources::file_sources().len());
        assert!(files().iter().all(|f| !f.instructions.is_empty()));
    }

    #[test]
    fn every_file_source_says_what_it_is_and_how_to_get_it() {
        for f in files() {
            assert!(!f.format.trim().is_empty() && !f.how.is_empty() && !f.notes.is_empty(), "{}", f.key);
            assert_eq!(f.instructions, f.how, "{}", f.key);
            assert!(f.how.iter().all(|h| !h.trim().is_empty()), "{}", f.key);
            assert_ne!(f.how.iter().map(String::as_str).collect::<Vec<_>>(), GENERIC_HOW, "{} has only the generic line", f.key);
        }
        let by = |k: &str| files().into_iter().find(|f| f.key == k).unwrap();
        assert!(!by("revolut").how.iter().any(|h| h.contains("Statement")));
    }
}
