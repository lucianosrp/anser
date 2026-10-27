//! Accounts made from the app, on a throwaway fresh install in
//! FINANCE_AUTH=all: the first account (only while there are no users)
//! becomes the admin; after that signup is closed by default; an admin's
//! invite makes one account, once, for 24 hours; that account signs in but
//! sees no data yet. Then the profile picture: upload checks, initials,
//! Gravatar through a stand-in fetcher.

use std::time::Duration;

use finance_server::{auth, avatar, server};
use serde_json::{json, Value};

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

/// Gravatar, stood in: one known address has a picture, the rest 404.
fn fake_gravatar(url: &str) -> anyhow::Result<(u16, Vec<u8>)> {
    if url.contains(&avatar::gravatar_hash("jane@example.com")) {
        Ok((200, png(80, 80)))
    } else {
        Ok((404, b"not found".to_vec()))
    }
}

struct Client {
    base: String,
    agent: ureq::Agent,
}

impl Client {
    fn call(&self, method: &str, path: &str, hdrs: &[(&str, &str)], body: Option<(&str, Vec<u8>)>) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let url = format!("{}{path}", self.base);
        let mut r = match (method, body) {
            ("GET", _) | ("DELETE", None) => {
                let mut b = if method == "GET" { self.agent.get(&url) } else { self.agent.delete(&url) };
                for (k, v) in hdrs {
                    b = b.header(*k, *v);
                }
                b.call().unwrap()
            }
            (m, body) => {
                let mut b = if m == "PUT" { self.agent.put(&url) } else { self.agent.post(&url) };
                for (k, v) in hdrs {
                    b = b.header(*k, *v);
                }
                let (ct, bytes) = body.unwrap_or(("application/json", b"null".to_vec()));
                b.header("Content-Type", ct).send(&bytes[..]).unwrap()
            }
        };
        let h = r.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
        (r.status().as_u16(), h, r.body_mut().with_config().limit(16 << 20).read_to_vec().unwrap())
    }

    fn post_json(&self, path: &str, hdrs: &[(&str, &str)], v: Value) -> (u16, Value) {
        let (s, _, b) = self.call("POST", path, hdrs, Some(("application/json", serde_json::to_vec(&v).unwrap())));
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }
}

