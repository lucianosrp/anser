//! Print the trips discovered in the local replica (a check for travel.rs):
//!   cargo run -p finance-core --example trips [-- -v] [-- -r]   (-v rows, -r routes)
use finance_core::{cache, travel, Ledger};

fn main() -> anyhow::Result<()> {
    finance_core::replica::unlock_for_tools()?;
    let verbose = std::env::args().any(|a| a == "-v");
    let routes = std::env::args().any(|a| a == "-r");
    let ledger = Ledger::load(&cache::db_path())?;
    let trips = travel::discover(&ledger);
    let atlas = travel::atlas(&ledger, &trips);
    for (t, r) in trips.iter().zip(&atlas.routes) {
        println!("{} → {} ({:>2} d)  {:<28} away {:>9.0}  booked {:>8.0}  rows {}+{}",
            t.start, t.end, t.days(), t.label(), t.away_hkd, t.booked_hkd, t.away_rows.len(), t.booked_rows.len());
        if routes {
            println!("    home {} · countries {:?}", r.home.name, r.country_days);
            for s in &r.stops {
                println!("    stop {} → {}  {:<16} {:<2} {:>8.0}  rows {}{}", s.first, s.last, s.place.name, s.place.cc, s.hkd, s.rows.len(),
                    if s.inferred { "  (inferred)" } else { "" });
            }
            for l in &r.legs {
                println!("    leg  {} → {}  {:?}{}", l.from.name, l.to.name, l.mode, if l.inferred { " (inferred)" } else { "" });
            }
            for x in &r.transit {
                let e = &ledger.entries[x.row];
                println!("    {:<8} {} {:>8.0}  {}", format!("{:?}", x.mode), e.date, x.hkd, e.description);
            }
            println!("    by mode {:?}", r.by_mode);
        }
        if verbose {
            for (tag, rows) in [("away", &t.away_rows), ("book", &t.booked_rows)] {
                for &r in rows.iter() {
                    let e = &ledger.entries[r];
                    println!("      {tag} {} {:<12} {:<4} {:>9.2} {:<16} {}", e.date, e.source, e.currency, e.amount, e.category, e.description);
                }
            }
        }
    }
    Ok(())
}
