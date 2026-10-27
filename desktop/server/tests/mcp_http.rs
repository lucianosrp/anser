//! MCP over HTTP (`/mcp`), its keys and its switch, on a household server
//! (the demo household as the owner's tree, Sam with a tree of his own):
//! what the endpoint answers, who may call it, what an MCP key may not
//! reach, and that a key only ever sees its own user's data.

use std::time::Duration;

use finance_engine::tenant;
use finance_server::{auth, server};
use serde_json::{json, Value};

struct Client {
    base: String,
    agent: ureq::Agent,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&self.body)))
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl Client {
    fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<&str>, extra: &[(&str, &str)]) -> Reply {
        let url = format!("{}{path}", self.base);
        let auth = token.map(|t| format!("Bearer {t}"));
        macro_rules! hdrs {
            ($b:expr) => {{
                let mut b = $b;
                if let Some(a) = &auth {
                    b = b.header("Authorization", a);
                }
                for (k, v) in extra {
                    b = b.header(*k, *v);
                }
                b
            }};
        }
        let mut r = match method {
            "GET" => hdrs!(self.agent.get(&url)).call(),
            "DELETE" => hdrs!(self.agent.delete(&url)).call(),
            m => {
                let b = if m == "PUT" { self.agent.put(&url) } else { self.agent.post(&url) };
                hdrs!(b.header("Content-Type", "application/json")).send(body.unwrap_or("{}").as_bytes())
            }
        }
        .unwrap();
        let headers = r.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
        Reply { status: r.status().as_u16(), headers, body: r.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap() }
    }

    fn rpc(&self, token: &str, msg: Value) -> Reply {
        self.call("POST", "/mcp", Some(token), Some(&msg.to_string()), &[("MCP-Protocol-Version", "2025-06-18"), ("Accept", "application/json, text/event-stream")])
    }

    fn tool(&self, token: &str, name: &str, args: Value) -> Value {
        let r = self.rpc(token, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}));
        assert_eq!(r.status, 200, "{}", r.text());
        let v = r.json();
        assert_eq!(v["result"]["isError"], false, "{v}");
        v["result"]["structuredContent"].clone()
    }

    fn pair(&self, uid: i64, name: &str) -> String {
        let (code, _) = auth::new_pair_code(uid).unwrap();
        let r = self.call("POST", "/api/pair", None, Some(&json!({"code": code, "device_name": name}).to_string()), &[]);
        assert_eq!(r.status, 200, "{}", r.text());
        r.json()["token"].as_str().unwrap().to_string()
    }

    /// An MCP key through the API, as the Settings page does.
    fn key(&self, device: &str, name: &str) -> (i64, String) {
        let r = self.call("POST", "/api/mcp/keys", Some(device), Some(&json!({"name": name}).to_string()), &[]);
        assert_eq!(r.status, 200, "{}", r.text());
        let v = r.json();
        (v["id"].as_i64().unwrap(), v["token"].as_str().unwrap().to_string())
    }
}

fn ping() -> Value {
    json!({"jsonrpc": "2.0", "id": 7, "method": "ping"})
}

/// One test, not two: both set the process's environment and start a server.
#[test]
fn mcp_over_http_and_from_the_shell() {
    the_cli_manages_keys_and_the_switch();
    the_mcp_endpoint_its_keys_and_its_switch();
}

