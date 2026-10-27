//! IBKR via the Flex Web Service.
//!
//! Not a trading API: the Flex token can only render pre-configured Activity
//! reports, so a leaked token exposes history but can never move money. A
//! fetch is two GETs: SendRequest returns a reference code, GetStatement
//! returns the XML once IBKR has generated it (polled with backoff).
//!
//! Writes: ibkr_trades (merged by broker trade id), ibkr_positions /
//! ibkr_cash (snapshot rewrite), ibkr_nav and ibkr_cash_flows (upserts),
//! dividends and withholding into ibkr_income (upserts; a Rust-only table,
//! created on first write), and the parsed statement into crypto_raw_sources. Equity OHLC for the held
//! symbols is stored by the valuation side's 3-day refresh and by
//! `refresh-prices` (prices::equity_ohlc -> db::upsert_equity_prices).
//!
//! The token rides in the query string, so URLs here are never logged.

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use quick_xml::events::{BytesStart, Event as XmlEvent};
use rusqlite::Connection;
use serde_json::json;

use super::http::{self, escape};
use crate::db::{self, IbkrCash, IbkrFlow, IbkrIncome, IbkrNav, IbkrPosition, IbkrTrade};
use crate::views::fx::Rates as RateResolver;
use super::{status, Log};
use crate::config;

pub const FLEX_BASE: &str = "https://ndcdyn.interactivebrokers.com/AccountManagement/FlexWebService";

// --- a minimal DOM: Flex XML is attributes on nested elements ---------------

#[derive(Debug, Default, Clone)]
pub struct Elem {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Elem>,
    pub text: String,
}

impl Elem {
    /// Attribute value (ElementTree's `el.get(name)`).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The attribute, `None` when missing or empty.
    fn nz(&self, name: &str) -> Option<&str> {
        self.get(name).filter(|v| !v.is_empty())
    }

    /// Every descendant (self included) with this tag, document order (ET.iter).
    pub fn iter<'a>(&'a self, tag: &'a str) -> Vec<&'a Elem> {
        let mut out = Vec::new();
        fn walk<'a>(e: &'a Elem, tag: &str, out: &mut Vec<&'a Elem>) {
            if e.tag == tag {
                out.push(e);
            }
            for c in &e.children {
                walk(c, tag, out);
            }
        }
        walk(self, tag, &mut out);
        out
    }

    /// First descendant with this tag, self excluded (ET `.find(".//tag")`).
    pub fn find(&self, tag: &str) -> Option<&Elem> {
        self.children.iter().find_map(|c| if c.tag == tag { Some(c) } else { c.find(tag) })
    }

    /// Text of the first direct child with this tag (ET `.findtext`).
    pub fn findtext(&self, tag: &str) -> Option<&str> {
        self.children.iter().find(|c| c.tag == tag).map(|c| c.text.as_str())
    }
}

fn start_elem(e: &BytesStart) -> Result<Elem> {
    let mut el = Elem { tag: e.name().as_ref().to_owned(), ..Default::default() };
    for a in e.attributes() {
        let a = a?;
        el.attrs.push((a.key.as_ref().to_owned(), quick_xml::escape::unescape(&a.value)?.into_owned()));
    }
    Ok(el)
}

pub fn parse_xml(bytes: &[u8]) -> Result<Elem> {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut stack: Vec<Elem> = vec![Elem::default()]; // a synthetic document node
    // Text arrives as Text and GeneralRef (`&amp;`) pieces; trim each run as a whole.
    let mut run = String::new();
    loop {
        let ev = reader.read_event().context("Flex XML")?;
        match &ev {
            XmlEvent::Text(t) => {
                run.push_str(&t);
                continue;
            }
            XmlEvent::GeneralRef(r) => {
                if let Some(c) = r.resolve_char_ref()? {
                    run.push(c);
                } else {
                    let name: &str = &r;
                    run.push_str(quick_xml::escape::resolve_predefined_entity(&name).with_context(|| format!("Flex XML: unknown entity &{name};"))?);
                }
                continue;
            }
            _ => {}
        }
        if !run.is_empty() {
            if let Some(top) = stack.last_mut() {
                top.text.push_str(run.trim());
            }
            run.clear();
        }
        match ev {
            XmlEvent::Start(e) => stack.push(start_elem(&e)?),
            XmlEvent::Empty(e) => {
                let el = start_elem(&e)?;
                stack.last_mut().unwrap().children.push(el);
            }
            XmlEvent::End(_) => {
                let el = stack.pop().unwrap();
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => bail!("Flex XML: unbalanced end tag"),
                }
            }
            XmlEvent::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t);
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }
    let doc = stack.pop().context("Flex XML: empty")?;
    doc.children.into_iter().next().context("Flex XML: no root element")
}

