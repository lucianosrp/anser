//! Just enough of the Chrome DevTools Protocol: JSON calls over the
//! browser's websocket (flat sessions), events kept only when asked for.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

/// How long one call may take (a screenshot of a 3x phone is the slowest).
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

pub struct Cdp {
    ws: WebSocket<TcpStream>,
    next: u64,
    events: VecDeque<Value>,
    /// keep events that arrive during calls (the screencast); else drop them
    pub keep_events: bool,
}

impl Cdp {
    /// Connect to the browser endpoint `ws://127.0.0.1:{port}{path}`.
    pub fn connect(port: u16, path: &str) -> Result<Cdp> {
        let tcp = TcpStream::connect(("127.0.0.1", port)).context("connecting to Chrome's DevTools port")?;
        tcp.set_nodelay(true)?;
        // screenshots are one big message each
        let cfg = WebSocketConfig::default().max_message_size(Some(512 << 20)).max_frame_size(Some(512 << 20));
        let url = format!("ws://127.0.0.1:{port}{path}");
        let (ws, _) = tungstenite::client::client_with_config(url.as_str(), tcp, Some(cfg))
            .map_err(|e| anyhow!("DevTools websocket handshake: {e}"))?;
        ws.get_ref().set_read_timeout(Some(Duration::from_millis(100)))?;
        Ok(Cdp { ws, next: 0, events: VecDeque::new(), keep_events: false })
    }

    /// One message, or None when nothing arrived within the read timeout.
    fn recv(&mut self) -> Result<Option<Value>> {
        match self.ws.read() {
            Ok(Message::Text(t)) => Ok(Some(serde_json::from_str(t.as_str())?)),
            Ok(Message::Binary(b)) => Ok(Some(serde_json::from_slice(&b)?)),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(anyhow!("DevTools connection: {e}")),
        }
    }

    /// Call `method` (on a page's session, or the browser's with None) and
    /// wait for its result.
    pub fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = s.into();
        }
        self.ws.send(Message::Text(msg.to_string().into())).with_context(|| format!("sending {method}"))?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        while Instant::now() < deadline {
            let Some(v) = self.recv()? else { continue };
            if v["id"].as_u64() == Some(id) {
                if let Some(e) = v.get("error") {
                    bail!("{method}: {e}");
                }
                return Ok(v["result"].clone());
            }
            if self.keep_events && v.get("method").is_some() {
                self.events.push_back(v);
            }
        }
        bail!("{method}: no answer in {}s", CALL_TIMEOUT.as_secs())
    }

    /// The events so far plus those arriving within `wait`.
    pub fn events(&mut self, wait: Duration) -> Result<Vec<Value>> {
        let mut out: Vec<Value> = self.events.drain(..).collect();
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if let Some(v) = self.recv()? {
                if v.get("method").is_some() {
                    out.push(v);
                }
            }
        }
        Ok(out)
    }

    /// Evaluate a JS expression in a page, by value.
    pub fn eval(&mut self, session: &str, expr: &str) -> Result<Value> {
        let r = self.call(Some(session), "Runtime.evaluate", json!({ "expression": expr, "returnByValue": true }))?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("{expr}: {ex}");
        }
        Ok(r["result"]["value"].clone())
    }
}
