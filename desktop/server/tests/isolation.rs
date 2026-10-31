//! Household isolation: user 2 can never read or write user 1's data (or
//! the other way round) through ANY route, and a new route that isn't on
//! the list below fails this test until it is.
//!
//! The owner's tree and a second user's are seeded with markers (OWNERMARK
//! and SAMMARK: in bank rows, a dropped file, a format definition, a
//! secret, the profile, rules, the investment setup, settings, a backup
//! archive). Then every route is called as Sam (reads, writes, dry runs,
//! the archive export and import, the avatar, the sources and secrets
//! APIs, admin routes, path tricks) and every answer is checked for the
//! owner's markers; afterwards the owner's whole tree must be unchanged,
//! byte for byte (the db compared row by row). The owner then reads every
//! route and must see none of Sam's. A non-owner admin can't read the
//! owner's data either (no impersonation), anonymous requests on a
//! household server get no data, and MCP reads one tree only: `mcp --user`
//! on stdio, and a user's MCP key over HTTP.
//! The server runs in strict tenant mode: a path resolved outside a tenant
//! would answer 500, which fails the test too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use finance_engine::fetch::verify::Verdict;
use finance_engine::tenant::{self, Tenant};
use finance_server::{api, auth, server, upload};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const OWNER_SECRET: &str = "okx-OWNERMARK-secret-key-QZ7W";
const SAM_SECRET: &str = "okx-SAMMARK-secret-key-XK4P";

fn tester(_key: &str, _fields: &BTreeMap<String, String>) -> Verdict {
    Verdict { ok: true, detail: "Read-only.".into(), refused: vec![], sections: vec![] }
}

/// Every route the server answers: (method, path). A path ending in `*`
/// stands for every path under it. [`every_route_literal_is_listed`] keeps
/// this complete.
const ROUTES: &[(&str, &str)] = &[
    // public
    ("GET", "/healthz"),
    ("GET", "/api/version"),
    ("GET", "/favicon.ico"),
    ("GET", "/favicon.png"),
    ("GET", "/"),
    ("GET", "/app"),
    ("GET", "/app/"),
    ("GET", "/app/finance.wasm"),
    ("GET", "/sw.js"),
    ("GET", "/login"),
    ("GET", "/api/whoami"),
    ("GET", "/api/demo/ledger.json"),
    ("GET", "/api/demo/export.json"),
    ("POST", "/api/pair"),
    ("POST", "/api/login"),
    ("POST", "/api/signup"),
    ("POST", "/api/signup/session"),
    // reads
    ("GET", "/api/ledger.json"),
    ("GET", "/api/live"),
    ("GET", "/api/export.json"),
    ("GET", "/api/db.sqlite"),
    ("GET", "/api/capabilities"),
    ("GET", "/api/sources"),
    ("GET", "/api/sources/catalog"),
    ("GET", "/api/files"),
    ("GET", "/api/formats"),
    ("GET", "/api/profile"),
    ("GET", "/api/profile/avatar"),
    ("GET", "/api/categories"),
    ("GET", "/api/rules"),
    ("GET", "/api/categorize/suggest"),
    ("GET", "/api/investments/setup"),
    ("GET", "/api/admin/backup-recipient"),
    ("GET", "/api/admin/archives"),
    ("GET", "/api/admin/archives/latest"),
    ("GET", "/api/admin/users"),
    ("GET", "/api/admin/devices"),
    ("GET", "/api/me/devices"),
    ("GET", "/api/mcp"),
    ("GET", "/mcp"),
    // writes on the caller's tree
    ("PUT", "/api/formats/*"),
    ("POST", "/upload"),
    ("POST", "/sync"),
    ("POST", "/api/refresh"),
    ("POST", "/api/overrides"),
    ("POST", "/api/preview"),
    ("PUT", "/api/profile"),
    ("PUT", "/api/settings/*"),
    ("PUT", "/api/mcp"),
    ("POST", "/mcp"),
    ("POST", "/api/mcp/keys"),
    ("DELETE", "/api/mcp/keys/*"),
    ("PUT", "/api/profile/avatar"),
    ("POST", "/api/profile/avatar"),
    ("DELETE", "/api/profile/avatar"),
    ("POST", "/api/profile/avatar/gravatar"),
    ("POST", "/api/categories"),
    ("PATCH", "/api/categories"),
    ("PATCH", "/api/categories/*"),
    ("POST", "/api/categories/*"),
    ("DELETE", "/api/categories/*"),
    ("POST", "/api/rules"),
    ("POST", "/api/rules/preview"),
    ("POST", "/api/rules/test"),
    ("POST", "/api/rules/starter"),
    ("PATCH", "/api/rules/*"),
    ("DELETE", "/api/rules/*"),
    ("PUT", "/api/txn/*"),
    ("POST", "/api/admin/investments/*"),
    ("PUT", "/api/admin/investments/*"),
    ("DELETE", "/api/admin/investments/*"),
    // the caller's own secrets, archive, backup key, devices
    ("PUT", "/api/sources/*"),
    ("DELETE", "/api/sources/*"),
    ("POST", "/api/sources/*"),
    ("POST", "/api/account/export"),
    ("POST", "/api/account/import"),
    ("PUT", "/api/admin/backup-recipient"),
    ("POST", "/api/me/pair-code"),
    ("POST", "/api/me/delete"),
    ("DELETE", "/api/me/devices/*"),
    ("POST", "/api/logout"),
    // admin: accounts and the server, never another user's data
    ("POST", "/api/admin/pair-code"),
    ("POST", "/api/admin/wipe"),
    ("POST", "/api/admin/auth-mode"),
    ("POST", "/api/admin/invite"),
    ("DELETE", "/api/admin/devices/*"),
    ("PATCH", "/api/admin/users/*"),
    ("DELETE", "/api/admin/users/*"),
    ("POST", "/api/secrets/import-env"),
    ("POST", "/api/webapp"),
];

