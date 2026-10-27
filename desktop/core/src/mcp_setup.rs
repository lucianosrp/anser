//! Settings, AI assistants: the server's MCP switch and keys (GET/PUT
//! /api/mcp, POST /api/mcp/keys, DELETE /api/mcp/keys/ID), and the text an
//! assistant's setup needs: the one-time key's ready-to-paste commands and
//! config blocks for each client. Pure and UI-free, so it is tested here.
//!
//! The secret only lives in the caller's panel state: nothing in this module
//! stores or logs a key.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client::{self, Request};

/// The name the server is registered under in every client.
pub const SERVER_NAME: &str = "anser";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpKey {
    pub id: i64,
    pub name: String,
    /// ms since the epoch
    pub created: i64,
    pub last_used: Option<i64>,
}

/// GET /api/mcp's answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpStatus {
    pub enabled: bool,
    pub path: String,
    pub keys: Vec<McpKey>,
}

/// POST /api/mcp/keys' answer: the secret is in `token`, once.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewKey {
    pub id: i64,
    pub name: String,
    pub created: i64,
    pub token: String,
}

fn failed(r: client::Reply) -> Result<client::Response, String> {
    match r {
        Ok(resp) if resp.ok() => Ok(resp),
        Ok(resp) if resp.status == 404 => Err("this server can't do MCP yet (update it)".into()),
        Ok(resp) if resp.status == 401 => Err("this device isn't signed in to the server: sign in under Settings, Account".into()),
        Ok(resp) => Err(resp.error()),
        Err(e) => Err(e),
    }
}

pub async fn status() -> Result<McpStatus, String> {
    let r = failed(client::send(Request::new("GET", "/api/mcp")).await)?;
    serde_json::from_slice(&r.body).map_err(|e| format!("unreadable MCP reply: {e}"))
}

pub async fn set_enabled(on: bool) -> Result<(), String> {
    failed(client::send(Request::new("PUT", "/api/mcp").json(&json!({"enabled": on}))).await).map(|_| ())
}

pub async fn create_key(name: &str) -> Result<NewKey, String> {
    let r = failed(client::send(Request::new("POST", "/api/mcp/keys").json(&json!({"name": name}))).await)?;
    let k: NewKey = serde_json::from_slice(&r.body).map_err(|e| format!("unreadable key reply: {e}"))?;
    if k.token.is_empty() {
        return Err("the server sent no key".into());
    }
    Ok(k)
}

pub async fn revoke_key(id: i64) -> Result<(), String> {
    failed(client::send(Request::new("DELETE", &format!("/api/mcp/keys/{id}"))).await).map(|_| ())
}

// ---------- what to paste ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssistantClient {
    #[default]
    ClaudeCode,
    Codex,
    Cursor,
    ClaudeDesktop,
    Other,
}

impl AssistantClient {
    pub const ALL: [AssistantClient; 5] = [Self::ClaudeCode, Self::Codex, Self::Cursor, Self::ClaudeDesktop, Self::Other];

    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::Other => "Other",
        }
    }

    /// Where the snippet goes, one line.
    pub fn where_to(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Run this in a terminal.",
            Self::Codex => "Run this in a terminal. The key is read from an environment variable, so put the export in your shell profile too.",
            Self::Cursor => "Add this to ~/.cursor/mcp.json (or the project's .cursor/mcp.json).",
            Self::ClaudeDesktop => {
                "Add this to claude_desktop_config.json (Settings, Developer, Edit Config). Claude Desktop only starts local \
                 commands, so mcp-remote bridges to the server; it needs Node.js."
            }
            Self::Other => "Any MCP client that speaks streamable HTTP: this URL, with this header on every request.",
        }
    }
}

/// The MCP endpoint of the server at `server`.
pub fn endpoint(server: &str) -> String {
    format!("{}/mcp", server.trim().trim_end_matches('/'))
}

