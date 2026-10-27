//! The investing deep dive's numbers from the local replica (a check for
//! invest.rs, fees.rs, exposure.rs, risk.rs):
//!   cargo run -p finance-core --example invest
use std::collections::HashMap;

use finance_core::{cache, exposure, fees, invest, risk, Bundle, Ledger};

fn main() -> anyhow::Result<()> {
    finance_core::replica::unlock_for_tools()?;
    let ledger = Ledger::load(&cache::db_path())?;
    let b = Bundle::load(&cache::bundle_path())?;
    let x = invest::Extra::load(&cache::db_path())?.with_ledger(&ledger, &b);
    let today = ledger.trades.iter().map(|t| t.date.clone()).max().unwrap_or_default();
    let as_of = b.str(&["ibkr", "snapshot", "as_of"]).unwrap_or(&today).to_string();

    println!("== broker DCA (as of {as_of})");
    let plans = invest::holding_plans(&ledger, &x.fx, &as_of, &HashMap::new());
    for h in &plans {
        let d = &h.dca;
        let (a, s) = d.months_active();
        let ds = d.day_split(&ledger.bars[&h.symbol]);
        println!("{:5} {} buys {:>8.2} {} in (fees {:.2} = {:.2}%)  avg px {:.2} vs mean {:.2} over {} bars: edge {:+.2}% (after fees {:+.2}%)  lump {:+.2}%  months {a}/{s}  up/down {}/{}  value HK$ {:.0} cost HK$ {:.0}",
            h.symbol, d.buys.len(), d.bought, h.currency, d.fees, d.fee_drag_pct(), d.avg_price(), d.mean_close, d.span_bars,
            d.edge_pct(), d.edge_after_fees_pct(), d.vs_lump_pct(), ds.up, ds.down, h.value_hkd(), h.cost_hkd());
        for bb in &d.buys {
            println!("      {} {:.4} @ {:.2} fee {:.2} ({:.2}%)  today {:+.1}%", bb.date, bb.qty, bb.paid_price(), bb.fee, bb.fee_pct(), d.buy_return_pct(bb));
        }
    }
    let p = invest::portfolio(&plans, &ledger);
    println!("portfolio: {} buys, cost HK$ {:.0}, value HK$ {:.0}, gain {:+.0}; fees HK$ {:.1} ({:.2}%); edge {:+.2}% (after fees {:+.2}%); lump HK$ {:.0} ({:+.2}%); months {}/{}; up/down {}/{}",
        p.buys, p.cost_hkd, p.value_hkd, p.gain_hkd(), p.fees_hkd, p.fee_drag_pct(), p.edge_pct, p.edge_after_fees_pct, p.lump_hkd, p.vs_lump_pct(), p.months_active, p.months_span, p.up_days, p.down_days);

    println!("\n== fees (schedules as of {})", fees::SCHEDULES_AS_OF);
    let rows = fees::reprice(&ledger.trades);
    for r in &rows {
        println!("{} {:5} {:>8.2} {}  paid {:.2} ({:.2}%)  fixed {:?}  tiered {:?}", r.date, r.symbol, r.value, r.currency, r.actual, r.actual_pct(),
            r.fixed.map(|f| (format!("{:.2}", f.0), f.1.label())), r.tiered.map(|f| (format!("{:.2}", f.0), f.1.label())));
    }
    let t = fees::Totals::of(&rows);
    println!("total: {} trades, HK$ {:.0} traded, paid HK$ {:.2} ({:.2}%), Fixed model HK$ {:.2}, Tiered HK$ {:.2}, Tiered saves HK$ {:.2}; floor set {} of {}",
        t.trades, t.value_hkd, t.actual_hkd, t.actual_pct(), t.fixed_hkd, t.tiered_hkd, t.tiered_saving_hkd(), t.floor_count, t.trades);
    for (v, c) in [(exposure::Venue::Us, "USD"), (exposure::Venue::Lse, "GBP"), (exposure::Venue::Lse, "USD")] {
        for plan in [fees::Plan::Fixed, fees::Plan::Tiered] {
            let s = fees::schedule(plan, v, c).unwrap();
            println!("  {:?} {c} {:?}: 0.25% from {:?}", v, plan, fees::order_size_for(s, 200., 0.25).map(|x| x.round()));
        }
    }
    let pr = fees::projection(&rows, &as_of, 91);
    println!("projection (last {} days, {} buys): money HK$ {:.0}/yr, paid HK$ {:.0}/yr ({:.2}%), Fixed HK$ {:.0}, Tiered HK$ {:.0}",
        pr.window_days, pr.trades_in_window, pr.money_yr_hkd, pr.actual_yr_hkd, pr.actual_pct(), pr.fixed_yr_hkd, pr.tiered_yr_hkd);

    println!("\n== returns split");
    let days = invest::nav_days(&b);
    let split = invest::broker_split(&ledger, &x.fx, &days);
    for s in split.iter().rev().take(3).rev() {
        println!("{} value {:.0} = in {:.0} + market {:.0} + fx {:.0} - fees {:.0} + other {:.0}", s.date, s.value, s.money_in, s.market, s.fx, s.fees, s.other);
    }
    for m in invest::all_split(&b, &split).iter().rev().take(4).rev() {
        println!("{} all: value {:.0} = in {:.0} + market {:.0} + fx {:.0} - fees {:.0}", m.month, m.value, m.money_in, m.market, m.fx, m.fees);
    }

    println!("\n== exposure (broker)");
    let mut ps: Vec<exposure::Position> = b.arr(&["ibkr", "snapshot", "positions"]).iter().filter_map(|p| {
        let desc = p.get("description").and_then(|d| d.as_str()).unwrap_or("");
        Some(exposure::Position::broker(p.get("symbol")?.as_str()?, desc, p.get("currency")?.as_str()?, p.get("value_base")?.as_f64()?))
    }).collect();
    let cash = b.f64(&["ibkr", "snapshot", "cash_base"]).unwrap_or(0.);
    ps.push(exposure::Position { symbol: "CASH".into(), label: "Cash".into(), description: String::new(), currency: finance_core::base::currency(), value_hkd: cash, kind: exposure::Kind::Cash, venue: exposure::Venue::Account });
    let e = exposure::exposure(&ps);
    println!("currency {:?}\nvenue {:?}\nkind {:?}", e.by_currency, e.by_venue, e.by_kind);
    println!("regions {:?}\nsectors {:?}", e.by_region.iter().map(|r| (r.0.clone(), format!("{:.1}", r.2))).collect::<Vec<_>>(), e.by_sector.iter().map(|r| (r.0.clone(), format!("{:.1}", r.2))).collect::<Vec<_>>());
    println!("held twice {:?}; overlaps {:?}", e.held_twice.iter().map(|h| (h.symbol.clone(), h.direct_hkd.round(), h.in_funds_hkd.round())).collect::<Vec<_>>(), e.overlaps.iter().map(|o| (o.0.clone(), o.1.clone(), o.2.round())).collect::<Vec<_>>());

    println!("\n== risk");
    for h in &plans {
        if let Some(r) = risk::risk(&h.symbol, &ledger.bars[&h.symbol], h.dca.buys.first().map(|b| b.date.as_str())) {
            println!("{:5} {} bars from {}  vol 1y {:?} all {:?}  dd {:.1}% ({}→{})  since buy {:?}  from high {:.1}%",
                r.symbol, r.bars, r.first_bar, r.vol_1y.map(|v| v.round()), r.vol_all.map(|v| v.round()), r.dd_all, r.dd_all_from, r.dd_all_to, r.dd_since_buy.map(|v| (v * 10.).round() / 10.), r.from_high);
        }
    }
    let series: Vec<(String, &[finance_core::Bar])> = plans.iter().map(|h| (h.symbol.clone(), ledger.bars[&h.symbol].as_slice()))
        .chain(std::iter::once(("URTH".to_string(), x.proxy_bars.as_slice()))).collect();
    let m = risk::matrix(&series, 252);
    for (i, s) in m.symbols.iter().enumerate() {
        println!("{s:5} {}", m.m[i].iter().map(|c| c.map(|c| format!("{c:+.2}")).unwrap_or("  —  ".into())).collect::<Vec<_>>().join(" "));
    }

    println!("\n== dividends: {:?} · flow kinds {:?}", invest::income(&x.cash_flows), invest::flow_kinds(&x.cash_flows));
    Ok(())
}
