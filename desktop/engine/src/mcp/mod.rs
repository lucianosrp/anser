//! The read-only MCP server: protocol (here) and tools ([`tools`]), over a
//! [`Source`] of the user's data. Nothing in it writes.
//!
//! A minimal hand-written JSON-RPC 2.0 server: `initialize`, `ping`,
//! `tools/list`, `tools/call` (notifications get no reply). It speaks
//! protocol versions 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05
//! (the client's when supported, else the newest) and is stateless, so the
//! same [`handle_payload`] serves a line-per-message stdio session
//! ([`serve`], `anser-server mcp`) and a Streamable HTTP POST (the server's
//! `/mcp`). The tool names, descriptions and input schemas are
//! assets/mcp_tools.json, the `tools/list` answer verbatim.
//!
//! The tools read through [`Source`]: a connection, the market feed to
//! price with, the core ledger and the net worth. The server's live
//! implementation and a future desktop one (its local replica, the offline
//! feed) differ only there.
//!
//! Results follow the conventions clients already know: the JSON value as
//! one text block (compact, non-ASCII kept) plus `structuredContent`. Tool
//! failures (a bad argument, an unknown name) are `isError: true` results;
//! protocol errors are JSON-RPC errors.

pub mod tools;

use std::sync::{Arc, OnceLock};

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};

use crate::views::{Ctx, MarketFeed};
use finance_core::Ledger;

const TOOLS_JSON: &str = include_str!("../../assets/mcp_tools.json");
/// Newest first: the answer to a client asking for one we don't know.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// What the tools read.
pub trait Source {
    /// The user's database.
    fn conn(&self) -> &Connection;
    /// What prices the views fetch through (the offline feed on a replica).
    fn feed(&self) -> &dyn MarketFeed;
    /// Every bank row, crypto event and broker trade (finance_core::Ledger,
    /// which the engine can't load itself), for the subscription detection
    /// and the DCA maths. Implementations keep it between calls.
    fn ledger(&self) -> Result<Arc<Ledger>>;
    /// The net worth the summary and holdings read: the live valuation, or
    /// by default the network-free snapshot one (memoized).
    fn net_worth(&self) -> Value {
        let ctx = Ctx::new(self.conn(), self.feed());
        ctx.net_worth_offline().clone()
    }
}

/// "A", "A and B", "A, B and C".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A source label's brand: "Revolut (GBP, legacy)" -> "Revolut", "Trezor
/// (cold wallet)" -> "Trezor".
fn brand(label: &str) -> String {
    label.split(" (").next().unwrap_or(label).split_whitespace().next().unwrap_or(label).to_string()
}

/// The `initialize` instructions, from what this server has (the
/// capabilities' sources with data, the investment setup's marked accounts)
/// and the profile's base currency, instead of naming one person's banks.
pub fn describe(caps: &Value, investments: &[String], base: &str) -> String {
    let sources: Vec<&Value> = caps.get("sources").and_then(Value::as_array).map(|v| v.iter().filter(|s| s["has_data"] == true).collect()).unwrap_or_default();
    let brands = |pred: &dyn Fn(&Value) -> bool| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in sources.iter().filter(|s| pred(s)) {
            let b = brand(s["label"].as_str().unwrap_or(""));
            if !b.is_empty() && !out.contains(&b) {
                out.push(b);
            }
        }
        out
    };
    let banks = brands(&|s| s["kind"] == "bank");
    // exchanges first, then wallets
    let mut crypto = brands(&|s| s["group"] == "crypto" && s["kind"] == "exchange");
    for b in brands(&|s| s["group"] == "crypto" && s["kind"] != "exchange") {
        if !crypto.contains(&b) {
            crypto.push(b);
        }
    }
    let mut parts = Vec::new();
    parts.push(if banks.is_empty() { "bank balances".to_string() } else { format!("bank balances ({})", banks.join(", ")) });
    parts.push("spending analytics".to_string());
    if !investments.is_empty() {
        parts.push(format!("{} investments", and_list(investments)));
    }
    if !crypto.is_empty() {
        parts.push(format!("the {} crypto ledger", crypto.join("/")));
    }
    format!(
        "Personal-finance data, read-only: {}. Call summary first: it gives net worth, this month and the last 12 months, \
         how fresh the data is, and the exact account, category and investment-account names the other tools take as filters. \
         All money is {base} unless a currency field says otherwise. Months are 'YYYY-MM', dates 'YYYY-MM-DD'.",
        match parts.as_slice() {
            [rest @ .., last] if rest.len() >= 2 => format!("{}, and {last}", rest.join(", ")),
            _ => and_list(&parts),
        }
    )
}

