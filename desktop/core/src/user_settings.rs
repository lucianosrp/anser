//! Budgets, the cash-flow calendar and saved filters, kept on the server
//! (the bundle's `user_settings` section) and shared by every device and the
//! web app. This is the client's copy: filled from each bundle, changed
//! locally at once on a write, and written through `PUT /api/settings/{key}`.
//!
//! An older server sends no section: [`server_side`] is false and the app
//! keeps using its local toml files.

use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::client::{self, BoxFuture, Request};
use crate::Bundle;

/// None: the server keeps no settings (or nothing loaded yet).
static STATE: Mutex<Option<Map<String, Value>>> = Mutex::new(None);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Take the server's settings from a freshly loaded bundle.
pub fn set_from_bundle(b: &Bundle) {
    let v = b.at(&["user_settings"]).as_object().cloned();
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = v;
}

pub fn server_side() -> bool {
    STATE.lock().map(|s| s.is_some()).unwrap_or(false)
}

pub fn get(key: &str) -> Option<Value> {
    STATE.lock().ok()?.as_ref()?.get(key).cloned()
}

/// Change a setting here now and write it to the server in the background.
/// False when the server keeps no settings (the caller saves locally).
pub fn save(key: &str, value: Value) -> bool {
    match put(key, value) {
        Some(f) => {
            client::spawn(f);
            true
        }
        None => false,
    }
}

/// Change a setting here now, and send it to the server: the returned
/// future does the write (the caller spawns it). None when the server keeps
/// no settings.
pub fn put(key: &str, value: Value) -> Option<BoxFuture> {
    {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        g.as_mut()?.insert(key.to_string(), value.clone());
    }
    let req = Request::new("PUT", &format!("/api/settings/{key}")).json(&value);
    let fut = client::send(req);
    Some(Box::pin(async move {
        let r = fut.await;
        let err = match &r {
            Ok(resp) if resp.ok() => None,
            Ok(resp) => Some(resp.error()),
            Err(e) => Some(e.clone()),
        };
        *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = err;
        r
    }))
}

/// The last write's error, if it failed (a page shows it).
pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().ok()?.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn follows_the_bundle() {
        set_from_bundle(&Bundle(json!({"version": 1})));
        assert!(!server_side());
        assert!(put("budgets", json!({})).is_none(), "an older server: stay local");
        set_from_bundle(&Bundle(json!({"user_settings": {"budgets": {"monthly": {"Dining": 1.0}}}})));
        assert!(server_side());
        assert_eq!(get("budgets"), Some(json!({"monthly": {"Dining": 1.0}})));
        assert_eq!(get("calendar"), None);
    }
}
