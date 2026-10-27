//! The MCP server (finance_engine::mcp): protocol behaviour through a
//! scripted stdio session, and every tool on the demo household (the
//! made-up Alex Demo of engine/src/demo.rs, offline, today pinned): shapes,
//! validation errors, and the totals that must tie out across tools.
//! The HTTP side (/mcp, keys, the switch) is tests/mcp_http.rs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::Result;
use finance_engine::mcp::{self, Source};
use finance_engine::views::{cache, Ctx, MarketFeed, Offline};
use serde_json::{json, Map, Value};

/// The demo household's db, built once for the whole file.
fn demo_db() -> PathBuf {
    static DB: OnceLock<PathBuf> = OnceLock::new();
    DB.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("finance-mcp-tests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // an empty home of its own: nothing of any real tree is read
        std::env::set_var("FINANCE_HOME", dir.join("home"));
        for k in ["FINANCE_DATA_DIR", "FINANCE_DB", "FINANCE_TRANSACTIONS_DIR", "FINANCE_FORMATS_DIR"] {
            std::env::remove_var(k);
        }
        std::env::set_var("FINANCE_OFFLINE", "1");
        std::env::set_var("FINANCE_TODAY", "2026-09-20");
        finance_engine::demo::write(&dir, chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()).unwrap();
        dir.join("finance.db")
    })
    .clone()
}

/// What a replica-backed source looks like: the db, the offline feed, the
/// ledger read from the same file.
struct Demo {
    conn: rusqlite::Connection,
    path: PathBuf,
}

impl Demo {
    fn new() -> Self {
        let path = demo_db();
        Demo { conn: rusqlite::Connection::open(&path).unwrap(), path }
    }
}

impl Source for Demo {
    fn conn(&self) -> &rusqlite::Connection {
        &self.conn
    }
    fn feed(&self) -> &dyn MarketFeed {
        &Offline
    }
    fn ledger(&self) -> Result<Arc<finance_core::Ledger>> {
        Ok(Arc::new(finance_core::Ledger::load(&self.path)?))
    }
}

/// A tool call's structuredContent; panics with the error text on isError.
fn tool(s: &Demo, name: &str, args: Value) -> Value {
    let r = mcp::call_tool(s, name, args.as_object().unwrap());
    assert_eq!(r["isError"], false, "{name} {args}: {}", r["content"][0]["text"]);
    // the text block is the same JSON, compact
    assert_eq!(r["content"][0]["text"].as_str().unwrap(), r["structuredContent"].to_string(), "{name}: text and structuredContent agree");
    r["structuredContent"].clone()
}

/// The error text of a call that must fail.
fn tool_err(s: &Demo, name: &str, args: Value) -> String {
    let r = mcp::call_tool(s, name, args.as_object().unwrap());
    assert_eq!(r["isError"], true, "{name} {args} should fail: {r}");
    r["content"][0]["text"].as_str().unwrap().to_string()
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 0.06, "{what}: {a} vs {b}");
}

