//! Per-holding broker maths from the executions: quantity, average cost,
//! cost basis in HKD at the rates of the day each lot was bought, realized
//! gain on sells, commissions paid. Market values come from the broker
//! snapshot (in the bundle); this is everything the snapshot does not carry.
//!
//! Average-cost method (not FIFO): a sell releases cost at the running
//! average, which is how IBKR shows "average price" for a position.

use std::collections::BTreeMap;

use crate::ledger::{Ledger, Trade};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Holding {
    pub symbol: String,
    pub currency: String,
    /// units held after every execution
    pub qty: f64,
    /// average cost per unit, trade currency (commissions included)
    pub avg_cost: f64,
    /// what the units still held cost, trade currency / HKD at the day's rate
    pub cost_ccy: f64,
    pub cost_hkd: f64,
    /// gains locked in by sells (HKD)
    pub realized_hkd: f64,
    /// every commission paid on this symbol (HKD)
    pub fees_hkd: f64,
    pub buys: usize,
    pub sells: usize,
    pub first_buy: String,
    pub last_trade: String,
}

/// One entry per symbol ever traded, in first-buy order.
pub fn holdings(ledger: &Ledger) -> Vec<Holding> {
    holdings_from(&ledger.trades)
}

pub fn holdings_from(trades: &[Trade]) -> Vec<Holding> {
    let mut by: BTreeMap<String, Holding> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for t in trades {
        let h = by.entry(t.symbol.clone()).or_insert_with(|| {
            order.push(t.symbol.clone());
            Holding { symbol: t.symbol.clone(), currency: t.currency.clone(), first_buy: t.date.clone(), ..Default::default() }
        });
        let fee_hkd = t.commission * t.fx_to_base;
        h.fees_hkd += fee_hkd;
        h.last_trade = t.date.clone();
        if t.qty > 0. {
            // cost = cash paid, commission included, so the average carries
            // the fees (IBKR's amount already has the commission in it)
            let paid = crate::invest::buy_cost(t);
            h.cost_ccy += paid;
            h.cost_hkd += paid * t.fx_to_base;
            h.qty += t.qty;
            h.buys += 1;
        } else if t.qty < 0. && h.qty > 0. {
            let sold = (-t.qty).min(h.qty);
            let frac = sold / h.qty;
            let (released_ccy, released_hkd) = (h.cost_ccy * frac, h.cost_hkd * frac);
            // net of the commission, whether or not amount already is
            let proceeds_hkd = t.amount.min(-t.qty * t.price - t.commission) * t.fx_to_base;
            h.realized_hkd += proceeds_hkd - released_hkd;
            h.cost_ccy -= released_ccy;
            h.cost_hkd -= released_hkd;
            h.qty -= sold;
            h.sells += 1;
        }
        h.avg_cost = if h.qty > 1e-9 { h.cost_ccy / h.qty } else { 0. };
    }
    order.into_iter().filter_map(|s| by.remove(&s)).collect()
}

#[cfg(test)]
mod tests {
    use super::holdings_from;
    use crate::ledger::Trade;

    fn t(qty: f64, price: f64, commission: f64, fx: f64) -> Trade {
        Trade { date: "2026-01-01".into(), symbol: "X".into(), currency: "USD".into(), qty, price,
                amount: -qty * price, commission, fx_to_base: fx }
    }

    #[test]
    fn average_cost_carries_fees_and_fx() {
        let h = &holdings_from(&[t(2., 100., 1., 7.8), t(2., 110., 1., 7.85)])[0];
        assert!((h.qty - 4.).abs() < 1e-9);
        assert!((h.cost_ccy - 422.).abs() < 1e-9); // 200 + 220 + 2 commissions
        assert!((h.avg_cost - 105.5).abs() < 1e-9);
        assert!((h.cost_hkd - (201. * 7.8 + 221. * 7.85)).abs() < 1e-6);
        assert!((h.fees_hkd - (7.8 + 7.85)).abs() < 1e-9);
    }

    #[test]
    fn ibkr_net_amount_counts_the_commission_once() {
        // IBKR's amount is the net cash: 400 of shares + 4 commission
        let mut buy = t(2., 200., 4., 7.8);
        buy.amount = -404.;
        let h = &holdings_from(&[buy])[0];
        assert!((h.cost_ccy - 404.).abs() < 1e-9);
        assert!((h.avg_cost - 202.).abs() < 1e-9);
    }

    #[test]
    fn sell_realizes_against_average() {
        let mut sell = t(-1., 130., 1., 8.);
        sell.amount = 130.;
        let h = &holdings_from(&[t(2., 100., 0., 8.), sell])[0];
        assert!((h.qty - 1.).abs() < 1e-9);
        assert!((h.realized_hkd - ((130. - 1.) * 8. - 100. * 8.)).abs() < 1e-6);
        assert!((h.cost_ccy - 100.).abs() < 1e-9);
    }
}
