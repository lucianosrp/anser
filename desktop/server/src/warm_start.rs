//! Single-flight warm-up: two /api/export.json requests that arrive while
//! the views are warming compute them once, a request for tenant B does not
//! wait on tenant A's pass, a request that waited matches an unwarmed one
//! (wall-clock fields aside), a panicking warm-up does not stall requests,
//! and waiters leave after the pass they joined (not a later rerun).

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use finance_engine::export;
use finance_engine::tenant::Tenant;
use serde_json::Value;

use super::*;
use crate::auth;

static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn tree(name: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("finance-warm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("transactions")).unwrap();
    std::env::set_var("FINANCE_HOME", &home);
    std::env::set_var("FINANCE_OFFLINE", "1");
    std::env::set_var("FINANCE_TODAY", "2026-09-20");
    for k in ["FINANCE_DATA_DIR", "FINANCE_DB", "FINANCE_TRANSACTIONS_DIR", "FINANCE_FORMATS_DIR", "FINANCE_AUTH", "FINANCE_ALLOW_SIGNUP"] {
        std::env::remove_var(k);
    }
    let conn = finance_engine::db::connect(&home.join("data/finance.db")).unwrap();
    finance_engine::categories::ensure(&conn).unwrap();
    finance_engine::categorize::ensure_rules(&conn).unwrap();
    finance_engine::invest_setup::ensure(&conn).unwrap();
    drop(conn);
    auth::bootstrap().unwrap();
    auth::set_mode(auth::Mode::Writes).unwrap(); // these tests read without a device
    home
}

#[derive(Clone)]
struct Client {
    base: String,
    agent: ureq::Agent,
}

impl Client {
    fn start() -> Self {
        let srv = bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", srv.server_addr().to_ip().unwrap());
        std::thread::spawn(move || run(srv));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        Client { base, agent }
    }

    fn get(&self, path: &str, token: Option<&str>) -> (u16, Vec<u8>) {
        let mut req = self.agent.get(&format!("{}{path}", self.base));
        if let Some(t) = token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        let mut r = req.call().unwrap();
        (r.status().as_u16(), r.body_mut().read_to_vec().unwrap())
    }

    fn pair(&self, uid: i64, name: &str) -> String {
        let (code, _) = auth::new_pair_code(uid).unwrap();
        let mut r = self
            .agent
            .post(&format!("{}/api/pair", self.base))
            .header("Content-Type", "application/json")
            .send(format!(r#"{{"code":"{code}","device_name":"{name}"}}"#))
            .unwrap();
        assert_eq!(r.status().as_u16(), 200);
        let v: Value = serde_json::from_slice(&r.body_mut().read_to_vec().unwrap()).unwrap();
        v["token"].as_str().unwrap().to_string()
    }
}

fn wait_until_warming() {
    let t = Instant::now();
    while !warming() {
        assert!(t.elapsed() < Duration::from_secs(2), "warm-up never started");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn untimed(mut v: Value) -> Value {
    v["generated_at"] = Value::Null;
    for path in ["/net_worth", "/overview/net_worth"] {
        if let Some(o) = v.pointer_mut(path).and_then(Value::as_object_mut) {
            o.insert("as_of".into(), Value::Null);
        }
    }
    v
}

#[test]
fn two_requests_during_warm_up_compute_once() {
    let _g = lock();
    tree("once");
    let c = Client::start();
    let before = export::tail_builds();
    let hold = hold_next_warm();
    Tenant::owner().enter(|| {
        warm_views_async();
        wait_until_warming();
    });
    std::thread::scope(|s| {
        let h1 = s.spawn(|| c.get("/api/export.json", None));
        let h2 = s.spawn(|| c.get("/api/export.json", None));
        std::thread::sleep(Duration::from_millis(50));
        drop(hold);
        let (s1, b1) = h1.join().unwrap();
        let (s2, b2) = h2.join().unwrap();
        assert_eq!(s1, 200);
        assert_eq!(s2, 200);
        assert!(serde_json::from_slice::<Value>(&b1).unwrap()["version"].is_number());
        assert!(serde_json::from_slice::<Value>(&b2).unwrap()["version"].is_number());
    });
    assert_eq!(export::tail_builds() - before, 1, "warm-up plus two requests: one compute");
    assert!(last_wait_ms() >= 40, "the waited log line: {} ms", last_wait_ms());
}

#[test]
fn other_tenant_does_not_wait() {
    let _g = lock();
    tree("tenant");
    let sam_id = auth::add_user("Sam", Some("sam@example.com"), false).unwrap();
    let c = Client::start();
    let _owner = c.pair(1, "owner");
    let sam = c.pair(sam_id, "sam");
    let hold = hold_next_warm();
    Tenant::owner().enter(|| {
        warm_views_async();
        wait_until_warming();
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tx.send(c.get("/api/export.json", Some(&sam))).unwrap();
    });
    let (status, body) = rx.recv_timeout(Duration::from_secs(3)).expect("tenant B waited on tenant A's warm-up");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    drop(hold);
}

#[test]
fn waiting_request_matches_unwarmed() {
    let _g = lock();
    tree("bytes");
    let c = Client::start();
    let (s, unwarmed) = c.get("/api/export.json", None);
    assert_eq!(s, 200);
    finance_engine::views::cache::clear();
    export::clear_tail();
    let hold = hold_next_warm();
    Tenant::owner().enter(|| {
        warm_views_async();
        wait_until_warming();
    });
    let h = std::thread::spawn({
        let c = c.clone();
        move || c.get("/api/export.json", None)
    });
    std::thread::sleep(Duration::from_millis(50));
    drop(hold);
    let (s, waited) = h.join().unwrap();
    assert_eq!(s, 200);
    let u: Value = serde_json::from_slice(&unwarmed).unwrap();
    let w: Value = serde_json::from_slice(&waited).unwrap();
    assert_eq!(untimed(w), untimed(u));
}

#[test]
fn panicking_warm_up_does_not_stall_requests() {
    let _g = lock();
    tree("panic");
    let c = Client::start();
    panic_next_warm();
    let hold = hold_next_warm();
    Tenant::owner().enter(|| {
        warm_views_async();
        wait_until_warming();
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let c2 = c.clone();
    std::thread::spawn(move || {
        tx.send(c2.get("/api/export.json", None)).unwrap();
    });
    std::thread::sleep(Duration::from_millis(50));
    drop(hold);
    let (status, body) = rx.recv_timeout(Duration::from_secs(3)).expect("request stalled after a panicking warm-up");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
}

#[test]
fn waiter_leaves_after_the_pass_it_joined() {
    let _g = lock();
    tree("pass");
    let c = Client::start();
    let hold1 = hold_next_warm();
    Tenant::owner().enter(|| {
        warm_views_async();
        wait_until_warming();
        warm_views_async(); // rerun after this pass
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let c2 = c.clone();
    std::thread::spawn(move || {
        tx.send(c2.get("/api/export.json", None)).unwrap();
    });
    std::thread::sleep(Duration::from_millis(50));
    let hold2 = hold_next_warm();
    drop(hold1);
    let (status, body) = rx.recv_timeout(Duration::from_secs(3)).expect("waiter waited for the rerun too");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    drop(hold2);
}
