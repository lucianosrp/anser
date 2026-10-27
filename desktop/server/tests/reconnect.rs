//! Jump back in: a sign-out with the device's resume key gets a code that
//! works only with that key, once, with no time limit; one per signed-out
//! device; an admin's sign-out of that device, removing the user, or five
//! wrong codes from it end it; wrong codes count toward the global
//! throttle; only hashes are stored. One test: the throttle and the
//! environment are the process's.

use std::time::Duration;

use finance_server::{auth, server};
use serde_json::{json, Value};

struct Client {
    base: String,
    agent: ureq::Agent,
}

impl Client {
    fn call(&self, method: &str, path: &str, hdrs: &[(&str, &str)], body: Option<Value>) -> (u16, Vec<(String, String)>, Value) {
        let url = format!("{}{path}", self.base);
        let mut r = match method {
            "GET" | "DELETE" => {
                let mut b = if method == "GET" { self.agent.get(&url) } else { self.agent.delete(&url) };
                for (k, v) in hdrs {
                    b = b.header(*k, *v);
                }
                b.call().unwrap()
            }
            _ => {
                let mut rb = self.agent.post(&url);
                for (k, v) in hdrs {
                    rb = rb.header(*k, *v);
                }
                rb.header("Content-Type", "application/json").send(serde_json::to_vec(&body.unwrap_or(Value::Null)).unwrap()).unwrap()
            }
        };
        let h = r.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
        let status = r.status().as_u16();
        let v = serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap_or(Value::Null);
        (status, h, v)
    }

    /// Pair a device of user `uid` with a fresh pairing code (and `key`):
    /// its bearer header and device id.
    fn device(&self, uid: i64, name: &str, key: Option<&str>) -> (String, i64) {
        let (code, _) = auth::new_pair_code(uid).unwrap();
        let (s, _, v) = self.call("POST", "/api/pair", &[], Some(json!({"code": code, "device_name": name, "resume_key": key})));
        assert_eq!(s, 200, "{v}");
        (format!("Bearer {}", v["token"].as_str().unwrap()), v["device"]["id"].as_i64().unwrap())
    }

    /// Sign out with `key`: the code to jump back in, if any.
    fn logout(&self, bearer: &str, key: Option<&str>) -> Option<String> {
        let body = match key {
            Some(k) => json!({"resume_key": k}),
            None => json!({}),
        };
        let (s, _, v) = self.call("POST", "/api/logout", &[("Authorization", bearer)], Some(body));
        assert_eq!(s, 200, "{v}");
        assert_eq!(self.call("GET", "/api/whoami", &[("Authorization", bearer)], None).2["paired"], json!(false), "the token is gone");
        v["reconnect_code"].as_str().map(String::from)
    }

    /// Sign in with `code` (and `key`): the status and the reply.
    fn pair(&self, code: &str, key: Option<&str>) -> (u16, Value) {
        let (s, _, v) = self.call("POST", "/api/pair", &[], Some(json!({"code": code, "device_name": "back", "resume_key": key})));
        (s, v)
    }
}