/// [`describe`] for what `src` has.
pub fn instructions(src: &dyn Source) -> String {
    let ctx = Ctx::new(src.conn(), src.feed());
    let caps = crate::views::capabilities::capabilities(&ctx);
    let accounts: Vec<String> = ctx.invest().accounts.iter().filter(|a| a.valued_by != "cost").map(|a| a.name.clone()).collect();
    describe(&caps, &accounts, &crate::views::fx::base_currency(src.conn()))
}

/// The tool list (name, description, inputSchema).
pub fn tool_list() -> &'static Value {
    static TOOLS: OnceLock<Value> = OnceLock::new();
    TOOLS.get_or_init(|| serde_json::from_str(TOOLS_JSON).expect("assets/mcp_tools.json is valid JSON"))
}

/// Serve MCP over this process's stdin/stdout until stdin closes.
pub fn run_stdio(src: &dyn Source) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(src, stdin.lock(), stdout.lock())
}

/// The loop behind [`run_stdio`], on any line reader/writer (tests pipe a
/// scripted session through it).
pub fn serve<R: BufRead, W: Write>(src: &dyn Source, input: R, mut out: W) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Err(_) => Some(error(Value::Null, -32700, "Parse error")),
            Ok(msg) => handle_payload(src, &msg),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// A JSON-RPC payload: one message, or a batch of them. None when it had
