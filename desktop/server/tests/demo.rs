//! "Just try it" end to end: the server hands the made-up household to a
//! browser nobody signed in (`/api/demo/*`) while the real data stays
//! behind the sign-in, and the client-side pages find something to show
//! in it: trips on the map, subscriptions, a DCA plan, budgets, broker
//! trades with their prices.

use std::collections::BTreeMap;
use std::time::Duration;

use finance_core::{budget, dca, recurring, travel, Bundle, Ledger};
use finance_server::{auth, server};

#[test]
fn the_demo_is_public_and_every_page_has_something() {
    let home = std::env::temp_dir().join(format!("finance-demo-routes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("data")).unwrap();
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_DATA_DIR", home.join("data"));
    std::env::set_var("FINANCE_DB", home.join("data/finance.db"));
    std::env::set_var("FINANCE_TRANSACTIONS_DIR", home.join("transactions"));
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::set_var("FINANCE_TODAY", "2026-09-20");
    std::env::set_var("FINANCE_AUTH", "all");
    auth::bootstrap().unwrap();
    let srv = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
    std::thread::spawn(move || server::run(srv));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .into();
    let get = |p: &str| {
        let mut r = agent.get(&format!("{base}{p}")).call().unwrap();
        (r.status().as_u16(), r.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap())
    };

    // the real data wants a sign-in; the demo doesn't
    assert_eq!(get("/api/export.json").0, 401);
    assert_eq!(get("/api/ledger.json").0, 401);
    let (s, b) = get("/api/demo/export.json");
    assert_eq!(s, 200);
    let bundle: serde_json::Value = serde_json::from_slice(&b).unwrap();
    assert_eq!((bundle["demo"].as_bool(), bundle["profile"]["name"].as_str()), (Some(true), Some(finance_engine::demo::NAME)));
    let (s, l) = get("/api/demo/ledger.json");
    assert_eq!(s, 200);
    // built once a day: the second visitor gets the same bytes
    assert_eq!(get("/api/demo/export.json").1, b);
    // nothing of it landed in the server's tree
    assert!(!home.join("data/finance.db").exists() || {
        let c = rusqlite::Connection::open(home.join("data/finance.db")).unwrap();
        c.query_row("SELECT COUNT(*) FROM bank_transactions", [], |r| r.get::<_, i64>(0)).unwrap_or(0) == 0
    });

    let ledger = Ledger::from_json(&l).unwrap();
    let bundle = Bundle(bundle);
    // Travel: a trip per country, with the cities on its route
    let trips = travel::discover(&ledger);
    let places: Vec<String> = trips.iter().flat_map(|t| t.places.clone()).collect();
    assert!(trips.len() >= 4, "{places:?}");
    for want in ["Japan", "United Kingdom", "United States", "Thailand"] {
        assert!(places.iter().any(|p| p.contains(want)), "{want} in {places:?}");
    }
    // Subscriptions: the monthly ones, and the forgotten one
    let series = recurring::detect(&ledger);
    let names: Vec<&str> = series.iter().filter(|s| s.active).map(|s| s.name.as_str()).collect();
    for want in ["STREAMFLIX", "TUNEWAVE", "MEDITATE", "APARTMENT RENT", "SALARY"] {
        assert!(names.iter().any(|n| n.to_uppercase().contains(want)), "{want} in {names:?}");
    }
    // Crypto: the weekly plan
    let price = bundle.f64(&["crypto", "price", "BTC"]);
    let plan = dca::analyze(&ledger, price, dca::Scope::Plan).expect("a DCA plan");
    let _ = plan;
    assert!(ledger.bars.get(dca::BTC_WEEKLY).is_some_and(|b| b.len() > 80));
    // Broker: trades and their daily bars
    assert!(ledger.trades.len() > 40 && ledger.income.len() >= 5);
    for sym in ["VWCE", "VHYL", "SXR8"] {
        assert!(ledger.bars.get(sym).is_some_and(|b| b.len() > 400), "{sym} bars");
    }
    // Budgets: this month against the budgets the bundle carries
    let budgets: BTreeMap<String, f64> =
        serde_json::from_value(bundle.at(&["user_settings", "budgets", "monthly"]).clone()).unwrap();
    let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
    let st = budget::month_status(&ledger, &budgets, today);
    let _ = st;
    // categories came along, none of the rows left as Other
    assert!(ledger.categories.len() >= 15);
    assert!(ledger.entries.iter().all(|e| e.category != "Other"));
    let _ = std::fs::remove_dir_all(&home);
}
