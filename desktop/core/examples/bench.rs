//! cargo run -p finance-core --example bench --release -- [query...]
//! Sync from $FINANCE_SERVER (or the configured one), load, time a few searches.
use std::time::Instant;

use finance_core::{cache, search::Searcher, sync, Bundle, Ledger};

fn main() -> anyhow::Result<()> {
    finance_core::replica::unlock_for_tools()?;
    let r = sync::pull()?;
    println!("sync {} : bundle {} KB, db {} KB in {:?}", r.server, r.bundle_bytes / 1024, r.db_bytes / 1024, r.elapsed);
    let t = Instant::now();
    let ledger = Ledger::load(&cache::db_path())?;
    let bundle = Bundle::load(&cache::bundle_path())?;
    println!("load {} entries + bundle in {:?}; net worth {:?}", ledger.entries.len(), t.elapsed(),
             bundle.f64(&["net_worth", "total_hkd"]));
    let mut s = Searcher::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let queries = if args.is_empty() {
        vec!["coffee".into(), "gym".into(), "cat:dining 2026-09".into(), "src:card >1000".into(),
             "12345".into(), "btc".into(), "vwra".into(), "ktchn".into()]
    } else { vec![args.join(" ")] };
    for q in queries {
        let t = Instant::now();
        let hits = s.search(&ledger, &q, 50);
        let dt = t.elapsed();
        let top = hits.first().map(|&i| &ledger.entries[i]);
        println!("{q:>22}: {:>4} hits in {dt:>10?}  top: {}", hits.len(),
                 top.map(|e| format!("{} {} {:.2} {} [{}]", e.date, e.description, e.amount, e.currency, e.category)).unwrap_or_default());
    }
    Ok(())
}
