//! What investing costs, and what idle cash forgoes: fees paid (IBKR
//! commissions, the fund platform's
//! fee), the yearly running cost of today's holdings (fund TER/OCF +
//! platform fee) and cash drag (bank cash x the 3-month T-bill yield).
//!
//! The TERs, the single stocks and the fund platform's fee schedule are
//! data (crate::invest_setup: the instruments and investment_accounts
//! tables). The section keeps its v1 names (`flexinvest`,
//! `flexinvest_platform_hkd`): they mean "the fund platform".

use serde_json::{json, Value};

use super::holdings::ibkr_snapshot;
use super::prices::equity_price_hist;
use super::{get_f64, num, r2, Ctx};

pub const TBILL_SYMBOL: &str = "^IRX"; // 13-week T-bill yield, in percent

/// A fund platform's fee schedule: a flat fee per month up to a balance,
/// a % a year of the whole balance above it.
struct Platform {
    ocf: f64,
    flat_yr: f64,
    cliff: f64,
    pct_above: f64,
}

impl Platform {
    fn of(a: &crate::invest_setup::Account) -> Platform {
        Platform {
            ocf: a.fund_ocf_pct.unwrap_or(0.0),
            flat_yr: a.platform_flat_month.unwrap_or(0.0) * 12.0,
            cliff: a.platform_cliff.unwrap_or(f64::INFINITY),
            pct_above: a.platform_pct_above.unwrap_or(0.0),
        }
    }

    /// The platform fee per year: (value, is the flat fee).
    fn yearly(&self, value_hkd: f64) -> (f64, bool) {
        if value_hkd <= self.cliff {
            (self.flat_yr, true)
        } else {
            (value_hkd * self.pct_above / 100.0, false)
        }
    }
}

/// A whole number as a JSON integer (the bundle carries the constants as ints).
fn int_or_num(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        json!(v as i64)
    } else {
        num(v)
    }
}

/// tbill_yield: {pct, as_of, stale} of the latest cached ^IRX close.
pub fn tbill_yield(ctx: &Ctx, live: bool) -> Option<Value> {
    let (hist, stale) = equity_price_hist(ctx, TBILL_SYMBOL, live, None, "1y");
    let last = hist.last_date()?;
    Some(json!({"pct": num(r2(hist.last_close().unwrap())), "as_of": last, "stale": stale}))
}

