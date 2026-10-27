//! Print the recurring series found in the local replica (a check for recurring.rs):
//!   cargo run -p finance-core --example recurring [-- -v]   (-v: every row of each series)
use finance_core::recurring::{self, Flow};
use finance_core::{cache, Ledger};

fn main() -> anyhow::Result<()> {
    finance_core::replica::unlock_for_tools()?;
    let verbose = std::env::args().any(|a| a == "-v");
    let ledger = Ledger::load(&cache::db_path())?;
    let t = std::time::Instant::now();
    let series = recurring::detect(&ledger);
    let took = t.elapsed();
    let newest = ledger.entries.iter().map(|e| e.date.as_str()).max().unwrap_or("").to_string();
    let flags = recurring::flags(&series, &newest);
    for (s, f) in series.iter().zip(&flags) {
        let kind = match s.flow {
            Flow::Charge => "out",
            Flow::Income => "in ",
            Flow::CardBill => "bill",
        };
        let change = s.change.as_ref().map(|c| format!("  {:.2}→{:.2} {} on {}", c.from, c.to, s.currency, c.date)).unwrap_or_default();
        println!("{} {kind:<4} {:<34} {:<20} {:<9} {:>9.2} {} ≈ HK${:>8.0}/yr  {} → {}  next {}  {:<22} rows {:>2}{}{change}  {:?}",
            if s.active { "●" } else { "○" }, s.name.chars().take(34).collect::<String>(), s.category, s.cadence.label(),
            s.typical, s.currency, s.yearly_hkd, s.first, s.last, s.next, s.account_label(), s.rows.len(),
            if s.varies { " ~" } else { "" }, f);
        if verbose {
            for &i in &s.rows {
                let e = &ledger.entries[i];
                println!("      {} {:<12} {:>9.2} {:<4} {}", e.date, e.source, e.amount, e.currency, e.description);
            }
        }
    }
    eprintln!("{} series ({} active) in {:.1} ms", series.len(), series.iter().filter(|s| s.active).count(), took.as_secs_f64() * 1000.);
    Ok(())
}