// --- field helpers ---------------------------------------------------------------

/// `v` parsed as f64, `default` when missing or unparsable.
fn num(v: Option<&str>, default: f64) -> f64 {
    v.and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(default)
}

/// `x`, or 1.0 when it is zero.
fn or1(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        x
    }
}

/// Round to cents, correctly rounded on the exact binary value (what
/// precision formatting does), as the stored rows are.
pub fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x)
}

/// A number as it appears in stored ids ("10.0", "0.4843").
fn float_id(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

/// Flex dates arrive as '20260710' or '2026-07-10'; normalize to ISO.
pub fn iso(d: Option<&str>) -> String {
    let d = d.unwrap_or("").trim();
    if d.len() >= 8 && d.as_bytes()[..8].iter().all(u8::is_ascii_digit) {
        return format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8]);
    }
    d.chars().take(10).collect()
}

/// Trade timestamp from dateTime ('yyyyMMdd;HHmmss') or tradeDate, as UTC.
pub fn ts_ms(el: &Elem) -> i64 {
    let raw = el.nz("dateTime").or_else(|| el.nz("tradeDate")).unwrap_or("");
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    let parsed = if digits.len() >= 14 {
        chrono::NaiveDateTime::parse_from_str(&digits[..14], "%Y%m%d%H%M%S").ok()
    } else if digits.len() >= 8 {
        chrono::NaiveDate::parse_from_str(&digits[..8], "%Y%m%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0))
    } else {
        None
    };
    parsed.map(|d| d.and_utc().timestamp_millis()).unwrap_or(0)
}

/// One NAV row per daily FlexStatement: BASE_SUMMARY ending cash plus every
/// open position at the statement's own fxRateToBase. Days with neither
/// (before the account was funded) are skipped.
pub fn parse_daily_nav(stmts: &[&Elem], base_ccy: &str) -> Vec<IbkrNav> {
    let mut out = Vec::new();
    for st in stmts {
        let day = iso(st.get("toDate"));
        if day.is_empty() {
            continue;
        }
        let mut cash = 0.0;
        for c in st.iter("CashReportCurrency") {
            if c.get("currency").unwrap_or("").eq_ignore_ascii_case("BASE_SUMMARY") {
                cash = num(c.get("endingCash"), 0.0);
            }
        }
        let pos: f64 = st
            .iter("OpenPosition")
            .into_iter()
            .filter(|p| p.get("levelOfDetail") != Some("LOT"))
            .map(|p| num(p.get("positionValue"), 0.0) * or1(num(p.get("fxRateToBase"), 1.0)))
            .sum();
        if cash.abs() < 0.01 && pos.abs() < 0.01 {
            continue;
        }
        out.push(IbkrNav {
            date: day,
            cash_base: round2(cash),
            positions_base: round2(pos),
            nav_base: round2(cash + pos),
            base_ccy: base_ccy.into(),
        });
    }
    out
}

/// A CashTransaction's booking day, currency and rate to base (its own
/// fxRateToBase, else the monthly FX cache).
fn cash_txn_basics(el: &Elem, base_ccy: &str, rate: &RateResolver) -> (String, String, f64) {
    let raw = el.nz("dateTime").or_else(|| el.nz("reportDate")).or_else(|| el.nz("settleDate")).unwrap_or("");
    let day = iso(Some(&raw.chars().take(8).collect::<String>()));
    let ccy = el.nz("currency").map(str::to_uppercase).unwrap_or_else(|| base_ccy.into());
    let mut fxb = num(el.get("fxRateToBase"), 0.0);
    if fxb == 0.0 {
        let ym: String = day.chars().take(7).collect();
        fxb = if ccy != base_ccy {
            let b = rate.rate(base_ccy, &ym);
            rate.rate(&ccy, &ym) / if b == 0.0 { 1.0 } else { b }
        } else {
            1.0
        };
    }
    (day, ccy, fxb)
}

/// Is this CashTransaction type investment income (or the tax on it)?
pub fn is_income_type(kind: &str) -> bool {
    let k = kind.to_lowercase();
    k.contains("dividend") || k.contains("in lieu") || k.contains("withholding")
}

/// Dividends, payments in lieu of dividends and withholding tax
/// (CashTransactions). Kept apart from the flows: they are returns, and
/// ibkr_cash_flows is the external-flow series the return maths divide by.
/// Keyed on the broker's transactionID, else on what the row says, so a
/// refetch of the same statement adds nothing.
pub fn parse_income(root: &Elem, base_ccy: &str, rate: &RateResolver) -> Vec<IbkrIncome> {
    let mut out = Vec::new();
    for el in root.iter("CashTransaction") {
        let kind = el.get("type").unwrap_or("");
        if !is_income_type(kind) {
            continue;
        }
        let amt = num(el.get("amount"), 0.0);
        if amt == 0.0 {
            continue;
        }
        let (day, ccy, fxb) = cash_txn_basics(el, base_ccy, rate);
        let description: String = el.get("description").unwrap_or("").chars().take(120).collect();
        // "QQQ(US46090E1038) Cash Dividend USD 0.71 per Share" when the row has no symbol
        let symbol = el.nz("symbol").map(String::from)
            .unwrap_or_else(|| description.split(['(', ' ']).next().unwrap_or("").trim().to_string());
        let id = el.nz("transactionID").map(String::from)
            .unwrap_or_else(|| format!("{day}|{kind}|{ccy}|{}|{description}", float_id(amt)));
        out.push(IbkrIncome {
            income_id: id,
            date: day,
            symbol,
            kind: kind.into(),
            currency: ccy,
            amount: amt,
            amount_base: round2(amt * fxb),
            description,
        });
    }
    out
}

/// External deposits / withdrawals (CashTransactions). Dividends, interest
/// and fees are returns, not flows. Converted at fxRateToBase when present,
/// else via the monthly FX cache.
pub fn parse_cash_flows(root: &Elem, base_ccy: &str, rate: &RateResolver) -> Vec<IbkrFlow> {
    let mut out = Vec::new();
    for el in root.iter("CashTransaction") {
        if el.get("type") != Some("Deposits/Withdrawals") {
            continue;
        }
        let amt = num(el.get("amount"), 0.0);
        if amt == 0.0 {
            continue;
        }
        let (day, ccy, fxb) = cash_txn_basics(el, base_ccy, rate);
        let fid = el.nz("transactionID").map(String::from).unwrap_or_else(|| format!("{day}-{ccy}-{}", float_id(amt)));
        out.push(IbkrFlow {
            flow_id: fid,
            date: day,
            kind: "Deposits/Withdrawals".into(),
            currency: ccy,
            amount: amt,
            amount_base: round2(amt * fxb),
            description: el.get("description").unwrap_or("").chars().take(80).collect(),
        });
    }
    out
}

/// Everything one statement yields.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub as_of: String,
    pub base_ccy: String,
    pub trades: Vec<IbkrTrade>,
    pub positions: Vec<IbkrPosition>,
    pub cash: Vec<IbkrCash>,
    pub nav: Vec<IbkrNav>,
    pub flows: Vec<IbkrFlow>,
    pub income: Vec<IbkrIncome>,
}