#[test]
fn first_account_invites_and_the_avatar() {
    let home = std::env::temp_dir().join(format!("finance-signup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    // offline: no live sweeps; the stand-in Gravatar still answers
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::remove_var("FINANCE_AUTH");
    std::env::remove_var("FINANCE_ALLOW_SIGNUP");
    avatar::set_fetcher(fake_gravatar);

    auth::bootstrap().unwrap();
    assert_eq!(auth::mode(), auth::Mode::All);
    assert_eq!(auth::user_count().unwrap(), 0);
    assert_eq!(auth::signup_mode(), "first");

    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let c = Client { base: base.clone(), agent };
    let browser_hdr: &[(&str, &str)] = &[("X-Finance-Request", "1"), ("Origin", &base)];

    // nobody yet: whoami says the first account may be made
    let (s, _, b) = c.call("GET", "/api/whoami", &[], None);
    let who: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!((s, who["signup"].clone(), who["paired"].clone()), (200, json!("first"), json!(false)));
    assert_eq!(c.call("GET", "/api/export.json", &[], None).0, 401);

    // a browser makes the first account: admin, owner of the data, a cookie
    let (s, v) = c.post_json("/api/signup/session", &[], json!({"name": "Jane Doe"}));
    assert_eq!(s, 403, "a browser's signup needs X-Finance-Request: {v}");
    let (s, h, b) = c.call("POST", "/api/signup/session", browser_hdr,
        Some(("application/json", serde_json::to_vec(&json!({"name": " Jane Doe ", "email": "jane@example.com"})).unwrap())));
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(s, 200, "{v}");
    assert_eq!((v["first"].clone(), v["data"].clone(), v["user"]["role"].clone()), (json!(true), json!(true), json!("admin")));
    assert!(v.get("token").is_none(), "a browser gets a cookie, not a token");
    let cookie = h.iter().find(|(k, _)| k.eq_ignore_ascii_case("set-cookie")).map(|(_, v)| v.clone()).unwrap();
    let session = cookie.split(';').next().unwrap().to_string();
    let jane: &[(&str, &str)] = &[("Cookie", &session), ("X-Finance-Request", "1"), ("Origin", &base)];
    assert_eq!(c.call("GET", "/api/ledger.json", jane, None).0, 200);
    let users = auth::users().unwrap();
    assert_eq!((users.len(), users[0].id, users[0].email.as_deref()), (1, 1, Some("jane@example.com")));
    assert_eq!(std::path::Path::new(&users[0].home), home.as_path());
    // the profile carries the name
    let (_, _, b) = c.call("GET", "/api/profile", jane, None);
    assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["name"], json!("Jane Doe"));

    // from now on: no first-account shortcut, and signup is closed by default
    assert_eq!(auth::signup_mode(), "invite");
    let (s, v) = c.post_json("/api/signup", &[], json!({"name": "Mallory"}));
    assert_eq!((s, v["reason"].clone()), (403, json!("closed")), "{v}");
    assert_eq!(auth::user_count().unwrap(), 1);
    std::env::set_var("FINANCE_ALLOW_SIGNUP", "0");
    assert_eq!(auth::signup_mode(), "invite", "only a yes opens it");
    std::env::remove_var("FINANCE_ALLOW_SIGNUP");

    // an invite: admins only
    let (s, _) = c.post_json("/api/admin/invite", &[], json!({}));
    assert_eq!(s, 401);
    let (s, inv) = c.post_json("/api/admin/invite", jane, json!({}));
    assert_eq!(s, 200, "{inv}");
    let code = inv["code"].as_str().unwrap().to_string();
    let until = inv["expires"].as_i64().unwrap() - finance_engine::db::now_ms();
    assert!((auth::INVITE_TTL_MS - 60_000..=auth::INVITE_TTL_MS).contains(&until), "24 hours");

    // a wrong code, then the right one (dashed, lowercase: still the code)
    let (s, v) = c.post_json("/api/signup", &[], json!({"name": "Sam", "invite": "ABCD-EFGH"}));
    assert_eq!((s, v["reason"].clone()), (400, json!("wrong")));
    let dashed = format!("{}-{}", &code[..4], &code[4..]).to_lowercase();
    let (s, v) = c.post_json("/api/signup", &[], json!({"name": "Sam", "email": "sam@example.com", "invite": dashed, "device_name": "sam's laptop"}));
    assert_eq!(s, 200, "{v}");
    assert_eq!((v["first"].clone(), v["data"].clone(), v["user"]["role"].clone()), (json!(false), json!(true), json!("user")));
    assert!(v["note"].as_str().unwrap().contains("its own data"));
    let sam_token = format!("Bearer {}", v["token"].as_str().unwrap());
    let sam_id = v["user"]["id"].as_i64().unwrap();
    let sam: &[(&str, &str)] = &[("Authorization", &sam_token)];
    // single use
    let (s, v) = c.post_json("/api/signup", &[], json!({"name": "Sam again", "invite": code}));
    assert_eq!((s, v["reason"].clone()), (400, json!("used")));
    // Sam is signed in, on a tree of his own: nothing of Jane's
    let (_, _, b) = c.call("GET", "/api/whoami", sam, None);
    let who: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!((who["paired"].clone(), who["user"]["data"].clone()), (json!(true), json!(true)));
    let (s, _, b) = c.call("GET", "/api/profile", sam, None);
    assert_eq!(s, 200);
    assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["name"], json!("Sam"));
    assert!(home.join("users").join(sam_id.to_string()).join("data/finance.db").exists(), "users/<id>/data/finance.db");
    assert_eq!(c.call("GET", "/api/profile/avatar", sam, None).0, 404, "his own: initials");
    assert_eq!(c.post_json("/api/admin/invite", sam, json!({})).0, 403);

    // an invite expires after 24 hours
    let (_, inv) = c.post_json("/api/admin/invite", jane, json!({}));
    let old = inv["code"].as_str().unwrap().to_string();
    let db = rusqlite::Connection::open(home.join("data/server.db")).unwrap();
    db.execute("UPDATE invites SET expires = ?1 WHERE code_hash = ?2", rusqlite::params![finance_engine::db::now_ms() - 1, auth::sha256_hex(&old)]).unwrap();
    let (s, v) = c.post_json("/api/signup", &[], json!({"name": "Late", "invite": old}));
    assert_eq!((s, v["reason"].clone()), (400, json!("expired")));
    assert_eq!(auth::user_count().unwrap(), 2, "only Sam was added");

    // FINANCE_ALLOW_SIGNUP opens it: an account without an invite, its own tree too
    std::env::set_var("FINANCE_ALLOW_SIGNUP", "1");
    assert_eq!(auth::signup_mode(), "open");
    let (s, v) = c.post_json("/api/signup", &[], json!({"name": "Open"}));
    assert_eq!((s, v["data"].clone()), (200, json!(true)), "{v}");
    std::env::remove_var("FINANCE_ALLOW_SIGNUP");
    let (s, v) = c.post_json("/api/signup", &[], json!({"name": "  "}));
    assert_eq!((s, v["reason"].clone()), (400, json!("invalid")));

    // pairing codes say why they're refused too
    let (pc, _) = auth::new_pair_code(1).unwrap();
    assert_eq!(c.post_json("/api/pair", &[], json!({"code": pc, "device_name": "phone"})).0, 200);
    let (s, v) = c.post_json("/api/pair", &[], json!({"code": pc}));
    assert_eq!((s, v["reason"].clone()), (400, json!("used")));

    // ---- the avatar ----
    let put = |bytes: Vec<u8>, ct: &str| c.call("PUT", "/api/profile/avatar", jane, Some((ct, bytes)));
    assert_eq!(c.call("GET", "/api/profile/avatar", jane, None).0, 404, "initials: no picture");
    assert_eq!(c.call("PUT", "/api/profile/avatar", &[], Some(("image/png", png(4, 4)))).0, 401, "a write needs a device");
    let (s, _, b) = put(b"GIF89a fake".to_vec(), "image/gif");
    assert_eq!(s, 422, "{}", String::from_utf8_lossy(&b));
    let (s, _, _) = put(vec![0u8; avatar::MAX_UPLOAD + 70 * 1024], "image/png");
    assert_eq!(s, 413);
    let (s, _, b) = put(png(500, 300), "image/png");
    assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(v["profile"]["avatar_kind"], json!("upload"));
    let (s, h, pic) = c.call("GET", "/api/profile/avatar", jane, None);
    assert_eq!(s, 200);
    assert!(h.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "image/png"));
    let img = image::load_from_memory(&pic).unwrap();
    assert_eq!((img.width(), img.height()), (256, 256));
    assert!(home.join("data/avatar/upload.png").exists(), "in the data dir, not finance.db");
    // multipart (the web app's file input) works the same
    let mp = finance_core::upload::encode_multipart("B0und", &[("me.png".into(), png(64, 64))]);
    let (s, _, _) = put(mp, "multipart/form-data; boundary=B0und");
    assert_eq!(s, 200);

    // gravatar: fetched by the server; a 404 says so and changes nothing
    let (s, v) = c.post_json("/api/profile/avatar/gravatar", jane, json!({"email": "nobody@example.com"}));
    assert_eq!((s, v["error"].clone()), (404, json!("No Gravatar for this email.")));
    let (s, v) = c.post_json("/api/profile/avatar/gravatar", jane, json!({"email": "Jane@Example.com"}));
    assert_eq!(s, 200, "{v}");
    assert_eq!((v["profile"]["avatar_kind"].clone(), v["profile"]["avatar_email"].clone()), (json!("gravatar"), json!("Jane@Example.com")));
    let (s, _, pic) = c.call("GET", "/api/profile/avatar?refresh=1", jane, None);
    assert_eq!((s, image::load_from_memory(&pic).unwrap().width()), (200, 256));

    // initials and a colour through the profile; back to initials drops the picture
    let (_, _, b) = c.call("GET", "/api/profile", jane, None);
    let mut p: Value = serde_json::from_slice(&b).unwrap();
    p["avatar_kind"] = json!("initials");
    p["avatar_initials"] = json!("jd");
    p["avatar_color"] = json!("chart-3");
    let (s, _, b) = c.call("PUT", "/api/profile", jane, Some(("application/json", serde_json::to_vec(&p).unwrap())));
    assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
    assert_eq!(c.call("GET", "/api/profile/avatar", jane, None).0, 404);
    let (_, _, b) = c.call("GET", "/api/export.json", jane, None);
    let bundle: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!((bundle["profile"]["avatar_initials"].clone(), bundle["profile"]["avatar_color"].clone()), (json!("JD"), json!("chart-3")));
    p["avatar_color"] = json!("blue");
    assert_eq!(c.call("PUT", "/api/profile", jane, Some(("application/json", serde_json::to_vec(&p).unwrap()))).0, 400);
    assert_eq!(c.call("DELETE", "/api/profile/avatar", jane, None).0, 200);
    assert!(!home.join("data/avatar/upload.png").exists());

    // ---- the throttle: ten wrong codes lock both doors ----
    for _ in 0..10 {
        c.post_json("/api/signup", &[], json!({"name": "Guess", "invite": "ZZZZ-ZZZZ"}));
    }
    let (s, v) = c.post_json("/api/signup", &[], json!({"name": "Guess", "invite": "ZZZZ-ZZZZ"}));
    assert_eq!((s, v["reason"].clone()), (429, json!("throttled")));
    assert_eq!(v["error"], json!("Too many wrong codes: try again in 10 minutes."));
    let (s, v) = c.post_json("/api/pair", &[], json!({"code": "ZZZZ-ZZZZ"}));
    assert_eq!((s, v["reason"].clone()), (429, json!("throttled")));
    // another client (the proxy's X-Forwarded-For) is not locked out with it
    let other = [("X-Forwarded-For", "100.64.0.7")];
    assert_eq!(c.post_json("/api/pair", &other, json!({"code": "ZZZZ-ZZZZ"})).0, 400);
    // a code made after the failures lets the locked client in
    let (code, _) = auth::new_pair_code(1).unwrap();
    let (s, v) = c.post_json("/api/pair", &[], json!({"code": code, "device_name": "back"}));
    assert_eq!(s, 200, "{v}");
    let _ = std::fs::remove_dir_all(home);
}
