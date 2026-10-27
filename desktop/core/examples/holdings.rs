//! cargo run -p finance-core --example holdings -- [db]
//! Per-holding broker maths from the local replica (or a given db).
fn main() -> anyhow::Result<()> {
    finance_core::replica::unlock_for_tools()?;
    let db = std::env::args().nth(1).map(std::path::PathBuf::from).unwrap_or_else(finance_core::cache::db_path);
    let ledger = finance_core::Ledger::load(&db)?;
    for h in finance_core::broker::holdings(&ledger) {
        println!("{:6} {:>9.4} {} avg {:>9.2}  cost {:>9.2} {} = HK$ {:>9.0}  fees HK$ {:>5.1}  realized {:>6.0}  buys {} sells {}  {}..{}",
            h.symbol, h.qty, "", h.avg_cost, h.cost_ccy, h.currency, h.cost_hkd, h.fees_hkd, h.realized_hkd, h.buys, h.sells, h.first_buy, h.last_trade);
    }
    Ok(())
}