/// Parse a FlexQueryResponse. A query set to "one statement per day" returns
/// hundreds of FlexStatements: trades aggregate across ALL of them,
/// positions/cash come from the newest.
pub fn parse_statement(root: &Elem, rate: &RateResolver) -> Result<Parsed> {
    let mut stmts = root.iter("FlexStatement");
    stmts.sort_by_key(|s| s.nz("toDate").or_else(|| s.nz("whenGenerated")).unwrap_or("").to_string());
    let Some(last) = stmts.last().copied() else {
        bail!("Flex response contained no FlexStatement (check the query's date period)");
    };
    let as_of = iso(last.nz("toDate").or_else(|| last.nz("whenGenerated")));

    // base currency: AccountInformation; else ConversionRate (every rate
    // converts INTO base); else any row whose fxRateToBase is exactly 1
    let mut base_ccy = root.find("AccountInformation").and_then(|a| a.get("currency")).unwrap_or("").to_uppercase();
    if base_ccy.is_empty() {
        base_ccy = root.find("ConversionRate").and_then(|c| c.get("toCurrency")).unwrap_or("").to_uppercase();
    }
    if base_ccy.is_empty() {
        let rows = root.iter("OpenPosition").into_iter().chain(root.iter("Trade"));
        for el in rows {
            if num(el.get("fxRateToBase"), 0.0) == 1.0 {
                if let Some(c) = el.nz("currency") {
                    base_ccy = c.to_uppercase();
                    break;
                }
            }
        }
    }
    if base_ccy.is_empty() {
        base_ccy = "EUR".into();
    }
    let ccy_of = |el: &Elem| el.nz("currency").map(str::to_uppercase).unwrap_or_else(|| base_ccy.clone());

    let mut trades = Vec::new();
    for t in root.iter("Trade") {
        if !matches!(t.get("levelOfDetail"), None | Some("") | Some("EXECUTION")) {
            continue; // summary/order/lot duplicates of the same fills
        }
        let (symbol, qty) = (t.get("symbol").unwrap_or(""), num(t.get("quantity"), 0.0));
        if symbol.is_empty() || qty == 0.0 {
            continue;
        }
        let tid = t
            .nz("tradeID")
            .or_else(|| t.nz("ibExecID"))
            .or_else(|| t.nz("transactionID"))
            .map(String::from)
            .unwrap_or_else(|| format!("{symbol}-{}-{}", t.get("dateTime").unwrap_or("None"), float_id(qty)));
        trades.push(IbkrTrade {
            trade_id: tid,
            ts: ts_ms(t),
            symbol: symbol.into(),
            description: t.get("description").unwrap_or("").into(),
            currency: ccy_of(t),
            qty,
            price: num(t.get("tradePrice"), 0.0),
            amount: num(t.get("netCash"), num(t.get("tradeMoney"), 0.0)),
            commission: num(t.get("ibCommission"), 0.0),
            fx_to_base: or1(num(t.get("fxRateToBase"), 1.0)),
        });
    }

    // positions from the newest statement that carries any (the section may
    // be absent from the query; then valuation falls back to cash + cost)
    let pos_stmt = stmts.iter().rev().find(|s| s.find("OpenPosition").is_some()).copied().unwrap_or(last);
    let mut positions = Vec::new();
    for p in pos_stmt.iter("OpenPosition") {
        if p.get("levelOfDetail") == Some("LOT") {
            continue;
        }
        let (symbol, qty) = (p.get("symbol").unwrap_or(""), num(p.get("position"), 0.0));
        if symbol.is_empty() || qty == 0.0 {
            continue;
        }
        let value = num(p.get("positionValue"), 0.0);
        let fxb = or1(num(p.get("fxRateToBase"), 1.0));
        let mark = num(p.get("markPrice"), 0.0);
        let rep = iso(p.get("reportDate"));
        positions.push(IbkrPosition {
            symbol: symbol.into(),
            description: p.get("description").unwrap_or("").into(),
            currency: ccy_of(p),
            qty,
            mark_price: (mark != 0.0).then_some(mark),
            value_ccy: value,
            cost_ccy: p.nz("costBasisMoney").map(|c| num(Some(c), 0.0)),
            value_base: round2(value * fxb),
            base_ccy: base_ccy.clone(),
            as_of: if rep.is_empty() { as_of.clone() } else { rep },
        });
    }

    // cash from the newest statement. Per-currency rows carry no fx rate, so
    // convert with the monthly FX cache; a base-summary-only query yields just
    // BASE_SUMMARY, already in base currency
    let ym: String = if as_of.is_empty() { config::utc_today().to_string() } else { as_of.clone() }.chars().take(7).collect();
    let base_hkd = or1(rate.rate(&base_ccy, &ym));
    let cash_rows = last.iter("CashReportCurrency");
    let mut cash = Vec::new();
    for c in &cash_rows {
        let ccy = c.get("currency").unwrap_or("").to_uppercase();
        if ccy.is_empty() || ccy == "BASE_SUMMARY" {
            continue;
        }
        let amt = num(c.get("endingCash"), 0.0);
        if amt.abs() < 0.01 {
            continue;
        }
        cash.push(IbkrCash {
            amount_base: round2(amt * rate.rate(&ccy, &ym) / base_hkd),
            currency: ccy,
            amount: amt,
            base_ccy: base_ccy.clone(),
            as_of: as_of.clone(),
        });
    }
    if cash.is_empty() {
        if let Some(c) = cash_rows.iter().find(|c| c.get("currency").unwrap_or("").eq_ignore_ascii_case("BASE_SUMMARY")) {
            let amt = num(c.get("endingCash"), 0.0);
            if amt.abs() >= 0.01 {
                cash.push(IbkrCash { currency: base_ccy.clone(), amount: amt, amount_base: round2(amt), base_ccy: base_ccy.clone(), as_of: as_of.clone() });
            }
        }
    }

    let nav = parse_daily_nav(&stmts, &base_ccy);
    let flows = parse_cash_flows(root, &base_ccy, rate);
    let income = parse_income(root, &base_ccy, rate);
    Ok(Parsed { as_of, base_ccy, trades, positions, cash, nav, flows, income })
}