/// A resume key as a client makes one: 64 hex characters.
fn key(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn rows() -> i64 {
    auth::open().unwrap().query_row("SELECT COUNT(*) FROM reconnects", [], |r| r.get(0)).unwrap()
}

#[test]
fn jump_back_in() {
    let home = std::env::temp_dir().join(format!("finance-reconnect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::remove_var("FINANCE_AUTH");
    std::env::remove_var("FINANCE_ALLOW_SIGNUP");

    auth::bootstrap().unwrap();
    let signed = auth::signup("Owner", None, None, "desk").unwrap();
    assert!(signed.first);
    // the admin's own device, signed in throughout
    let desk = format!("Bearer {}", signed.token);
    let admin: &[(&str, &str)] = &[("Authorization", &desk)];

    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let c = Client {
        base: base.clone(),
        agent: ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into(),
    };

    // ---- no resume key (an older client, Sign out for good): no code, nothing left ----
    let (b, _) = c.device(1, "old", None);
    assert_eq!(c.logout(&b, None), None);
    let (b, _) = c.device(1, "shared", None);
    let (s, _, v) = c.call("POST", "/api/logout", &[("Authorization", &b)], Some(json!({"resume_key": "too short"})));
    assert_eq!((s, v.get("reconnect_code")), (200, None), "a malformed key is no key");
    assert_eq!(rows(), 0);
    // nobody signed in: nothing to sign out, no code
    let (s, _, v) = c.call("POST", "/api/logout", &[], Some(json!({"resume_key": key(9)})));
    assert_eq!((s, v.get("reconnect_code")), (200, None));

    // ---- the code works only with its resume key, once ----
    let k1 = key(1);
    let (b, _) = c.device(1, "laptop", Some(&k1));
    let code = c.logout(&b, Some(&k1)).expect("a code to jump back in");
    assert_eq!(code.len(), auth::CODE_LEN);
    // only hashes are kept
    let mut raw = std::fs::read(home.join("data/server.db")).unwrap();
    raw.extend(std::fs::read(home.join("data/server.db-wal")).unwrap_or_default());
    let has = |s: &str| raw.windows(s.len()).any(|w| w == s.as_bytes());
    assert!(!has(&code) && !has(&k1), "no plaintext code or resume key in server.db");
    assert!(has(&auth::sha256_hex(&code)) && has(&auth::sha256_hex(&k1)));
    let (s, v) = c.pair(&code, None);
    assert_eq!((s, v["reason"].clone()), (400, json!("wrong")), "an older client (no resume key) can't use it");
    let (s, v) = c.pair(&code, Some(&key(2)));
    assert_eq!((s, v["reason"].clone()), (400, json!("wrong")), "another device can't use it");
    let (s, v) = c.pair(&code.to_lowercase(), Some(&k1));
    assert_eq!(s, 200, "{v}");
    assert_eq!(c.call("GET", "/api/whoami", &[("Authorization", &format!("Bearer {}", v["token"].as_str().unwrap()))], None).2["paired"], json!(true));
    let (s, _) = c.pair(&code, Some(&k1));
    assert_eq!(s, 400, "single use");
    assert_eq!(rows(), 0);

    // ---- two devices signed out independently: each keeps its own code ----
    let (kp, kl) = (key(3), key(4));
    let (bp, phone) = c.device(1, "phone", Some(&kp));
    let (bl, laptop) = c.device(1, "laptop", Some(&kl));
    let cp = c.logout(&bp, Some(&kp)).unwrap();
    let cl = c.logout(&bl, Some(&kl)).unwrap();
    assert_ne!(cp, cl);
    let (_, _, v) = c.call("GET", "/api/admin/devices", admin, None);
    let out: Vec<i64> = v["signed_out"].as_array().unwrap().iter().map(|d| d["id"].as_i64().unwrap()).collect();
    assert_eq!(out, vec![phone, laptop], "{v}");
    assert_eq!(v["signed_out"][0]["name"], json!("phone"));
    assert_eq!(c.pair(&cp, Some(&kl)).0, 400, "the phone's code on the laptop");
    assert_eq!(c.pair(&cl, Some(&kl)).0, 200);
    assert_eq!(c.pair(&cp, Some(&kp)).0, 200, "the phone's code still works after the laptop's");
    // signing out again replaces this device's earlier code
    let (b, _) = c.device(1, "phone", Some(&kp));
    let first = c.logout(&b, Some(&kp)).unwrap();
    // ... and signing in on it by a pairing code spends it (it is back)
    let (b, _) = c.device(1, "phone", Some(&kp));
    assert_eq!(rows(), 0, "signed back in on that device: its code is spent");
    assert_eq!(c.pair(&first, Some(&kp)).0, 400);
    let second = c.logout(&b, Some(&kp)).unwrap();
    let (b, _) = c.device(1, "phone", Some(&kp));
    let third = c.logout(&b, Some(&kp)).unwrap();
    assert_eq!(rows(), 1, "one code per signed-out device");
    assert_eq!(c.pair(&second, Some(&kp)).0, 400, "replaced");
    assert_eq!(c.pair(&third, Some(&kp)).0, 200);

    // ---- an admin's Sign out of that device ends its code ----
    let kt = key(5);
    let (b, tablet) = c.device(1, "tablet", Some(&kt));
    let ct = c.logout(&b, Some(&kt)).unwrap();
    // a device paired since never takes the signed-out one's id
    let (newer, newer_id) = c.device(1, "newer", None);
    assert!(newer_id > tablet);
    let (s, _, v) = c.call("DELETE", &format!("/api/admin/devices/{tablet}"), admin, None);
    assert_eq!((s, v["ok"].clone()), (200, json!(true)), "{v}");
    assert_eq!(c.call("GET", "/api/whoami", &[("Authorization", &newer)], None).2["paired"], json!(true), "only the tablet");
    assert_eq!(c.pair(&ct, Some(&kt)).0, 400, "cancelled by the admin");
    assert_eq!(c.call("GET", "/api/admin/devices", admin, None).2["signed_out"], json!([]));

    // ---- removing the user ends theirs ----
    let sam = auth::add_user("Sam", None, false).unwrap();
    let ks = key(6);
    let (b, _) = c.device(sam, "sam's phone", Some(&ks));
    let cs = c.logout(&b, Some(&ks)).unwrap();
    assert_eq!(c.call("DELETE", &format!("/api/admin/users/{sam}"), admin, None).0, 200);
    assert_eq!(rows(), 0);
    assert_eq!(c.pair(&cs, Some(&ks)).0, 400);

    // ---- a browser: the same, with its session cookie ----
    let kb = key(10);
    let pre: &[(&str, &str)] = &[("X-Finance-Request", "1"), ("Origin", &base)];
    let (code, _) = auth::new_pair_code(1).unwrap();
    let (s, h, _) = c.call("POST", "/api/login", pre, Some(json!({"code": code, "resume_key": &kb})));
    assert_eq!(s, 200);
    let cookie = |h: &[(String, String)]| h.iter().find(|(k, _)| k.eq_ignore_ascii_case("set-cookie")).map(|(_, v)| v.split(';').next().unwrap().to_string()).unwrap();
    let session = cookie(&h);
    let (s, _, v) = c.call("POST", "/api/logout", &[("Cookie", &session), pre[0], pre[1]], Some(json!({"resume_key": &kb})));
    assert_eq!(s, 200, "{v}");
    let cb = v["reconnect_code"].as_str().expect("the browser's code").to_string();
    assert_eq!(c.call("GET", "/api/whoami", &[("Cookie", &session)], None).2["paired"], json!(false));
    let (s, h, _) = c.call("POST", "/api/login", pre, Some(json!({"code": &cb, "resume_key": &kb})));
    assert_eq!(s, 200);
    assert_eq!(c.call("GET", "/api/whoami", &[("Cookie", &cookie(&h))], None).2["paired"], json!(true), "back in");

    // ---- five wrong codes from a device lock its code ----
    let kx = key(7);
    let (b, locked) = c.device(1, "lost", Some(&kx));
    let cx = c.logout(&b, Some(&kx)).unwrap();
    let wrong = if cx == "ABCDEFGH" { "ABCDEFGJ" } else { "ABCDEFGH" };
    for _ in 0..auth::MAX_RECONNECT_FAILS {
        let e = auth::pair_with(wrong, "x", Some(&kx)).unwrap_err();
        assert_eq!(e.downcast_ref::<auth::CodeError>(), Some(&auth::CodeError::Wrong));
    }
    let (s, v) = c.pair(&cx, Some(&kx));
    assert_eq!((s, v["reason"].clone()), (400, json!("locked")), "{v}");
    assert_eq!(c.call("DELETE", &format!("/api/admin/devices/{locked}"), admin, None).2["ok"], json!(true), "an admin still clears it");

    // ---- wrong codes count toward the global throttle ----
    let kz = key(8);
    let (b, _) = c.device(1, "throttled", Some(&kz));
    let cz = c.logout(&b, Some(&kz)).unwrap();
    let mut n = 0;
    while c.pair(wrong, Some(&kz)).0 != 429 {
        n += 1;
        assert!(n < 12, "never throttled");
    }
    let (s, v) = c.pair(&cz, Some(&kz));
    assert_eq!((s, v["reason"].clone()), (429, json!("throttled")), "even the right code waits");

    let _ = std::fs::remove_dir_all(&home);
}