/// nothing to answer (notifications only): Streamable HTTP's 202.
pub fn handle_payload(src: &dyn Source, payload: &Value) -> Option<Value> {
    match payload {
        Value::Array(batch) if batch.is_empty() => Some(error(Value::Null, -32600, "Invalid Request")),
        Value::Array(batch) => {
            let replies: Vec<Value> = batch.iter().filter_map(|m| handle(src, m)).collect();
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        msg => handle(src, msg),
    }
}

/// The reply to a body that isn't JSON.
pub fn parse_error() -> Value {
    error(Value::Null, -32700, "Parse error")
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// One JSON-RPC message -> its reply (None for notifications).
pub fn handle(src: &dyn Source, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let Some(method) = msg.get("method").and_then(|m| m.as_str()) else {
        // a response or garbage: answer only if it carried an id
        return id.map(|id| error(id, -32600, "Invalid Request"));
    };
    let id = id?; // notifications (notifications/initialized, cancelled, ...) get no reply
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
            let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "anser", "version": finance_core::VERSION},
                "instructions": instructions(src),
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tool_list()}),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(|n| n.as_str()) else {
                return Some(error(id, -32602, "Invalid params: missing tool name"));
            };
            let args = match params.get("arguments") {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(m)) => m.clone(),
                Some(_) => return Some(error(id, -32602, "Invalid params: arguments must be an object")),
            };
            call_tool(src, name, &args)
        }
        _ => return Some(error(id, -32601, &format!("Method not found: {method}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn tool_error(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Validates the arguments against the tool's inputSchema (missing /
/// unexpected / wrongly typed / out-of-enum arguments) and returns the
/// error message on failure: one entry per argument, with what was wrong
/// and the value given. A null optional argument counts as absent.
fn validate(tool: &str, schema: &Value, args: &Map<String, Value>) -> Result<(), String> {
    let props = schema["properties"].as_object().cloned().unwrap_or_default();
    let mut errs: Vec<String> = Vec::new();
    let input = Value::Object(args.clone());
    let input_s = input.to_string();
    for req in schema["required"].as_array().into_iter().flatten().filter_map(|r| r.as_str()) {
        if args.get(req).is_none_or(Value::is_null) {
            errs.push(format!(
                "{req}\n  Missing required argument [type=missing_argument, input_value={input_s}, input_type=dict]"
            ));
        }
    }
    for (k, v) in args {
        let Some(p) = props.get(k) else {
            errs.push(format!(
                "{k}\n  Unexpected keyword argument [type=unexpected_keyword_argument, input_value={v}, input_type={}]",
                type_name(v)
            ));
            continue;
        };
        if v.is_null() {
            continue;
        }
        let ty = p["type"].as_str().unwrap_or("string");
        let ok = match (ty, v) {
            ("string", Value::String(_)) => true,
            ("integer", Value::Number(n)) => n.is_i64() || n.as_f64().is_some_and(|x| x.fract() == 0.0),
            ("integer", Value::String(s)) => s.trim().parse::<i64>().is_ok(),
            ("number", Value::Number(_)) => true,
            ("number", Value::String(s)) => s.trim().parse::<f64>().is_ok(),
            ("boolean", Value::Bool(_)) => true,
            _ => false,
        };
        if !ok {
            let (what, code) = match ty {
                "integer" => ("a valid integer", "int_type"),
                "number" => ("a valid number", "float_type"),
                "boolean" => ("a valid boolean", "bool_type"),
                _ => ("a valid string", "string_type"),
            };
            errs.push(format!("{k}\n  Input should be {what} [type={code}, input_value={v}, input_type={}]", type_name(v)));
            continue;
        }
        if let (Some(options), Some(s)) = (p["enum"].as_array(), v.as_str()) {
            if !options.iter().any(|o| o == s) {
                let list: Vec<String> = options.iter().filter_map(|o| o.as_str()).map(|o| format!("'{o}'")).collect();
                errs.push(format!(
                    "{k}\n  Input should be {} [type=literal_error, input_value={v}, input_type={}]",
                    and_list(&list).replacen(" and ", " or ", 1),
                    type_name(v)
                ));
            }
        }
    }
    if errs.is_empty() {
        return Ok(());
    }
    let n = errs.len();
    Err(format!(
        "{n} validation error{} for call[{tool}]\n{}",
        if n == 1 { "" } else { "s" },
        errs.join("\n")
    ))
}

/// tools/call: run one tool, MCP-shaped result.
pub fn call_tool(src: &dyn Source, name: &str, args: &Map<String, Value>) -> Value {
    let Some(spec) = tool_list().as_array().and_then(|t| t.iter().find(|t| t["name"] == name)) else {
        return tool_error(format!("Unknown tool: '{name}'"));
    };
    if let Err(msg) = validate(name, &spec["inputSchema"], args) {
        return tool_error(msg);
    }
    match tools::call(src, name, args) {
        Ok(v) => json!({
            "content": [{"type": "text", "text": v.to_string()}],
            "structuredContent": v,
            "isError": false,
        }),
        Err(e) => tool_error(format!("Error calling tool '{name}': {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(key: &str, label: &str, kind: &str, group: &str) -> Value {
        json!({"key": key, "label": label, "kind": kind, "group": group, "has_data": true})
    }

    #[test]
    fn the_instructions_say_what_this_server_has() {
        let caps = json!({"sources": [
            src("n26", "N26", "bank", "banks"), src("hsbc_card", "HSBC credit card", "card", "cards"), src("hsbc", "HSBC", "bank", "banks"),
            src("revolut_old_gbp", "Revolut (GBP, legacy)", "bank", "banks"), src("revolut_new", "Revolut", "bank", "banks"),
            src("trezor", "Trezor (cold wallet)", "crypto_wallet", "crypto"), src("binance", "Binance", "exchange", "crypto"),
            src("okx", "OKX", "exchange", "crypto"),
            json!({"key": "ibkr", "label": "Interactive Brokers", "kind": "broker", "group": "broker", "has_data": false}),
        ]});
        let tail = "Call summary first: it gives net worth, this month and the last 12 months, how fresh the data is, and the exact \
                    account, category and investment-account names the other tools take as filters. All money is HKD unless a currency \
                    field says otherwise. Months are 'YYYY-MM', dates 'YYYY-MM-DD'.";
        assert_eq!(
            describe(&caps, &["HSBC FlexInvest".into(), "IBKR".into()], "HKD"),
            format!("Personal-finance data, read-only: bank balances (N26, HSBC, Revolut), spending analytics, HSBC FlexInvest \
                     and IBKR investments, and the Binance/OKX/Trezor crypto ledger. {tail}")
        );
        let eur = json!({"sources": [src("n26", "N26", "bank", "banks")]});
        assert_eq!(
            describe(&eur, &[], "EUR"),
            format!("Personal-finance data, read-only: bank balances (N26) and spending analytics. {tail}").replace("HKD", "EUR")
        );
        assert!(describe(&Value::Null, &[], "HKD").starts_with("Personal-finance data, read-only: bank balances and spending analytics."));
    }

    #[test]
    fn validation_reads_like_the_established_errors() {
        let schema = json!({"properties": {"a": {"type": "string", "enum": ["x", "y"]}, "n": {"type": "integer"}}, "required": ["a"]});
        let call = |v: Value| validate("t", &schema, v.as_object().unwrap());
        assert!(call(json!({"a": "x", "n": "7"})).is_ok());
        assert!(call(json!({"a": "x", "n": null})).is_ok(), "a null optional is absent");
        assert!(call(json!({})).unwrap_err().starts_with("1 validation error for call[t]\na\n  Missing required argument"));
        assert!(call(json!({"a": "z"})).unwrap_err().contains("Input should be 'x' or 'y' [type=literal_error"));
        assert!(call(json!({"a": "x", "n": 1.5})).unwrap_err().contains("Input should be a valid integer"));
        assert!(call(json!({"a": "x", "q": 1})).unwrap_err().contains("Unexpected keyword argument"));
    }
}