fn creds() -> Result<(String, String)> {
    match (config::env("IBKR_FLEX_TOKEN"), config::env("IBKR_FLEX_QUERY_ID")) {
        (Some(t), Some(q)) => Ok((t, q)),
        _ => bail!("Missing IBKR_FLEX_TOKEN / IBKR_FLEX_QUERY_ID {}", crate::secrets::where_to_set()),
    }
}

/// "error 1012: " when the Flex response carries an ErrorCode (status.rs
/// maps the codes to plain words).
fn flex_code(root: &Elem) -> String {
    root.findtext("ErrorCode").map(str::trim).filter(|c| !c.is_empty()).map(|c| format!("error {c}: ")).unwrap_or_default()
}

/// Which sections a Flex response carried, for the Doctor.
pub fn sections(root: &Elem) -> status::FlexSections {
    let flows = root.iter("CashTransaction");
    let kind = |el: &&Elem| el.get("type").unwrap_or("").to_lowercase();
    status::FlexSections {
        trades: root.iter("Trade").len(),
        open_positions: root.iter("OpenPosition").len(),
        cash_report: root.iter("CashReportCurrency").len(),
        nav: root.iter("EquitySummaryByReportDateInBase").len()
            .max(root.iter("FlexStatement").iter().filter(|s| s.find("CashReportCurrency").is_some()).count()),
        cash_transactions: flows.len(),
        dividends: flows.iter().filter(|e| { let k = kind(e); k.contains("dividend") || k.contains("in lieu") }).count(),
        withholding: flows.iter().filter(|e| kind(e).contains("withholding")).count(),
    }
}