/// Fees paid, running costs of the holdings and cash drag.
pub fn investment_costs(ctx: &Ctx, flex_value_hkd: f64, cash_hkd: f64, live: bool) -> Value {
    let conn = ctx.conn;
    let rates = ctx.rates();
    let ym = ctx.ym_now();
    let setup = ctx.invest();
    let platform = setup.platform();
    let fee_match = platform.and_then(|a| a.fee_match.clone());
    let base = crate::views::fx::base_currency(conn);

    // --- paid ---
    let (comm, n): (f64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(ABS(commission) * COALESCE(fx_to_base, 1)), 0) c, COUNT(*) n, MIN(ts) first \
             FROM ibkr_trades WHERE commission IS NOT NULL",
            [],
            |r| Ok((get_f64(r, 0).unwrap_or(0.0), r.get(1)?)),
        )
        .unwrap_or((0.0, 0));
    let mut flex_paid = 0.0;
    for b in ctx.bank().iter() {
        // lower(description) LIKE '%<fee_match>%' AND amount < 0
        if let (Some(a), Some(m)) = (b.amount.filter(|a| *a < 0.0), fee_match.as_deref()) {
            if b.description.to_lowercase().contains(m) {
                flex_paid += -a * rates.rate(&b.currency, b.ym());
            }
        }
    }
    // kept as is: the commissions are in the IBKR base currency, labelled
    // _hkd without conversion (right while the account base is HKD)
    let paid = json!({"ibkr_commissions_hkd": num(r2(comm)), "ibkr_trades": n,
                      "flexinvest_platform_hkd": num(r2(flex_paid)), "total_hkd": num(r2(comm + flex_paid))});

    // --- running (per year, at today's values) ---
    struct H {
        holding: String,
        value_hkd: Option<f64>,
        ter_pct: Option<f64>,
        fee_yr: Option<Value>,
        fee_sort: f64,
    }
    let mut holdings: Vec<H> = Vec::new();
    if let Some(snap) = ibkr_snapshot(conn) {
        for p in &snap.positions {
            let mut value = p.get("value_base").and_then(Value::as_f64).unwrap_or(0.0);
            let ccy = p.get("base_ccy").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(&base);
            if ccy != base {
                value *= rates.rate(ccy, &ym);
            }
            let sym = p.get("symbol").and_then(Value::as_str).unwrap_or("");
            let ter = setup.ter(sym);
            let fee = ter.map(|t| r2(value * t / 100.0));
            holdings.push(H { holding: format!("{} {sym}", setup.ibkr_name()), value_hkd: Some(r2(value)), ter_pct: ter,
                              fee_yr: fee.map(num), fee_sort: fee.unwrap_or(0.0) });
        }
    }
    if let (true, Some(acct)) = (flex_value_hkd > 0.0, platform) {
        let p = Platform::of(acct);
        let (platform, flat) = p.yearly(flex_value_hkd);
        let fund_fee = r2(flex_value_hkd * p.ocf / 100.0);
        holdings.push(H { holding: format!("{} (fund)", acct.name), value_hkd: Some(r2(flex_value_hkd)),
                          ter_pct: Some(p.ocf), fee_yr: Some(num(fund_fee)), fee_sort: fund_fee });
        // a flat fee stays a JSON integer
        let fee = if flat { int_or_num(p.flat_yr) } else { num(r2(platform)) };
        holdings.push(H { holding: format!("{} (platform)", acct.name), value_hkd: None,
                          ter_pct: Some(r2(platform / flex_value_hkd * 100.0)), fee_yr: Some(fee),
                          fee_sort: r2(platform) });
    }
    holdings.sort_by(|a, b| b.fee_sort.partial_cmp(&a.fee_sort).unwrap_or(std::cmp::Ordering::Equal));
    let total_value = holdings.iter().fold(0.0, |a, h| a + h.value_hkd.unwrap_or(0.0));
    let running_yr = holdings.iter().fold(0.0, |a, h| a + h.fee_sort);
    let unknown: Vec<&str> = holdings.iter().filter(|h| h.ter_pct.is_none()).map(|h| h.holding.as_str()).collect();
    let running = json!({
        "holdings": holdings.iter().map(|h| json!({"holding": h.holding, "value_hkd": h.value_hkd.map(num),
            "ter_pct": h.ter_pct.map(num), "fee_yr_hkd": h.fee_yr})).collect::<Vec<_>>(),
        "total_yr_hkd": num(r2(running_yr)),
        "pct": if total_value > 0.0 { num(r2(running_yr / total_value * 100.0)) } else { Value::Null },
        "value_hkd": num(r2(total_value)),
        "unknown": unknown,
    });

    // --- cash drag ---
    let mut drag = Value::Null;
    if let Some(ty) = tbill_yield(ctx, live) {
        if cash_hkd > 0.0 {
            let yr = cash_hkd * ty["pct"].as_f64().unwrap_or(0.0) / 100.0;
            let mut m = ty.as_object().cloned().unwrap_or_default();
            m.insert("cash_hkd".into(), num(r2(cash_hkd)));
            m.insert("yr_hkd".into(), num(r2(yr)));
            m.insert("mo_hkd".into(), num(r2(yr / 12.0)));
            drag = Value::Object(m);
        }
    }

    let flex = match platform.map(Platform::of) {
        Some(p) => json!({"value_hkd": num(r2(flex_value_hkd)), "cliff_hkd": int_or_num(p.cliff),
                          "headroom_hkd": num(r2(p.cliff - flex_value_hkd)),
                          "flat_yr_hkd": int_or_num(p.flat_yr), "pct_above": num(p.pct_above)}),
        None => Value::Null,
    };
    json!({"paid": paid, "running": running, "cash_drag": drag, "flexinvest": flex})
}
