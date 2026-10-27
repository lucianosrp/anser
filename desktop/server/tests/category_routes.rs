//! The Phase 3 routes end to end on a throwaway tree: the startup
//! migration, a row's category, a rule's preview / create / edit / test /
//! delete, categories renamed, merged and deleted (budgets follow), the
//! suggestions on synthetic data, the starter pack, and data_rev moving in
//! the replies, capabilities and the bundle. Writes need a device.

use std::time::Duration;

use serde_json::{json, Value};

use finance_server::{auth, server};

fn tree() -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("finance-cat-routes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("data")).unwrap();
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::remove_var("FINANCE_AUTH");
    let c = finance_engine::db::connect(&home.join("data/finance.db")).unwrap();
    let mut rows = String::new();
    // a monthly streaming charge nobody has a rule for, a coffee chain, a
    // transfer from the owner's own name, a deposit, a partner payment
    for m in 1..=8 {
        rows += &format!("('n26','','EUR','2026-0{m}-03',-12.99,'MUBI LONDON','mubi{m}','f',0),");
        rows += &format!("('hsbc_card','','HKD','2026-0{m}-10',-48,'PACIFIC COFFEE 12 CENTRAL','pc{m}','f',0),");
    }
    rows += "('hsbc','','HKD','2026-08-01',5000,'TRANSFER FROM JANE Q DOE','self1','f',0),";
    rows += "('hsbc','','HKD','2026-08-02',-20000,'FLAT DEPOSIT KINGS RD','dep1','f',0),";
    rows += "('hsbc','','HKD','2026-08-05',800,'PAYMENT FROM SAM PARTNERLY','par1','f',0),";
    rows += "('hsbc','','HKD','2026-08-06',-300,'Sushi Bar Toro','sushi1','f',0)";
    c.execute_batch(&format!(
        "INSERT INTO bank_transactions(source, account, currency, txn_date, amount, description, raw_hash, source_file, ingested_at) VALUES {rows};
         INSERT INTO fx_rates(base, quote, month, rate, fetched_at) VALUES ('EUR', 'HKD', '2026-01-01', 8.5, 0);"
    ))
    .unwrap();
    finance_engine::categorize::categorize_all(&c).unwrap();
    // the owner's own rule for the partner (an existing tree's rules are theirs)
    c.execute("INSERT INTO category_rules(priority, pattern, category, is_transfer) VALUES (28, 'partnerly', 'Partner', 1)", []).unwrap();
    finance_engine::categorize::categorize_all(&c).unwrap();
    home
}