fn run(src: &dyn Source, session: &str) -> HashMap<String, Value> {
    let mut out = Vec::new();
    mcp::serve(src, session.as_bytes(), &mut out).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            (if v.is_array() { "batch".to_string() } else { v["id"].to_string() }, v)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// protocol
// ---------------------------------------------------------------------------

const TOOLS: [&str; 8] = ["summary", "trend", "breakdown", "transactions", "recurring", "holdings", "performance", "activity"];

#[test]
fn a_scripted_session() {
    let s = Demo::new();
    let session = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"trend","arguments":{}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}
{"jsonrpc":"2.0","id":5,"method":"nope"}
not json
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"transactions","arguments":{"limit":3,"bogus":1}}}
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"breakdown","arguments":{"by":"planet"}}}
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"transactions","arguments":{"limit":"two"}}}
{"jsonrpc":"2.0","id":9,"method":"ping"}
{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"summary","arguments":[]}}
{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"arguments":{}}}
{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"trend","arguments":{"metric":"income","from_month":"2026-1"}}}
{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"transactions","arguments":{"limit":"2","text":null,"account":null}}}
{"jsonrpc":"2.0","id":14,"result":{}}
[{"jsonrpc":"2.0","id":15,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/cancelled"},{"jsonrpc":"2.0","id":16,"method":"ping"}]
"#;
    let r = run(&s, session);
    // one reply per request (the batch's is one line), none for the notifications
    assert_eq!(r.len(), 16, "{:#?}", r.keys().collect::<Vec<_>>());

    let init = &r["1"]["result"];
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["serverInfo"]["name"], "anser");
    assert!(init["capabilities"]["tools"].is_object());
    let ins = init["instructions"].as_str().unwrap();
    assert!(ins.contains("Call summary first") && ins.contains("EUR") && ins.contains("N26"), "{ins}");

    let list = r["2"]["result"]["tools"].as_array().unwrap();
    assert_eq!(list.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), TOOLS);
    for t in list {
        assert!(t["description"].as_str().unwrap().len() > 60, "{} says when to use it", t["name"]);
        assert_eq!(t["inputSchema"]["type"], "object");
        // nothing names a bank or a broker
        let text = t.to_string().to_lowercase();
        for brand in ["ibkr", "interactive", "hsbc", "flexinvest", "n26", "revolut", "binance", "okx"] {
            assert!(!text.contains(brand), "{} names {brand}", t["name"]);
        }
    }
    let msg = |id: &str| r[id]["result"]["content"][0]["text"].as_str().unwrap().to_string();
    assert!(msg("3").starts_with("1 validation error for call[trend]\nmetric\n  Missing required argument"));
    assert_eq!(msg("4"), "Unknown tool: 'nope'");
    assert_eq!(r["5"]["error"]["code"], -32601);
    assert_eq!(r["null"]["error"]["code"], -32700);
    assert!(msg("6").contains("Unexpected keyword argument"));
    assert!(msg("7").contains("Input should be 'category', 'merchant', 'account' or 'currency' [type=literal_error"));
    assert!(msg("8").contains("Input should be a valid integer"));
    assert_eq!(r["9"]["result"], json!({}));
    assert_eq!(r["10"]["error"]["code"], -32602);
    assert_eq!(r["11"]["error"]["code"], -32602);
    assert!(msg("12").contains("from_month must be YYYY-MM, got '2026-1'"));
    assert_eq!(r["13"]["result"]["isError"], false, "null optionals count as absent");
    assert_eq!(r["14"]["error"]["code"], -32600, "a stray response with an id");
    let batch = r["batch"].as_array().unwrap();
    assert_eq!(batch.iter().map(|b| b["id"].clone()).collect::<Vec<_>>(), [json!(15), json!(16)], "the notification in the batch has no reply");
}

