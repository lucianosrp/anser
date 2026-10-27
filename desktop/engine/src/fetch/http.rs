//! The one HTTP client every fetcher goes through. Centralised so three
//! rules hold everywhere:
//!
//! * FINANCE_OFFLINE=1 means no network at all -- checked here, before any
//!   socket is opened, so no fetcher can forget it.
//! * Only GET. Nothing in this app may place trades or move money, and the
//!   exchange keys are read-only anyway; not having a POST helper makes that
//!   structural instead of a convention.
//! * URLs carrying secrets (the IBKR Flex token, signed Binance queries) are
//!   never logged; errors name the path only.

use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::config;

/// A finished response: status, the headers fetchers look at, and the body.
pub struct Resp {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.body).context("response is not JSON")
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Bail when the process must stay off the network.
pub fn ensure_online() -> Result<()> {
    if config::offline() {
        bail!("offline (FINANCE_OFFLINE=1)");
    }
    Ok(())
}

/// An agent with a whole-request deadline. Status codes are returned, not
/// raised: the exchange clients decide themselves what a 429 or a 400 means.
pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent("Mozilla/5.0 (finance-dashboard)")
        .build()
        .into()
}

/// GET with extra headers. `what` names the request in errors (never the URL:
/// it may carry a token or a signature).
pub fn get(agent: &ureq::Agent, url: &str, headers: &[(&str, &str)], what: &str) -> Result<Resp> {
    ensure_online()?;
    let mut req = agent.get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req.call().map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let body = resp
        .body_mut()
        .with_config()
        .limit(256 << 20)
        .read_to_vec()
        .map_err(|e| anyhow::anyhow!("{what}: reading body: {e}"))?;
    Ok(Resp { status, retry_after, body })
}

/// `application/x-www-form-urlencoded` for query strings (space -> '+',
/// everything non-unreserved escaped).
pub fn urlencode(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", escape(k), escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_is_form_encoding() {
        let q = urlencode(&[("symbols", r#"["BTCUSDT","ETHUSDT"]"#.into()), ("a b", "c&d".into())]);
        assert_eq!(q, "symbols=%5B%22BTCUSDT%22%2C%22ETHUSDT%22%5D&a+b=c%26d");
    }
}
