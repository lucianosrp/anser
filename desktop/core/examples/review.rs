//! Print a year in review and the budget suggestions from the local replica
//! (a check for review.rs and budget.rs):
//!   cargo run -p finance-core --example review [-- 2025]
use finance_core::{budget, cache, review, travel, Bundle, Ledger};

fn main() -> anyhow::Result<()> {
    finance_core::replica::unlock_for_tools()?;
    let ledger = Ledger::load(&cache::db_path())?;
    let bundle = Bundle::load(&cache::bundle_path())?;
    let today = chrono::Local::now().date_naive();
    let year = std::env::args().nth(1).and_then(|y| y.parse().ok()).or(review::default_year(&bundle, today)).unwrap_or(2025);
    let trips = travel::discover(&ledger);
    let r = review::build(&bundle, &ledger, &trips, year, today);
    println!("{year}{}: income {:.0}  spending {:.0} (prev {:?})  saved {:.0}  rate {:?}",
        if r.ytd { " to date" } else { "" }, r.income, r.spending, r.prev_spending, r.saved(), r.savings_rate());
    println!("net worth {:?}", r.net_worth_change());
    for c in &r.categories {
        println!("  {:<24} {:>9.0}  prev {:?}", c.category, c.hkd, c.prev);
    }
    for m in &r.merchants {
        println!("  {:<24} {:>9.0}  x{}", m.name, m.hkd, m.count);
    }
    for &i in &r.biggest {
        let e = &ledger.entries[i];
        println!("  big {} {:>9.0} {}", e.date, e.hkd.unwrap_or(0.) * e.share, e.description);
    }
    println!("priciest {:?}\ncheapest {:?}", r.priciest_month(), r.cheapest_month());
    println!("trips {} ({} d) {:.0}", r.trips.len(), r.trip_days, r.travel_hkd);
    println!("funds {:.0} broker {:.0} crypto {:.0}  btc {:?}", r.funds_hkd, r.broker_hkd, r.crypto_hkd, r.btc);
    println!("coffee {} {:.0}  txns {} spending rows {}  most visited {:?}  no-spend {:?}",
        r.coffee_count, r.coffee_hkd, r.txn_count, r.spending_count, r.most_visited, r.no_spend);

    let by = budget::by_month(&ledger);
    let month = today.format("%Y-%m").to_string();
    println!("\nsuggested budgets for {month}:");
    let s = budget::suggest(&by, &month);
    for (c, v) in &s {
        println!("  {c:<24} {v:>7.0}");
    }
    let st = budget::month_status(&ledger, &s, today);
    for l in &st.lines {
        println!("  {:<24} spent {:>7.0} projected {:>7.0} {:?}", l.category, l.spent, l.projected, l.status());
    }
    Ok(())
}
