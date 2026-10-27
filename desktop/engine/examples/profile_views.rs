//! Where the uncached bundle time goes: every export section timed on a
//! fresh Ctx, in bundle order, then the whole bundle.
//!
//!   FINANCE_DB=/path/to/copy.db FINANCE_OFFLINE=1 FINANCE_TODAY=2026-10-03 \
//!     cargo run -p finance-engine --release --example profile_views [ROUNDS]

use std::time::Instant;

use finance_engine::views::{api, crypto, prices, Ctx};
use finance_engine::{db, export};

fn main() {
    let conn = db::open().expect("open db");
    if std::env::args().nth(1).as_deref() == Some("loop") {
        // for a sampling profiler: the uncached bundle, over and over
        // (PROF_SIGNAL=1: SIGUSR1 to self every 20ms, for gdb to sample on)
        if std::env::var_os("PROF_SIGNAL").is_some() {
            let pid = std::process::id().to_string();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let _ = std::process::Command::new("kill").args(["-USR1", &pid]).status();
            });
        }
        let t = Instant::now();
        let mut n = 0;
        // PROF_VIEWS=1: only the views, over inputs (bank rows, ledgers,
        // sheet, ...) already shared from the first round
        let views_only = std::env::var("PROF_VIEWS").is_ok_and(|v| !v.is_empty());
        while t.elapsed().as_secs() < 30 {
            if views_only {
                let ctx = Ctx::offline(&conn);
                let _ = api::get_overview(&ctx, None, None, false);
                let _ = api::get_banks(&ctx, None, None);
                let _ = api::get_spending(&ctx, None, None);
                let inv = api::get_investments(&ctx, true).ok();
                let _ = api::get_costs(&ctx, true, inv.as_ref());
                let _ = api::get_flexinvest(&ctx, None, None);
                let _ = api::get_ibkr(&ctx);
                let _ = api::get_month_range(&ctx);
                let _ = api::get_receivables(&ctx);
            } else {
                finance_engine::views::cache::clear();
                let ctx = Ctx::offline(&conn);
                let _ = api::get_net_worth_now(&ctx, true);
                let _ = export::bundle(&ctx, None);
            }
            n += 1;
        }
        println!("{n} rounds, {:.2} ms each", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
        return;
    }
    let rounds: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(20);
    let mut rows: Vec<(&str, Vec<f64>)> = Vec::new();
    let mut add = |name: &'static str, ms: f64| match rows.iter_mut().find(|(n, _)| *n == name) {
        Some((_, v)) => v.push(ms),
        None => rows.push((name, vec![ms])),
    };
    for _ in 0..rounds {
        finance_engine::views::cache::clear();
        let ctx = Ctx::offline(&conn);
        macro_rules! time {
            ($name:expr, $e:expr) => {{
                let t = Instant::now();
                let v = $e;
                add($name, t.elapsed().as_secs_f64() * 1000.0);
                v
            }};
        }
        time!("db::open", db::open().ok());
        time!("run_ingest", finance_engine::ingest::run_ingest(&conn).ok());
        time!("fingerprint", ctx.fingerprint().is_some());
        time!("ctx.bank", ctx.bank());
        time!("ctx.export", ctx.export());
        time!("ctx.rates", ctx.rates());
        time!("ctx.ledgers", ctx.ledgers());
        time!("ctx.full_sheet", ctx.full_sheet().len());
        time!("net_worth(offline)", ctx.net_worth_offline().clone());
        time!("overview", api::get_overview(&ctx, None, None, false).ok());
        time!("banks", api::get_banks(&ctx, None, None).ok());
        time!("spending", api::get_spending(&ctx, None, None).ok());
        let inv = time!("investments", api::get_investments(&ctx, true).ok());
        time!("costs", api::get_costs(&ctx, true, inv.as_ref()).ok());
        time!("flexinvest", api::get_flexinvest(&ctx, None, None).ok());
        time!("ibkr", api::get_ibkr(&ctx).ok());
        time!("month_range", api::get_month_range(&ctx).ok());
        time!("receivables", api::get_receivables(&ctx).ok());
        time!("net_worth(live, fresh ctx)", api::get_net_worth_now(&Ctx::offline(&conn), true).ok());
        let (px, _, _) = time!("crypto prices", prices::live_or_cached_prices(&ctx, &crypto::ASSETS));
        time!("crypto compute", crypto::compute(&ctx.export(), &px, &ctx.ym_now()).is_some());
        let b = time!("bundle (warm caches)", export::bundle(&Ctx::offline(&conn), None));
        finance_engine::views::cache::clear();
        let c2 = db::open().unwrap();
        time!("bundle (cold caches)", export::bundle(&Ctx::offline(&c2), None));
        time!("serialize bundle", serde_json::to_vec(&b).map(|v| v.len()).ok());
    }
    for (name, mut v) in rows {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("{name:<22} median {:>8.3} ms  min {:>8.3}", v[v.len() / 2], v[0]);
    }
}
