//! Pasted HSBC FlexInvest texts -> the "SMS Backup & Restore" XML the
//! server's `hsbc_sms` source already reads, so a copied message uploads
//! like an export (same parser, same dedupe on the P-reference). Here, not
//! in the engine, because the web build has no engine; the engine's parser
//! takes [`UNITS_PATTERN`] from here, so the check and the parse can't drift.

use std::sync::OnceLock;

use chrono::{Local, NaiveDate, TimeZone, Utc};
use regex::Regex;

/// One unit-trust confirmation ("12.345 UNITS U50004 SUBSCRIBED AT HKD 20.1234
/// PER UNIT. P123456"). Tolerant on the tail (the period after PER UNIT is
/// optional -- wording has drifted before) and covers fund switches, which
/// move units exactly like a subscription/redemption pair.
pub const UNITS_PATTERN: &str = concat!(
    r"(?i)([\d,]+\.?\d*)\s+UNITS\s+(U\d+)\s+(SUBSCRIBED|REDEEMED|SWITCHED\s+IN|SWITCHED\s+OUT)",
    r"\s+AT\s+HKD\s*([\d,]+\.?\d*)\s+PER\s+UNIT\.?\s*(P\d+)"
);

pub fn units_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(UNITS_PATTERN).unwrap())
}

/// The messages in pasted text that hold a confirmation: blocks separated
/// by blank lines, or, when a block holds several confirmations, its lines.
pub fn confirmations_in(text: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        match units_re().find_iter(block).count() {
            0 => {}
            1 => out.push(block.trim().to_string()),
            _ => out.extend(block.lines().filter(|l| units_re().is_match(l)).map(|l| l.trim().to_string())),
        }
    }
    out
}

/// A minimal SMS-backup file with one `<sms>` per message, stamped `date`
/// at local noon (a pasted text carries no time of its own).
pub fn sms_xml(messages: &[String], date: NaiveDate) -> String {
    let noon = date.and_hms_opt(12, 0, 0).unwrap_or_default();
    let ms = Local
        .from_local_datetime(&noon)
        .earliest()
        .map(|t| t.timestamp_millis())
        .unwrap_or_else(|| Utc.from_utc_datetime(&noon).timestamp_millis());
    let mut xml = format!("<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\n<smses count=\"{}\">\n", messages.len());
    for m in messages {
        xml.push_str(&format!("  <sms address=\"HSBC\" date=\"{ms}\" date_sent=\"{ms}\" body=\"{}\" />\n", escape(m)));
    }
    xml.push_str("</smses>\n");
    xml
}

/// The upload name: `hsbc-sms-paste-YYYYMMDD-HHMMSS.xml`.
pub fn file_name() -> String {
    format!("hsbc-sms-paste-{}.xml", Local::now().format("%Y%m%d-%H%M%S"))
}

/// POST /upload of the file as multipart (the web build, which has no
/// file to hand the picker; native queues the file on the Upload page).
pub fn upload_request(name: &str, xml: &str) -> crate::client::Request {
    const BOUNDARY: &str = "----finance-paste";
    let mut req = crate::client::Request::new("POST", "/upload");
    let head = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n");
    req.body = Some([head.as_bytes(), xml.as_bytes(), format!("\r\n--{BOUNDARY}--\r\n").as_bytes()].concat());
    req.content_type = Some(format!("multipart/form-data; boundary={BOUNDARY}"));
    req
}

/// Attribute-value escaping; newlines and tabs as character references,
/// since XML attribute normalization turns raw ones into spaces.
fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            '\n' => o.push_str("&#10;"),
            '\t' => o.push_str("&#9;"),
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_confirmations_and_ignores_the_rest() {
        assert!(confirmations_in("hello there").is_empty());
        let one = "HSBC: 12.345 UNITS U50004 SUBSCRIBED AT HKD 20.1234 PER UNIT. P123456";
        assert_eq!(confirmations_in(&format!("\n{one}\n")), vec![one.to_string()]);
        // two on consecutive lines, and two blank-line separated
        let two = "5 UNITS U50005 REDEEMED AT HKD 1.5 PER UNIT. P2";
        assert_eq!(confirmations_in(&format!("{one}\n{two}")), vec![one.to_string(), two.to_string()]);
        assert_eq!(confirmations_in(&format!("{one}\r\n\r\n{two}")).len(), 2);
    }
}
