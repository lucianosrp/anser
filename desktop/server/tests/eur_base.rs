//! A synthetic EUR-based household (Phase 4): the profile says EUR, the
//! FX pairs come from the data and are quoted in EUR, the bundle's figures
//! are in EUR (its `fx.base` says so), and the investment setup is edited
//! through the admin routes. Needs no personal data.

use std::time::Duration;

use serde_json::{json, Value};

use finance_server::{auth, server};

fn tree() -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("finance-eur-base-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("data")).unwrap();
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::set_var("FINANCE_TODAY", "2026-09-20");
    std::env::remove_var("FINANCE_AUTH");
    let c = finance_engine::db::connect(&home.join("data/finance.db")).unwrap();
    let mut rows = String::new();
    let mut eur = 0.0;
    let mut gbp = 500.0;
    for m in 1..=9 {
        // n26 in EUR: a salary, a supermarket, an index fund top-up
        for (d, amt, desc) in [(1, 3000.0, "ACME GMBH SALARY"), (5, -80.0, "REWE MARKT BERLIN"), (10, -200.0, "SPARPLAN FLEXINVEST")] {
            eur += amt;
            rows += &format!("('n26','','EUR','2026-0{m}-{d:02}',{amt},{eur},'{desc}','n{m}{d}','f',0),");
        }
        // a Revolut GBP account
        gbp -= 20.0;
        rows += &format!("('revolut_new','GBP','GBP','2026-0{m}-12',-20,{gbp},'TESCO LONDON','r{m}','f',0),");
    }
    rows.pop();
    c.execute_batch(&format!(
        "INSERT INTO bank_transactions(source, account, currency, txn_date, amount, balance, description, raw_hash, source_file, ingested_at) VALUES {rows};"
    ))
    .unwrap();
    let mut fx = String::new();
    for m in 1..=9 {
        fx += &format!("('GBP','EUR','2026-0{m}-01',1.17,0),('USD','EUR','2026-0{m}-01',0.92,0),");
    }
    fx.pop();
    c.execute_batch(&format!("INSERT INTO fx_rates(base, quote, month, rate, fetched_at) VALUES {fx};")).unwrap();
    // the owner's own rule: the savings plan is an investment
    c.execute("INSERT INTO category_rules(priority, pattern, category, is_transfer) VALUES (25, 'sparplan', 'Investments', 1)", []).unwrap();
    finance_engine::categorize::categorize_all(&c).unwrap();
    home
}

