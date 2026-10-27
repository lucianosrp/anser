//! HSBC unit-trust SMS confirmations,
//! from an "SMS Backup & Restore" XML export.
//!
//! The only reliable record of REAL FlexInvest executions ("12.345 UNITS
//! U50004 SUBSCRIBED AT HKD 20.1234 PER UNIT. P123456") -- HSBC exposes no CSV
//! for the Monthly Investment Plan. Every subscription lands in
//! fund_subscriptions, keyed on the confirmation ref so overlapping backups
//! are harmless.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use quick_xml::events::Event;
use regex::Regex;

use super::{suffix_lower, BalanceMethod, FileSource, FundRow, Kind, Parsed, SourceSpec};

/// The unit-trust confirmation regex lives in core (`paste`), where the
/// web build's pasted-text check shares it.
fn sub_re() -> &'static Regex {
    finance_core::paste::units_re()
}

/// SMS timestamp attr -> ms; '0', '', missing and garbage all -> 0
/// (surrounding whitespace tolerated).
fn ts_ms(v: Option<&str>) -> i64 {
    match v {
        None | Some("") => 0,
        Some(s) => super::textfmt::strip(s).parse().unwrap_or(0),
    }
}

/// Every `<sms>` element's (body, date_sent, date) attributes, or None when
/// the file is not well-formed XML (such a file is skipped entirely).
fn sms_attrs(xml: &str) -> Option<Vec<[Option<String>; 3]>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() != "sms" {
                    continue;
                }
                let mut vals: [Option<String>; 3] = [None, None, None];
                for a in e.attributes() {
                    let a = a.ok()?;
                    let slot = match a.key.as_ref() {
                        "body" => 0,
                        "date_sent" => 1,
                        "date" => 2,
                        _ => continue,
                    };
                    vals[slot] = Some(quick_xml::escape::unescape(&a.value).ok()?.into_owned());
                }
                out.push(vals);
            }
            Ok(Event::Eof) => return Some(out),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

pub struct HsbcSmsSource;

static SPEC: SourceSpec = SourceSpec {
    key: "hsbc_sms",
    kind: Kind::FundUnits,
    currency: Some("HKD"),
    balance_method: BalanceMethod::Reported,
    cashflow: false,
    label: "HSBC unit trust (SMS)",
};

impl FileSource for HsbcSmsSource {
    fn spec(&self) -> &SourceSpec {
        &SPEC
    }

    fn matches(&self, path: &Path) -> bool {
        if suffix_lower(path) != ".xml" {
            return false;
        }
        let Ok(f) = std::fs::File::open(path) else { return false };
        let mut head = Vec::new();
        if f.take(4096).read_to_end(&mut head).is_err() {
            return false;
        }
        head.windows(6).any(|w| w == b"<smses")
    }

    fn parse(&self, paths: &[PathBuf]) -> Result<Parsed> {
        let mut rows = Vec::new();
        for p in paths {
            let bytes = std::fs::read(p)?;
            let Ok(xml) = std::str::from_utf8(&bytes) else { continue };
            let Some(smses) = sms_attrs(xml) else { continue };
            for [body, date_sent, date] in smses {
                let Some(m) = sub_re().captures(body.as_deref().unwrap_or("")) else { continue };
                let mut units: f64 = m[1].replace(',', "").parse()?;
                let action = super::textfmt::split_ws(&m[3].to_uppercase()).join(" ");
                if action == "REDEEMED" || action == "SWITCHED OUT" {
                    units = -units;
                }
                // date_sent can legitimately be the STRING '0' (backup apps):
                // fall back to `date` whenever it is zero, not just missing
                let ms = match ts_ms(date_sent.as_deref()) {
                    0 => ts_ms(date.as_deref()),
                    v => v,
                };
                let trade_date = if ms != 0 {
                    chrono::DateTime::from_timestamp_millis(ms)
                        .map(|t| t.format("%Y-%m-%d").to_string())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                rows.push(FundRow {
                    reference: m[5].to_string(),
                    trade_date,
                    fund: m[2].to_uppercase(),
                    units,
                    px: m[4].replace(',', "").parse()?,
                });
            }
        }
        Ok(Parsed::FundUnits(rows))
    }
}