fn covered(lit: &str) -> bool {
    ROUTES.iter().any(|(_, p)| match p.strip_suffix('*') {
        Some(prefix) => lit.starts_with(prefix) || prefix.starts_with(lit),
        None => *p == lit || p.starts_with(lit),
    })
}

/// Every route-looking string literal in the server's source must be on
/// the list: a route added without being listed (and so without being
/// called as another user below) fails here.
#[test]
fn every_route_literal_is_listed() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![];
    for d in [src.clone(), src.join("cli")] {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.path().extension().is_some_and(|x| x == "rs") {
                files.push(e.path());
            }
        }
    }
    let mut missing = vec![];
    let mut seen = 0;
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("\"/") {
            rest = &rest[i + 1..];
            let end = rest.find('"').unwrap_or(rest.len());
            let lit = &rest[..end];
            rest = &rest[end..];
            // a route literal: /api/..., /app..., /upload, /sync, /login, ...
            let lit = lit.split(['?', ' ', '\\', '{']).next().unwrap_or("");
            let routeish = lit.starts_with("/api/")
                || ["/upload", "/sync", "/login", "/healthz", "/sw.js", "/app", "/favicon", "/mcp"].iter().any(|r| lit.starts_with(r));
            if !routeish {
                continue;
            }
            seen += 1;
            if !covered(lit) {
                missing.push(format!("{}: {lit}", f.file_name().unwrap().to_string_lossy()));
            }
        }
    }
    assert!(seen > 50, "the scan found the routes ({seen})");
    assert!(missing.is_empty(), "routes not on the isolation list (add them to ROUTES and to the calls below): {missing:#?}");
}

#[test]
fn strict_mode_refuses_an_unscoped_path() {
    // a fresh thread: the flag is process-wide, the tenant per thread
    let r = std::thread::spawn(|| {
        tenant::set_strict(true);
        let unscoped = std::panic::catch_unwind(finance_engine::config::db_path);
        let scoped = Tenant::user_at(9, Path::new("/x/users/9")).enter(finance_engine::config::db_path);
        (unscoped.is_err(), scoped)
    })
    .join()
    .unwrap();
    assert!(r.0, "no tenant in strict mode: refused");
    assert_eq!(r.1, PathBuf::from("/x/users/9/data/finance.db"));
}

// ---------------------------------------------------------------------------

struct Client {
    base: String,
    agent: ureq::Agent,
}

impl Client {
    fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<(&str, Vec<u8>)>, extra: &[(&str, &str)]) -> (u16, Vec<u8>) {
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
                let b = match m {
                    "PUT" => self.agent.put(&url),
                    "PATCH" => self.agent.patch(&url),
                    _ => self.agent.post(&url),
                };
                let (ct, data) = body.unwrap_or(("application/json", b"{}".to_vec()));
                hdrs!(b.header("Content-Type", ct)).send(&data[..])
            }
        }
        .unwrap();
        (r.status().as_u16(), r.body_mut().with_config().limit(256 << 20).read_to_vec().unwrap())
    }

    fn json(&self, method: &str, path: &str, token: Option<&str>, v: Value) -> (u16, Value) {
        let (s, b) = self.call(method, path, token, Some(("application/json", serde_json::to_vec(&v).unwrap())), &[]);
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    fn pair(&self, uid: i64, name: &str) -> String {
        let (code, _) = auth::new_pair_code(uid).unwrap();
        let (s, v) = self.json("POST", "/api/pair", None, json!({"code": code, "device_name": name}));
        assert_eq!(s, 200, "{v}");
        v["token"].as_str().unwrap().to_string()
    }
}

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 30, 30]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn multipart(fields: &[(&str, &str)], file: Option<(&str, &[u8])>) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend_from_slice(format!("--XB\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
    }
    if let Some((name, data)) = file {
        body.extend_from_slice(format!("--XB\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--XB--\r\n");
    ("multipart/form-data; boundary=XB".into(), body)
}

/// Every table of a db, row by row, as text (the WAL folded in): equal
/// digests = equal content.
fn db_digest(p: &Path) -> String {
    let c = rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let tables: Vec<String> = {
        let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").unwrap();
        let v = st.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        v
    };
    let mut h = Sha256::new();
    for t in tables {
        h.update(t.as_bytes());
        let mut st = c.prepare(&format!("SELECT * FROM \"{t}\" ORDER BY rowid")).or_else(|_| c.prepare(&format!("SELECT * FROM \"{t}\""))).unwrap();
        let n = st.column_count();
        let mut rows = st.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            for i in 0..n {
                h.update(format!("{:?}|", r.get_ref(i).unwrap()).as_bytes());
            }
            h.update(b"\n");
        }
    }
    hex::encode(h.finalize())
}

fn files_digest(dir: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files_digest(&p, out);
        } else {
            out.insert(p.display().to_string(), hex::encode(Sha256::digest(std::fs::read(&p).unwrap_or_default())));
        }
    }
}

