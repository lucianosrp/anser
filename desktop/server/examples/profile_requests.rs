//! Request-path timings without HTTP: the export bundle and the ledger the
//! web app reads, cold (view cache cleared) and warm.
//!
//!   FINANCE_DB=/path/to/copy.db FINANCE_TRANSACTIONS_DIR=/empty/dir FINANCE_OFFLINE=1 \
//!     FINANCE_TODAY=2026-10-03 cargo run -p finance-server --release --example profile_requests [ROUNDS]

use std::time::Instant;

use finance_engine::views::{cache, Ctx};
use finance_engine::{db, export};
use finance_server::feed::REQUEST_FEED;
use finance_server::{live, server, webapp};

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn time(rounds: usize, cold: bool, mut f: impl FnMut()) -> f64 {
    median(
        (0..rounds)
            .map(|_| {
                if cold {
                    cache::clear();
                }
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect(),
    )
}

fn main() {
    let rounds: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(20);
    for cold in [true, false] {
        let label = if cold { "cold" } else { "warm" };
        println!("{label:<5} db::open        {:>8.3} ms", time(rounds, cold, || drop(db::open().unwrap())));
        println!("{label:<5} auto_ingest     {:>8.3} ms", time(rounds, cold, || drop(server::auto_ingest())));
        println!("{label:<5} live::net_worth {:>8.3} ms", time(rounds, cold, || drop(live::net_worth())));
        println!(
            "{label:<5} bundle_bytes    {:>8.3} ms",
            time(rounds, cold, || {
                let conn = db::open().unwrap();
                drop(export::bundle_bytes(&Ctx::new(&conn, &REQUEST_FEED), &serde_json::Value::Null))
            })
        );
        println!("{label:<5} ledger_json     {:>8.3} ms", time(rounds, cold, || drop(webapp::ledger_json().unwrap())));
    }
}
