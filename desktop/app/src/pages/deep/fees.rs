//! Deep dive › Fees: what set each commission, the order size that would
//! bring it under 0.25%, the next 12 months at the current pattern, and the
//! trade history re-priced on IBKR's Fixed and Tiered plans.

use std::collections::BTreeMap;

use finance_core::exposure::Venue;
use finance_core::fees::{self, Plan, Totals, SCHEDULES, SCHEDULES_AS_OF};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{c, col, day, flex, note, table, takeaway, tone, Data};
use crate::fmt;
use crate::ui::{self, sp};
use crate::zoom::px;

/// The fee share the order-size table aims under.
pub const TARGET_PCT: f64 = 0.25;
/// The window the yearly projection scales up.
pub const WINDOW_DAYS: i64 = 91;

fn venue_label(v: Venue, ccy: &str) -> String {
    match v {
        Venue::Lse => format!("London, {ccy} line"),
        Venue::Us => "US".into(),
        other => other.label().into(),
    }
}

pub fn render(d: &Data, cx: &App) -> Div {
    let t = cx.theme();
    let rows = &d.repriced;
    let all = Totals::of(rows);
    let proj = fees::projection(rows, &d.as_of, WINDOW_DAYS);
    let model_matches = rows.iter().all(|r| r.fixed.is_some_and(|f| (f.0 - r.actual).abs() < 0.01));

    let line = format!(
        "{} of {} orders paid the per-order minimum (or the 1% cap under it): {} so far, {:.2}% of what was bought. At the last {} days' pattern that is about {} a year. Re-priced on IBKR Tiered (approximate) the same orders come to about {}, {} less.",
        fmt::count(all.floor_count as f64), fmt::count(all.trades as f64), fmt::hkd(all.actual_hkd), all.actual_pct(), WINDOW_DAYS,
        fmt::hkd(proj.actual_yr_hkd), fmt::hkd(all.tiered_hkd), fmt::hkd(all.tiered_saving_hkd()));

    let tiles = h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch().children([
        ui::Tile::explained("Paid so far", "Every commission charged on the broker trades, in HKD at each trade's rate.", fmt::hkd(all.actual_hkd))
            .sub(format!("{:.2}% of {} traded", all.actual_pct(), fmt::hkd(all.value_hkd))).render(cx),
        ui::Tile::explained("Set by the minimum", "Orders whose commission the per-order minimum decided (or IBKR's 1% cap, which only bites under the minimum). For these the per-share rate never mattered: a bigger order would have paid the same fee.",
            format!("{} of {}", all.floor_count, all.trades))
            .sub(if model_matches { "the Fixed schedule reproduces every fee".to_string() } else { "per the Fixed schedule".to_string() }).render(cx),
        ui::Tile::explained("Next 12 months", "The buys of the last 91 days, scaled to a year: same symbols, same sizes, same fees.",
            format!("{}/yr", fmt::hkd(proj.actual_yr_hkd)))
            .sub(format!("{:.2}% of {} a year bought", proj.actual_pct(), fmt::hkd(proj.money_yr_hkd))).render(cx),
        ui::Tile::explained("On Tiered (approx.)", "The same orders priced with IBKR's Tiered components (see the schedule table): a lower minimum plus exchange and clearing fees. Approximate: check IBKR's current schedule.",
            format!("{}/yr", fmt::hkd(proj.tiered_yr_hkd)))
            .sub(format!("history {} instead of {}", fmt::hkd(all.tiered_hkd), fmt::hkd(all.fixed_hkd))).tone(fmt::money_in()).render(cx),
    ].map(|x| x.min_w(px(180.))));

    // ---- order size for a fee under 0.25%, per venue ----
    let mut venues: BTreeMap<(String, String), Vec<&fees::Repriced>> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.buy) {
        venues.entry((format!("{:?}", r.venue), r.currency.clone())).or_default().push(r);
    }
    let size_rows: Vec<Vec<super::Cell>> = venues.values().filter_map(|v| {
        let r0 = v[0];
        let fixed = fees::schedule(Plan::Fixed, r0.venue, &r0.currency)?;
        let tiered = fees::schedule(Plan::Tiered, r0.venue, &r0.currency)?;
        let mut sizes: Vec<f64> = v.iter().map(|r| r.value).collect();
        sizes.sort_by(f64::total_cmp);
        let median = sizes[sizes.len() / 2];
        let price = v.iter().map(|r| r.price).sum::<f64>() / v.len() as f64;
        let (fee_now, _) = fees::commission(fixed, median / price, price);
        let fx = v.iter().map(|r| r.fx).sum::<f64>() / v.len() as f64;
        let on_fixed = fees::order_size_for(fixed, price, TARGET_PCT);
        let on_tiered = fees::order_size_for(tiered, price, TARGET_PCT);
        let ccy = &r0.currency;
        Some(vec![
            c(venue_label(r0.venue, ccy)),
            c(fmt::count(v.len() as f64)),
            c(super::ccy(median, ccy)),
            c(super::ccy(fee_now, ccy)),
            tone(format!("{:.2}%", fee_now / median * 100.), if fee_now / median * 100. > TARGET_PCT { t.warning } else { fmt::money_in() }),
            c(on_fixed.map(|s| super::ccy(s, ccy)).unwrap_or("—".into())),
            c(on_fixed.map(|s| fmt::hkd(s * fx)).unwrap_or("—".into())),
            c(on_tiered.map(|s| super::ccy(s, ccy)).unwrap_or("—".into())),
        ])
    }).collect();
    let size_cols = [flex("Venue"), col("Buys", 44.), col("Typical order", 100.), col("Fee", 72.), col("Fee %", 56.),
        col("≤0.25% on Fixed", 112.), col(fmt::local_static("in HK$"), 84.), col("on Tiered", 100.)];
    let sizes = ui::card(cx).gap_2()
        .child(ui::section_info("Order size for fees under 0.25%", "per venue, at your typical order",
            "Typical order is the median buy at that venue. The ≤0.25% columns are the smallest single order whose commission is at most 0.25% of it, at the holdings' average price. Fewer, larger orders reach it; the same money in smaller orders pays the minimum each time.", cx).mt_0())
        .child(table("fee-sizes", &size_cols, size_rows, cx));

    // ---- every order ----
    let warn = t.warning;
    let order_rows: Vec<Vec<super::Cell>> = rows.iter().rev().map(|r| {
        let bind = r.fixed.map(|f| f.1);
        vec![
            c(format!("{} {}", day(&r.date), &r.date[..4])),
            c(format!("{} {}", if r.buy { "Buy" } else { "Sell" }, r.symbol)),
            c(super::ccy(r.value, &r.currency)),
            c(fmt::dec2(r.actual)),
            tone(format!("{:.2}%", r.actual_pct()), if r.actual_pct() > TARGET_PCT { warn } else { t.foreground }),
            match bind {
                Some(b) if b.is_floor() => tone(b.label(), warn),
                Some(b) => c(b.label()),
                None => c("—"),
            },
            c(r.tiered.map(|f| fmt::dec2(f.0)).unwrap_or("—".into())),
            c(r.tiered.map(|f| format!("{:.2}%", f.0 / r.value * 100.)).unwrap_or("—".into())),
        ]
    }).collect();
    let order_cols = [flex("Date"), col("Order", 84.), col("Value", 104.), col("Fee", 56.), col("Fee %", 56.), col("Set by", 64.), col("Tiered", 56.), col("Tiered %", 64.)];
    let orders = ui::card(cx).gap_2()
        .child(ui::section_info("Every order", "newest first · fees in the trading currency",
            "Set by: what decided the commission on the Fixed plan — the per-order minimum, the 1% cap (an order so small that 1% of it is under the minimum), or the rate. Tiered: the same order on the Tiered plan, approximate.", cx).mt_0())
        .child(table("fee-orders", &order_cols, order_rows, cx));

    // ---- Fixed vs Tiered per holding ----
    let mut by_sym: BTreeMap<&str, Totals> = BTreeMap::new();
    for r in rows {
        by_sym.entry(r.symbol.as_str()).or_default().add(r);
    }
    let mut sym_rows: Vec<(&str, Totals)> = by_sym.into_iter().collect();
    sym_rows.sort_by(|a, b| b.1.actual_hkd.total_cmp(&a.1.actual_hkd));
    let mut vs_rows: Vec<Vec<super::Cell>> = sym_rows.iter().map(|(s, x)| vec![
        c(s.to_string()),
        c(fmt::count(x.trades as f64)),
        c(fmt::dec2(x.actual_hkd)),
        c(fmt::dec2(x.fixed_hkd)),
        c(fmt::dec2(x.tiered_hkd)),
        tone(fmt::dec2(x.tiered_saving_hkd()), fmt::signed_color(x.tiered_saving_hkd())),
    ]).collect();
    vs_rows.push(vec![
        c("Total"), c(fmt::count(all.trades as f64)), c(fmt::dec2(all.actual_hkd)), c(fmt::dec2(all.fixed_hkd)), c(fmt::dec2(all.tiered_hkd)),
        tone(fmt::dec2(all.tiered_saving_hkd()), fmt::signed_color(all.tiered_saving_hkd())),
    ]);
    let vs_cols = [flex("Holding"), col("Orders", 52.), col(fmt::local_static("Paid HK$"), 80.), col(fmt::local_static("Fixed HK$"), 80.), col(fmt::local_static("Tiered HK$"), 84.), col("Difference", 84.)];
    let vs = ui::card(cx).gap_2()
        .child(ui::section_info("Fixed vs Tiered", &fmt::local("your trades re-priced on both plans, HKD"),
            "Fixed is the account's plan: its model reproduces what was charged. Tiered adds exchange and clearing fees on top of a lower commission and minimum; on small orders the minimum dominates, so the gap is mostly the difference in minimums. Difference = Fixed minus Tiered (positive = Tiered cheaper). FX conversion costs are the same on both and left out.", cx).mt_0())
        .child(table("fee-vs", &vs_cols, vs_rows, cx))
        .child(note(format!("The next 12 months at the last {WINDOW_DAYS} days' pattern: about {} on Fixed, {} on Tiered.",
            fmt::hkd(proj.fixed_yr_hkd), fmt::hkd(proj.tiered_yr_hkd)), cx));

    // ---- the schedules, documented ----
    let sched_rows = SCHEDULES.iter().enumerate().map(|(i, s)| {
        h_flex().gap_3().py(px(6.)).items_start().text_size(px(crate::ui::ts::BODY))
            .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx)))
            .child(div().w(px(170.)).flex_shrink_0().font_weight(FontWeight::MEDIUM)
                .child(format!("{} · {}", s.plan.label(), venue_label(s.venue, s.currency))))
            .child(div().flex_1().min_w(px(160.)).text_color(t.muted_foreground).child(s.note))
    });
    let sched = ui::card(cx).gap_2()
        .child(ui::section("IBKR schedules used", &format!("approximate, check IBKR's current schedule · compiled {SCHEDULES_AS_OF}"), cx).mt_0())
        .child(v_flex().children(sched_rows))
        .child(note("Left out: higher volume tiers, exchange rebates and per-venue maker/taker fees, regulatory fees on sells, FX conversion costs. UK-listed ETFs carry no stamp duty.", cx));

    v_flex().gap(px(sp::GRID))
        .child(takeaway(line, cx))
        .child(tiles)
        .child(sizes)
        .child(vs)
        .child(orders)
        .child(sched)
}
