//! The source test: do these keys work, and are they read-only? Run before
//! a key is saved (PUT /api/sources/{key}/secret refuses on failure) and on
//! demand (POST /api/sources/{key}/test).
//!
//! * Binance: GET /api/v3/account (the key reads), then
//!   GET /sapi/v1/account/apiRestrictions; any trading, withdrawal,
//!   transfer, margin, futures or options permission is refused.
//! * OKX: GET /api/v5/account/config, whose `perm` must be exactly
//!   `read_only`, then GET /api/v5/account/balance.
//! * IBKR Flex: SendRequest + GetStatement, reporting the sections the
//!   statement carries (a Flex token can't trade by design).
//!
//! The verdicts ([`judge_binance`], [`judge_okx`], [`judge_ibkr`]) are pure
//! functions of the payloads, tested on recorded shapes. Every error is
//! scrubbed of the candidate values before it leaves here.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use super::{binance, ibkr, okx, status};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Verdict {
    pub ok: bool,
    /// one line for the owner
    pub detail: String,
    /// the permissions that made it fail (empty when ok)
    pub refused: Vec<String>,
    /// IBKR: the sections the statement carried
    pub sections: Vec<String>,
}

impl Verdict {
    fn fail(detail: impl Into<String>) -> Self {
        Verdict { ok: false, detail: detail.into(), ..Default::default() }
    }
}

/// Binance permissions a read-only key must not have.
const BINANCE_FORBIDDEN: [&str; 9] = [
    "enableSpotAndMarginTrading", "enableWithdrawals", "enableInternalTransfer", "permitsUniversalTransfer", "enableMargin",
    "enableFutures", "enablePortfolioMarginTrading", "enableVanillaOptions", "enableFixApiTrade",
];

pub fn judge_binance(restrictions: &Value) -> Verdict {
    let refused: Vec<String> =
        BINANCE_FORBIDDEN.iter().filter(|k| restrictions.get(**k).and_then(Value::as_bool) == Some(true)).map(|k| k.to_string()).collect();
    if !refused.is_empty() {
        return Verdict {
            ok: false,
            detail: format!("This key can do more than read ({}). Make a key with only Enable Reading.", refused.join(", ")),
            refused,
            ..Default::default()
        };
    }
    if restrictions.get("enableReading").and_then(Value::as_bool) != Some(true) {
        return Verdict::fail("This key can't read the account: tick Enable Reading.");
    }
    let ip = restrictions.get("ipRestrict").and_then(Value::as_bool) == Some(true);
    Verdict {
        ok: true,
        detail: if ip { "Read-only, IP-restricted.".into() } else { "Read-only. Restricting it to your server's IP is recommended.".into() },
        ..Default::default()
    }
}

/// `config`: the first row of /api/v5/account/config's data.
pub fn judge_okx(config: &Value, balance: Result<(), String>) -> Verdict {
    let perm = config.get("perm").and_then(Value::as_str).unwrap_or("");
    let perms: Vec<&str> = perm.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    let refused: Vec<String> = perms.iter().filter(|p| **p != "read_only").map(|p| p.to_string()).collect();
    if !refused.is_empty() {
        return Verdict {
            ok: false,
            detail: format!("This key has {} permission. Make a key with Read only.", refused.join(" and ")),
            refused,
            ..Default::default()
        };
    }
    if perms != ["read_only"] {
        return Verdict::fail(format!("Unexpected permissions {perm:?}: expected read_only."));
    }
    match balance {
        Ok(()) => Verdict { ok: true, detail: "Read-only; the balance reads.".into(), ..Default::default() },
        Err(e) => Verdict::fail(format!("Read-only, but the balance didn't read: {e}")),
    }
}

/// `sections`: (name, rows) per Flex section in the statement.
pub fn judge_ibkr(sections: &status::FlexSections) -> Verdict {
    let all = [
        ("Trades", sections.trades),
        ("Open positions", sections.open_positions),
        ("Cash report", sections.cash_report),
        ("NAV", sections.nav),
        ("Cash transactions", sections.cash_transactions),
        ("Dividends", sections.dividends),
        ("Withholding", sections.withholding),
    ];
    let present: Vec<String> = all.iter().filter(|(_, n)| *n > 0).map(|(k, _)| k.to_string()).collect();
    let missing: Vec<&str> = all[..4].iter().filter(|(_, n)| *n == 0).map(|(k, _)| *k).collect();
    let detail = if missing.is_empty() {
        format!("The statement reads: {}.", present.join(", "))
    } else {
        format!("The statement reads, but has no {} (add them to the Flex query, or there's none in its period).", missing.join(", "))
    };
    Verdict { ok: true, detail, sections: present, ..Default::default() }
}

fn scrubbed(e: &anyhow::Error, values: &[String]) -> String {
    status::scrub(&format!("{e:#}"), values)
}

