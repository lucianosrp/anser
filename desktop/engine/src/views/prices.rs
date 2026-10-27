//! Prices, read side: cached equity closes (equity_prices), the crypto
//! balance snapshots and the live-or-cached crypto tickers. Fetches go
//! through MarketFeed.

use rusqlite::{params, Connection};

use super::feed::Bar;
use super::{get_f64, Ctx};

pub const STABLE: [&str; 7] = ["USDT", "USDC", "USD", "BUSD", "FDUSD", "TUSD", "DAI"];

pub fn is_stable(asset: &str) -> bool {
    STABLE.contains(&asset)
}

/// One symbol's cached daily closes, sorted by date.
#[derive(Clone, Debug, Default)]
pub struct Hist {
    pub dates: Vec<String>,
    pub closes: Vec<f64>,
}

impl Hist {
    pub fn is_empty(&self) -> bool {
        self.dates.is_empty()
    }

    /// hist[dates[max(bisect_right(dates, day) - 1, 0)]]: the close on or
    /// before `day` (the first close when `day` predates the series).
    pub fn on_or_before(&self, day: &str) -> f64 {
        let i = super::bisect_right(&self.dates, day);
        self.closes[i.saturating_sub(1)]
    }

    /// hist[dates[max(bisect_left(dates, day) - 1, 0)]]: the last close
    /// strictly before `day` -- the month-end mark when `day` is the 1st of
    /// the next month.
    pub fn before(&self, day: &str) -> f64 {
        let i = super::bisect_left(&self.dates, day);
        self.closes[i.saturating_sub(1)]
    }

    pub fn last_date(&self) -> Option<&str> {
        self.dates.last().map(String::as_str)
    }

    pub fn last_close(&self) -> Option<f64> {
        self.closes.last().copied()
    }
}

/// db.load_equity_prices.
pub fn load_equity_prices(conn: &Connection, symbol: &str) -> Hist {
    let mut h = Hist::default();
    if let Ok(mut stmt) = conn.prepare_cached("SELECT date, close FROM equity_prices WHERE symbol=? ORDER BY date") {
        if let Ok(rows) = stmt.query_map([symbol], |r| Ok((r.get::<_, String>(0)?, get_f64(r, 1).unwrap_or(0.0)))) {
            for (d, c) in rows.flatten() {
                h.dates.push(d);
                h.closes.push(c);
            }
        }
    }
    h
}

/// db.equity_price_currency.
pub fn equity_price_currency(conn: &Connection, symbol: &str) -> Option<String> {
    conn.query_row(
        "SELECT currency FROM equity_prices WHERE symbol=? AND currency IS NOT NULL LIMIT 1",
        [symbol],
        |r| r.get(0),
    )
    .ok()
}

/// db.upsert_equity_prices with the daily bars (close + OHLC).
pub fn upsert_equity_bars(
    conn: &Connection,
    symbol: &str,
    bars: &std::collections::BTreeMap<String, Bar>,
    currency: Option<&str>,
) {
    for (d, (o, h, l, c)) in bars {
        let _ = conn.execute(
            "INSERT INTO equity_prices(symbol,date,close,currency,open,high,low) VALUES(?,?,?,?,?,?,?) \
             ON CONFLICT(symbol,date) DO UPDATE SET close=excluded.close, \
             currency=COALESCE(excluded.currency, currency), open=COALESCE(excluded.open, open), \
             high=COALESCE(excluded.high, high), low=COALESCE(excluded.low, low)",
            params![symbol, d, c, currency, o, h, l],
        );
    }
}

/// The "older than 3 days" cutoff for cached closes.
pub fn refresh_before(ctx: &Ctx) -> String {
    (ctx.today - chrono::Duration::days(3)).to_string()
}

/// Cached daily closes
/// of any Yahoo symbol, refreshed through the feed when older than 3 days
/// and `live`. Returns (hist, stale). A symbol cached under a different
/// currency than `want_ccy` is wiped first.
pub fn equity_price_hist(ctx: &Ctx, symbol: &str, live: bool, want_ccy: Option<&str>, range: &str) -> (std::sync::Arc<Hist>, bool) {
    let conn = ctx.conn;
    if let Some(want) = want_ccy {
        if let Some(stored) = equity_price_currency(conn, symbol) {
            if stored != want {
                let _ = conn.execute("DELETE FROM equity_prices WHERE symbol=?", [symbol]);
                ctx.invalidate_equity(symbol);
            }
        }
    }
    let mut hist = ctx.equity(symbol);
    let cutoff = refresh_before(ctx);
    let old = |h: &Hist| h.last_date().is_none_or(|d| d < cutoff.as_str());
    if live && old(&hist) {
        match ctx.feed.equity_ohlc(symbol, range, want_ccy) {
            Ok(bars) => {
                upsert_equity_bars(conn, symbol, &bars, want_ccy);
                ctx.invalidate_equity(symbol);
                hist = ctx.equity(symbol);
            }
            Err(_) => return (hist, true),
        }
    }
    let stale = old(&hist);
    (hist, stale)
}