/// The header an MCP client sends.
pub fn header_line(key: &str) -> String {
    format!("Authorization: Bearer {key}")
}

/// Inside a shell's double quotes.
fn sh_dq(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// A TOML basic string, quotes included.
fn toml_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The ready-to-paste setup of `client` for `url` (the endpoint) and `key`.
pub fn snippet(client: AssistantClient, url: &str, key: &str) -> String {
    let name = SERVER_NAME;
    let pretty = |v: Value| serde_json::to_string_pretty(&v).unwrap_or_default();
    match client {
        AssistantClient::ClaudeCode => format!(
            "claude mcp add --transport http {name} \"{}\" --header \"{}\"",
            sh_dq(url),
            sh_dq(&header_line(key))
        ),
        // the CLI takes the bearer token from an environment variable
        AssistantClient::Codex => format!(
            "export FINANCE_MCP_KEY=\"{}\"\ncodex mcp add {name} --url \"{}\" --bearer-token-env-var FINANCE_MCP_KEY",
            sh_dq(key),
            sh_dq(url)
        ),
        AssistantClient::Cursor => pretty(json!({
            "mcpServers": { name: { "url": url, "headers": { "Authorization": format!("Bearer {key}") } } }
        })),
        // mcp-remote documents the header with no space after the colon and the
        // value in an env var (a space in args breaks it on Windows)
        AssistantClient::ClaudeDesktop => {
            let mut args = vec![json!("-y"), json!("mcp-remote"), json!(url)];
            if url.starts_with("http://") {
                args.push(json!("--allow-http"));
            }
            args.extend([json!("--header"), json!("Authorization:${FINANCE_MCP_AUTH}")]);
            pretty(json!({
                "mcpServers": { name: { "command": "npx", "args": args, "env": { "FINANCE_MCP_AUTH": format!("Bearer {key}") } } }
            }))
        }
        AssistantClient::Other => format!("URL: {url}\nHeader: {}", header_line(key)),
    }
}

/// Local mode: the command that adds `anser mcp` (stdio, this
/// device's copy, no key) to Claude Code, for the executable at `exe`.
pub fn local_command(exe: &str) -> String {
    let q = if exe.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c)) { exe.to_string() } else { format!("\"{}\"", sh_dq(exe)) };
    format!("claude mcp add {SERVER_NAME}-local -- {q} mcp")
}

/// Local mode: the config block for Claude Desktop, Cursor and the like.
pub fn local_json(exe: &str) -> String {
    serde_json::to_string_pretty(&json!({"mcpServers": {format!("{SERVER_NAME}-local"): {"command": exe, "args": ["mcp"]}}})).unwrap_or_default()
}

/// A second way to set `client` up, when it has one: Codex's config.toml
/// block (streamable HTTP servers take a url and extra headers).
pub fn alt_snippet(client: AssistantClient, url: &str, key: &str) -> Option<String> {
    (client == AssistantClient::Codex).then(|| {
        format!(
            "[mcp_servers.{SERVER_NAME}]\nurl = {}\nhttp_headers = {{ \"Authorization\" = {} }}",
            toml_str(url),
            toml_str(&format!("Bearer {key}"))
        )
    })
}

/// A prompt to paste to an agent, so it adds the server itself.
pub fn agent_prompt(client: AssistantClient, url: &str, key: &str) -> String {
    format!(
        "Add my personal-finance MCP server to {label}. It is read-only (it cannot move money).\n\
         Name: {SERVER_NAME}\nURL: {url}\nTransport: streamable HTTP\nHeader on every request: {header}\n\n\
         Setup for {label}:\n{snip}{alt}\n\n\
         Keep the key out of the repo and out of your replies. When it is added, list the server's tools to check it works.",
        label = client.label(),
        header = header_line(key),
        snip = snippet(client, url, key),
        alt = alt_snippet(client, url, key).map(|a| format!("\nor in the config file:\n{a}")).unwrap_or_default()
    )
}

