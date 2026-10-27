//! The market-data seam: every network read the views need (monthly FX,
//! equity OHLC / quotes, live crypto prices, exchange balances) as one
//! trait. The server's fetchers implement it; the views only ever see
//! Ok(data) or Err, and on Err take their cache fallback.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use chrono::NaiveDate;

/// One daily bar in the symbol's major currency unit: (open, high, low, close).
pub type Bar = (f64, f64, f64, f64);

pub trait MarketFeed: Send + Sync {
    /// One rate per month (the month's last daily
    /// rate) for base->quote over start..=end, as (month start, rate).
    fn fx_monthly(&self, base: &str, quote: &str, start: NaiveDate, end: NaiveDate) -> Result<Vec<(NaiveDate, f64)>> {
        let _ = (base, quote, start, end);
        bail!("offline")
    }

    /// {'YYYY-MM-DD': bar} daily bars of `symbol` over a
    /// Yahoo range ('1y', '5y', '10y'), pinned to the listing in `want_ccy`.
    fn equity_ohlc(&self, symbol: &str, range: &str, want_ccy: Option<&str>) -> Result<BTreeMap<String, Bar>> {
        let _ = (symbol, range, want_ccy);
        bail!("offline")
    }

    /// The current price of `symbol`, major unit.
    fn equity_now(&self, symbol: &str) -> Result<f64> {
        let _ = symbol;
        bail!("offline")
    }

    /// Current USD price per asset (stables = 1.0; a
    /// symbol absent from a successful response is 0.0). Err on transport
    /// failure -- the caller falls back to the last balance snapshot.
    fn crypto_prices(&self, assets: &[String]) -> Result<BTreeMap<String, f64>> {
        let _ = assets;
        bail!("offline")
    }

    /// Every ApiSource's live_balances() as (source key, {asset: amount}),
    /// each within the 3s budget. Err when any source failed or timed out
    /// (the views then fall back to the snapshot).
    fn exchange_balances(&self) -> Result<Vec<(String, BTreeMap<String, f64>)>> {
        bail!("offline")
    }
}

/// No network: every fetch fails, so every view serves its cache.
#[derive(Clone, Copy, Debug, Default)]
pub struct Offline;

impl MarketFeed for Offline {}
