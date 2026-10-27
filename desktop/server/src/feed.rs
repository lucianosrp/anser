//! The live MarketFeed: the views' network seam (engine views/feed.rs) on
//! top of the engine fetchers. Under FINANCE_OFFLINE=1 the views never see
//! it (views::Ctx::new swaps in feed::Offline), and the fetchers refuse the
//! network on their own anyway.
//!
//! * fx_monthly      -> fetch::fx::fetch_exchange_rates (frankfurter)
//! * equity_ohlc/now -> fetch::prices (Yahoo)
//! * crypto_prices   -> fetch::live::cached_live_prices (3s bound, 60s TTL);
//!   through [`REQUEST_FEED`] the same cache is read stale-while-revalidate
//!   (fetch::live::swr_live_prices), so a page or the bundle never waits on
//!   Binance: it gets the last tickers (or the snapshot fallback) while a
//!   background thread refetches.
//! * exchange_balances -> fetch::live::sweep (every source + the equity
//!   proxy quote in parallel, one 3s deadline). The proxy quote it gathered
//!   is kept briefly so the valuation's equity_now(proxy) right after the
//!   sweep doesn't pay a second round trip.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use chrono::NaiveDate;

use finance_engine::fetch;
use finance_engine::views::feed::{Bar, MarketFeed};

/// The live valuation's wait on its parallel fetches, and
/// live_or_cached_prices' timeout.
const LIVE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a swept proxy quote stands in for a fresh equity_now call.
const SWEPT_QUOTE_TTL: Duration = Duration::from_secs(10);
/// Past this, request paths stop serving the last tickers as live and fall
/// back to the snapshot (flagged stale): the refetches have been failing.
const PRICES_MAX_AGE: Duration = Duration::from_secs(300);

pub struct LiveFeed {
    /// wait for the tickers (CLI, MCP, the background live refresh), or
    /// serve what's cached and refetch behind (HTTP requests)
    blocking: bool,
}

static SWEPT_QUOTE: Mutex<Option<(Instant, String, f64)>> = Mutex::new(None);

/// Waits on the network (bounded): the CLI, MCP and the server's
/// background live refresh.
pub static FEED: LiveFeed = LiveFeed { blocking: true };

/// What HTTP requests fetch through: the crypto tickers never block.
pub static REQUEST_FEED: LiveFeed = LiveFeed { blocking: false };

impl MarketFeed for LiveFeed {
    fn fx_monthly(&self, base: &str, quote: &str, start: NaiveDate, end: NaiveDate) -> Result<Vec<(NaiveDate, f64)>> {
        Ok(fetch::fx::fetch_exchange_rates(start, end, base, quote)?.into_iter().collect())
    }

    fn equity_ohlc(&self, symbol: &str, range: &str, want_ccy: Option<&str>) -> Result<BTreeMap<String, Bar>> {
        fetch::prices::equity_ohlc(symbol, range, want_ccy)
    }

    fn equity_now(&self, symbol: &str) -> Result<f64> {
        if let Some((at, sym, px)) = SWEPT_QUOTE.lock().ok().and_then(|q| q.clone()) {
            if sym == symbol && at.elapsed() < SWEPT_QUOTE_TTL {
                return Ok(px);
            }
        }
        fetch::prices::equity_now(symbol)
    }

    fn crypto_prices(&self, assets: &[String]) -> Result<BTreeMap<String, f64>> {
        if self.blocking {
            fetch::live::cached_live_prices(assets, LIVE_TIMEOUT)
        } else {
            fetch::live::swr_live_prices(assets, LIVE_TIMEOUT, PRICES_MAX_AGE)
        }
    }

    fn exchange_balances(&self) -> Result<Vec<(String, BTreeMap<String, f64>)>> {
        let sweep = fetch::live::sweep(LIVE_TIMEOUT);
        if let (Some((sym, px)), Ok(mut q)) = (sweep.equity_now, SWEPT_QUOTE.lock()) {
            *q = Some((Instant::now(), sym, px));
        }
        if !sweep.ok {
            bail!("a live balance source failed or timed out");
        }
        Ok(sweep.balances.into_iter().collect())
    }
}