// ---------------------------------------------------------------------------
// crypto balance snapshots
// ---------------------------------------------------------------------------

pub struct SnapAsset {
    pub asset: String,
    pub amount: Option<f64>,
    pub price: Option<f64>,
}

/// Each exchange's most recent batch, in query order.
pub struct Snapshot {
    pub ts: i64,
    pub by_exchange: Vec<(String, Vec<SnapAsset>)>,
}

pub fn last_snapshot_ts(conn: &Connection) -> i64 {
    conn.query_row("SELECT MAX(ts) AS m FROM crypto_balance_snapshots", [], |r| r.get::<_, Option<i64>>(0))
        .ok()
        .flatten()
        .unwrap_or(0)
}

pub fn latest_snapshot(conn: &Connection) -> Snapshot {
    let ts = last_snapshot_ts(conn);
    let mut out = Snapshot { ts, by_exchange: Vec::new() };
    if ts == 0 {
        return out;
    }
    let sql = "SELECT s.exchange, s.asset, s.amount, s.price FROM crypto_balance_snapshots s \
               JOIN (SELECT exchange, MAX(ts) mts FROM crypto_balance_snapshots GROUP BY exchange) m \
               ON s.exchange=m.exchange AND s.ts=m.mts";
    if let Ok(mut stmt) = conn.prepare_cached(sql) {
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, get_f64(r, 2), get_f64(r, 3)))
        });
        if let Ok(rows) = rows {
            for (ex, asset, amount, price) in rows.flatten() {
                let a = SnapAsset { asset, amount, price };
                match out.by_exchange.iter_mut().find(|(e, _)| *e == ex) {
                    Some((_, v)) => match v.iter_mut().find(|x| x.asset == a.asset) {
                        Some(slot) => *slot = a, // a later value for the same key wins, in place
                        None => v.push(a),
                    },
                    None => out.by_exchange.push((ex, vec![a])),
                }
            }
        }
    }
    out
}

/// db.add_snapshot.
pub fn add_snapshot(conn: &Connection, ts: i64, exchange: &str, balances: &[(String, f64)], prices: &[(String, f64)]) {
    for (a, amt) in balances {
        let px = prices.iter().find(|(k, _)| k == a).map(|(_, p)| *p).filter(|p| *p != 0.0);
        let _ = conn.execute(
            "INSERT OR IGNORE INTO crypto_balance_snapshots(ts,exchange,asset,amount,price) VALUES(?,?,?,?,?)",
            params![ts, exchange, a, amt, px],
        );
    }
}

/// Live tickers through the feed,
/// else the latest snapshot's price per asset (first exchange carrying a
/// price), stale, with as_of = the snapshot's local HH:MM. Prices come back
/// in `assets` order.
pub fn live_or_cached_prices(ctx: &Ctx, assets: &[&str]) -> (Vec<(String, f64)>, bool, Option<String>) {
    let names: Vec<String> = assets.iter().map(|a| a.to_string()).collect();
    if let Ok(px) = ctx.feed.crypto_prices(&names) {
        if !px.is_empty() {
            let mut out: Vec<(String, f64)> = Vec::new();
            // stables first (set by the stable pre-fill), then the rest
            for a in assets.iter().filter(|a| is_stable(a)) {
                out.push((a.to_string(), px.get(*a).copied().unwrap_or(1.0)));
            }
            for a in assets.iter().filter(|a| !is_stable(a)) {
                out.push((a.to_string(), px.get(*a).copied().unwrap_or(0.0)));
            }
            return (out, false, None);
        }
    }
    let snap = ctx.snapshot();
    let out = assets
        .iter()
        .map(|a| {
            let mut px = 0.0;
            for (_, holdings) in &snap.by_exchange {
                if let Some(row) = holdings.iter().find(|h| h.asset == *a) {
                    if let Some(p) = row.price.filter(|p| *p != 0.0) {
                        px = p;
                        break;
                    }
                }
            }
            (a.to_string(), px)
        })
        .collect();
    let as_of = (snap.ts != 0).then(|| {
        // display-only stamp: local time
        chrono::DateTime::from_timestamp_millis(snap.ts)
            .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default()
    });
    (out, true, as_of)
}