fn fetch_statement() -> Result<Elem> {
    let (token, query_id) = creds()?;
    fetch_statement_with(&token, &query_id)
}

/// SendRequest + GetStatement with explicit credentials (the source test
/// runs it on keys that aren't saved yet).
pub fn fetch_statement_with(token: &str, query_id: &str) -> Result<Elem> {
    let (token, query_id) = (token.to_string(), query_id.to_string());
    let agent = http::agent(Duration::from_secs(30));
    let url = format!("{FLEX_BASE}/SendRequest?t={}&q={}&v=3", escape(&token), escape(&query_id));
    let r = http::get(&agent, &url, &[], "Flex SendRequest")?;
    if r.status >= 400 {
        bail!("Flex SendRequest: HTTP {}", r.status);
    }
    let root = parse_xml(&r.body)?;
    if !root.findtext("Status").unwrap_or("").eq_ignore_ascii_case("success") {
        bail!("Flex SendRequest: {}{}", flex_code(&root), root.findtext("ErrorMessage").filter(|m| !m.is_empty()).unwrap_or("unknown error"));
    }
    let reference = root.findtext("ReferenceCode").unwrap_or("").to_string();
    let base = root.findtext("Url").map(str::trim).filter(|u| !u.is_empty()).map(String::from).unwrap_or(format!("{FLEX_BASE}/GetStatement"));
    let agent = http::agent(Duration::from_secs(60));
    for attempt in 0..8u64 {
        // generation is asynchronous server-side
        sleep(Duration::from_secs(if attempt == 0 { 1 } else { (2 * attempt).min(10) }));
        let url = format!("{base}?t={}&q={}&v=3", escape(&token), escape(&reference));
        let r = http::get(&agent, &url, &[], "Flex GetStatement")?;
        if r.status >= 400 {
            bail!("Flex GetStatement: HTTP {}", r.status);
        }
        let root = parse_xml(&r.body)?;
        if root.tag == "FlexQueryResponse" {
            return Ok(root);
        }
        let msg = root.findtext("ErrorMessage").unwrap_or("").to_string();
        let low = msg.to_lowercase();
        if !low.contains("progress") && !low.contains("try again") {
            bail!("Flex GetStatement: {}{}", flex_code(&root), if msg.is_empty() { root.tag.clone() } else { msg });
        }
    }
    bail!("Flex GetStatement: statement not ready after retries")
}

