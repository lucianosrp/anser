//! The Investments page's numbers from the local replica (a check for
//! portfolio.rs and jkb.rs):
//!   cargo run -p finance-core --example portfolio [-- 2026-10-03]
use finance_core::portfolio::{Account, Portfolio};
use finance_core::{cache, invest, jkb, Bundle, Ledger};

fn opt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:+.2}%")).unwrap_or("—".into())
}

fn main() -> anyhow::Result<()> {
    finance_core::replica::unlock_for_tools()?;
    let today = std::env::args().nth(1).unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
    let ledger = Ledger::load(&cache::db_path())?;
    let b = Bundle::load(&cache::bundle_path())?;
    let x = invest::Extra::load(&cache::db_path())?.with_ledger(&ledger, &b);
    let p = Portfolio::build(&b, &ledger, &x, &today);

    println!("== as of {today}: value HK$ {:.0}, in HK$ {:.0}, gain HK$ {:.0} ({:+.2}%), IRR {:?} (S&P {:?})",
        p.value(), p.invested(), p.value() - p.invested(), (p.value() / p.invested() - 1.) * 100.,
        b.f64(&["investments", "totals", "irr_pct"]), b.f64(&["investments", "spx", "irr_pct"]));
    for (n, r) in [("you", Some(p.twr)), ("S&P", Some(p.spx)), ("MSCI World (URTH)", p.msci)] {
        if let Some(r) = r {
            println!("  TWR {n:18} month {}  ytd {}  since {}", opt(r.month), opt(r.ytd), opt(r.since));
        }
    }

    println!("\n== accounts");
    for a in &p.accounts {
        println!("{:10} value {:>9.0} in {:>9.0} gain {:>+8.0} ({})  ytd {:?}  spark {} pts",
            a.account.label(), a.value, a.invested, a.gain(), opt(a.gain_pct()), a.gain_ytd.map(|v| v.round()), a.spark.len());
    }

    println!("\n== holdings");
    for h in &p.holdings {
        println!("{:8} {:10} value {:>9.0} cost {:>9.0} gain {:>+7.0} ({})  1w {} 1m {} ytd {}  ytd gain {:?}  first {:?} {:?}",
            h.symbol, h.account.map(|a| a.label()).unwrap_or(""), h.value_hkd, h.cost_hkd, h.gain_hkd(), opt(h.gain_pct()),
            opt(h.ret_1w), opt(h.ret_1m), opt(h.ret_ytd), h.gain_ytd_hkd.map(|v| v.round()), h.first_buy, h.proxy);
    }

    println!("\n== money in, last 12 months (Flex / IBKR / Crypto)");
    for m in p.months_in.iter().rev().take(12).rev() {
        println!("{}  {:>8.0} {:>8.0} {:>8.0}  = {:>8.0}", m.month, m.by[0], m.by[1], m.by[2], m.total());
    }
    for (a, months) in &p.buy_months {
        let s = jkb::streak(months, &today[..7]);
        println!("{:10} buys in {}/{} months, current streak {}, longest {}, longest gap {:?}, missed {}",
            a.label(), s.active, s.span, s.current, s.longest, s.longest_gap, s.missed.len());
    }
    let all: Vec<String> = p.buy_months.values().flatten().cloned().collect();
    let s = jkb::streak(&all, &today[..7]);
    println!("overall    buys in {}/{} months, current {}, longest {}, gap {:?}", s.active, s.span, s.current, s.longest, s.longest_gap);
    if let Some(z) = p.btc_pause(&ledger) {
        println!("BTC pause {}..{} ({} mo): {:.2}/mo median -> US$ {:.0} in, {:.6} BTC, worth US$ {:.0} now: {:+.0}",
            z.from, z.to, z.months, z.monthly_usd, z.money_usd, z.units, z.value_usd, z.cost_usd());
    }

    println!("\n== crossover (pace from the last 6 months' trend)");
    let done = b.completed_trend();
    let last6: Vec<_> = done.iter().rev().take(6).collect();
    let pace = last6.iter().map(|m| m.get("invested").and_then(|v| v.as_f64()).unwrap_or(0.) + m.get("invested_crypto").and_then(|v| v.as_f64()).unwrap_or(0.)).sum::<f64>() / last6.len().max(1) as f64;
    for rate in [4., 6., 8.] {
        let c = jkb::crossover(p.value(), pace, rate, 1200);
        println!("{rate}%: pace HK$ {pace:.0}/mo, expected gain HK$ {:.0}/mo ({:.1}% of it), crosses in {:?} months at HK$ {:.0}", c.gain_now, c.ratio * 100., c.months, c.value_then);
    }

    println!("\n== buy the dip vs DCA");
    for c in p.dip_cases(&ledger) {
        println!("{:5} {} {} buys from {}: in {:.2} {}; DCA {:.2}, dip buyer {:.2} ({} dips, {:.2} cash) -> DCA ahead {:+.2}%",
            c.symbol, c.period, c.buys, c.first, c.r.money_in, c.currency, c.r.dca_value(), c.r.dip_value(), c.r.dips.len(), c.r.dip_cash, c.r.dca_ahead_pct());
    }

    println!("\n== best periods");
    let d = p.broker_daily_pnl();
    let bs = jkb::best_share(&d.iter().map(|x| x.1).collect::<Vec<_>>(), 5);
    println!("broker: {} days, market HK$ {:.0}, best 5 HK$ {:.0}, without them HK$ {:.0}", bs.periods, bs.total, bs.best, bs.without());
    let w = p.btc_weekly_pnl();
    let bs = jkb::best_share(&w.iter().map(|x| x.1).collect::<Vec<_>>(), 5);
    println!("BTC:    {} weeks, market HK$ {:.0}, best 5 HK$ {:.0}, without them HK$ {:.0}", bs.periods, bs.total, bs.best, bs.without());

    println!("\n== income");
    for r in &p.rewards {
        println!("{:5} {} rewards, {:.8} units, US$ {:.2} (this year {:.2}), last {}", r.asset, r.count, r.qty, r.usd, r.usd_ytd, r.last);
    }
    println!("broker: {:?}", p.broker_income);
    let _ = Account::ALL;
    Ok(())
}