/// The owner's whole tree: the db's content, and every file of it
/// (server-wide files and the users/ trees aside).
fn owner_digest(home: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("finance.db".into(), db_digest(&home.join("data/finance.db")));
    for d in ["transactions", "formats", "data/avatar", "data/backups"] {
        files_digest(&home.join(d), &mut out);
    }
    for f in ["data/fetch_status.json", "secrets/1.json.enc"] {
        let p = home.join(f);
        out.insert(f.into(), hex::encode(Sha256::digest(std::fs::read(&p).unwrap_or_default())));
    }
    out
}

fn has(b: &[u8], needle: &str) -> bool {
    b.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn a_household_member_never_reaches_another_members_data() {
    let home = std::env::temp_dir().join(format!("finance-isolation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::fs::create_dir_all(home.join("formats")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    for k in ["FINANCE_DATA_DIR", "FINANCE_DB", "FINANCE_TRANSACTIONS_DIR", "FINANCE_FORMATS_DIR", "FINANCE_AUTH", "FINANCE_ALLOW_SIGNUP"] {
        std::env::remove_var(k);
    }
    std::env::set_var("FINANCE_OFFLINE", "1");
    for k in finance_engine::secrets::SECRET_KEYS {
        std::env::remove_var(k);
    }
    api::set_tester(tester);

    // ---- the owner's tree, an existing install ----
    {
        let c = finance_engine::db::connect(&home.join("data/finance.db")).unwrap();
        c.execute_batch("INSERT INTO fx_rates(base,quote,month,rate,fetched_at) VALUES('EUR','HKD','2026-01',8.5,0);").unwrap();
        finance_engine::db::insert_bank_transactions(&c, &[finance_engine::db::NewBankTxn {
            source: "hsbc".into(), account: "OWNERMARK-acct".into(), currency: "HKD".into(), txn_date: "2026-09-01".into(), amount: Some(-5.0),
            balance: Some(1234.5), description: "OWNERMARK tea house".into(), raw_hash: "owner-h1".into(), source_file: "owner.csv".into(), ingested_at: 1,
        }]).unwrap();
    }
    std::fs::write(home.join("transactions/owner-OWNERMARK.csv"), "OWNERMARK,1\n").unwrap();
    let monzo = finance_engine::sources::format::BUILTIN[0].1;
    let def: String = monzo
        .lines()
        .map(|l| if l.starts_with("id ") || l.starts_with("id=") { "id = \"ownermark_bank\"".to_string() } else if l.starts_with("label") { "label = \"OWNERMARK Bank\"".to_string() } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(home.join("formats/ownermark.toml"), def).unwrap();
    std::fs::create_dir_all(home.join("data/backups/archives")).unwrap();
    std::fs::write(home.join("data/backups/archives/finance-20260101-000000.finance"), "OWNERMARK archive").unwrap();
    auth::bootstrap().unwrap();
    finance_engine::secrets::put(1, &[("OKX_API_KEY".to_string(), OWNER_SECRET.to_string())].into()).unwrap();
    Tenant::owner().enter(|| {
        let c = finance_engine::db::open().unwrap();
        let mut p = finance_engine::profile::get(&c).unwrap().unwrap_or_default();
        p.name = Some("OWNERMARK Owner".into());
        finance_engine::profile::put(&c, &p).unwrap();
        finance_server::avatar::save_upload(&finance_server::avatar::to_square_png(&png()).unwrap()).unwrap();
        finance_engine::profile::picture_changed(&c, "upload", None).unwrap();
    });

    // ---- the household: Sam (a user) and Ada (a second admin) ----
    let sam_id = auth::add_user("Sam", Some("sam@example.com"), false).unwrap();
    let ada_id = auth::add_user("Ada", None, true).unwrap();
    assert!(sam_id > 1 && ada_id > sam_id);
    assert!(auth::household());
    let sam_t = auth::tenant_of(&sam_id.to_string()).unwrap();
    assert_eq!(sam_t.home, home.join("users").join(sam_id.to_string()));
    sam_t.enter(|| {
        let c = finance_engine::db::open().unwrap();
        finance_engine::db::insert_bank_transactions(&c, &[finance_engine::db::NewBankTxn {
            source: "hsbc".into(), account: "SAMMARK-acct".into(), currency: "HKD".into(), txn_date: "2026-09-02".into(), amount: Some(-7.0),
            balance: Some(99.0), description: "SAMMARK coffee".into(), raw_hash: "sam-h1".into(), source_file: "sam.csv".into(), ingested_at: 1,
        }]).unwrap();
    });

    // from here on, the server and everything it runs is tenant-scoped
    tenant::set_strict(true);
    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let c = Client { base, agent };
    let owner = c.pair(1, "owner laptop");
    let sam = c.pair(sam_id, "sam phone");
    let ada = c.pair(ada_id, "ada laptop");
    let (o, s) = (Some(owner.as_str()), Some(sam.as_str()));

    // the owner's own markers, through the API (rules, investments, settings, backup key)
    assert_eq!(c.json("POST", "/api/rules", o, json!({"pattern": "OWNERMARK_RULE", "category": "OwnermarkCat"})).0, 200);
    assert_eq!(c.json("PUT", "/api/admin/investments/instruments/OWNSYM1", o, json!({"kind": "fund", "name": "OWNERMARK fund"})).0, 200);
    assert_eq!(c.json("PUT", "/api/settings/budgets", o, json!({"OWNERMARK": 100})).0, 200);
    // the home setup: valid is stored, an invalid body or mortgage mode is a 400, and the owner's landlord is a marker
    assert_eq!(c.json("PUT", "/api/settings/home", o, json!({"mode": "rent", "rent": {"monthly": 100.0, "landlord_match": "OWNERMARK landlord"}})).0, 200);
    assert_eq!(c.json("PUT", "/api/settings/home", o, json!({"mode": "mortgage"})).0, 400, "a mortgage needs its loan");
    let loan = |p: f64, rate: f64| json!({"mode": "mortgage", "mortgage": {"principal": p, "annual_rate_pct": rate, "term_months": 300, "start": "2026-01-15", "lender_match": "OWNERMARK lender"}});
    assert_eq!(c.json("PUT", "/api/settings/home", o, loan(500000.0, 3.5)).0, 200);
    assert_eq!(c.json("PUT", "/api/settings/home", o, loan(0.0, 3.5)).0, 400);
    assert_eq!(c.json("PUT", "/api/settings/home", o, loan(500000.0, 45.0)).0, 400);
    assert_eq!(c.json("PUT", "/api/settings/home", o, json!({"mode": "rent", "rent": {"lease": {"end": "someday"}}})).0, 400);
    assert_eq!(c.json("PUT", "/api/settings/home", o, json!({"rent": {}})).0, 400);
    let (_, owner_key) = finance_engine::archive::generate_identity();
    assert_eq!(c.json("PUT", "/api/admin/backup-recipient", o, json!({"recipient": owner_key})).0, 200);
    let (s0, b0) = c.call("GET", "/api/export.json", o, None, &[]);
    assert!(s0 == 200 && has(&b0, "OWNERMARK"), "the owner sees their own data");
    let (_, src) = c.call("GET", "/api/sources", o, None, &[]);
    assert!(has(&src, "QZ7W"), "the owner's key hint is theirs");
    // Sam's own secret, set through the API
    let (st, v) = c.json("PUT", "/api/sources/okx/secret", s,
        json!({"fields": {"OKX_API_KEY": SAM_SECRET, "OKX_API_SECRET": "SAMMARK-secret-two-0003", "OKX_PASSPHRASE": "SAMMARK-pass-0004"}}));
    assert_eq!(st, 200, "{v}");
    assert!(home.join(format!("secrets/{sam_id}.json.enc")).exists());
    std::thread::sleep(Duration::from_millis(300)); // the owner's warmers settle (reads only)
    let before = owner_digest(&home);
    // the owner's settings kept in server.db (the backup key) are theirs too
    let owner_recipient = c.json("GET", "/api/admin/backup-recipient", o, Value::Null).1["recipient"].clone();
    assert_eq!(owner_recipient, json!(owner_key));
    assert_eq!(c.json("GET", "/api/admin/backup-recipient", s, Value::Null).1["recipient"], Value::Null, "Sam has none yet");

    // ---- every route, as Sam ----
    let leak = |who: &str, method: &str, path: &str, st: u16, b: &[u8], marks: &[&str]| {
        assert_ne!(st, 500, "{who} {method} {path}: 500 {}", String::from_utf8_lossy(b));
        for m in marks {
            assert!(!has(b, m), "{who} {method} {path} ({st}) leaked {m}: {}", String::from_utf8_lossy(&b[..b.len().min(400)]));
        }
    };
    let owner_marks = ["OWNERMARK", OWNER_SECRET, "QZ7W"];
    let sam_marks = ["SAMMARK", SAM_SECRET, "XK4P"];
    let reads: Vec<&str> = ROUTES.iter().filter(|(m, p)| *m == "GET" && !p.ends_with('*')).map(|(_, p)| *p).collect();
    let mut sam_reads_ok = 0;
    for p in &reads {
        let (st, b) = c.call("GET", p, s, None, &[]);
        leak("sam", "GET", p, st, &b, &owner_marks);
        if st == 200 && has(&b, "SAMMARK") {
            sam_reads_ok += 1;
        }
    }
    assert!(sam_reads_ok >= 3, "Sam reads his own data ({sam_reads_ok} routes)");
    // his avatar is initials: the owner's picture isn't his
    assert_eq!(c.call("GET", "/api/profile/avatar", s, None, &[]).0, 404);
    // archives: none of the owner's
    let (st, b) = c.call("GET", "/api/admin/archives", s, None, &[]);
    assert!(st == 200 && !has(&b, "finance-20260101"), "{}", String::from_utf8_lossy(&b));
    assert_eq!(c.call("GET", "/api/admin/archives/latest", s, None, &[]).0, 404);
    // path tricks on the static and dynamic routes
    for p in ["/app/../data/finance.db", "/app/..%2Fdata%2Ffinance.db", "/app/%2e%2e/server.db", "/api/formats/../../data", "/api/txn/../export.json"] {
        let (st, b) = c.call("GET", p, s, None, &[]);
        leak("sam", "GET", p, st, &b, &owner_marks);
    }

    // writes and dry runs, as Sam: each on his tree, if anywhere
    let fmt = monzo.replace("monzo", "sammark_bank");
    let (pct, pbody) = multipart(&[], Some(("sam-SAMMARK.csv", b"Date,Amount\n2026-01-01,1\n")));
    let up = upload::encode_multipart("B0und", &[("sam-SAMMARK.csv".into(), b"Date,Amount\n2026-01-01,1\n".to_vec())]);
    let (act, abody) = multipart(&[], Some(("p.png", &png())));
    let writes: Vec<(&str, String, Option<(String, Vec<u8>)>, Vec<(&str, &str)>)> = vec![
        ("PUT", "/api/formats/sammark_bank".into(), Some(("text/plain".into(), fmt.into_bytes())), vec![("X-Finance-Format", "1")]),
        ("PUT", "/api/formats/ownermark_bank".into(), Some(("text/plain".into(), monzo.replace("monzo", "ownermark_bank").into_bytes())), vec![("X-Finance-Format", "1")]),
        ("POST", "/upload".into(), Some(("multipart/form-data; boundary=B0und".into(), up)), vec![("Accept", "application/json")]),
        ("POST", "/sync".into(), None, vec![]),
        ("POST", "/api/refresh".into(), None, vec![]),
        ("POST", "/api/overrides".into(), Some(("application/json".into(), br#"[{"source":"hsbc","raw_hash":"owner-h1","category":"Hacked","share":null,"note":null,"created_at":1}]"#.to_vec())), vec![]),
        ("POST", "/api/preview".into(), Some((pct.clone(), pbody.clone())), vec![]),
        ("PUT", "/api/profile".into(), Some(("application/json".into(), br#"{"base_currency":"HKD","name":"Sam SAMMARK"}"#.to_vec())), vec![]),
        ("PUT", "/api/settings/budgets".into(), Some(("application/json".into(), br#"{"SAMMARK":1}"#.to_vec())), vec![]),
        ("PUT", "/api/settings/home".into(), Some(("application/json".into(), br#"{"mode":"rent","rent":{"landlord_match":"SAMMARK landlord"}}"#.to_vec())), vec![]),
        ("PUT", "/api/profile/avatar".into(), Some(("image/png".into(), png())), vec![]),
        ("POST", "/api/profile/avatar".into(), Some((act, abody)), vec![]),
        ("POST", "/api/profile/avatar/gravatar".into(), Some(("application/json".into(), br#"{"email":"sam@example.com"}"#.to_vec())), vec![]),
        ("DELETE", "/api/profile/avatar".into(), None, vec![]),
        ("POST", "/api/categories".into(), Some(("application/json".into(), br#"{"name":"SamCat"}"#.to_vec())), vec![]),
        ("PATCH", "/api/categories".into(), Some(("application/json".into(), br#"{"order":["SamCat","OwnermarkCat"]}"#.to_vec())), vec![]),
        ("PATCH", "/api/categories/OwnermarkCat".into(), Some(("application/json".into(), br#"{"name":"Stolen"}"#.to_vec())), vec![]),
        ("POST", "/api/categories/OwnermarkCat/merge".into(), Some(("application/json".into(), br#"{"into":"Other"}"#.to_vec())), vec![]),
        ("DELETE", "/api/categories/OwnermarkCat".into(), None, vec![]),
        ("POST", "/api/rules".into(), Some(("application/json".into(), br#"{"pattern":"SAMMARK_RULE","category":"SamCat"}"#.to_vec())), vec![]),
        ("POST", "/api/rules/preview".into(), Some(("application/json".into(), br#"{"pattern":"tea","category":"X"}"#.to_vec())), vec![]),
        ("POST", "/api/rules/test".into(), Some(("application/json".into(), br#"{"description":"OWNERMARK tea house","amount":-5}"#.to_vec())), vec![]),
        ("POST", "/api/rules/starter".into(), None, vec![]),
        ("PATCH", "/api/rules/1".into(), Some(("application/json".into(), br#"{"pattern":"x"}"#.to_vec())), vec![]),
        ("DELETE", "/api/rules/1".into(), None, vec![]),
        ("DELETE", "/api/rules/2".into(), None, vec![]),
        ("PUT", "/api/txn/hsbc/owner-h1/category".into(), Some(("application/json".into(), br#"{"category":"Hacked"}"#.to_vec())), vec![]),
        ("POST", "/api/admin/investments/accounts".into(), Some(("application/json".into(), br#"{"name":"SAMMARK broker","matches":["x"],"valued_by":"cost"}"#.to_vec())), vec![]),
        ("PUT", "/api/admin/investments/accounts/HSBC%20FlexInvest".into(), Some(("application/json".into(), br#"{"name":"Stolen","matches":["x"],"valued_by":"cost"}"#.to_vec())), vec![]),
        ("DELETE", "/api/admin/investments/accounts/HSBC%20FlexInvest".into(), None, vec![]),
        ("PUT", "/api/admin/investments/instruments/OWNSYM1".into(), Some(("application/json".into(), br#"{"kind":"stock"}"#.to_vec())), vec![]),
        ("DELETE", "/api/admin/investments/instruments/OWNSYM1".into(), None, vec![]),
        ("POST", "/api/sources/okx/test".into(), Some(("application/json".into(), br#"{"fields":{}}"#.to_vec())), vec![]),
        ("PUT", "/api/sources/binance/secret".into(), Some(("application/json".into(), br#"{"fields":{"BINANCE_API_KEY":"SAMMARK-binance-key-0005","BINANCE_API_SECRET":"SAMMARK-binance-secret-0006"}}"#.to_vec())), vec![]),
        ("DELETE", "/api/sources/binance/secret".into(), None, vec![]),
        ("PUT", "/api/admin/backup-recipient".into(), Some(("application/json".into(), serde_json::to_vec(&json!({"recipient": finance_engine::archive::generate_identity().1})).unwrap())), vec![]),
        ("POST", "/api/me/pair-code".into(), None, vec![]),
        ("PUT", "/api/mcp".into(), Some(("application/json".into(), br#"{"enabled":true}"#.to_vec())), vec![]),
        ("POST", "/api/mcp/keys".into(), Some(("application/json".into(), br#"{"name":"sam key"}"#.to_vec())), vec![]),
        ("DELETE", "/api/mcp/keys/1".into(), None, vec![]),
        ("POST", "/mcp".into(), Some(("application/json".into(), br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec())), vec![]),
        // admin routes: refused (Sam isn't one)
        ("POST", "/api/admin/pair-code".into(), None, vec![]),
        ("POST", "/api/admin/wipe".into(), Some(("application/json".into(), br#"{"confirm":"DELETE ALL"}"#.to_vec())), vec![]),
        ("POST", "/api/admin/auth-mode".into(), Some(("application/json".into(), br#"{"mode":"off"}"#.to_vec())), vec![]),
        ("POST", "/api/admin/invite".into(), None, vec![]),
        ("DELETE", "/api/admin/devices/1".into(), None, vec![]),
        ("DELETE", "/api/me/devices/1".into(), None, vec![]),
        ("PATCH", "/api/admin/users/1".into(), Some(("application/json".into(), br#"{"name":"pwned"}"#.to_vec())), vec![]),
        ("DELETE", "/api/admin/users/1".into(), None, vec![]),
        ("POST", "/api/secrets/import-env".into(), None, vec![]),
        ("POST", "/api/webapp".into(), None, vec![("X-Finance-Install", "1")]),
        // public account routes with a device: nothing of the owner's
        ("POST", "/api/pair".into(), Some(("application/json".into(), br#"{"code":"AAAABBBB"}"#.to_vec())), vec![]),
        ("POST", "/api/signup".into(), Some(("application/json".into(), br#"{"name":"x"}"#.to_vec())), vec![]),
    ];
    for (m, p, body, extra) in &writes {
        let (st, b) = c.call(m, p, s, body.as_ref().map(|(ct, d)| (ct.as_str(), d.clone())), extra);
        leak("sam", m, p, st, &b, &owner_marks);
        if p.starts_with("/api/admin/") && !p.starts_with("/api/admin/investments/") && !p.starts_with("/api/admin/backup-recipient") {
            assert_eq!(st, 403, "{m} {p}: admins only");
        }
    }
    // his own devices only: the owner's aren't listed, nor his to sign out
    let (_, b) = c.call("GET", "/api/me/devices", o, None, &[]);
    let owner_devs: Vec<i64> = serde_json::from_slice::<serde_json::Value>(&b).unwrap()["devices"]
        .as_array().unwrap().iter().filter_map(|d| d["id"].as_i64()).collect();
    assert!(!owner_devs.is_empty());
    let (_, b) = c.call("GET", "/api/me/devices", s, None, &[]);
    let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
    assert!(v["devices"].as_array().unwrap().iter().all(|d| d["user_id"].as_i64() == Some(sam_id)), "{v}");
    for id in &owner_devs {
        assert_eq!(c.call("DELETE", &format!("/api/me/devices/{id}"), s, None, &[]).0, 400, "the owner's device {id}");
    }
    // his archive: his data only, and importing it back stays on his tree
    let (st, file) = c.call("POST", "/api/account/export", s, Some(("application/json", br#"{"passphrase":"sam passphrase!"}"#.to_vec())), &[]);
    assert_eq!(st, 200);
    let a = finance_engine::archive::open(&file[..], &finance_engine::archive::Unseal::Passphrase("sam passphrase!".into())).unwrap();
    let mut plain = vec![];
    for (_, d) in a.originals() {
        plain.extend_from_slice(d);
    }
    plain.extend_from_slice(a.db_bytes());
    assert!(has(&plain, "SAMMARK") && !has(&plain, "OWNERMARK"), "the archive is Sam's tree");
    let (ct, body) = multipart(&[("passphrase", "sam passphrase!"), ("mode", "merge"), ("dry_run", "0")], Some(("a.finance", &file)));
    let (st, b) = c.call("POST", "/api/account/import", s, Some((&ct, body)), &[]);
    assert_eq!(st, 200, "{}", String::from_utf8_lossy(&b));
    leak("sam", "POST", "/api/account/import", st, &b, &owner_marks);
    // an owner's archive is no way in either: Sam can't open what isn't his
    let (ct, body) = multipart(&[("identity", "AGE-SECRET-KEY-1QQQ"), ("mode", "merge")], Some(("o.finance", b"OWNERMARK archive")));
    let (st, b) = c.call("POST", "/api/account/import", s, Some((&ct, body)), &[]);
    leak("sam", "POST", "/api/account/import", st, &b, &owner_marks);
    // signing out revokes the device: one of Sam's others, so he carries on
    let spare = c.pair(sam_id, "sam's spare");
    let (st, _) = c.call("POST", "/api/logout", Some(&spare), None, &[]);
    assert_eq!(st, 200);
    assert_eq!(c.json("GET", "/api/whoami", Some(&spare), Value::Null).1["paired"], false);

    std::thread::sleep(Duration::from_millis(500)); // Sam's background work (warm, sync) lands
    let after = owner_digest(&home);
    let changed: Vec<&String> = before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).collect();
    assert!(changed.is_empty(), "Sam's requests changed the owner's tree: {changed:?}");
    // ... and landed on his
    assert!(home.join(format!("users/{sam_id}/transactions/sam-SAMMARK.csv")).exists(), "the upload is in his tree");
    assert!(home.join(format!("users/{sam_id}/formats/sammark_bank.toml")).exists());
    assert!(!home.join("formats/sammark_bank.toml").exists() && !home.join("transactions/sam-SAMMARK.csv").exists());
    assert!(auth::users().unwrap().iter().any(|u| u.id == 1 && u.name != "pwned"));
    assert_eq!(c.json("GET", "/api/admin/backup-recipient", o, Value::Null).1["recipient"], owner_recipient, "Sam's key is his own");
    assert_ne!(c.json("GET", "/api/admin/backup-recipient", s, Value::Null).1["recipient"], owner_recipient);

    // ---- the owner reads everything: nothing of Sam's ----
    for p in &reads {
        let (st, b) = c.call("GET", p, o, None, &[]);
        leak("owner", "GET", p, st, &b, &sam_marks);
    }
    let (_, src) = c.call("GET", "/api/sources", o, None, &[]);
    assert!(has(&src, "QZ7W") && !has(&src, "XK4P"), "each sees their own key's hint");
    // ---- a second admin: no impersonation, no reading the owner's data ----
    let a_ = Some(ada.as_str());
    for p in &reads {
        let (st, b) = c.call("GET", p, a_, None, &[]);
        leak("ada (admin)", "GET", p, st, &b, &owner_marks);
        leak("ada (admin)", "GET", p, st, &b, &sam_marks);
    }
    let (st, b) = c.call("POST", "/api/secrets/import-env", a_, None, &[]);
    assert_eq!(st, 400, "the owner's .env isn't another admin's: {}", String::from_utf8_lossy(&b));
    let (st, v) = c.json("GET", "/api/admin/users", a_, Value::Null);
    assert_eq!(st, 200);
    assert!(!v.to_string().contains("users/"), "no paths: {v}");
    // ---- no device on a household server: no data at all ----
    for p in &reads {
        let (st, b) = c.call("GET", p, None, None, &[]);
        let public = matches!(*p, "/healthz" | "/api/version" | "/favicon.ico" | "/favicon.png" | "/" | "/app" | "/app/" | "/app/finance.wasm" | "/sw.js" | "/login" | "/api/whoami" | "/api/demo/ledger.json" | "/api/demo/export.json");
        if !public {
            assert_eq!(st, 401, "anonymous GET {p}");
        }
        leak("anonymous", "GET", p, st, &b, &owner_marks);
    }
    for (m, p, body, extra) in &writes {
        let (st, b) = c.call(m, p, None, body.as_ref().map(|(ct, d)| (ct.as_str(), d.clone())), extra);
        leak("anonymous", m, p, st, &b, &owner_marks);
    }
    assert_eq!(owner_digest(&home), before, "nothing anonymous reached the owner's tree");

    // ---- the sync scheduler's view: each user's own sources ----
    let ada_t = auth::tenant_of("Ada").unwrap();
    assert!(sam_t.enter(finance_server::syncer::has_sources), "Sam set OKX up");
    assert!(!ada_t.enter(finance_server::syncer::has_sources), "Ada has nothing set up (the owner's keys aren't hers)");
    let ts = finance_server::syncer::tenants();
    assert_eq!(ts.iter().map(|t| t.uid).collect::<Vec<_>>(), vec![1, sam_id, ada_id]);
    assert_eq!(ts[1].enter(finance_engine::fetch::status::path), home.join(format!("users/{sam_id}/data/fetch_status.json")));
    assert_eq!(ts[0].enter(finance_engine::fetch::status::path), home.join("data/fetch_status.json"), "the owner's, unchanged");

    // ---- MCP --user: one tree ----
    let bin = env!("CARGO_BIN_EXE_anser-server");
    let session = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"transactions","arguments":{"text":"MARK","limit":50}}}),
    ];
    let input: String = session.iter().map(|v| format!("{v}\n")).collect();
    let run = |args: &[&str]| {
        use std::io::Write;
        let mut child = std::process::Command::new(bin)
            .args(args)
            .env("FINANCE_HOME", &home)
            .env("FINANCE_OFFLINE", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
    };
    let sam_mcp = run(&["mcp", "--user", "Sam"]);
    assert!(sam_mcp.contains("SAMMARK coffee") && !sam_mcp.contains("OWNERMARK"), "{sam_mcp}");
    let owner_mcp = run(&["mcp"]);
    assert!(owner_mcp.contains("OWNERMARK tea") && !owner_mcp.contains("SAMMARK"), "{owner_mcp}");
    let by_id = run(&["mcp", "--user", &sam_id.to_string()]);
    assert!(by_id.contains("SAMMARK coffee") && !by_id.contains("OWNERMARK"));
    assert!(run(&["mcp", "--user", "nobody"]).contains("no user named"));

    // ---- MCP over HTTP: a user's key reads that user's tree only ----
    let mcp_text = |key: &str, text: &str| {
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"transactions","arguments":{"text":text,"limit":50}}});
        let (st, b) = c.call("POST", "/mcp", Some(key), Some(("application/json", serde_json::to_vec(&body).unwrap())), &[]);
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&b));
        String::from_utf8_lossy(&b).to_string()
    };
    assert_eq!(c.json("PUT", "/api/mcp", o, json!({"enabled": true})).0, 200);
    // Sam switched his on above (his writes); each makes a key through the API
    let (st, v) = c.json("POST", "/api/mcp/keys", s, json!({"name": "sam cursor"}));
    assert_eq!(st, 200, "{v}");
    let sam_key = v["token"].as_str().unwrap().to_string();
    let (st, v) = c.json("POST", "/api/mcp/keys", o, json!({"name": "owner claude"}));
    assert_eq!(st, 200, "{v}");
    let owner_key = v["token"].as_str().unwrap().to_string();
    let sam_http = mcp_text(&sam_key, "MARK");
    assert!(sam_http.contains("SAMMARK coffee") && !sam_http.contains("OWNERMARK"), "{sam_http}");
    let owner_http = mcp_text(&owner_key, "MARK");
    assert!(owner_http.contains("OWNERMARK tea") && !owner_http.contains("SAMMARK"), "{owner_http}");
    // a key opens nothing else: not even its own user's data routes
    for k in [&sam_key, &owner_key] {
        assert_eq!(c.call("GET", "/api/export.json", Some(k), None, &[]).0, 403);
        assert_eq!(c.call("GET", "/api/mcp", Some(k), None, &[]).0, 403);
    }
    // Sam's key list is his, and the owner's key isn't his to revoke
    let sam_keys = c.json("GET", "/api/mcp", s, Value::Null).1;
    assert!(!sam_keys.to_string().contains("owner claude") && sam_keys.to_string().contains("sam cursor"), "{sam_keys}");
    let owner_key_id = c.json("GET", "/api/mcp", o, Value::Null).1["keys"][0]["id"].as_i64().unwrap();
    assert_eq!(c.json("DELETE", &format!("/api/mcp/keys/{owner_key_id}"), s, Value::Null).0, 404);
    assert!(mcp_text(&owner_key, "MARK").contains("OWNERMARK tea"), "still the owner's");

    // ---- the danger zone: the owner can't delete themself; a member can,
    // keeping their data or not, and never touching the owner's ----
    let before = owner_digest(&home);
    let (st, _) = c.call("POST", "/api/me/delete", None, Some(("application/json", br#"{"purge":true}"#.to_vec())), &[]);
    assert_eq!(st, 401, "anonymous");
    assert_eq!(c.json("POST", "/api/me/delete", o, json!({"purge": true})).0, 400, "the owner");
    let sam_home = home.join(format!("users/{sam_id}"));
    assert!(sam_home.exists());
    assert_eq!(c.json("POST", "/api/me/delete", s, json!({"purge": true})).0, 200);
    assert!(!sam_home.exists(), "Sam's tree is gone with purge");
    assert!(!auth::users().unwrap().iter().any(|u| u.id == sam_id));
    assert_eq!(c.json("GET", "/api/whoami", s, Value::Null).1["paired"], false, "Sam's device went with the account");
    assert_eq!(c.call("POST", "/mcp", Some(&sam_key), Some(("application/json", br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec())), &[]).0, 401, "and his MCP key");
    assert_eq!(owner_digest(&home), before, "the owner's tree is untouched");

    let _ = std::fs::remove_dir_all(&home);
}