/// Write one parsed statement (split out so tests can feed synthetic XML).
pub fn store_parsed(conn: &Connection, p: &Parsed) -> Result<usize> {
    let added = db::upsert_ibkr_trades(conn, &p.trades)?;
    db::replace_ibkr_state(conn, &p.positions, &p.cash)?;
    if !p.nav.is_empty() {
        db::upsert_ibkr_nav(conn, &p.nav)?;
    }
    if !p.flows.is_empty() {
        db::upsert_ibkr_cash_flows(conn, &p.flows)?;
    }
    if !p.income.is_empty() {
        db::upsert_ibkr_income(conn, &p.income)?;
    }
    db::upsert_raw(
        conn,
        "ibkr",
        "flex_statement",
        &json!({"as_of": p.as_of, "base_ccy": p.base_ccy, "trades": p.trades, "positions": p.positions, "cash": p.cash}).to_string(),
    )?;
    Ok(added)
}

pub fn fetch(conn: &Connection, log: Log) -> Result<()> {
    let root = fetch_statement()?;
    status::record_sections("ibkr", sections(&root));
    let rate = RateResolver::load(conn);
    let p = parse_statement(&root, &rate)?;
    let added = store_parsed(conn, &p)?;
    log(&format!(
        "  IBKR: {} trade(s) ({added} new/updated), {} position(s), {} cash balance(s), {} daily NAV row(s), {} cash flow(s), {} income row(s), as of {}",
        p.trades.len(),
        p.positions.len(),
        p.cash.len(),
        p.nav.len(),
        p.flows.len(),
        p.income.len(),
        p.as_of
    ));
    Ok(())
}

