//! The stdio loop (engine::mcp::serve, behind `anser-server mcp` and
//! `anser mcp`) on a small made-up db opened read-only, as a
//! replica is: initialize, tools/list and a tools/call are answered, and a
//! line that isn't JSON gets a JSON-RPC error without ending the session.
//! (Every tool on the demo household: finance-server's tests/mcp.rs.)

use std::sync::Arc;

use anyhow::{bail, Result};
use finance_engine::mcp::{self, Source};
use finance_engine::views::{MarketFeed, Offline};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

/// A read-only connection and the offline feed; no ledger (the two tools
/// that read one aren't called here).
struct Replica(Connection);

impl Source for Replica {
    fn conn(&self) -> &Connection {
        &self.0
    }
    fn feed(&self) -> &dyn MarketFeed {
        &Offline
    }
    fn ledger(&self) -> Result<Arc<finance_core::Ledger>> {
        bail!("no ledger in this test")
    }
}

#[test]
fn the_stdio_loop_serves_a_read_only_copy() {
    let dir = std::env::temp_dir().join(format!("finance-mcp-stdio-{}", std::process::id()));
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
    let conn = Connection::open_with_flags(dir.join("finance.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

    let session = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
this is not json
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"broken":
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"summary","arguments":{}}}
"#;
    let mut out = Vec::new();
    mcp::serve(&Replica(conn), session.as_bytes(), &mut out).unwrap();
    let replies: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).expect("stdout is JSON lines only")).collect();
    // one reply per request and per bad line, none for the notification
    assert_eq!(replies.len(), 5, "{replies:#?}");

    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "anser");

    for bad in [&replies[1], &replies[3]] {
        assert_eq!(bad["error"]["code"], -32700, "{bad}");
        assert_eq!(bad["id"], Value::Null);
    }

    // the loop lived on after the first bad line
    assert_eq!(replies[2]["id"], 2);
    assert_eq!(replies[2]["result"]["tools"].as_array().unwrap().len(), 8);

    assert_eq!(replies[4]["id"], 3);
    assert_eq!(replies[4]["result"]["isError"], false, "{}", replies[4]);
    assert!(replies[4]["result"]["structuredContent"].is_object());
    let _ = std::fs::remove_dir_all(&dir);
}
