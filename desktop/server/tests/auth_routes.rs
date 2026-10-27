//! Accounts end to end on a throwaway new install: the default mode (all),
//! pairing (a code works once), the browser login cookie, the admin code,
//! whoami, the sources APIs with a stand-in tester (refusals, never echoing
//! a secret), settings and profile writes, and /api/preview.

use std::collections::BTreeMap;
use std::time::Duration;

use finance_engine::fetch::verify::Verdict;
use finance_server::{api, auth, server};
use serde_json::{json, Value};

const SECRET: &str = "sk_live_SUPERSECRET_abcdefghijklmnopWXYZ";

fn tester(key: &str, fields: &BTreeMap<String, String>) -> Verdict {
    // a "trading" secret is refused, like a key with trade permission;
    // an error message quoting the secret must not reach the client
    let s = fields.get("BINANCE_API_SECRET").cloned().unwrap_or_default();
    if s.contains("TRADE") {
        return Verdict { ok: false, detail: "This key can do more than read (enableSpotAndMarginTrading).".into(),
            refused: vec!["enableSpotAndMarginTrading".into()], sections: vec![] };
    }
    Verdict { ok: key == "binance", detail: "Read-only.".into(), refused: vec![], sections: vec![] }
}

#[test]
fn pairing_login_sources_settings() {
    let home = std::env::temp_dir().join(format!("finance-auth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::remove_var("FINANCE_AUTH");
    for k in finance_engine::secrets::SECRET_KEYS {
        std::env::remove_var(k);
    }
    api::set_tester(tester);

    let boot = auth::bootstrap().unwrap();
    assert_eq!(boot.default_mode, Some(auth::Mode::All), "no finance.db before: a new install locks reads too");
    assert!(boot.backup.is_none());
    // a fresh install has no users until someone claims it (signup.rs tests
    // the app's way); `anser-server pair` claims it for the shell's user
    assert!(auth::users().unwrap().is_empty());
    auth::ensure_owner().unwrap();
    assert_eq!(auth::users().unwrap()[0].role, "admin");
    assert!(auth::bootstrap().unwrap().default_mode.is_none(), "a second start changes nothing");
    assert_eq!(auth::users().unwrap().len(), 1);

    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let call = |method: &str, path: &str, hdrs: &[(&str, &str)], body: Option<Value>| -> (u16, Vec<(String, String)>, Vec<u8>) {
        let url = format!("{base}{path}");
        let mut r = match (method, &body) {
            ("GET", _) => {
                let mut b = agent.get(&url);
                for (k, v) in hdrs {
                    b = b.header(*k, *v);
                }
                b.call().unwrap()
            }
            ("DELETE", _) => {
                let mut b = agent.delete(&url);
                for (k, v) in hdrs {
                    b = b.header(*k, *v);
                }
                b.call().unwrap()
            }
            (m, b) => {
                let mut rb = if m == "PUT" { agent.put(&url) } else { agent.post(&url) };
                for (k, v) in hdrs {
                    rb = rb.header(*k, *v);
                }
                rb.header("Content-Type", "application/json").send(serde_json::to_vec(&b.clone().unwrap_or(Value::Null)).unwrap()).unwrap()
            }
        };
        let h = r.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
        (r.status().as_u16(), h, r.body_mut().read_to_vec().unwrap())
    };
    let js = |b: &[u8]| serde_json::from_slice::<Value>(b).unwrap();

    // mode all: data needs a device; the web app's shell and /login don't
    assert_eq!(call("GET", "/api/export.json", &[], None).0, 401);
    assert_eq!(call("GET", "/api/db.sqlite", &[], None).0, 401);
    assert_eq!(call("GET", "/healthz", &[], None).0, 200);
    assert_eq!(call("GET", "/login", &[], None).0, 200);
    let (s, _, b) = call("GET", "/api/whoami", &[], None);
    assert_eq!((s, js(&b)["paired"].clone(), js(&b)["auth_mode"].clone()), (200, json!(false), json!("all")));
    // a rebinding Host is refused before anything else
    assert_eq!(call("GET", "/healthz", &[("Host", "attacker.example")], None).0, 421);

    // pairing: a code works once
    let (code, _) = auth::new_pair_code(1).unwrap();
    assert_eq!(call("POST", "/api/pair", &[], Some(json!({"code": "WRONGONE", "device_name": "x"}))).0, 400);
    let (s, _, b) = call("POST", "/api/pair", &[], Some(json!({"code": code.to_lowercase(), "device_name": "laptop"})));
    assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
    let token = js(&b)["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("fin_") && token.len() == 68);
    assert_eq!(call("POST", "/api/pair", &[], Some(json!({"code": code, "device_name": "again"}))).0, 400, "spent");
    // only the hash is stored
    let raw = std::fs::read(home.join("data/server.db")).unwrap();
    assert!(!raw.windows(token.len()).any(|w| w == token.as_bytes()), "the token itself is never stored");
    let bearer = format!("Bearer {token}");
    let auth_h: &[(&str, &str)] = &[("Authorization", &bearer)];
    assert_eq!(call("GET", "/api/export.json", auth_h, None).0, 200);
    let (_, _, b) = call("GET", "/api/whoami", auth_h, None);
    assert_eq!((js(&b)["device"]["name"].clone(), js(&b)["user"]["role"].clone()), (json!("laptop"), json!("admin")));
    assert_eq!(call("GET", "/api/export.json", &[("Authorization", "Bearer fin_nope")], None).0, 401);

    // the admin code, then a browser login: cookie, and its writes need the header
    let (s, _, b) = call("POST", "/api/admin/pair-code", auth_h, None);
    assert_eq!(s, 200);
    let code2 = js(&b)["code"].as_str().unwrap().to_string();
    assert_eq!(call("POST", "/api/login", &[], Some(json!({"code": code2}))).0, 403, "no X-Finance-Request");
    let (s, h, _) = call("POST", "/api/login", &[("X-Finance-Request", "1"), ("Origin", &base)], Some(json!({"code": code2})));
    assert_eq!(s, 200);
    let cookie = h.iter().find(|(k, _)| k.eq_ignore_ascii_case("set-cookie")).map(|(_, v)| v.clone()).unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("Secure") && cookie.contains("SameSite=Strict"), "{cookie}");
    let session = cookie.split(';').next().unwrap().to_string();
    let browser: &[(&str, &str)] = &[("Cookie", &session)];
    assert_eq!(call("GET", "/api/ledger.json", browser, None).0, 200);
    assert_eq!(call("PUT", "/api/settings/budgets", browser, Some(json!({"monthly": {}}))).0, 403, "cookie write without the header");
    let browser_w: &[(&str, &str)] = &[("Cookie", &session), ("X-Finance-Request", "1"), ("Origin", "https://evil.example")];
    assert_eq!(call("PUT", "/api/settings/budgets", browser_w, Some(json!({"monthly": {}}))).0, 403, "foreign Origin");
    let browser_ok: &[(&str, &str)] = &[("Cookie", &session), ("X-Finance-Request", "1"), ("Origin", &base)];
    assert_eq!(call("PUT", "/api/settings/budgets", browser_ok, Some(json!({"monthly": {"Dining": 2200.0}}))).0, 200);

    // settings and profile land in the bundle
    let (s, _, b) = call("PUT", "/api/profile", auth_h, Some(json!({"base_currency": "eur", "home_country": "de", "name": "Test"})));
    assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
    let (_, _, b) = call("GET", "/api/export.json", auth_h, None);
    let bundle = js(&b);
    assert_eq!(bundle["user_settings"]["budgets"]["monthly"]["Dining"], json!(2200.0));
    assert_eq!(bundle["profile"]["base_currency"], json!("EUR"));
    assert_eq!(call("PUT", "/api/settings/secrets", auth_h, Some(json!({}))).0, 404);

    // sources: the catalog, a refused trading key, a saved read-only key
    let (s, _, b) = call("GET", "/api/sources/catalog", auth_h, None);
    assert_eq!((s, js(&b)["api"].as_array().unwrap().len()), (200, 3));
    assert_eq!(call("PUT", "/api/sources/binance/secret", &[], Some(json!({}))).0, 401, "secret routes always need a device");
    let trading = format!("{SECRET}TRADE");
    let (s, _, b) = call("PUT", "/api/sources/binance/secret", auth_h,
        Some(json!({"fields": {"BINANCE_API_KEY": "abcdefghijklmnop1234", "BINANCE_API_SECRET": trading}})));
    assert_eq!(s, 422);
    assert_eq!(js(&b)["test"]["refused"], json!(["enableSpotAndMarginTrading"]));
    assert!(!String::from_utf8_lossy(&b).contains(SECRET));
    let (s, _, b) = call("PUT", "/api/sources/binance/secret", auth_h,
        Some(json!({"fields": {"BINANCE_API_KEY": "abcdefghijklmnop1234", "BINANCE_API_SECRET": SECRET}})));
    assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
    assert!(!String::from_utf8_lossy(&b).contains(SECRET), "never echoed");
    // the store is encrypted at rest, outside data/
    let enc = std::fs::read(home.join("secrets/1.json.enc")).unwrap();
    assert!(!enc.windows(12).any(|w| w == &SECRET.as_bytes()[..12]));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("secrets/master.key")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let mode = std::fs::metadata(home.join("secrets")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
    // GET /api/sources: configured from the store, the last 4 only, and only to a device
    let (_, _, b) = call("GET", "/api/sources", auth_h, None);
    let text = String::from_utf8_lossy(&b).to_string();
    assert!(!text.contains(SECRET) && !text.contains("abcdefghijklmnop1234"), "{text}");
    let src = js(&b);
    let bin = src["api"].as_array().unwrap().iter().find(|a| a["key"] == "binance").unwrap().clone();
    assert_eq!((bin["configured"].clone(), bin["configured_via"].clone()), (json!(true), json!("store")));
    assert_eq!(bin["fields"][1]["hint"], json!("…WXYZ"));
    // the fetchers read it live, no restart
    assert_eq!(finance_engine::config::env("BINANCE_API_SECRET").as_deref(), Some(SECRET));
    let (_, _, b) = call("GET", "/api/capabilities", auth_h, None);
    let caps = js(&b);
    let c = caps["sources"].as_array().unwrap().iter().find(|s| s["key"] == "binance").unwrap().clone();
    assert_eq!(c["configured_via"], json!("store"));
    // test on demand, then remove
    let (s, _, b) = call("POST", "/api/sources/binance/test", auth_h, None);
    assert_eq!((s, js(&b)["ok"].clone()), (200, json!(true)));
    assert_eq!(call("DELETE", "/api/sources/binance/secret", auth_h, None).0, 200);
    assert_eq!(finance_engine::config::env("BINANCE_API_SECRET"), None);

    // the auth mode switch is admin-only, and takes effect at once
    let (s, _, _) = call("POST", "/api/admin/auth-mode", auth_h, Some(json!({"mode": "writes"})));
    assert_eq!(s, 200);
    assert_eq!(call("GET", "/api/export.json", &[], None).0, 200, "writes: reads open again");
    assert_eq!(auth::mode(), auth::Mode::Writes);

    // preview: nothing written
    let mp = finance_core::upload::encode_multipart("B0und", &[("x.xlsx".into(), b"PK".to_vec())]);
    let mut r = agent.post(&format!("{base}/api/preview")).header("Authorization", &bearer)
        .header("Content-Type", "multipart/form-data; boundary=B0und").send(&mp[..]).unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: Value = serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap();
    assert_eq!(v["files"][0]["ok"], json!(false));
    assert!(std::fs::read_dir(home.join("transactions")).unwrap().next().is_none());

    // logout revokes the browser's device
    assert_eq!(call("POST", "/api/logout", browser_ok, None).0, 200);
    assert_eq!(call("GET", "/api/whoami", browser, None).2.windows(13).any(|w| w == b"\"paired\":true"), false);
    let _ = std::fs::remove_dir_all(home);
}
