//! `GET /api/sources`: the state of every data source, as the server sees
//! it. anser-server builds it (server/src/sources.rs), the Doctor judges it
//! (`doctor::source_checks`). It never carries a secret: whether a key is
//! configured is a boolean, and error text is scrubbed of URLs and tokens
//! before it is stored.

use serde::{Deserialize, Serialize};

use crate::fees::PlanGuess;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sources {
    /// the server's clock (unix ms), so ages don't depend on this machine's
    pub generated_ms: i64,
    /// binance, okx, ibkr
    pub api: Vec<ApiSource>,
    /// the file sources (bank and wallet exports): newest row per source
    pub files: Vec<FileSource>,
    pub ibkr: Option<Ibkr>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiSource {
    pub key: String,
    pub label: String,
    /// every .env key the fetcher needs is set
    pub configured: bool,
    /// the .env keys it reads (names only)
    pub env_keys: Vec<String>,
    /// unix ms: the last fetch that worked (from the fetch log, else the
    /// newest raw payload), and the last attempt
    pub last_ok_ms: Option<i64>,
    pub last_attempt_ms: Option<i64>,
    /// the last attempt's error, scrubbed; None when it worked
    pub last_error: Option<String>,
    /// where the keys come from: "store" (the encrypted secrets store),
    /// ".env" or "env"; None when not configured (or an older server)
    #[serde(default)]
    pub configured_via: Option<String>,
    /// per key: set or not, from where, and (to a paired device only) its
    /// last 4 characters
    #[serde(default)]
    pub fields: Vec<FieldState>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldState {
    pub key: String,
    pub set: bool,
    pub via: Option<String>,
    /// "…WXYZ" (or "set" for a short value); None without a device token
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileSource {
    pub source: String,
    /// "bank" or "wallet"
    pub kind: String,
    /// YYYY-MM-DD of its newest row
    pub newest: Option<String>,
    pub rows: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ibkr {
    /// YYYY-MM-DD
    pub latest_trade: Option<String>,
    pub positions_as_of: Option<String>,
    pub sections: Sections,
    /// a Flex error code in the last error, in plain words
    pub flex_error: Option<FlexError>,
    pub plan: Option<PlanGuess>,
}

/// Which Flex query sections the data shows. From the last statement when
/// the server has fetched since it started logging them ("statement"),
/// else inferred from the stored tables ("stored"), which can't tell
/// dividends or withholding (None).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sections {
    pub from: String,
    pub trades: bool,
    pub open_positions: bool,
    pub cash_report: bool,
    pub nav: bool,
    pub cash_transactions: bool,
    pub dividends: Option<bool>,
    pub withholding: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlexError {
    pub code: i64,
    /// token_expired, token_invalid, query_invalid, ip_restricted, throttled, transient, other
    pub kind: String,
    pub words: String,
}