#[test]
fn the_batch_and_the_versions() {
    let s = Demo::new();
    // a batch answers as an array, in order, without the notification's reply
    let reply = mcp::handle_payload(
        &s,
        &json!([{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":"b","method":"ping"}]),
    )
    .unwrap();
    assert_eq!(reply.as_array().unwrap().iter().map(|r| r["id"].clone()).collect::<Vec<_>>(), [json!(1), json!("b")]);
    // notifications alone: nothing to say (HTTP 202)
    assert!(mcp::handle_payload(&s, &json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
    assert!(mcp::handle_payload(&s, &json!([{"jsonrpc":"2.0","method":"notifications/initialized"}])).is_none());
    assert_eq!(mcp::handle_payload(&s, &json!([])).unwrap()["error"]["code"], -32600);
    // the client's version when supported, else the newest
    for (asked, answered) in [("2025-11-25", "2025-11-25"), ("2025-06-18", "2025-06-18"), ("2025-03-26", "2025-03-26"), ("2024-11-05", "2024-11-05"), ("2023-01-01", "2025-11-25"), ("", "2025-11-25")] {
        let r = mcp::handle(&s, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":asked}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], answered, "asked {asked:?}");
    }
    let r = mcp::handle(&s, &json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).unwrap();
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
}

// ---------------------------------------------------------------------------
// the tools, on the demo household
// ---------------------------------------------------------------------------

#[test]
fn summary_is_the_start_here_tool() {
    let s = Demo::new();
    let v = tool(&s, "summary", json!({}));
    assert_eq!(v["currency"], "EUR");
    let nw = &v["net_worth"];
    let parts: f64 = ["cash", "investments", "crypto", "owed_to_you"].iter().map(|k| f(&nw[*k])).sum();
    close(f(&nw["total"]), parts, "net worth = its classes");
    assert!(f(&nw["total"]) > 10_000.0 && f(&nw["cash"]) > 0.0 && f(&nw["investments"]) > 0.0 && f(&nw["crypto"]) > 0.0);
    assert!(nw.get("liabilities").is_none(), "no mortgage in the demo: no empty field");
    let t = &v["trailing_12_months"];
    assert_eq!((t["from"].as_str(), t["to"].as_str()), (Some("2025-09"), Some("2026-08")), "the last 12 complete months");
    close(f(&t["savings"]), f(&t["income"]) - f(&t["spending"]), "savings");
    close(f(&t["savings_rate"]), f(&t["savings"]) / f(&t["income"]) * 100.0, "savings rate");
    close(f(&t["avg_monthly_spending"]), f(&t["spending"]) / 12.0, "monthly average");
    assert_eq!(v["this_month"]["month"], "2026-09");
    // the vocabulary the other tools take
    let names = |k: &str| v[k].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert!(names("accounts").contains(&"N26".to_string()) && names("accounts").contains(&"N26 · Savings".to_string()));
    assert!(names("categories").contains(&"Groceries".to_string()) && !names("categories").contains(&"Investments".to_string()));
    assert!(names("transfer_categories").contains(&"Investments".to_string()));
    assert_eq!(names("investment_accounts"), ["Interactive Brokers", "Crypto"]);
    assert_eq!((v["months"]["from"].as_str(), v["months"]["to"].as_str()), (Some("2024-12"), Some("2026-09")));
    assert!(v["freshness"]["prices_stale"].is_boolean());
    // every name it lists is accepted by the filters
    for a in names("accounts") {
        tool(&s, "transactions", json!({"account": a, "limit": 1}));
    }
    for c in names("categories") {
        tool(&s, "trend", json!({"metric": format!("category:{c}"), "from_month": "2026-01", "to_month": "2026-02"}));
    }
    for a in names("investment_accounts") {
        tool(&s, "holdings", json!({"account": a}));
        tool(&s, "performance", json!({"account": a}));
        tool(&s, "activity", json!({"account": a, "limit": 1}));
    }
    // lean: no nulls, no empty arrays or objects anywhere
    fn lean(v: &Value, at: &str) {
        match v {
            Value::Null => panic!("null at {at}"),
            Value::Array(a) => {
                assert!(!a.is_empty(), "empty array at {at}");
                a.iter().for_each(|x| lean(x, at));
            }
            Value::Object(m) => {
                assert!(!m.is_empty(), "empty object at {at}");
                m.iter().for_each(|(k, x)| lean(x, &format!("{at}.{k}")));
            }
            _ => {}
        }
    }
    lean(&v, "summary");
}

#[test]
fn income_and_spending_tie_out_across_the_tools() {
    let s = Demo::new();
    let sum = tool(&s, "summary", json!({}));
    let (income, spending) = (f(&sum["trailing_12_months"]["income"]), f(&sum["trailing_12_months"]["spending"]));
    let period = json!({"from_month": "2025-09", "to_month": "2026-08"});
    let with = |extra: Value| {
        let mut a = period.as_object().unwrap().clone();
        a.extend(extra.as_object().unwrap().clone());
        Value::Object(a)
    };
    for by in ["category", "merchant", "account", "currency"] {
        for (kind, want) in [("spending", spending), ("income", income)] {
            let b = tool(&s, "breakdown", with(json!({"by": by, "kind": kind, "top": 3})));
            close(f(&b["total"]), want, &format!("breakdown {by} {kind} = summary"));
            let rows = b["rows"].as_array().unwrap();
            assert!(rows.len() <= 4, "top 3 and Other");
            close(rows.iter().map(|r| f(&r["total"])).sum(), want, &format!("the rows sum to the total ({by} {kind})"));
            assert!((rows.iter().map(|r| f(&r["share_pct"])).sum::<f64>() - 100.0).abs() < 0.3, "shares (rounded to 0.1 each)");
            assert_eq!(rows.windows(2).filter(|w| f(&w[0]["total"]) < f(&w[1]["total"]) && w[1]["name"] != "Other").count(), 0, "biggest first");
        }
    }
    // the same months through trend
    let months = |metric: &str| {
        let t = tool(&s, "trend", with(json!({"metric": metric})));
        t["series"].as_array().unwrap().iter().map(|p| f(&p["value"])).collect::<Vec<_>>()
    };
    close(months("spending").iter().sum(), spending, "trend spending");
    close(months("income").iter().sum(), income, "trend income");
    close(months("savings").iter().sum(), income - spending, "trend savings");
    // and the spending view's own monthly trend (the dashboard's number)
    let conn = rusqlite::Connection::open(demo_db()).unwrap();
    let ctx = Ctx::offline(&conn);
    let view = cache::spending(&ctx, Some("2025-09"), Some("2026-08")).unwrap();
    let dash: f64 = view["trend"].as_array().unwrap().iter().map(|r| f(&r["spend"])).sum();
    close(dash, spending, "the spending page");
    let dash_in: f64 = view["trend"].as_array().unwrap().iter().map(|r| f(&r["income"])).sum();
    close(dash_in, income, "the spending page's income");
    // categories: the series of one category sums to its breakdown row
    let cats = tool(&s, "breakdown", with(json!({"by": "category", "top": 50})));
    let groceries = cats["rows"].as_array().unwrap().iter().find(|r| r["name"] == "Groceries").unwrap();
    close(months("category:Groceries").iter().sum(), f(&groceries["total"]), "category trend = breakdown row");
    assert!(groceries["change_pct"].is_number() && groceries["count"].as_i64().unwrap() > 20);
    let prev = tool(&s, "breakdown", json!({"by": "category", "from_month": "2024-09", "to_month": "2025-08", "top": 50}));
    let before = prev["rows"].as_array().unwrap().iter().find(|r| r["name"] == "Groceries").map(|r| f(&r["total"])).unwrap_or(0.0);
    close(f(&groceries["change_pct"]), ((f(&groceries["total"]) - before) / before * 100.0 * 10.0).round() / 10.0, "change against the previous equal period");
    assert_eq!(cats["previous_total"], tool(&s, "breakdown", json!({"by": "category", "from_month": "2024-09", "to_month": "2025-08"}))["total"]);
}

#[test]
fn trend_series_and_statistics() {
    let s = Demo::new();
    let t = tool(&s, "trend", json!({"metric": "net_worth", "from_month": "2025-01", "to_month": "2026-09"}));
    let series = t["series"].as_array().unwrap();
    assert_eq!(series.len(), 21);
    let vals: Vec<f64> = series.iter().map(|p| f(&p["value"])).collect();
    close(f(&t["min"]["value"]), vals.iter().cloned().fold(f64::MAX, f64::min), "min");
    close(f(&t["max"]["value"]), vals.iter().cloned().fold(f64::MIN, f64::max), "max");
    close(f(&t["avg"]), vals.iter().sum::<f64>() / vals.len() as f64, "avg");
    close(f(&t["change"]["abs"]), vals[vals.len() - 1] - vals[0], "change");
    close(f(&t["change"]["pct"]), (vals[vals.len() - 1] - vals[0]) / vals[0] * 100.0, "change pct");
    // the last point is the net worth summary shows (same view, same month)
    close(vals[vals.len() - 1], f(&tool(&s, "summary", json!({}))["net_worth"]["total"]), "now");
    // an account's balance: the savings account only grows by what goes in
    let a = tool(&s, "trend", json!({"metric": "account:savings", "from_month": "2026-06", "to_month": "2026-09"}));
    assert_eq!(a["series"].as_array().unwrap().len(), 4);
    assert_eq!(tool(&s, "trend", json!({"metric": "account:N26 · Savings", "from_month": "2026-06", "to_month": "2026-09"}))["series"], a["series"]);
    // rates are percents, one decimal, and flagged
    let r = tool(&s, "trend", json!({"metric": "savings_rate"}));
    assert_eq!(r["unit"], "percent");
    assert!(r.get("currency").is_none());
    assert!(r["series"].as_array().unwrap().iter().all(|p| (f(&p["value"]) * 10.0).fract().abs() < 1e-6));
    // explicit bounds are taken as given (the default period is the one clipped to the data)
    let e = tool(&s, "trend", json!({"metric": "income", "from_month": "2019-01", "to_month": "2024-12"}));
    assert_eq!(e["series"].as_array().unwrap().len(), 72, "explicit bounds are honoured");
    // errors that say what is valid
    assert!(tool_err(&s, "trend", json!({"metric": "vibes"})).contains("metric must be net_worth"));
    assert!(tool_err(&s, "trend", json!({"metric": "category:Nope"})).contains("Unknown category 'Nope'. Known: "));
    assert!(tool_err(&s, "trend", json!({"metric": "account:Nope"})).contains("Unknown account 'Nope'. Known: N26"));
    assert!(tool_err(&s, "trend", json!({"metric": "category:Investments"})).contains("transfer category"));
    assert!(tool_err(&s, "trend", json!({"metric": "income", "from_month": "2026-05", "to_month": "2026-01"})).contains("is after"));
}

#[test]
fn transactions_filter_sort_and_page() {
    let s = Demo::new();
    let all = tool(&s, "transactions", json!({"limit": 500}));
    let n = all["matches"].as_i64().unwrap();
    assert!(n > 800);
    assert_eq!(all["rows"].as_array().unwrap().len(), 500);
    let dates: Vec<&str> = all["rows"].as_array().unwrap().iter().map(|r| r["date"].as_str().unwrap()).collect();
    assert!(dates.windows(2).all(|w| w[0] >= w[1]), "newest first");
    // paging walks every row once
    let mut seen = 0;
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut a = json!({"limit": 200});
        if let Some(c) = &cursor {
            a["cursor"] = json!(c);
        }
        let p = tool(&s, "transactions", a);
        seen += p["rows"].as_array().unwrap().len() as i64;
        pages += 1;
        match p["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!((seen, pages), (n, (n as usize).div_ceil(200)));
    assert!(tool_err(&s, "transactions", json!({"cursor": "garbage"})).contains("cursor"));
    // filters
    let rent = tool(&s, "transactions", json!({"text": "apartment rent", "direction": "out", "limit": 100}));
    assert_eq!(rent["matches"], 22);
    assert!(rent["rows"].as_array().unwrap().iter().all(|r| r["category"] == "Rent" && f(&r["amount"]) < 0.0 && r["currency"] == "EUR"));
    close(f(&rent["net_total"]), -27500.0, "the rent paid, in the base currency");
    let ins = tool(&s, "transactions", json!({"direction": "in", "category": "salary", "from_date": "2026-01-01", "to_date": "2026-03-31", "limit": 50}));
    assert_eq!(ins["matches"], 3);
    assert!(ins["rows"].as_array().unwrap().iter().all(|r| f(&r["amount"]) > 0.0 && r["category"] == "Salary"));
    let big = tool(&s, "transactions", json!({"min_amount": 1000, "sort": "amount", "limit": 100}));
    let rows = big["rows"].as_array().unwrap();
    assert!(!rows.is_empty() && rows.iter().all(|r| f(&r["amount"]).abs() >= 1000.0), "{big}");
    // largest first, by value in the base currency (each row shows its own)
    let euros: Vec<f64> = rows.iter().filter(|r| r["currency"] == "EUR").map(|r| f(&r["amount"]).abs()).collect();
    assert!(euros.len() > 20 && euros.windows(2).all(|w| w[0] >= w[1]), "largest first: {euros:?}");
    let small = tool(&s, "transactions", json!({"max_amount": 5, "limit": 100}));
    assert!(small["rows"].as_array().unwrap().iter().all(|r| f(&r["amount"]).abs() <= 5.0));
    let pocket = tool(&s, "transactions", json!({"account": "n26 · savings", "limit": 100}));
    assert!(pocket["rows"].as_array().unwrap().iter().all(|r| r["account"] == "N26 · Savings"));
    assert!(tool_err(&s, "transactions", json!({"account": "Nowhere"})).contains("Unknown account 'Nowhere'. Known: N26"));
    assert!(tool_err(&s, "transactions", json!({"from_date": "yesterday"})).contains("from_date must be YYYY-MM-DD"));
    // a foreign currency keeps its own amount
    let usd = tool(&s, "transactions", json!({"account": "Revolut · USD", "limit": 1}));
    assert_eq!(usd["rows"][0]["currency"], "USD");
}

#[test]
fn recurring_lists_the_charges_and_their_cost() {
    let s = Demo::new();
    let v = tool(&s, "recurring", json!({}));
    assert_eq!(v["currency"], "EUR");
    let items = v["items"].as_array().unwrap();
    assert_eq!(v["count"].as_u64().unwrap() as usize, items.len());
    close(f(&v["monthly_total"]), items.iter().map(|i| f(&i["monthly"])).sum(), "the total is the sum of the monthly costs");
    let names: Vec<String> = items.iter().map(|i| i["name"].as_str().unwrap().to_uppercase()).collect();
    for want in ["STREAMFLIX", "TUNEWAVE", "MEDITATE", "APARTMENT RENT"] {
        assert!(names.iter().any(|n| n.contains(want)), "{want} in {names:?}");
    }
    assert!(!names.iter().any(|n| n.contains("SALARY")), "income is not a charge");
    let rent = items.iter().find(|i| i["name"].as_str().unwrap().to_uppercase().contains("APARTMENT RENT")).unwrap();
    assert_eq!((rent["cadence"].as_str(), f(&rent["amount"])), (Some("monthly"), 1250.0));
    assert!(rent["last"].as_str().unwrap() <= "2026-09-20" && rent["next"].as_str().unwrap() > rent["last"].as_str().unwrap());
    let flix = items.iter().find(|i| i["name"].as_str().unwrap().to_uppercase().contains("STREAMFLIX")).unwrap();
    let ch = &flix["price_change"];
    assert!(f(&ch["to"]) > f(&ch["from"]) && f(&ch["pct"]) > 0.0, "{flix}");
    let monthly: Vec<f64> = items.iter().map(|i| f(&i["monthly"])).collect();
    assert!(monthly.windows(2).all(|w| w[0] >= w[1]), "biggest first");
}

#[test]
fn holdings_add_up_to_the_valuation() {
    let s = Demo::new();
    let nw = s.net_worth();
    let v = tool(&s, "holdings", json!({}));
    close(f(&v["total"]), f(&nw["investments_hkd"]) + f(&nw["crypto_hkd"]), "the positions are the net worth's investments and crypto");
    let pos = v["positions"].as_array().unwrap();
    close(pos.iter().map(|p| f(&p["value"])).sum(), f(&v["total"]), "positions sum");
    close(pos.iter().map(|p| f(&p["weight_pct"])).sum(), 100.0, "weights");
    let vwce = pos.iter().find(|p| p["symbol"] == "VWCE").unwrap();
    assert_eq!((vwce["account"].as_str(), vwce["class"].as_str(), f(&vwce["quantity"])), (Some("Interactive Brokers"), Some("equity"), 54.0));
    close(f(&vwce["gain"]), f(&vwce["value"]) - f(&vwce["cost"]), "gain");
    close(f(&vwce["gain_pct"]), f(&vwce["gain"]) / f(&vwce["cost"]) * 100.0, "gain %");
    close(f(&vwce["value"]), f(&vwce["quantity"]) * f(&vwce["price"]), "value = quantity x price");
    assert!(pos.iter().any(|p| p["symbol"] == "BTC" && p["class"] == "crypto" && p["price_currency"] == "USD"));
    assert!(pos.iter().any(|p| p["class"] == "cash" && p["account"] == "Interactive Brokers"));
    assert_eq!(v["prices_stale"], true, "offline: cached prices");
    assert_eq!(v["broker_as_of"], "2026-09-18");
    let values: Vec<f64> = pos.iter().map(|p| f(&p["value"])).collect();
    assert!(values.windows(2).all(|w| w[0] >= w[1]), "biggest first");
    // grouped, filtered
    for by in ["account", "asset_class", "currency"] {
        let g = tool(&s, "holdings", json!({"group_by": by}));
        assert!(g.get("positions").is_none());
        close(g["groups"].as_array().unwrap().iter().map(|x| f(&x["value"])).sum(), f(&v["total"]), by);
    }
    let by_account = tool(&s, "holdings", json!({"group_by": "account"}));
    assert_eq!(by_account["groups"].as_array().unwrap().iter().map(|g| g["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Interactive Brokers", "Crypto"]);
    let crypto = tool(&s, "holdings", json!({"class": "crypto"}));
    assert!(crypto["positions"].as_array().unwrap().iter().all(|p| p["class"] == "crypto"));
    let broker = tool(&s, "holdings", json!({"account": "interactive"}));
    assert!(broker["positions"].as_array().unwrap().iter().all(|p| p["account"] == "Interactive Brokers"));
    close(f(&broker["total"]) + f(&crypto["total"]), f(&v["total"]), "the two accounts are everything");
    assert!(tool_err(&s, "holdings", json!({"account": "Nowhere"})).contains("Unknown investment account 'Nowhere'. Known: "));
}

#[test]
fn performance_ties_to_holdings_and_the_broker() {
    let s = Demo::new();
    let h = tool(&s, "holdings", json!({}));
    let v = tool(&s, "performance", json!({}));
    let series = v["series"].as_array().unwrap();
    assert_eq!(series.len(), 22);
    close(f(&series[series.len() - 1]["value"]), f(&h["total"]), "the last month is today's value");
    let p = &v["period"];
    close(f(&p["end_value"]) - f(&p["start_value"]) - f(&p["contributions"]), f(&p["gain"]), "gain = growth less contributions");
    close(series.iter().map(|r| f(&r["contributed"])).sum(), f(&p["contributions"]), "contributions");
    assert!(p["time_weighted_pct"].is_number() && p["money_weighted_annual_pct"].is_number());
    assert_eq!(p["benchmark"]["symbol"], "^GSPC");
    let notes: Vec<&str> = v["notes"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    assert!(notes.iter().any(|n| n.contains("approximation")) && notes.iter().any(|n| n.contains("approximated")), "approximations are labelled: {notes:?}");
    close(f(&v["since_start"]["gain"]), f(&p["gain"]), "since start = the whole period here");
    // a window: the values before it become the opening value
    let w = tool(&s, "performance", json!({"from_month": "2026-01", "to_month": "2026-06"}));
    assert_eq!(w["series"].as_array().unwrap().len(), 6);
    close(f(&w["period"]["start_value"]), f(&series.iter().find(|r| r["month"] == "2025-12").unwrap()["value"]), "opening value");
    close(f(&w["period"]["end_value"]), f(&series.iter().find(|r| r["month"] == "2026-06").unwrap()["value"]), "closing value");
    // the broker alone: its exact figures, income, fees
    let b = tool(&s, "performance", json!({"account": "Interactive Brokers"}));
    assert!(f(&b["broker_exact"]["time_weighted_pct"]) > 0.0 && f(&b["broker_exact"]["net_deposits"]) == 11000.0);
    assert!(b["income_received"]["dividends"].is_number());
    assert!(f(&b["fees"]["commissions_paid"]) > 0.0 && f(&b["fees"]["running_cost_per_year"]) > 0.0);
    let last = b["series"].as_array().unwrap().last().unwrap().clone();
    let broker_value = f(&tool(&s, "holdings", json!({"account": "Interactive Brokers"}))["total"]);
    close(f(&last["value"]), broker_value, "the broker's last month");
    close(f(&b["period"]["contributions"]), 11000.0, "the broker's deposits");
    close(f(&b["period"]["gain"]), broker_value - 11000.0, "gain = value less deposits");
    assert!(b["period"].get("time_weighted_pct").is_none(), "its exact one is in broker_exact");
    // crypto alone: its own value and money in; the whole portfolio is the sum of the accounts
    let c = tool(&s, "performance", json!({"account": "Crypto"}));
    assert!(c["series"][0]["contributed"].as_f64().unwrap() > 0.0 && c.get("broker_exact").is_none());
    assert!(c["notes"].to_string().contains("only available for the whole portfolio and for the broker"));
    close(f(&c["period"]["contributions"]) + f(&b["period"]["contributions"]), f(&p["contributions"]), "the accounts' money in adds up");
    close(f(&c["period"]["end_value"]) + f(&b["period"]["end_value"]), f(&p["end_value"]), "and their values");
    assert!(tool_err(&s, "performance", json!({"account": "Nowhere"})).contains("Unknown investment account"));
    assert!(tool_err(&s, "performance", json!({"from_month": "2026-06", "to_month": "2026-01"})).contains("is after"));
}

#[test]
fn activity_is_the_investment_ledger() {
    let s = Demo::new();
    let v = tool(&s, "activity", json!({"limit": 500}));
    let rows = v["rows"].as_array().unwrap();
    let dates: Vec<&str> = rows.iter().map(|r| r["date"].as_str().unwrap()).collect();
    assert!(dates.windows(2).all(|w| w[0] >= w[1]), "newest first");
    let kinds: std::collections::BTreeSet<&str> = rows.iter().map(|r| r["type"].as_str().unwrap()).collect();
    for k in ["buy", "deposit", "dividend"] {
        assert!(kinds.contains(k), "{k} in {kinds:?}");
    }
    // the totals by type are over every match
    let sums = |t: &str| rows.iter().filter(|r| r["type"] == t).map(|r| f(&r["amount"])).sum::<f64>();
    assert_eq!(v["matches"].as_u64().unwrap() as usize, rows.len(), "the demo's ledger fits one page of 500");
    assert!((f(&v["totals"]["buy"]) - sums("buy")).abs() < 1.0, "buys: the rows are rounded one by one");
    close(f(&v["totals"]["deposit"]), 11000.0, "the broker deposits");
    // a symbol: its buys, and the DCA summary
    let w = tool(&s, "activity", json!({"symbol": "vwce", "type": "buy", "limit": 5}));
    assert_eq!(w["matches"], 22);
    assert_eq!(w["rows"].as_array().unwrap().len(), 5);
    assert!(w["rows"].as_array().unwrap().iter().all(|r| r["symbol"] == "VWCE" && r["type"] == "buy" && f(&r["quantity"]) > 0.0 && f(&r["price"]) > 0.0));
    let dca = &w["dca"];
    assert_eq!((dca["buys"].as_i64(), dca["first_buy"].as_str(), dca["currency"].as_str()), (Some(22), Some("2024-12-06"), Some("EUR")));
    let hold = tool(&s, "holdings", json!({"account": "Interactive Brokers"}));
    let vwce = hold["positions"].as_array().unwrap().iter().find(|p| p["symbol"] == "VWCE").unwrap();
    close(f(&dca["gain"]), f(&vwce["gain"]), "the DCA gain is the position's");
    close(f(&dca["price_now"]) * f(&vwce["quantity"]) - f(&dca["invested"]), f(&dca["gain"]), "gain = value - invested");
    close(f(&dca["invested"]), f(&vwce["cost"]), "invested = cost");
    // BTC: the coin plan, in USD
    let b = tool(&s, "activity", json!({"symbol": "BTC", "limit": 1}));
    assert_eq!(b["dca"]["currency"], "USD");
    assert!(b["dca"]["buys"].as_i64().unwrap() > 50 && f(&b["dca"]["avg_cost"]) > 0.0);
    // no DCA for a one-off or an unknown symbol, and the filters
    assert!(tool(&s, "activity", json!({"symbol": "NOPE"})).get("dca").is_none());
    let d = tool(&s, "activity", json!({"type": "dividend", "from_date": "2026-01-01", "to_date": "2026-06-30"}));
    assert!(d["rows"].as_array().unwrap().iter().all(|r| r["type"] == "dividend" && r["date"].as_str().unwrap() >= "2026-01-01" && r["date"].as_str().unwrap() <= "2026-06-30"));
    let fund = tool(&s, "activity", json!({"account": "Crypto", "limit": 500}));
    assert!(fund["rows"].as_array().unwrap().iter().all(|r| r["account"] == "Crypto"));
    assert!(tool_err(&s, "activity", json!({"account": "Nowhere"})).contains("Unknown investment account"));
    assert!(tool_err(&s, "activity", json!({"type": "gift"})).contains("literal_error"));
}

#[test]
fn an_empty_household_answers_every_tool() {
    // a db with nothing in it: no errors, nothing invented
    let dir = std::env::temp_dir().join(format!("finance-mcp-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    demo_db(); // the env of the file
    let path = dir.join("finance.db");
    drop(finance_engine::db::connect(&path).unwrap());
    let s = Demo { conn: rusqlite::Connection::open(&path).unwrap(), path };
    for (name, args) in [
        ("summary", json!({})),
        ("trend", json!({"metric": "net_worth"})),
        ("trend", json!({"metric": "spending"})),
        ("breakdown", json!({"by": "category"})),
        ("transactions", json!({})),
        ("recurring", json!({})),
        ("holdings", json!({})),
        ("performance", json!({})),
        ("activity", json!({})),
    ] {
        tool(&s, name, args);
    }
    assert_eq!(tool(&s, "holdings", json!({}))["total"], 0.0);
    assert_eq!(tool(&s, "transactions", json!({}))["matches"], 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn warm_calls_are_fast() {
    let s = Demo::new();
    let calls: Vec<(&str, Value)> = vec![
        ("summary", json!({})),
        ("trend", json!({"metric": "net_worth"})),
        ("trend", json!({"metric": "category:Groceries"})),
        ("breakdown", json!({"by": "merchant"})),
        ("breakdown", json!({"by": "category"})),
        ("transactions", json!({"text": "market", "limit": 50})),
        ("recurring", json!({})),
        ("holdings", json!({})),
        ("performance", json!({})),
        ("activity", json!({"symbol": "VWCE"})),
    ];
    // a warm cache: one pass first
    for (name, args) in &calls {
        tool(&s, name, args.clone());
    }
    let mut report = String::new();
    for (name, args) in &calls {
        let mut ms: Vec<f64> = (0..21)
            .map(|_| {
                let t = Instant::now();
                let m: Map<String, Value> = args.as_object().unwrap().clone();
                let r = mcp::call_tool(&s, name, &m);
                assert_eq!(r["isError"], false);
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        let p50 = ms[10];
        report.push_str(&format!("{name:<13} {:<28} p50 {p50:6.2} ms  max {:6.2} ms\n", args.to_string().chars().take(28).collect::<String>(), ms[20]));
        // the target is 50 ms (release, a Raspberry Pi 5 class machine); a debug build on a busy CI box gets room
        assert!(p50 < 250.0, "{name} p50 {p50} ms\n{report}");
    }
    eprintln!("MCP tools, demo household, warm cache:\n{report}");
}