fn the_mcp_endpoint_its_keys_and_its_switch() {
    let home = std::env::temp_dir().join(format!("finance-mcp-http-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("data")).unwrap();
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    for k in ["FINANCE_DATA_DIR", "FINANCE_DB", "FINANCE_TRANSACTIONS_DIR", "FINANCE_FORMATS_DIR", "FINANCE_AUTH", "FINANCE_ALLOW_SIGNUP", "FINANCE_ALLOWED_HOSTS"] {
        std::env::remove_var(k);
    }
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::set_var("FINANCE_TODAY", "2026-09-20");
    for k in finance_engine::secrets::SECRET_KEYS {
        std::env::remove_var(k);
    }
    // the owner's tree: the demo household, an existing install
    let demo = home.join("demo");
    finance_engine::demo::write(&demo, chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()).unwrap();
    std::fs::copy(demo.join("finance.db"), home.join("data/finance.db")).unwrap();
    auth::bootstrap().unwrap();
    // Sam: a tree of his own, with one marked row
    let sam_id = auth::add_user("Sam", None, false).unwrap();
    auth::tenant_of(&sam_id.to_string()).unwrap().enter(|| {
        let c = finance_engine::db::open().unwrap();
        finance_engine::db::insert_bank_transactions(&c, &[finance_engine::db::NewBankTxn {
            source: "hsbc".into(), account: "SAMMARK-acct".into(), currency: "HKD".into(), txn_date: "2026-09-02".into(), amount: Some(-7.0),
            balance: Some(99.0), description: "SAMMARK coffee".into(), raw_hash: "sam-h1".into(), source_file: "sam.csv".into(), ingested_at: 1,
        }]).unwrap();
    });
    assert!(auth::household());
    tenant::set_strict(true);
    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).http_status_as_error(false).max_redirects(0).build().into();
    let c = Client { base, agent };
    let owner = c.pair(1, "owner laptop");
    let sam = c.pair(sam_id, "sam phone");

    // ---- off by default: the same 404 as an unknown route, for a key; an ordinary device token is refused ----
    assert_eq!(c.rpc(&owner, ping()).status, 403);
    let status = c.call("GET", "/api/mcp", Some(&owner), None, &[]).json();
    assert_eq!((status["enabled"].as_bool(), status["path"].as_str(), status["keys"].as_array().map(Vec::len)), (Some(false), Some("/mcp"), Some(0)));
    let (key_id, key) = c.key(&owner, "Claude Code");
    assert!(key.starts_with("fin_") && key.len() == 68);
    assert_eq!(c.rpc(&key, ping()).status, 404, "a key works only once the user switched MCP on");
    assert_eq!(c.rpc(&key, ping()).text(), c.call("GET", "/api/nothing-here", Some(&owner), None, &[]).text().replace("/api/nothing-here", "/mcp"), "no tell-tale");

    // ---- the switch ----
    assert_eq!(c.call("PUT", "/api/mcp", Some(&owner), Some(r#"{"enabled": "yes"}"#), &[]).status, 400);
    assert_eq!(c.call("PUT", "/api/mcp", Some(&owner), Some(r#"{}"#), &[]).status, 400);
    assert_eq!(c.call("PUT", "/api/mcp", Some(&owner), Some(r#"{"enabled": true}"#), &[]).json()["enabled"], true);
    assert_eq!(c.call("PUT", "/api/mcp", None, Some(r#"{"enabled": true}"#), &[]).status, 401, "no anonymous switch");
    let status = c.call("GET", "/api/mcp", Some(&owner), None, &[]).json();
    assert_eq!(status["enabled"], true);
    let keys = status["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!((keys[0]["id"].as_i64(), keys[0]["name"].as_str()), (Some(key_id), Some("Claude Code")));
    assert!(keys[0]["last_used"].is_number(), "the 404s above were authenticated: the key was seen");
    assert!(keys[0]["created"].as_i64().unwrap() > 0);
    assert!(!status.to_string().contains(&key) && !status.to_string().contains("token") && !status.to_string().contains("hash"), "never the secret: {status}");
    // the switch is not in the export bundle (the owner's output doesn't move)
    assert!(!String::from_utf8_lossy(&c.call("GET", "/api/export.json", Some(&owner), None, &[]).body).contains("\"mcp\""));

    // ---- what the endpoint speaks ----
    let r = c.rpc(&key, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}));
    assert_eq!(r.status, 200);
    assert!(r.header("content-type").unwrap().starts_with("application/json"), "JSON response mode, no SSE");
    assert!(r.header("mcp-session-id").is_none(), "stateless");
    assert_eq!(r.json()["result"]["protocolVersion"], "2025-11-25");
    let list = c.rpc(&key, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).json();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 8);
    let summary = c.tool(&key, "summary", json!({}));
    assert_eq!(summary["currency"], "EUR");
    assert!(summary["net_worth"]["total"].as_f64().unwrap() > 10_000.0);
    let tx = c.tool(&key, "transactions", json!({"text": "freshmart", "limit": 2}));
    assert_eq!(tx["matches"], 42);
    // an ordinary device token is refused, whatever the switch says
    assert_eq!(c.rpc(&owner, ping()).status, 403);
    // a notification: accepted, no body; a batch: an array; garbage: a JSON-RPC parse error
    let n = c.rpc(&key, json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    assert_eq!((n.status, n.body.len()), (202, 0));
    let n = c.rpc(&key, json!([{"jsonrpc": "2.0", "method": "notifications/initialized"}]));
    assert_eq!((n.status, n.body.len()), (202, 0));
    let b = c.rpc(&key, json!([ping(), {"jsonrpc": "2.0", "method": "notifications/cancelled"}, {"jsonrpc": "2.0", "id": 8, "method": "ping"}]));
    assert_eq!(b.status, 200);
    assert_eq!(b.json().as_array().unwrap().len(), 2);
    let bad = c.call("POST", "/mcp", Some(&key), Some("not json"), &[]);
    assert_eq!((bad.status, bad.json()["error"]["code"].clone()), (400, json!(-32700)));
    // no stream to listen on, no sessions to end
    for m in ["GET", "DELETE"] {
        let r = c.call(m, "/mcp", Some(&key), None, &[]);
        assert_eq!(r.status, 405, "{m}");
        assert_eq!(r.header("allow"), Some("POST"));
    }

    // ---- who may call it ----
    assert_eq!(c.rpc("fin_", ping()).status, 401, "a malformed token");
    assert_eq!(c.call("POST", "/mcp", None, Some(&ping().to_string()), &[]).status, 401, "no token");
    let wrong = format!("fin_{}", "0".repeat(64));
    assert_eq!(c.rpc(&wrong, ping()).status, 401, "an unknown token");
    // a browser session is refused: the cookie of a normal device, and of a key
    for token in [&owner, &key] {
        let cookie = format!("finance_session={token}");
        let r = c.call("POST", "/mcp", None, Some(&ping().to_string()), &[("Cookie", &cookie), ("X-Finance-Request", "1")]);
        assert_eq!(r.status, 403, "a cookie on /mcp");
        assert!(r.text().contains("bearer"), "{}", r.text());
    }
    // DNS rebinding: a Host the server doesn't know, and an Origin that isn't ours
    let r = c.call("POST", "/mcp", Some(&key), Some(&ping().to_string()), &[("Host", "rebind.attacker.net")]);
    assert_eq!(r.status, 421);
    for origin in ["https://evil.example.com", "null"] {
        let r = c.call("POST", "/mcp", Some(&key), Some(&ping().to_string()), &[("Origin", origin)]);
        assert_eq!(r.status, 403, "Origin {origin}");
    }
    let r = c.call("GET", "/mcp", Some(&key), None, &[("Origin", "https://evil.example.com")]);
    assert_eq!(r.status, 403, "the Origin check covers a GET too");
    for origin in ["http://localhost:3000", "https://box.tail1234.ts.net"] {
        let r = c.call("POST", "/mcp", Some(&key), Some(&ping().to_string()), &[("Origin", origin)]);
        assert_eq!(r.status, 200, "Origin {origin} is on the allow-list");
    }

    // ---- what a key may not reach: everything but /mcp ----
    for (m, p) in [
        ("GET", "/api/export.json"),
        ("GET", "/api/ledger.json"),
        ("GET", "/api/db.sqlite"),
        ("GET", "/api/live"),
        ("GET", "/api/whoami"),
        ("GET", "/api/profile"),
        ("GET", "/api/mcp"),
        ("GET", "/api/admin/devices"),
        ("GET", "/healthz"),
        ("PUT", "/api/mcp"),
        ("POST", "/api/mcp/keys"),
        ("DELETE", "/api/mcp/keys/1"),
        ("POST", "/upload"),
        ("POST", "/api/refresh"),
        ("PUT", "/api/settings/budgets"),
        ("POST", "/api/me/pair-code"),
        ("POST", "/api/logout"),
    ] {
        let r = c.call(m, p, Some(&key), None, &[]);
        assert_eq!(r.status, 403, "{m} {p} with an MCP key: {}", r.text());
        assert!(r.text().contains("MCP key"), "{m} {p}: {}", r.text());
    }
    // as a cookie too
    let cookie = format!("finance_session={key}");
    assert_eq!(c.call("GET", "/api/export.json", None, None, &[("Cookie", &cookie)]).status, 403);
    // and the device that made it still reads everything
    assert_eq!(c.call("GET", "/api/export.json", Some(&owner), None, &[]).status, 200);

    // ---- managing keys: the caller's own, writes as everywhere else ----
    let cookie_owner = format!("finance_session={owner}");
    let make = |extra: &[(&str, &str)]| c.call("POST", "/api/mcp/keys", None, Some(r#"{"name": "from a browser"}"#), extra);
    let mk = |extra: &[(&str, &str)]| {
        let mut all = vec![("Cookie", cookie_owner.as_str())];
        all.extend_from_slice(extra);
        make(&all)
    };
    assert_eq!(mk(&[]).status, 403, "a browser session needs X-Finance-Request");
    let made = mk(&[("X-Finance-Request", "1")]);
    assert_eq!(made.status, 200);
    let browser_key_id = made.json()["id"].as_i64().unwrap();
    let listed = c.call("GET", "/api/mcp", None, None, &[("Cookie", &cookie_owner)]).json();
    assert_eq!(listed["keys"].as_array().unwrap().len(), 2);
    let unused = listed["keys"].as_array().unwrap().iter().find(|k| k["id"] == browser_key_id).unwrap();
    assert_eq!((unused["name"].as_str(), unused["last_used"].clone()), (Some("from a browser"), Value::Null), "never used");
    assert_eq!(c.call("POST", "/api/mcp/keys", Some(&owner), Some(r#"{"name": ""}"#), &[]).status, 400);
    assert_eq!(c.call("POST", "/api/mcp/keys", Some(&owner), Some(&json!({"name": "x".repeat(61)}).to_string()), &[]).status, 400);
    assert_eq!(c.call("POST", "/api/mcp/keys", None, Some(r#"{"name": "anon"}"#), &[]).status, 401);
    assert_eq!(c.call("POST", "/api/mcp/keys", Some(&owner), Some(r#"{"name": "a"}"#), &[("Origin", "https://evil.example.com")]).status, 403);
    // a key cannot be made from a request that isn't loopback or TLS: the proxy case is the gate's (secret routes)
    assert_eq!(c.call("DELETE", "/api/mcp/keys/abc", Some(&owner), None, &[]).status, 400);
    assert_eq!(c.call("DELETE", "/api/mcp/keys/99999", Some(&owner), None, &[]).status, 404);

    // ---- one user's key reads only that user's data ----
    assert_eq!(c.rpc(&key, ping()).status, 200);
    assert_eq!(c.call("PUT", "/api/mcp", Some(&sam), Some(r#"{"enabled": true}"#), &[]).status, 200);
    let (sam_key_id, sam_key) = c.key(&sam, "Sam's Cursor");
    let sam_rows = c.tool(&sam_key, "transactions", json!({"text": "mark"}));
    assert!(sam_rows["matches"] == 1 && sam_rows["rows"][0]["description"] == "SAMMARK coffee", "{sam_rows}");
    assert_eq!(c.tool(&sam_key, "transactions", json!({"text": "freshmart"}))["matches"], 0, "none of the owner's rows");
    assert_eq!(c.tool(&key, "transactions", json!({"text": "SAMMARK"}))["matches"], 0, "none of Sam's");
    let sam_summary = c.tool(&sam_key, "summary", json!({}));
    assert_eq!(sam_summary["currency"], finance_engine::config::NEW_PROFILE_CURRENCY, "a new member starts in the neutral default");
    assert!(!sam_summary.to_string().contains("N26") && !sam_summary.to_string().contains("Groceries"), "{sam_summary}");
    // each lists only their own keys, and cannot revoke the other's
    assert_eq!(c.call("GET", "/api/mcp", Some(&sam), None, &[]).json()["keys"].as_array().unwrap().iter().map(|k| k["id"].as_i64().unwrap()).collect::<Vec<_>>(), [sam_key_id]);
    assert_eq!(c.call("DELETE", &format!("/api/mcp/keys/{key_id}"), Some(&sam), None, &[]).status, 404, "not Sam's key");
    assert_eq!(c.rpc(&key, ping()).status, 200);
    // a paired device's id is not a key's: revoking it that way is a 404 too
    let owner_device = c.call("GET", "/api/whoami", Some(&owner), None, &[]).json()["device"]["id"].as_i64().unwrap();
    assert_eq!(c.call("DELETE", &format!("/api/mcp/keys/{owner_device}"), Some(&owner), None, &[]).status, 404);
    assert_eq!(c.call("GET", "/api/whoami", Some(&owner), None, &[]).json()["paired"], true);

    // ---- revoke, switch off ----
    assert_eq!(c.call("DELETE", &format!("/api/mcp/keys/{browser_key_id}"), Some(&owner), None, &[]).status, 200);
    assert_eq!(c.call("DELETE", &format!("/api/mcp/keys/{key_id}"), Some(&owner), None, &[]).json()["ok"], true);
    assert_eq!(c.rpc(&key, ping()).status, 401, "a revoked key is no key");
    assert_eq!(c.call("GET", "/api/mcp", Some(&owner), None, &[]).json()["keys"].as_array().unwrap().len(), 0);
    assert_eq!(c.rpc(&sam_key, ping()).status, 200);
    assert_eq!(c.call("PUT", "/api/mcp", Some(&sam), Some(r#"{"enabled": false}"#), &[]).json()["enabled"], false);
    assert_eq!(c.rpc(&sam_key, ping()).status, 404, "off again: the key stays, the door closes");
    assert_eq!(c.call("GET", "/api/mcp", Some(&owner), None, &[]).json()["enabled"], true, "the owner's switch is the owner's");
    assert_eq!(c.call("GET", "/api/mcp", Some(&sam), None, &[]).json()["keys"].as_array().unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(&home);
}

fn the_cli_manages_keys_and_the_switch() {
    use std::process::Command;
    let home = std::env::temp_dir().join(format!("finance-mcp-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("data")).unwrap();
    let demo = home.join("demo");
    finance_engine::demo::write(&demo, chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()).unwrap();
    std::fs::copy(demo.join("finance.db"), home.join("data/finance.db")).unwrap();
    let bin = env!("CARGO_BIN_EXE_anser-server");
    let run = |args: &[&str]| {
        let o = Command::new(bin)
            .args(args)
            .env("FINANCE_HOME", &home)
            .env("FINANCE_OFFLINE", "1")
            .env_remove("FINANCE_DB")
            .env_remove("FINANCE_DATA_DIR")
            .output()
            .unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
    };
    let (ok, out) = run(&["mcp-key", "list"]);
    assert!(ok && out.contains("is off"), "{out}");
    let (ok, out) = run(&["mcp-key", "create", "Codex"]);
    assert!(ok && out.contains("shown once") && out.contains("mcp-enable"), "{out}");
    let token = out.lines().find(|l| l.starts_with("fin_")).unwrap().trim().to_string();
    assert_eq!(token.len(), 68);
    let (ok, out) = run(&["mcp-key", "list"]);
    assert!(ok && out.contains("Codex") && out.contains("never") && !out.contains(&token), "the list never shows the secret: {out}");
    assert!(run(&["mcp-key", "create"]).1.contains("usage: anser-server mcp-key"));
    assert!(!run(&["mcp-key", "revoke", "x"]).0);
    let (_, out) = run(&["mcp-enable"]);
    assert!(out.contains("is on"), "{out}");
    assert!(run(&["mcp-key", "list"]).1.contains("is on"));
    // the key opens the endpoint of a server on that tree
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::set_var("FINANCE_TODAY", "2026-09-20");
    for k in ["FINANCE_DB", "FINANCE_DATA_DIR", "FINANCE_AUTH"] {
        std::env::remove_var(k);
    }
    auth::bootstrap().unwrap();
    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).http_status_as_error(false).build().into();
    let c = Client { base, agent };
    assert_eq!(c.tool(&token, "summary", json!({}))["currency"], "EUR");
    let (_, out) = run(&["mcp-key", "list"]);
    assert!(!out.contains("never"), "used now: {out}");
    let id = out.split("key ").nth(1).unwrap().split_whitespace().next().unwrap().to_string();
    let (ok, out) = run(&["mcp-key", "revoke", &id]);
    assert!(ok && out.contains("revoked"), "{out}");
    assert_eq!(c.rpc(&token, ping()).status, 401);
    assert!(run(&["mcp-key", "revoke", &id]).1.contains("no such key"));
    let (_, out) = run(&["mcp-disable"]);
    assert!(out.contains("is off"), "{out}");
    let _ = std::fs::remove_dir_all(&home);
}
