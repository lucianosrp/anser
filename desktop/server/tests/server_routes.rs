//! The HTTP surface end to end, against a throwaway data tree: health,
//! favicon, 404, live fallback, the db snapshot, the data the web app reads,
//! the not-installed root, an installed web app's service worker and
//! manifest, format definitions, and the push contract (/api/files,
//! /upload, /api/overrides), with a paired device's token (an existing tree
//! defaults to FINANCE_AUTH=writes).

use std::time::Duration;

use finance_server::{auth, server, upload};

fn tree() -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("finance-routes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("data")).unwrap();
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    let c = rusqlite::Connection::open(home.join("data/finance.db")).unwrap();
    c.execute_batch(
        "CREATE TABLE ingested_files (path TEXT NOT NULL, source TEXT NOT NULL, file_hash TEXT NOT NULL, row_count INTEGER NOT NULL,
           rows_added INTEGER, ingested_at INTEGER NOT NULL, size INTEGER, mtime_ns INTEGER, PRIMARY KEY (path, file_hash));
         CREATE TABLE txn_overrides (source TEXT NOT NULL, raw_hash TEXT NOT NULL, category TEXT NOT NULL, share REAL, note TEXT,
           created_at INTEGER NOT NULL, PRIMARY KEY (source, raw_hash));
         CREATE TABLE crypto_raw_sources (exchange TEXT NOT NULL, source TEXT NOT NULL, payload TEXT NOT NULL, fetched_at INTEGER NOT NULL,
           PRIMARY KEY (exchange, source));",
    )
    .unwrap();
    home
}

