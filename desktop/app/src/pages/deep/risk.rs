//! Deep dive › Risk: per holding from its daily closes, volatility and the
//! worst falls (over the whole bar history and since the first buy), and a
//! correlation heatmap of daily returns, with the FlexInvest fund's proxy.

use finance_core::risk::{self, Risk};
use finance_core::Bar;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use super::{c, col, flex, note, table, takeaway, tone, Data};
use crate::fmt;
use crate::heatmap::{self, Heatmap};
use crate::ui::{self, sp};
use crate::zoom::px;

/// Days of daily returns behind each correlation.
const LOOKBACK: usize = 252;

fn mon_year(date: &str) -> String {
    fmt::month_label(&date[..date.len().min(7)])
}

pub fn render(d: &Data, cx: &App) -> Div {
    let t = cx.theme();
    let held = d.held();
    let risks: Vec<Risk> = held.iter().filter_map(|h| {
        risk::risk(&h.symbol, d.ledger.bars.get(&h.symbol)?, h.dca.buys.first().map(|b| b.date.as_str()))
    }).collect();
    if risks.is_empty() {
        return v_flex().child(ui::empty("No daily prices for the holdings yet — run finance refresh-prices on the server, then sync", cx));
    }

    let mut series: Vec<(String, &[Bar])> = held.iter().filter_map(|h| Some((h.symbol.clone(), d.ledger.bars.get(&h.symbol)?.as_slice()))).collect();
    if d.extra.proxy_bars.len() > 30 {
        series.push(("FlexInv.".into(), d.extra.proxy_bars.as_slice()));
    }
    let m = risk::matrix(&series, LOOKBACK);

    // the takeaway: the bumpiest holding, the calmest, the closest pair
    let by_vol = |r: &&Risk| r.vol_1y.unwrap_or(0.);
    let wild = risks.iter().max_by(|a, b| by_vol(a).total_cmp(&by_vol(b)));
    let calm = risks.iter().min_by(|a, b| by_vol(a).total_cmp(&by_vol(b)));
    let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
    for i in 0..m.symbols.len() {
        for j in i + 1..m.symbols.len() {
            if let Some(c) = m.m[i][j] {
                pairs.push((i, j, c));
            }
        }
    }
    pairs.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut line = String::new();
    if let (Some(w), Some(c)) = (wild, calm) {
        line.push_str(&format!("{} is the bumpiest: {:.0}% a year of volatility, now {:.0}% under its high. {} is the calmest at {:.0}%.",
            w.symbol, by_vol(&w), -w.from_high, c.symbol, by_vol(&c)));
    }
    if let (Some(hi), Some(lo)) = (pairs.first(), pairs.last()) {
        line.push_str(&format!(" {} and {} move most alike ({:+.2}); {} and {} least ({:+.2}).",
            m.symbols[hi.0], m.symbols[hi.1], hi.2, m.symbols[lo.0], m.symbols[lo.1], lo.2));
    }

    let warn = t.warning;
    let rows: Vec<Vec<super::Cell>> = risks.iter().map(|r| vec![
        c(r.symbol.clone()),
        c(format!("{} · {}", mon_year(&r.first_bar), fmt::count(r.bars as f64))),
        c(r.vol_1y.map(|v| format!("{v:.0}%")).unwrap_or("—".into())),
        c(r.vol_all.map(|v| format!("{v:.0}%")).unwrap_or("—".into())),
        r.dd_since_buy.map(|x| tone(format!("{x:.1}%"), if x < -15. { warn } else { t.foreground })).unwrap_or(c("—")),
        tone(format!("{:.1}%", r.dd_all), if r.dd_all < -30. { warn } else { t.foreground }),
        c(format!("{} → {}", mon_year(&r.dd_all_from), mon_year(&r.dd_all_to))),
        c(format!("{:.1}%", r.from_high)),
    ]).collect();
    let cols = [flex("Holding"), col("Bars from", 110.), col("Vol 1y", 56.), col("Vol all", 56.), col("Since buy", 72.), col("Worst ever", 76.), col("When", 120.), col("vs high", 60.)];
    let tbl = ui::card(cx).gap_2()
        .child(ui::section_info("Per holding", "from daily closes in the trading currency",
            fmt::local("Vol: the yearly standard deviation of daily returns (√252), over the last year and over all the bars cached (up to five years). Since buy: the deepest fall from a peak since your first buy. Worst ever: the deepest peak-to-trough fall in the cached history, with when it ran. vs high: the last close against the highest close cached. Currency moves against HKD are not in these."), cx).mt_0())
        .child(table("risk-table", &cols, rows, cx));

    let k = m.symbols.len();
    let heat = Heatmap { id: "risk-heat".into(), labels: m.symbols.iter().map(|s| SharedString::from(s.clone())).collect(), m: m.m.clone(), n: m.n.clone() };
    let heat_card = ui::card(cx).gap_2()
        .child(h_flex().justify_between().flex_wrap().gap_2()
            .child(ui::section_info("Moving together", &format!("correlation of daily returns, last {LOOKBACK} shared days"),
                format!("+1: they move as one; 0: unrelated; −1: opposite. Each pair uses the days both traded. London closes hours before New York, so a move that hits both on the same day can land a day apart, and London–US pairs read lower than they are (VWRA vs VT is the same market).{}",
                    if m.symbols.iter().any(|s| s == "FlexInv.") { " FlexInv. is the FlexInvest fund's MSCI World proxy (URTH)." } else { "" }), cx).mt_0())
            .child(h_flex().gap_3().child(ui::legend(t.primary, "together", "", cx)).child(ui::legend(fmt::money_out(), "opposite", "", cx))))
        .child(div().max_w(px(80. + 64. * k as f32)).child(heatmap::render(heat, cx)));

    v_flex().gap(px(sp::GRID))
        .child(takeaway(line, cx))
        .child(tbl)
        .child(heat_card)
        .child(note(format!("Past swings describe the ride so far; they don't forecast the next one.{}", {
            let short: Vec<&str> = risks.iter().filter(|r| r.bars < 250).map(|r| r.symbol.as_str()).collect();
            if short.is_empty() { String::new() } else { format!(" {} {} under a year of prices, so {} numbers rest on few days.", short.join(", "), if short.len() == 1 { "has" } else { "have" }, if short.len() == 1 { "its" } else { "their" }) }
        }), cx))
}