/// The default name of a new key: "Claude Code on laptop", or with the date.
pub fn default_key_name(client: AssistantClient, device: &str, date: &str) -> String {
    let device = device.trim().trim_end_matches(" (desktop)");
    let name = if device.is_empty() { format!("{} {date}", client.label()) } else { format!("{} on {device}", client.label()) };
    name.chars().take(60).collect()
}

/// "never", "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn relative(now_ms: i64, then_ms: Option<i64>) -> String {
    let Some(then) = then_ms else { return "never".into() };
    let secs = (now_ms - then).max(0) / 1000;
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=172_799 => "1 day ago".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

// ---------- the Test button ----------

/// initialize, then tools/list, against `url` with `key`: "Connected, 8
/// tools, 42 ms". Blocking: call it off the UI thread. Native only (a
/// browser's fetch can't send the bearer key without its cookie).
#[cfg(feature = "native")]
pub fn test_connection(url: &str, key: &str) -> Result<String, String> {
    let agent = crate::upload::agent(std::time::Duration::from_secs(20));
    let started = std::time::Instant::now();
    let call = |id: u32, method: &str, params: Value| -> Result<Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut r = agent
            .post(url)
            .header("Authorization", format!("Bearer {key}"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .send(serde_json::to_vec(&body).unwrap_or_default().as_slice())
            .map_err(|e| e.to_string())?;
        let status = r.status().as_u16();
        let bytes = r.body_mut().with_config().limit(4 << 20).read_to_vec().map_err(|e| e.to_string())?;
        match status {
            200 => {}
            401 | 403 => return Err("the server refused the key".into()),
            404 => return Err("MCP is off on the server, or the URL is wrong".into()),
            s => return Err(format!("HTTP {s}")),
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| "the server's answer isn't MCP".to_string())?;
        match v.get("error") {
            Some(e) => Err(e["message"].as_str().unwrap_or("the server returned an error").to_string()),
            None => Ok(v["result"].clone()),
        }
    };
    call(1, "initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "finance-settings-test", "version": "1"}}))?;
    let tools = call(2, "tools/list", json!({}))?;
    let n = tools["tools"].as_array().map_or(0, Vec::len);
    Ok(format!("Connected, {n} tool{}, {} ms", if n == 1 { "" } else { "s" }, started.elapsed().as_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://box.tail1234.ts.net:8787/mcp";
    const KEY: &str = "fin_abc123";

    #[test]
    fn endpoint_joins_cleanly() {
        assert_eq!(endpoint("https://h:8787/"), "https://h:8787/mcp");
        assert_eq!(endpoint(" http://127.0.0.1:8787 "), "http://127.0.0.1:8787/mcp");
    }

    #[test]
    fn local_mode_setup() {
        assert_eq!(local_command("/opt/finance/anser"), "claude mcp add anser-local -- /opt/finance/anser mcp");
        assert_eq!(local_command("/my apps/fd"), "claude mcp add anser-local -- \"/my apps/fd\" mcp");
        let v: Value = serde_json::from_str(&local_json("/x/fd")).unwrap();
        assert_eq!(v["mcpServers"]["anser-local"], json!({"command": "/x/fd", "args": ["mcp"]}));
    }

    #[test]
    fn claude_code_command() {
        assert_eq!(
            snippet(AssistantClient::ClaudeCode, URL, KEY),
            "claude mcp add --transport http anser \"https://box.tail1234.ts.net:8787/mcp\" --header \"Authorization: Bearer fin_abc123\""
        );
        // shell metacharacters stay literal inside the double quotes
        let s = snippet(AssistantClient::ClaudeCode, URL, "a\"b$c`d\\e");
        assert!(s.contains(r#"Bearer a\"b\$c\`d\\e""#), "{s}");
    }

    #[test]
    fn codex_toml_escapes() {
        let c = snippet(AssistantClient::Codex, URL, KEY);
        assert_eq!(c, "export FINANCE_MCP_KEY=\"fin_abc123\"\ncodex mcp add anser --url \"https://box.tail1234.ts.net:8787/mcp\" --bearer-token-env-var FINANCE_MCP_KEY");
        assert!(alt_snippet(AssistantClient::Cursor, URL, KEY).is_none());
        let s = alt_snippet(AssistantClient::Codex, URL, "k\"ey\\").unwrap();
        assert!(s.starts_with("[mcp_servers.anser]\nurl = \"https://"));
        assert!(s.contains(r#"http_headers = { "Authorization" = "Bearer k\"ey\\" }"#), "{s}");
    }

    #[test]
    fn json_clients_are_valid_json_with_the_key() {
        let v: Value = serde_json::from_str(&snippet(AssistantClient::Cursor, URL, "k\"ey")).unwrap();
        assert_eq!(v["mcpServers"]["anser"]["url"], URL);
        assert_eq!(v["mcpServers"]["anser"]["headers"]["Authorization"], "Bearer k\"ey");

        let v: Value = serde_json::from_str(&snippet(AssistantClient::ClaudeDesktop, URL, KEY)).unwrap();
        let f = &v["mcpServers"]["anser"];
        assert_eq!(f["command"], "npx");
        assert_eq!(f["args"][1], "mcp-remote");
        assert_eq!(f["args"][2], URL);
        assert!(!f["args"].to_string().contains("--allow-http"));
        assert_eq!(f["env"]["FINANCE_MCP_AUTH"], "Bearer fin_abc123");
        // plain http (a loopback server) needs the bridge's flag
        let v: Value = serde_json::from_str(&snippet(AssistantClient::ClaudeDesktop, "http://127.0.0.1:8787/mcp", KEY)).unwrap();
        assert!(v["mcpServers"]["anser"]["args"].to_string().contains("--allow-http"));
    }

    #[test]
    fn other_and_prompt() {
        assert_eq!(snippet(AssistantClient::Other, URL, KEY), format!("URL: {URL}\nHeader: Authorization: Bearer {KEY}"));
        let p = agent_prompt(AssistantClient::Codex, URL, KEY);
        assert!(p.contains(URL) && p.contains("Authorization: Bearer fin_abc123") && p.contains("codex mcp add") && p.contains("[mcp_servers.anser]"));
    }

    #[test]
    fn key_names() {
        assert_eq!(default_key_name(AssistantClient::Codex, "laptop (desktop)", "2026-10-16"), "Codex on laptop");
        assert_eq!(default_key_name(AssistantClient::Cursor, "", "2026-10-16"), "Cursor 2026-10-16");
        assert!(default_key_name(AssistantClient::Cursor, &"x".repeat(200), "d").chars().count() <= 60);
    }

    #[test]
    fn relative_labels() {
        let now = 1_000_000_000_000;
        assert_eq!(relative(now, None), "never");
        assert_eq!(relative(now, Some(now - 5_000)), "just now");
        assert_eq!(relative(now, Some(now - 5 * 60_000)), "5 min ago");
        assert_eq!(relative(now, Some(now - 3 * 3_600_000)), "3 h ago");
        assert_eq!(relative(now, Some(now - 86_400_000)), "1 day ago");
        assert_eq!(relative(now, Some(now - 2 * 86_400_000)), "2 days ago");
        assert_eq!(relative(now, Some(now + 9_000)), "just now");
    }

    #[test]
    fn status_parses() {
        let s: McpStatus = serde_json::from_value(json!({"enabled": true, "path": "/mcp",
            "keys": [{"id": 3, "name": "a", "created": 5, "last_used": null}, {"id": 4, "name": "b", "created": 6, "last_used": 9}]})).unwrap();
        assert!(s.enabled && s.keys.len() == 2 && s.keys[0].last_used.is_none() && s.keys[1].last_used == Some(9));
    }
}