#[test]
fn categories_and_rules_end_to_end() {
    let home = tree();
    let boot = auth::bootstrap().unwrap();
    auth::set_mode(auth::Mode::Writes).unwrap(); // the reads below carry no device
    assert!(boot.categories_migrated, "an existing tree gets the categories table at startup");
    {
        let c = finance_engine::db::connect(&home.join("data/finance.db")).unwrap();
        let p = finance_engine::profile::Profile { base_currency: "HKD".into(), name: Some("Jane Doe".into()), aliases: vec!["Jane Q Doe".into()], ..Default::default() };
        finance_engine::profile::put(&c, &p).unwrap();
        finance_engine::profile::put_setting(&c, "budgets", &json!({"monthly": {"Dining": 2000.0, "Subscriptions": 100.0}})).unwrap();
        let users: i64 = c.query_row("SELECT COUNT(*) FROM category_rules WHERE origin = 'user'", [], |r| r.get(0)).unwrap();
        let all: i64 = c.query_row("SELECT COUNT(*) FROM category_rules", [], |r| r.get(0)).unwrap();
        assert_eq!(users, all, "every existing rule is a user rule");
    }
    let (code, _) = auth::new_pair_code(1).unwrap();
    let (token, _) = auth::pair(&code, "categories test").unwrap();
    let bearer = format!("Bearer {token}");
    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).http_status_as_error(false).build().into();

    let get = |p: &str| -> Value {
        let mut r = agent.get(&format!("{base}{p}")).call().unwrap();
        assert_eq!(r.status().as_u16(), 200, "GET {p}");
        serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap()
    };
    let send = |m: &str, p: &str, body: Value, auth: bool| -> (u16, Value) {
        let url = format!("{base}{p}");
        let bytes = serde_json::to_vec(&body).unwrap();
        let r = match m {
            "POST" => agent.post(&url),
            "PUT" => agent.put(&url),
            "PATCH" => agent.patch(&url),
            _ => {
                let mut b = agent.delete(&url);
                if auth {
                    b = b.header("Authorization", &bearer);
                }
                let mut r = b.call().unwrap();
                return (r.status().as_u16(), serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap_or(Value::Null));
            }
        };
        let r = r.header("Content-Type", "application/json");
        let r = if auth { r.header("Authorization", &bearer) } else { r };
        let mut r = r.send(&bytes[..]).unwrap();
        (r.status().as_u16(), serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap_or(Value::Null))
    };
    let rev = || get("/api/capabilities")["data_rev"].as_i64().unwrap();
    assert_eq!(rev(), 0);

    // the lists
    let cats = get("/api/categories");
    let names: Vec<&str> = cats["categories"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Dining") && names.contains(&"Other") && names.contains(&"Partner"));
    let partner = cats["categories"].as_array().unwrap().iter().find(|c| c["name"] == "Partner").unwrap();
    assert_eq!(partner["kind"], "partner");
    let rules = get("/api/rules");
    assert!(rules["rules"].as_array().unwrap().len() > 290);

    // writes want a device
    assert_eq!(send("PUT", "/api/txn/hsbc_card/pc1/category", json!({"category": "Coffee"}), false).0, 401);

    // one row: pinned, the reply names it, data_rev moves everywhere
    let (s, v) = send("PUT", "/api/txn/hsbc_card/pc1/category", json!({"category": "Coffee", "note": "test"}), true);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["changed"], json!([{"source": "hsbc_card", "raw_hash": "pc1", "category": "Coffee", "share": null}]));
    assert_eq!(v["data_rev"], 1);
    assert!(v["categories"].as_array().unwrap().iter().any(|c| c["name"] == "Coffee"), "a new category joins the table");
    assert_eq!(rev(), 1);
    let (_, bundle) = (0, get("/api/export.json"));
    assert_eq!(bundle["data_rev"], 1, "the bundle carries data_rev");
    let ledger = get("/api/ledger.json");
    assert_eq!(ledger["data_rev"], 1);
    assert!(ledger["categories"].as_array().unwrap().iter().any(|c| c["name"] == "Coffee"));
    let row = ledger["entries"].as_array().unwrap().iter().find(|e| e["raw_hash"] == "pc1").unwrap();
    assert_eq!(row["category"], "Coffee");

    // all like this: preview, then the rule
    let (s, p) = send("POST", "/api/rules/preview", json!({"pattern": "pacific coffee", "direction": "out", "category": "Coffee"}), true);
    assert_eq!(s, 200, "{p}");
    assert_eq!((p["matches"].as_u64(), p["pinned"].as_u64(), p["would_change"].as_u64()), (Some(8), Some(1), Some(7)));
    assert_eq!(p["sample"].as_array().unwrap().len(), 8);
    assert_eq!(rev(), 1, "a preview writes nothing");
    let (s, v) = send("POST", "/api/rules", json!({"pattern": "pacific coffee", "direction": "out", "category": "Coffee", "priority": p["priority"]}), true);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["changed"].as_array().unwrap().len(), 7);
    let id = v["result"].as_i64().unwrap();
    let t = send("POST", "/api/rules/test", json!({"description": "PACIFIC COFFEE 99 TST", "amount": -40}), true).1;
    assert_eq!(t["category"], "Coffee");
    assert_eq!(t["winner"]["id"], id);
    // edit: money in only -> the rows go back
    let (s, v) = send("PATCH", &format!("/api/rules/{id}"), json!({"direction": "in"}), true);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["changed"].as_array().unwrap().len(), 7);
    let (s, v) = send("DELETE", &format!("/api/rules/{id}"), Value::Null, true);
    assert_eq!(s, 200, "{v}");
    assert!(v["rules"].as_array().unwrap().iter().all(|r| r["id"] != id));
    assert_eq!(send("PATCH", "/api/rules/999999", json!({"enabled": false}), true).0, 400);

    // rename (budgets follow), merge, delete
    let (s, v) = send("PATCH", "/api/categories/Dining", json!({"name": "Eating out", "icon": "Utensils"}), true);
    assert_eq!(s, 200, "{v}");
    assert!(v["changed"].as_array().unwrap().iter().any(|c| c["raw_hash"] == "sushi1" && c["category"] == "Eating out"));
    let b = get("/api/export.json")["user_settings"]["budgets"]["monthly"].clone();
    assert_eq!((b["Eating out"].as_f64(), b.get("Dining")), (Some(2000.0), None));
    let (s, v) = send("POST", "/api/categories/Coffee/merge", json!({"into": "Eating out"}), true);
    assert_eq!(s, 200, "{v}");
    assert!(v["changed"].as_array().unwrap().iter().any(|c| c["raw_hash"] == "pc1" && c["category"] == "Eating out"));
    let (s, v) = send("DELETE", "/api/categories/Utilities%20%2F%20Telecom?into=Other", Value::Null, true);
    assert_eq!(s, 200, "{v}");
    assert!(v["categories"].as_array().unwrap().iter().all(|c| c["name"] != "Utilities / Telecom"));
    let (s, _) = send("PATCH", "/api/categories/Partner", json!({"kind": "nonsense"}), true);
    assert_eq!(s, 400);
    let (s, v) = send("PATCH", "/api/categories", json!({"order": ["Eating out", "Groceries"]}), true);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["categories"][0]["name"], "Eating out");

    // suggestions: the uncategorized clusters, with reasons
    let sg = get("/api/categorize/suggest");
    let cl = sg["clusters"].as_array().unwrap();
    let find = |k: &str| cl.iter().find(|c| c["key"].as_str() == Some(k)).unwrap_or_else(|| panic!("no cluster {k}: {sg}"));
    let mubi = find("mubi london");
    assert_eq!((mubi["count"].as_u64(), mubi["direction"].as_str(), mubi["pattern"].as_str()), (Some(8), Some("out"), Some("mubi london")));
    assert_eq!(mubi["proposal"]["source"], "recurring", "{mubi}");
    assert_eq!(mubi["proposal"]["category"], "Subscriptions");
    let me = find("jane q doe");
    assert_eq!(me["proposal"]["source"], "self", "{me}");
    let dep = cl.iter().find(|c| c["samples"][0].as_str().is_some_and(|s| s.contains("DEPOSIT"))).unwrap();
    assert_eq!((dep["proposal"]["source"].as_str(), dep["proposal"]["category"].as_str()), (Some("deposit"), Some("Deposit")));
    assert!(cl.windows(2).all(|w| w[0]["score"].as_f64() >= w[1]["score"].as_f64()), "ranked by spend x count");
    assert_eq!(sg["starter"], false, "plenty of rules");
    // one click: accept the proposal as a user rule
    let (s, v) = send("POST", "/api/rules", json!({"pattern": mubi["pattern"], "direction": mubi["direction"], "category": "Subscriptions",
        "priority": mubi["priority"], "note": "from suggestions"}), true);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["changed"].as_array().unwrap().len(), 8);
    assert!(get("/api/categorize/suggest")["clusters"].as_array().unwrap().iter().all(|c| c["key"] != "mubi london"));

    // the starter pack adds only what is missing
    let (s, v) = send("POST", "/api/rules/starter", Value::Null, true);
    assert_eq!(s, 200, "{v}");
    assert!(v["result"].as_u64().unwrap() > 0, "the deleted category's rules come back as seed rules");
    let final_rev = rev();
    assert!(final_rev >= 10, "every write moved data_rev: {final_rev}");
    let _ = std::fs::remove_dir_all(&home);
}
