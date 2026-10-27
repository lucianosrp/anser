//! Print the BTC DCA summary from the local replica (a check for dca.rs):
//!   cargo run -p finance-core --example dca
use finance_core::{cache, dca, Ledger};

fn main() -> anyhow::Result<()> {
    finance_core::replica::unlock_for_tools()?;
    let ledger = Ledger::load(&cache::db_path())?;
    for scope in [dca::Scope::Plan, dca::Scope::All] {
        println!("== {}", scope.label());
        let Some(d) = dca::analyze(&ledger, None, scope) else { println!("no BTC buys or candles"); return Ok(()) };
        let (active, span) = d.months_active();
        println!("{} buys {} → {}, ${:.0} in, {:.6} BTC bought, {:.6} rewards, {:.6} sold for ${:.0}",
            d.buys.len(), d.buys[0].date, d.buys[d.buys.len() - 1].date, d.bought, d.bought_qty, d.rewards_qty, d.sold_qty, d.sold);
        println!("avg cost ${:.0}  mean close ${:.0}  edge {:+.1}%  price ${:.0}", d.avg_cost(), d.mean_close, d.edge_pct(), d.price);
        println!("value ${:.0}  gain ${:.0} ({:+.1}%)  vs lump {:+.1}%  buys in profit {}/{}  weeks in profit {:.0}%  worst {:.1}%  months {active}/{span}",
            d.value(), d.gain(), d.gain_pct(), d.vs_lump_pct(), d.buys_in_profit(), d.buys.len(), d.points_in_profit_pct(), d.worst_drawdown_pct());
        for (venue, kind) in [("binance", "auto-invest"), ("okx", "spot-buy")] {
            let v: Vec<_> = d.buys.iter().filter(|b| b.venue == venue && b.kind == kind).collect();
            println!("  {venue} {kind}: {} buys ${:.0}", v.len(), v.iter().map(|b| b.cost).sum::<f64>());
        }
    }
    Ok(())
}