/// Test `fields` (the candidate values, by .env key) for source `key`.
/// Network; blocking (seconds; IBKR up to a minute).
pub fn test(key: &str, fields: &BTreeMap<String, String>) -> Verdict {
    let values: Vec<String> = fields.values().filter(|v| v.len() >= 4).cloned().collect();
    let f = |k: &str| fields.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    match key {
        "binance" => {
            let (Some(k), Some(s)) = (f("BINANCE_API_KEY"), f("BINANCE_API_SECRET")) else {
                return Verdict::fail("Both the API key and the secret key are needed.");
            };
            let api = binance::Client::new(&k, &s);
            if let Err(e) = api.get("/api/v3/account", &[("omitZeroBalances", "true".into())]) {
                return Verdict::fail(format!("The key doesn't read the account: {}", scrubbed(&e, &values)));
            }
            match api.get("/sapi/v1/account/apiRestrictions", &[]) {
                Ok(r) => judge_binance(&r),
                Err(e) => Verdict::fail(format!("Couldn't read the key's permissions: {}", scrubbed(&e, &values))),
            }
        }
        "okx" => {
            let (Some(k), Some(s), Some(p)) = (f("OKX_API_KEY"), f("OKX_API_SECRET"), f("OKX_PASSPHRASE")) else {
                return Verdict::fail("The API key, secret key and passphrase are all needed.");
            };
            let api = okx::Client::new(&k, &s, &p);
            let config = match api.get("/api/v5/account/config", &[]) {
                Ok(d) => d.into_iter().next().unwrap_or(Value::Null),
                Err(e) => return Verdict::fail(format!("The key doesn't read the account: {}", scrubbed(&e, &values))),
            };
            let balance = api.get("/api/v5/account/balance", &[]).map(|_| ()).map_err(|e| scrubbed(&e, &values));
            judge_okx(&config, balance)
        }
        "ibkr" => {
            let (Some(t), Some(q)) = (f("IBKR_FLEX_TOKEN"), f("IBKR_FLEX_QUERY_ID")) else {
                return Verdict::fail("Both the Flex token and the query ID are needed.");
            };
            match ibkr::fetch_statement_with(&t, &q) {
                Ok(root) => judge_ibkr(&ibkr::sections(&root)),
                Err(e) => Verdict::fail(scrubbed(&e, &values)),
            }
        }
        _ => Verdict::fail(format!("{key} is not an API source")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binance_rejects_any_power_beyond_reading() {
        let ro = json!({"ipRestrict": false, "enableReading": true, "enableWithdrawals": false, "enableInternalTransfer": false,
            "permitsUniversalTransfer": false, "enableMargin": false, "enableFutures": false, "enableSpotAndMarginTrading": false,
            "enableVanillaOptions": false, "enablePortfolioMarginTrading": false});
        assert!(judge_binance(&ro).ok);
        for k in ["enableSpotAndMarginTrading", "enableWithdrawals", "enableInternalTransfer", "permitsUniversalTransfer", "enableFutures", "enableMargin"] {
            let mut v = ro.clone();
            v[k] = json!(true);
            let j = judge_binance(&v);
            assert!(!j.ok && j.refused == vec![k.to_string()], "{k}: {j:?}");
        }
        let mut v = ro.clone();
        v["enableReading"] = json!(false);
        assert!(!judge_binance(&v).ok);
    }

    #[test]
    fn okx_wants_exactly_read_only() {
        assert!(judge_okx(&json!({"perm": "read_only"}), Ok(())).ok);
        let j = judge_okx(&json!({"perm": "read_only,trade"}), Ok(()));
        assert!(!j.ok && j.refused == vec!["trade".to_string()]);
        let j = judge_okx(&json!({"perm": "read_only,withdraw,trade"}), Ok(()));
        assert_eq!(j.refused, vec!["withdraw".to_string(), "trade".to_string()]);
        assert!(!judge_okx(&json!({}), Ok(())).ok, "no perm field: not proven read-only");
        assert!(!judge_okx(&json!({"perm": "read_only"}), Err("50113".into())).ok);
    }

    #[test]
    fn ibkr_reports_sections() {
        let s = status::FlexSections { trades: 3, open_positions: 2, cash_report: 1, nav: 1, cash_transactions: 4, dividends: 2, withholding: 1 };
        let j = judge_ibkr(&s);
        assert!(j.ok && j.sections.len() == 7, "{j:?}");
        let j = judge_ibkr(&status::FlexSections { trades: 1, ..Default::default() });
        assert!(j.ok && j.detail.contains("Open positions"), "{j:?}");
    }

    #[test]
    fn missing_fields_fail_without_network() {
        let j = test("binance", &BTreeMap::new());
        assert!(!j.ok);
        assert!(!test("nope", &BTreeMap::new()).ok);
    }
}