#[test]
fn routes_and_push_contract() {
    let home = tree();
    std::env::remove_var("FINANCE_AUTH");
    let boot = auth::bootstrap().unwrap();
    assert_eq!(boot.default_mode, Some(auth::Mode::All), "a tree with a finance.db starts closed");
    auth::set_mode(auth::Mode::Writes).unwrap(); // this test exercises the open-read routes
    let (code, _) = auth::new_pair_code(1).unwrap();
    let (token, _) = auth::pair(&code, "routes test").unwrap();
    let bearer = format!("Bearer {token}");
    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let get = |p: &str| {
        let mut r = agent.get(&format!("{base}{p}")).call().unwrap();
        let cc = r.headers().get("Cache-Control").map(|v| v.to_str().unwrap().to_string());
        (r.status().as_u16(), cc, r.body_mut().read_to_vec().unwrap())
    };

    assert_eq!(get("/healthz"), (200, Some("no-store".into()), b"ok".to_vec()));
    let (s, _, body) = get("/api/version");
    assert_eq!(s, 200);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((v["kind"].as_str(), v["bundle_version"].as_i64()), (Some("anser-server"), Some(1)));
    // the build's version, and nothing else on a route that needs no sign-in
    assert_eq!(v["version"].as_str(), Some(finance_core::VERSION));
    assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["kind", "version", "bundle_version"]);
    assert_eq!(get("/favicon.ico").0, 204);
    let (s, _, body) = get("/nope");
    assert_eq!(s, 404);
    assert!(String::from_utf8_lossy(&body).contains("/nope"));
    let (s, _, body) = get("/api/live");
    assert_eq!(s, 200);
    let live: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(live["stale"], true, "offline: the stale snapshot valuation, never a 500");
    let (s, _, body) = get("/api/db.sqlite");
    assert_eq!(s, 200);
    assert!(body.starts_with(b"SQLite format 3\0"));
    // no web app installed: / and /app/ say how to install it; no HTML pages
    for p in ["/", "/app/"] {
        let (s, _, body) = get(p);
        assert_eq!(s, 404, "{p}");
        assert!(String::from_utf8_lossy(&body).contains("build-web.sh --install"), "{p}");
    }
    for p in ["/banks", "/spending", "/upload"] {
        assert_eq!(get(p).0, 404, "{p} is not a page anymore");
    }
    let (s, _, body) = get("/api/export.json");
    assert_eq!(s, 200);
    assert!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["version"].is_number());
    let (s, _, body) = get("/api/capabilities");
    assert_eq!(s, 200);
    let caps: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(caps["server"]["kind"], "anser-server");
    assert_eq!(caps["features"]["broker"], false, "an empty tree has no broker");
    let (s, _, body) = get("/api/sources");
    assert_eq!(s, 200);
    let src: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(src["api"].as_array().unwrap().len(), 3);
    assert!(src["api"][0]["configured"].is_boolean());
    let (s, _, body) = get("/api/ledger.json");
    assert_eq!(s, 200);
    assert!(serde_json::from_slice::<serde_json::Value>(&body).is_ok());

    // upload: saved, then the same content skipped
    let mp = upload::encode_multipart("B0und", &[("n26.csv".into(), b"Date,Amount\n2026-01-01,1\n".to_vec())]);
    let unauth = agent.post(&format!("{base}/upload")).header("Content-Type", "multipart/form-data; boundary=B0und").send(&mp[..]).unwrap();
    assert_eq!(unauth.status().as_u16(), 401, "writes need a device token");
    let post = |body: &[u8]| {
        let r = agent
            .post(&format!("{base}/upload"))
            .header("Authorization", &bearer)
            .header("Content-Type", "multipart/form-data; boundary=B0und")
            .send(body)
            .unwrap();
        (r.status().as_u16(), r.headers().get("Location").unwrap().to_str().unwrap().to_string())
    };
    assert_eq!(post(&mp), (303, "/upload?saved=n26.csv".into()));
    assert_eq!(post(&mp), (303, "/upload?skipped=n26.csv".into()));
    assert!(home.join("transactions/n26.csv").exists());

    // /api/files now knows that content
    let (_, _, body) = get("/api/files");
    let known: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let want = format!("{:x}", md5_of(b"Date,Amount\n2026-01-01,1\n"));
    assert!(known["hashes"].as_array().unwrap().iter().any(|h| h == &serde_json::json!(want)), "{known}");

    // overrides: idempotent
    let o = r#"[{"source":"n26","raw_hash":"abc","category":"Rent","share":null,"note":null,"created_at":1}]"#;
    for added in [1, 0] {
        let mut r = agent.post(&format!("{base}/api/overrides")).header("Authorization", &bearer).header("Content-Type", "application/json").send(o).unwrap();
        assert_eq!(r.status().as_u16(), 200);
        let v: serde_json::Value = serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap();
        assert_eq!(v["added"], added);
    }

    // format definitions: the built-ins are listed; PUT installs one
    let (s, _, body) = get("/api/formats");
    assert_eq!(s, 200);
    let f: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(f["formats"].as_array().unwrap().iter().any(|x| x["id"] == "monzo" && x["builtin"] == true), "{f}");
    let def = "id = \"acme\"\nlabel = \"Acme\"\ncurrency = \"EUR\"\n[detect]\ncolumns = [\"When\", \"What\", \"How much\"]\n\
               [columns]\ndate = \"When\"\namount = \"How much\"\ndescription = \"What\"\n";
    // a file nobody reads yet sits in transactions/ ...
    std::fs::write(home.join("transactions/acme.csv"), "When,What,How much\n2026-03-01,Coffee,-3.20\n2026-03-02,Salary,100\n").unwrap();
    // a write like the others: no device token, no format
    let unauth = agent.put(&format!("{base}/api/formats/acme")).header("X-Finance-Format", "1").send(def.as_bytes()).unwrap();
    assert_eq!(unauth.status().as_u16(), 401, "PUT /api/formats needs a device token");
    let put = |id: &str, guard: bool, body: &str| {
        let mut r = agent.put(&format!("{base}/api/formats/{id}")).header("Authorization", &bearer);
        if guard {
            r = r.header("X-Finance-Format", "1");
        }
        let mut r = r.send(body.as_bytes()).unwrap();
        (r.status().as_u16(), String::from_utf8_lossy(&r.body_mut().read_to_vec().unwrap()).into_owned())
    };
    assert_eq!(put("acme", false, def).0, 400, "the guard header is required");
    assert_eq!(put("other", true, def).0, 400, "the path's id must be the file's");
    assert_eq!(put("acme", true, "id = \"acme\"").0, 400, "validated before it is written");
    assert!(!home.join("formats/acme.toml").exists());
    assert_eq!(put("acme", true, def), (200, r#"{"id":"acme","saved":true}"#.into()));
    assert_eq!(std::fs::read_to_string(home.join("formats/acme.toml")).unwrap(), def);
    // ... and is ingested once its definition arrives
    let c = rusqlite::Connection::open(home.join("data/finance.db")).unwrap();
    let n: i64 = c.query_row("SELECT COUNT(*) FROM bank_transactions WHERE source='acme'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
    // the desktop app's client half: refused unpaired, then (paired) an
    // idempotent re-put, and the list
    assert!(finance_core::upload::put_format(&agent, &base, "acme", def).is_err());
    std::env::set_var("FINANCE_TOKEN", &token);
    finance_core::upload::put_format(&agent, &base, "acme", def).unwrap();
    assert!(finance_core::upload::put_format(&agent, &base, "acme", "nope").is_err());
    let remote = finance_core::upload::server_formats(&agent, &base).unwrap().unwrap();
    assert!(remote.iter().any(|f| f.id == "acme" && !f.builtin && f.toml == def));

    // POST /sync redirects back to the referring page
    let r = agent.post(&format!("{base}/sync")).header("Authorization", &bearer).header("Referer", "http://x/banks?m=1").send(&b""[..]).unwrap();
    assert_eq!(r.headers().get("Location").unwrap().to_str().unwrap(), "/banks?m=1");

    // POST /api/refresh answers when done, with a result per source (none
    // configured here: just the live valuation), and needs a device
    assert_eq!(agent.post(&format!("{base}/api/refresh")).send(&b""[..]).unwrap().status().as_u16(), 401);
    let t = std::time::Instant::now();
    let mut r = agent.post(&format!("{base}/api/refresh")).header("Authorization", &bearer).send(&b""[..]).unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: serde_json::Value = serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["sources"][0]["source"], "prices");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());

    // an installed web app: the service worker from the root, the manifest
    // with its own type, both revalidated on every load
    let app = home.join("data/webapp");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("index.html"), "<!doctype html>").unwrap();
    std::fs::write(app.join("sw.js"), "// sw").unwrap();
    std::fs::write(app.join("manifest.webmanifest"), "{}").unwrap();
    let typed = |p: &str| {
        let r = agent.get(&format!("{base}{p}")).call().unwrap();
        let h = |k: &str| r.headers().get(k).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        (r.status().as_u16(), h("Content-Type"), h("Cache-Control"))
    };
    assert_eq!(typed("/sw.js"), (200, "text/javascript; charset=utf-8".into(), "no-cache".into()));
    assert_eq!(typed("/app/manifest.webmanifest"), (200, "application/manifest+json".into(), "no-cache".into()));
    assert_eq!(typed("/").0, 200);
    let _ = std::fs::remove_dir_all(home);
}

fn md5_of(b: &[u8]) -> md5_hex::Hex {
    md5_hex::Hex(b.to_vec())
}

mod md5_hex {
    use md5::{Digest, Md5};
    pub struct Hex(pub Vec<u8>);
    impl std::fmt::LowerHex for Hex {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "{}", hex::encode(Md5::digest(&self.0)))
        }
    }
}