/// No-op for the crypto live sweep: IBKR holds equities, valued from the
/// stored Flex snapshot, and statement generation is far too slow for the
/// live page's 3s budget.
pub fn live_balances() -> BTreeMap<String, f64> {
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<FlexQueryResponse queryName="q" type="AF">
<FlexStatements count="2">
<FlexStatement accountId="U1" fromDate="20260929" toDate="20260930" whenGenerated="20261001;080000">
<CashReport><CashReportCurrency currency="BASE_SUMMARY" endingCash="100" /></CashReport>
<Trades><Trade symbol="NVDA" description="NVIDIA CORP" currency="USD" quantity="0.5" tradePrice="200" netCash="-101" ibCommission="-1" fxRateToBase="7.8" tradeID="T1" dateTime="20260930;143000" levelOfDetail="EXECUTION" /></Trades>
</FlexStatement>
<FlexStatement accountId="U1" fromDate="20260930" toDate="20261001" whenGenerated="20261002;080000">
<AccountInformation currency="HKD" />
<OpenPositions>
<OpenPosition symbol="NVDA" description="NVIDIA CORP" currency="USD" position="0.5" markPrice="210" positionValue="105" costBasisMoney="101" fxRateToBase="7.8" reportDate="20261001" levelOfDetail="SUMMARY" />
<OpenPosition symbol="NVDA" position="0.5" positionValue="105" levelOfDetail="LOT" />
</OpenPositions>
<CashReport>
<CashReportCurrency currency="BASE_SUMMARY" endingCash="50" />
<CashReportCurrency currency="USD" endingCash="10" />
<CashReportCurrency currency="EUR" endingCash="0.001" />
</CashReport>
<Trades>
<Trade symbol="NVDA" currency="USD" quantity="0.5" tradePrice="200" netCash="-101" tradeID="T1" dateTime="20260930;143000" />
<Trade symbol="NVDA" currency="USD" quantity="0.5" tradePrice="200" levelOfDetail="ORDER" tradeID="O1" />
<Trade symbol="VT" currency="USD" quantity="2" tradePrice="100" tradeMoney="-200" tradeDate="20261001" />
</Trades>
<CashTransactions>
<CashTransaction type="Deposits/Withdrawals" currency="HKD" amount="1000" dateTime="20260915;100000" transactionID="C1" description="CASH RECEIPTS" />
<CashTransaction type="Dividends" currency="USD" amount="1" transactionID="C2" />
<CashTransaction type="Deposits/Withdrawals" currency="USD" amount="100" reportDate="20260920" description="x" />
<CashTransaction type="Withholding Tax" currency="USD" amount="-0.3" symbol="NVDA" fxRateToBase="7.8" dateTime="20260925;000000" transactionID="C4" description="NVDA(US67066G1040) Cash Dividend USD 0.01 per Share - US Tax" />
<CashTransaction type="Payment In Lieu Of Dividends" currency="USD" amount="2" reportDate="20260926" description="VT(US9220427424) Payment in Lieu of Dividend" />
</CashTransactions>
</FlexStatement>
</FlexStatements>
</FlexQueryResponse>"#;

    fn rates() -> (rusqlite::Connection, RateResolver) {
        let c = crate::db::memory().unwrap();
        c.execute_batch("INSERT INTO fx_rates VALUES('USD','HKD','2026-09-01',7.8,0),('USD','HKD','2026-10-01',7.85,0);").unwrap();
        let r = RateResolver::load(&c);
        (c, r)
    }

    #[test]
    fn income_rows_stored_apart_and_refetch_adds_nothing() {
        let root = parse_xml(XML.as_bytes()).unwrap();
        let (c, rate) = rates();
        let p = parse_statement(&root, &rate).unwrap();
        assert_eq!(p.flows.len(), 2, "dividends never become external flows");
        let kinds: Vec<&str> = p.income.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(kinds, ["Dividends", "Withholding Tax", "Payment In Lieu Of Dividends"]);
        let wht = &p.income[1];
        assert_eq!((wht.income_id.as_str(), wht.symbol.as_str(), wht.date.as_str(), wht.amount, wht.amount_base),
                   ("C4", "NVDA", "2026-09-25", -0.3, -2.34));
        let pil = &p.income[2];
        assert_eq!((pil.symbol.as_str(), pil.date.as_str()), ("VT", "2026-09-26"), "symbol from the description");
        assert!(pil.income_id.starts_with("2026-09-26|Payment In Lieu Of Dividends|USD|2.0|"), "{}", pil.income_id);

        assert_eq!(db::upsert_ibkr_income(&c, &p.income).unwrap(), 3);
        store_parsed(&c, &p).unwrap();
        // the same statement again (a refetch) adds nothing
        let again = parse_statement(&parse_xml(XML.as_bytes()).unwrap(), &rate).unwrap();
        assert_eq!(db::upsert_ibkr_income(&c, &again.income).unwrap(), 0);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM ibkr_income", [], |r| r.get(0)).unwrap();
        let flows: i64 = c.query_row("SELECT COUNT(*) FROM ibkr_cash_flows", [], |r| r.get(0)).unwrap();
        assert_eq!((n, flows), (3, 2));
    }

    #[test]
    fn income_table_stays_out_of_the_shared_schema() {
        // connect() must leave sqlite_master as SCHEMA has it: the table
        // appears only once IBKR income is written
        let c = crate::db::memory().unwrap();
        let has = |c: &rusqlite::Connection| -> i64 {
            c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='ibkr_income'", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(has(&c), 0);
        db::upsert_ibkr_income(&c, &[]).unwrap();
        assert_eq!(has(&c), 1);
    }

    #[test]
    fn sections_of_a_statement() {
        let s = sections(&parse_xml(XML.as_bytes()).unwrap());
        assert_eq!((s.trades, s.open_positions, s.cash_report, s.nav), (4, 2, 4, 2));
        assert_eq!((s.cash_transactions, s.dividends, s.withholding), (5, 2, 1));
        let err = parse_xml(b"<FlexStatementResponse><Status>Fail</Status><ErrorCode>1012</ErrorCode><ErrorMessage>Token has expired.</ErrorMessage></FlexStatementResponse>").unwrap();
        assert_eq!(flex_code(&err), "error 1012: ");
    }

    #[test]
    fn parses_synthetic_statement() {
        let root = parse_xml(XML.as_bytes()).unwrap();
        let (c, rate) = rates();
        let p = parse_statement(&root, &rate).unwrap();
        assert_eq!((p.as_of.as_str(), p.base_ccy.as_str()), ("2026-10-01", "HKD"));
        let ids: Vec<&str> = p.trades.iter().map(|t| t.trade_id.as_str()).collect();
        assert_eq!(ids, ["T1", "T1", "VT-None-2.0"], "order rows skipped, fallback id built");
        assert_eq!(p.trades[0].ts, 1790778600000);
        assert_eq!(p.trades[0].fx_to_base, 7.8);
        assert_eq!(p.trades[2].amount, -200.0, "netCash missing -> tradeMoney");
        assert_eq!(p.positions.len(), 1);
        assert_eq!(p.positions[0].value_base, 819.0);
        assert_eq!(p.positions[0].cost_ccy, Some(101.0));
        assert_eq!(p.cash.len(), 1, "dust and BASE_SUMMARY dropped");
        assert_eq!((p.cash[0].currency.as_str(), p.cash[0].amount_base), ("USD", 78.5));
        assert_eq!(p.nav.len(), 2);
        assert_eq!((p.nav[1].cash_base, p.nav[1].positions_base, p.nav[1].nav_base), (50.0, 819.0, 869.0));
        assert_eq!(p.flows.len(), 2);
        assert_eq!(p.flows[1].flow_id, "2026-09-20-USD-100.0");
        assert_eq!(p.flows[1].amount_base, 780.0);

        // writes: trades merge by id, positions/cash snapshot-rewrite
        store_parsed(&c, &p).unwrap();
        store_parsed(&c, &p).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM ibkr_trades", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM ibkr_positions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let raw: String = c.query_row("SELECT payload FROM crypto_raw_sources WHERE exchange='ibkr'", [], |r| r.get(0)).unwrap();
        assert!(raw.contains("\"base_ccy\":\"HKD\""));
    }

    #[test]
    fn base_ccy_fallbacks() {
        let xml = r#"<FlexQueryResponse><FlexStatements><FlexStatement toDate="20260101">
            <Trades><Trade symbol="X" currency="eur" quantity="1" fxRateToBase="1" /></Trades>
            </FlexStatement></FlexStatements></FlexQueryResponse>"#;
        let (_c, rate) = rates();
        let p = parse_statement(&parse_xml(xml.as_bytes()).unwrap(), &rate).unwrap();
        assert_eq!(p.base_ccy, "EUR");
        let empty = parse_xml(b"<FlexQueryResponse><FlexStatements/></FlexQueryResponse>").unwrap();
        assert!(parse_statement(&empty, &rate).is_err());
    }

    #[test]
    fn send_request_response_shape() {
        let xml = b"<FlexStatementResponse timestamp='x'><Status>Success</Status><ReferenceCode>123</ReferenceCode><Url>https://x/GetStatement</Url></FlexStatementResponse>";
        let r = parse_xml(xml).unwrap();
        assert_eq!(r.findtext("Status"), Some("Success"));
        assert_eq!(r.findtext("ReferenceCode"), Some("123"));
    }

    #[test]
    fn round2_rounds_the_exact_value() {
        assert_eq!(round2(2.675), 2.67);
        assert_eq!(round2(819.0000001), 819.0);
        assert_eq!(iso(Some("20260710")), "2026-07-10");
        assert_eq!(iso(Some("2026-07-10;x")), "2026-07-10");
    }

    /// The parity snapshot's stored trades round-trip through the raw
    /// flex_statement payload stored alongside them.
    #[test]
    fn parity_raw_payload_matches_tables() {
        let Some(conn) = crate::fetch::parity_db() else { return };
        let raw: String = conn.query_row("SELECT payload FROM crypto_raw_sources WHERE exchange='ibkr'", [], |r| r.get(0)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        for t in v["trades"].as_array().unwrap() {
            let (qty, amount): (f64, f64) = conn
                .query_row("SELECT qty, amount FROM ibkr_trades WHERE trade_id=?", [t["trade_id"].as_str().unwrap()], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            assert_eq!((qty, amount), (t["qty"].as_f64().unwrap(), t["amount"].as_f64().unwrap()));
        }
    }
}