#[test]
fn a_eur_household_reads_in_euros() {
    let home = tree();
    auth::bootstrap().unwrap();
    auth::set_mode(auth::Mode::Writes).unwrap(); // the reads below carry no device
    {
        let c = finance_engine::db::connect(&home.join("data/finance.db")).unwrap();
        let p = finance_engine::profile::Profile { base_currency: "EUR".into(), home_country: Some("DE".into()), ..Default::default() };
        finance_engine::profile::put(&c, &p).unwrap();
        // the pairs: the data's GBP plus USD, quoted in EUR; never EUR itself
        let pairs = finance_engine::views::fx::fx_pairs(&c);
        assert_eq!(pairs, vec![("GBP".to_string(), "EUR".to_string()), ("USD".to_string(), "EUR".to_string())]);
    }
    let (code, _) = auth::new_pair_code(1).unwrap();
    let (token, _) = auth::pair(&code, "eur test").unwrap();
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
        let mut r = if m == "DELETE" {
            let mut b = agent.delete(&url);
            if auth {
                b = b.header("Authorization", &bearer);
            }
            b.call().unwrap()
        } else {
            let b = match m {
                "POST" => agent.post(&url),
                _ => agent.put(&url),
            };
            let b = b.header("Content-Type", "application/json");
            let b = if auth { b.header("Authorization", &bearer) } else { b };
            b.send(&bytes[..]).unwrap()
        };
        (r.status().as_u16(), serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap_or(Value::Null))
    };

    // the bundle is in euros
    let b = get("/api/export.json");
    assert_eq!(b["profile"]["base_currency"], "EUR");
    assert_eq!(b["fx"]["base"], "EUR");
    assert_eq!(b["fx"]["GBP"], json!(1.17));
    assert_eq!(b["fx"]["USD"], json!(0.92));
    assert!(b["fx"].get("EUR").is_none(), "the base has no rate");
    assert!(b["fx"].get("HKD").is_none(), "nothing in the data is in HKD");
    let nw = &b["net_worth"];
    let (eur_balance, gbp_balance) = (9.0 * (3000.0 - 80.0 - 200.0), 500.0 - 9.0 * 20.0);
    let banks = nw["banks_hkd"].as_f64().unwrap();
    assert!((banks - (eur_balance + gbp_balance * 1.17)).abs() < 0.01, "banks {banks}");
    assert_eq!(nw["total_eur"], nw["total_hkd"], "the second figure is the base itself");
    let n26 = nw["by_source"].as_array().unwrap().iter().find(|s| s["currency"] == "EUR").unwrap();
    assert_eq!(n26["native_amount"].as_f64(), n26["hkd_value"].as_f64(), "a EUR account needs no conversion");
    assert!(nw["by_source"].as_array().unwrap().iter().all(|s| s["no_fx"] != true), "every account has a rate");

    // the investment setup: read, edited by an admin device, data_rev moves
    let s = get("/api/investments/setup");
    assert_eq!(s["benchmark"], "^GSPC");
    assert_eq!((s["accounts"].as_array().unwrap().len(), s["instruments"].as_array().unwrap().len()), (0, 0), "a new tree: none of the original install's");
    let rev0 = b["data_rev"].as_i64().unwrap();
    assert_eq!(send("PUT", "/api/admin/investments/instruments/CSPX", json!({"kind": "fund", "ter_pct": 0.07}), false).0, 401, "an admin device only");
    let (st, v) = send("PUT", "/api/admin/investments/instruments/CSPX", json!({"kind": "fund", "ter_pct": 0.07}), true);
    assert_eq!(st, 200, "{v}");
    assert!(v["instruments"].as_array().unwrap().iter().any(|i| i["symbol"] == "CSPX" && i["ter_pct"] == json!(0.07)));
    assert_eq!(v["data_rev"].as_i64().unwrap(), rev0 + 1);
    let (st, v) = send("POST", "/api/admin/investments/accounts", json!({"name": "Fund plan", "matches": ["nothing"]}), true);
    assert_eq!(st, 200, "{v}");
    let (st, v) = send("PUT", "/api/admin/investments/accounts/Fund%20plan",
        json!({"name": "Index fund plan", "matches": ["sparplan"], "valued_by": "proxy", "proxy": "URTH", "proxy_label": "MSCI World",
               "fund_ocf_pct": 0.2, "platform_flat_month": 0, "platform_pct_above": 0.25, "platform_cliff": 0}), true);
    assert_eq!(st, 200, "{v}");
    let (st, _) = send("POST", "/api/admin/investments/accounts", json!({"name": "Index fund plan"}), true);
    assert_eq!(st, 400, "taken");
    let (st, _) = send("PUT", "/api/admin/investments/instruments/NOT%20A%20SYMBOL", json!({"kind": "fund"}), true);
    assert_eq!(st, 400);
    // the plan's top-ups now file to it (at cost: no proxy prices offline)
    let b = get("/api/export.json");
    let inv = b["net_worth"]["investments_holdings"].as_array().unwrap();
    let plan = inv.iter().find(|h| h["holding"] == "Index fund plan").expect("the renamed account holds the top-ups");
    assert!((plan["cost"].as_f64().unwrap() - 1800.0).abs() < 0.01);
    let costs = &b["costs"];
    assert_eq!(costs["flexinvest"]["cliff_hkd"], json!(0), "the platform block follows the account");
    assert_eq!(send("DELETE", "/api/admin/investments/instruments/CSPX", Value::Null, true).0, 200);
    assert_eq!(send("DELETE", "/api/admin/investments/accounts/Nope", Value::Null, true).0, 400);
    let _ = std::fs::remove_dir_all(&home);
}
