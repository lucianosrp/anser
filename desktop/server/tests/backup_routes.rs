//! Backups and the account archive over HTTP on a throwaway tree: the
//! backup recipient (validated), the nightly archive, listing and the
//! streamed latest, the streamed passphrase export, and import (dry run
//! first, then the real merge, 0 duplicates). Admin only.

use std::time::Duration;

use finance_engine::archive::{self, Unseal};
use finance_server::{auth, backups, server};
use serde_json::{json, Value};

#[test]
fn backups_and_archives_over_http() {
    let home = std::env::temp_dir().join(format!("finance-backups-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::remove_var("FINANCE_AUTH");
    {
        let c = finance_engine::db::connect(&home.join("data/finance.db")).unwrap();
        c.execute_batch("INSERT INTO fx_rates(base,quote,month,rate,fetched_at) VALUES('EUR','HKD','2026-01',8.5,0);").unwrap();
        finance_engine::db::insert_bank_transactions(&c, &[finance_engine::db::NewBankTxn {
            source: "hsbc".into(), account: "a".into(), currency: "HKD".into(), txn_date: "2026-09-01".into(), amount: Some(-5.0),
            balance: Some(10.0), description: "tea".into(), raw_hash: "h1".into(), source_file: "x.csv".into(), ingested_at: 1,
        }]).unwrap();
    }
    std::fs::write(home.join("transactions/x.csv"), "a,b\n").unwrap();
    auth::bootstrap().unwrap();
    // a secret in the store: never exported unless asked
    finance_engine::secrets::put(1, &[("OKX_API_KEY".to_string(), "okx-secret-value-123".to_string())].into()).unwrap();

    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).http_status_as_error(false).build().into();
    let (code, _) = auth::new_pair_code(1).unwrap();
    let mut r = agent.post(&format!("{base}/api/pair")).header("Content-Type", "application/json")
        .send(serde_json::to_vec(&json!({"code": code, "device_name": "laptop"})).unwrap()).unwrap();
    let token = serde_json::from_slice::<Value>(&r.body_mut().read_to_vec().unwrap()).unwrap()["token"].as_str().unwrap().to_string();
    let bearer = format!("Bearer {token}");

    let get = |path: &str, auth: bool| {
        let mut b = agent.get(&format!("{base}{path}"));
        if auth {
            b = b.header("Authorization", &bearer);
        }
        let mut r = b.call().unwrap();
        (r.status().as_u16(), r.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap())
    };
    let send = |method: &str, path: &str, body: Value| {
        let b = if method == "PUT" { agent.put(&format!("{base}{path}")) } else { agent.post(&format!("{base}{path}")) };
        let mut r = b.header("Authorization", &bearer).header("Content-Type", "application/json").send(serde_json::to_vec(&body).unwrap()).unwrap();
        (r.status().as_u16(), r.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap())
    };
    let js = |b: &[u8]| serde_json::from_slice::<Value>(b).unwrap();

    // admin only
    assert_eq!(get("/api/admin/archives", false).0, 401);
    assert_eq!(agent.post(&format!("{base}/api/account/export")).send(b"{}".as_slice()).unwrap().status().as_u16(), 401);

    // the recipient: validated, stored, used by the nightly backup
    assert_eq!(send("PUT", "/api/admin/backup-recipient", json!({"recipient": "not-a-key"})).0, 400);
    let (id, public) = archive::generate_identity();
    let (s, b) = send("PUT", "/api/admin/backup-recipient", json!({"recipient": public, "keep": 3}));
    assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
    assert_eq!(js(&get("/api/admin/backup-recipient", true).1)["recipient"], json!(public));
    assert_eq!(get("/api/admin/archives/latest", true).0, 404, "none yet");
    let (path, size) = backups::nightly().unwrap().unwrap();
    assert!(size > 0 && path.starts_with(home.join("data/backups/archives")));
    let list = js(&get("/api/admin/archives", true).1);
    assert_eq!(list["archives"].as_array().unwrap().len(), 1);
    let (s, latest) = get("/api/admin/archives/latest", true);
    assert_eq!(s, 200);
    assert_eq!(latest, std::fs::read(&path).unwrap(), "streamed byte for byte");
    let rep = archive::verify(&latest[..], &Unseal::Identity(id.clone())).unwrap();
    assert!(!rep.manifest.includes_secrets);

    // export with a passphrase, streamed
    assert_eq!(send("POST", "/api/account/export", json!({"passphrase": "short"})).0, 400);
    let (s, file) = send("POST", "/api/account/export", json!({"passphrase": "a long passphrase"}));
    assert_eq!(s, 200, "{}", String::from_utf8_lossy(&file));
    let a = archive::open(&file[..], &Unseal::Passphrase("a long passphrase".into())).unwrap();
    assert!(a.secrets().unwrap().is_none());
    assert!(!file.windows(8).any(|w| w == b"okx-secr"));
    let (_, with) = send("POST", "/api/account/export", json!({"passphrase": "a long passphrase", "with_secrets": true}));
    let w = archive::open(&with[..], &Unseal::Passphrase("a long passphrase".into())).unwrap();
    assert_eq!(w.secrets().unwrap().unwrap()["OKX_API_KEY"], "okx-secret-value-123");

    // import: a dry run first, then the merge; the same data adds nothing
    let import = |fields: &[(&str, &str)], data: &[u8]| {
        let mut body = Vec::new();
        for (k, v) in fields {
            body.extend_from_slice(format!("--XB\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
        }
        body.extend_from_slice(b"--XB\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.finance\"\r\nContent-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n--XB--\r\n");
        let mut r = agent.post(&format!("{base}/api/account/import")).header("Authorization", &bearer)
            .header("Content-Type", "multipart/form-data; boundary=XB").send(&body[..]).unwrap();
        (r.status().as_u16(), js(&r.body_mut().read_to_vec().unwrap()))
    };
    let (s, v) = import(&[("passphrase", "wrong passphrase")], &file);
    assert_eq!(s, 400, "{v}");
    let (s, v) = import(&[("passphrase", "a long passphrase"), ("mode", "merge")], &file);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["report"]["dry_run"], json!(true), "a dry run unless asked");
    let (s, v) = import(&[("identity", &id), ("mode", "merge"), ("dry_run", "0")], &latest);
    assert_eq!(s, 200, "{v}");
    let added: i64 = v["report"]["tables"].as_array().unwrap().iter().map(|t| t["added"].as_i64().unwrap()).sum();
    assert_eq!(added, 0, "{}", v["text"]);
    let (s, v) = import(&[("passphrase", "a long passphrase"), ("mode", "fresh")], &file);
    assert_eq!(s, 409, "fresh refuses a tree with data: {v}");
    let _ = std::fs::remove_dir_all(&home);
}
