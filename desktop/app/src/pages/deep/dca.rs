//! Deep dive › DCA: each broker holding as a dollar-cost averaging plan on
//! its daily bars (its own currency), and the whole broker in HKD.

use std::rc::Rc;

use finance_core::dca::{bar_of, Buy, Dca};
use finance_core::invest::HoldingPlan;
use finance_core::Bar;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, chart::BarChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{c, col, day, flex, signed_pct, table, takeaway, tone, units, Data, OnPick};
use crate::fmt;
use crate::linechart::{self, Chart, Dot, Series};
use crate::ui::{self, Pt, sp};
use crate::zoom::px;

pub const EXPLAIN_EDGE: &str = "The average price you paid (commissions aside) against the average daily close from your first to your last buy of that holding. A fixed amount on a schedule buys more units when the price is low, so a steady plan tends to pay a little under the average. Positive = the averaging bought below the average price. The broker figure weights each holding by the money that went in.";
const EXPLAIN_AFTER: &str = "The same comparison with the commissions inside the price you paid. When it is lower than the edge, the fees took back more than the averaging won.";
const EXPLAIN_LUMP: &str = "Each holding's whole amount bought at its first buy instead, valued at today's price. In a rising market the lump sum usually wins; averaging trades that for not having to pick the entry day.";
const EXPLAIN_DAYS: &str = "A buy counts as a down day when that day's close was below the previous close. Scheduled buys land on whatever day the calendar picks, so this is about luck, not skill.";

/// The day's move of the bar a buy fell on, % (None without a bar before).
fn day_move(bars: &[Bar], date: &str) -> Option<f64> {
    let i = bar_of(bars, date)?;
    let prev = bars.get(i.checked_sub(1)?)?.close;
    (prev > 0.).then(|| (bars[i].close / prev - 1.) * 100.)
}

pub fn render(d: &Data, symbol: Option<&str>, on_pick: OnPick, cx: &App) -> Div {
    let picked = symbol.and_then(|s| d.plans.iter().find(|h| h.symbol == s));
    let picker = picker(d, picked.map(|h| h.symbol.as_str()), on_pick, cx);
    let body = match picked {
        Some(h) => holding(d, h, cx),
        None => broker(d, cx),
    };
    v_flex().gap(px(sp::GRID)).child(picker).child(body)
}

fn picker(d: &Data, picked: Option<&str>, on_pick: OnPick, cx: &App) -> Div {
    let t = cx.theme();
    let chip = |label: String, value: Option<String>, logo: bool| {
        let on = picked == value.as_deref();
        let on_pick = on_pick.clone();
        h_flex().id(SharedString::from(format!("dca-pick-{label}"))).gap_2().items_center()
            .px(px(10.)).py(px(4.)).rounded(px(8.)).border_1().cursor_pointer()
            .border_color(if on { t.primary } else { ui::hairline(cx) })
            .when(on, |d| d.bg(t.secondary))
            .hover(|s| s.bg(t.secondary))
            .on_click(move |_, w, cx| on_pick(value.clone(), w, cx))
            .when(logo, |d| d.when_some(crate::logo::tile(crate::logo::for_symbol(&label), 16., cx), |d, l| d.child(l)))
            .child(div().text_sm().font_semibold().child(label.clone()))
    };
    h_flex().gap_2().flex_wrap()
        .child(chip("Whole broker".into(), None, false))
        .children(d.plans.iter().map(|h| chip(h.symbol.clone(), Some(h.symbol.clone()), true)))
}

// ------------------------------------------------------------------ broker

fn broker(d: &Data, cx: &App) -> Div {
    let p = &d.portfolio;
    let fees_beat_edge = p.edge_pct > 0. && p.edge_after_fees_pct < 0.;
    let line = format!(
        "Across {} buys, the averaging paid {} the average price over each holding's buying days{}. Commissions came to {:.2}% of the money in, {}. Buying it all at each holding's first buy would be worth {} now, {}.",
        fmt::count(p.buys as f64),
        if p.edge_pct >= 0. { format!("{:.2}% below", p.edge_pct) } else { format!("{:.2}% above", -p.edge_pct) },
        if p.edge_pct.abs() < 0.5 { ", close to break-even" } else { "" },
        p.fee_drag_pct(),
        if fees_beat_edge { format!("more than the averaging saved: {} after fees", super::pct2(p.edge_after_fees_pct)) } else { format!("{} after fees", super::pct2(p.edge_after_fees_pct)) },
        fmt::hkd(p.lump_hkd),
        if p.vs_lump_pct() >= 0. { format!("{:.1}% less than the plan", p.vs_lump_pct() / (1. + p.vs_lump_pct() / 100.)) } else { format!("{:.1}% more than the plan", -p.vs_lump_pct() / (1. + p.vs_lump_pct() / 100.)) },
    );
    let n = p.up_days + p.down_days;
    let tiles = h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch().children([
        ui::Tile::explained("DCA edge", EXPLAIN_EDGE, super::pct2(p.edge_pct)).tone(super::sign_color(p.edge_pct, 2))
            .sub(format!("{} after fees", super::pct2(p.edge_after_fees_pct))).render(cx),
        ui::Tile::explained("vs lump sum", EXPLAIN_LUMP, fmt::pct(p.vs_lump_pct())).tone(fmt::signed_color(p.vs_lump_pct()))
            .sub(format!("all at the first buys: {}", fmt::hkd(p.lump_hkd))).render(cx),
        ui::Tile::explained("Fee drag", EXPLAIN_AFTER, format!("{:.2}%", p.fee_drag_pct()))
            .sub(format!("{} on {} in", fmt::hkd(p.fees_hkd), fmt::hkd(p.cost_hkd))).render(cx),
        ui::stat("Consistency", format!("{} of {} months", p.months_active, p.months_span),
            format!("{} buys · {} in profit today", p.buys, p.in_profit), None, cx),
        ui::Tile::explained("Down days", EXPLAIN_DAYS, format!("{} of {}", p.down_days, n))
            .sub(format!("{} buys on up days", p.up_days)).render(cx),
    ].map(|x| x.min_w(px(170.))));

    let rows: Vec<Vec<super::Cell>> = d.plans.iter().map(|h| {
        let x = &h.dca;
        vec![
            c(h.symbol.clone()),
            c(fmt::count(x.buys.len() as f64)),
            c(fmt::int(h.cost_hkd())),
            signed_pct(x.edge_pct(), 2),
            signed_pct(x.edge_after_fees_pct(), 2),
            signed_pct(x.vs_lump_pct(), 1),
            tone(format!("{:.2}%", x.fee_drag_pct()), if x.fee_drag_pct() >= 0.75 { cx.theme().warning } else { cx.theme().foreground }),
            signed_pct(x.gain_pct(), 1),
        ]
    }).collect();
    let cols = [flex("Holding"), col("Buys", 40.), col(fmt::local_static("In HK$"), 70.), col("Edge", 60.), col("After fees", 70.), col("vs lump", 60.), col("Fees", 52.), col("Return", 60.)];
    let per_holding = ui::card(cx).gap_2()
        .child(ui::section_info("Per holding", "pick a holding above for its prices", "Edge: the average price paid against the average daily close over that holding's buying days, in its own currency; After fees puts the commissions in. vs lump: the plan against its whole amount at the first buy. Fees: commissions as a share of the money in (amber from 0.75%). Return: value at the last close against the money in, fees included.", cx).mt_0())
        .child(table("dca-holdings", &cols, rows, cx));

    let mut buys: Vec<(&HoldingPlan, &Buy)> = d.plans.iter().flat_map(|h| h.dca.buys.iter().map(move |b| (h, b))).collect();
    buys.sort_by(|a, b| a.1.date.cmp(&b.1.date));
    let pts: Vec<Pt> = buys.iter().enumerate().map(|(i, (h, b))| Pt {
        label: format!("{} {}{}", h.symbol, day(&b.date), "\u{200B}".repeat(i)).into(),
        v: h.dca.buy_return_pct(b),
    }).collect();
    v_flex().gap(px(sp::GRID))
        .child(takeaway(line, cx))
        .child(tiles)
        .child(per_holding)
        .child(each_buy(pts, "dca-all-buys", cx))
}

fn each_buy(pts: Vec<Pt>, id: &'static str, cx: &App) -> Div {
    let (up, down) = (fmt::money_in(), fmt::money_out());
    let margin = (pts.len() / 4).max(1);
    let chart = BarChart::new(pts).appear(false)
        .id(id)
        .band(|p: &Pt| p.label.clone())
        .value(|p: &Pt| p.v)
        .value_axis(true)
        .tick_margin(margin)
        .value_tick_format(|v| format!("{v:.0}%"))
        .tooltip_value(|_, v| fmt::pct(v).into())
        .fill(move |p: &Pt, _, _, _| if p.v >= 0. { up } else { down });
    ui::chart_card("Every buy today", "each buy's return at the last close, commission included", 250., chart, cx)
}

// ------------------------------------------------------------------ one holding

fn holding(d: &Data, h: &HoldingPlan, cx: &App) -> Div {
    let t = cx.theme();
    let x: &Dca = &h.dca;
    let bars = d.ledger.bars.get(&h.symbol).map(Vec::as_slice).unwrap_or(&[]);
    let ccy = h.currency.as_str();
    let (a, s) = x.months_active();
    let ds = x.day_split(bars);
    let one = x.buys.len() == 1;
    let line = if one {
        format!("{}: one buy so far, at {} — {} against that day's close; the commission was {:.2}% of it. Averaging needs more than one buy to show anything.",
            h.symbol, super::ccy(x.avg_price(), ccy), super::pct2(x.edge_pct()), x.fee_drag_pct())
    } else {
        format!("{}: {} buys at an average {} against an average close of {} over those {} trading days: an edge of {}. The commissions ({:.2}% of the money in) make it {} after fees.",
            h.symbol, x.buys.len(), super::ccy(x.avg_price(), ccy), super::ccy(x.mean_close, ccy), x.span_bars,
            super::pct2(x.edge_pct()), x.fee_drag_pct(), super::pct2(x.edge_after_fees_pct()))
    };
    let tiles = h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch().children([
        ui::Tile::explained("DCA edge", EXPLAIN_EDGE, super::pct2(x.edge_pct())).tone(super::sign_color(x.edge_pct(), 2))
            .sub(format!("{} after fees", super::pct2(x.edge_after_fees_pct()))).id("dd-edge").render(cx),
        ui::Tile::explained("vs lump sum", EXPLAIN_LUMP, fmt::pct(x.vs_lump_pct())).tone(fmt::signed_color(x.vs_lump_pct()))
            .sub(format!("all at {}: {}", day(&x.buys[0].date), fmt::hkd(h.lump_hkd()))).id("dd-lump").render(cx),
        ui::stat("Position", fmt::hkd(h.value_hkd()), format!("{} on {} in", fmt::pct(x.gain_pct()), fmt::hkd(h.cost_hkd())),
            Some(fmt::signed_color(x.gain())), cx),
        ui::Tile::explained("Fee drag", EXPLAIN_AFTER, format!("{:.2}%", x.fee_drag_pct()))
            .sub(format!("{} in commissions", super::ccy(x.fees, ccy))).id("dd-fee").render(cx),
        ui::stat("Consistency", format!("{a} of {s} months"), format!("{} of {} buys on down days", ds.down, ds.up + ds.down), None, cx),
    ].map(|x| x.min_w(px(170.))));

    // price since a month before the first buy, your average, the buys
    let start = bar_of(bars, &x.buys[0].date).unwrap_or(0).saturating_sub(21);
    let window = &bars[start..];
    let n = window.len();
    let avg_by_date = |date: &str| x.points.iter().rev().find(|p| p.date.as_str() <= date).map(|p| p.avg_cost).filter(|v| *v > 0.);
    let max_cost = x.buys.iter().map(|b| b.cost).fold(1e-9, f64::max);
    let dots = x.buys.iter().filter_map(|b| {
        let i = bar_of(window, &b.date)?;
        Some(Dot { x: i as f32, y: b.paid_price(), r: 3. + 3. * (b.cost / max_cost).sqrt() as f32, color: fmt::money_in() })
    }).collect();
    let step = (n / 5).max(1);
    let chart = Chart {
        id: SharedString::from(format!("dd-price-{}", h.symbol)),
        labels: window.iter().map(|b| SharedString::from(b.date.clone())).collect(),
        x_ticks: (0..n).filter(|i| i % step == 0).map(|i| (i, SharedString::from(day(&window[i].date)))).collect(),
        series: vec![
            Series::line("Close", window.iter().map(|b| Some(b.close)).collect(), t.foreground.opacity(0.6)).width(1.4),
            Series::line("Your average", window.iter().map(|b| avg_by_date(&b.date)).collect(), t.primary).dashed().width(1.8),
        ],
        dots,
        log: false,
        height: 260.,
        y_fmt: Rc::new(fmt::dec2),
    };
    let legend = h_flex().gap_3().flex_wrap()
        .child(ui::legend(t.foreground.opacity(0.6), "daily close", super::ccy(x.price, ccy), cx))
        .child(ui::legend(t.primary, "your average, fees incl.", super::ccy(x.avg_cost(), ccy), cx))
        .child(ui::legend(fmt::money_in(), "buys", fmt::count(x.buys.len() as f64), cx));
    let price = ui::chart_card_with("Price and what you paid", &format!("{ccy} · daily · hover for a day"), legend, 330.,
        linechart::render(chart, cx), cx);

    let rows: Vec<Vec<super::Cell>> = x.buys.iter().map(|b| {
        let mv = day_move(bars, &b.date);
        vec![
            c(b.date.clone()),
            c(units(b.qty)),
            c(fmt::dec2(b.paid_price())),
            c(fmt::dec2(b.cost)),
            c(fmt::dec2(b.fee)),
            tone(format!("{:.2}%", b.fee_pct()), if b.fee_pct() >= 0.75 { t.warning } else { t.foreground }),
            mv.map(|m| signed_pct(m, 1)).unwrap_or(c("—")),
            signed_pct(x.buy_return_pct(b), 1),
        ]
    }).collect();
    let cols = [flex("Date"), col("Units", 64.), col("Price", 76.), col(if ccy == "GBP" { "Paid GBP" } else { "Paid USD" }, 80.), col("Fee", 56.), col("Fee %", 56.), col("Day move", 70.), col("Today", 64.)];
    let list = ui::card(cx).gap_2()
        .child(ui::section("Every buy", &format!("{ccy} · fee = the commission on that order"), cx).mt_0())
        .child(table(&format!("dca-buys-{}", h.symbol), &cols, rows, cx));
    let pts: Vec<Pt> = x.buys.iter().enumerate().map(|(i, b)| Pt { label: format!("{}{}", day(&b.date), "\u{200B}".repeat(i)).into(), v: x.buy_return_pct(b) }).collect();
    v_flex().gap(px(sp::GRID))
        .child(takeaway(line, cx))
        .child(tiles)
        .child(price)
        .child(list)
        .child(each_buy(pts, "dca-one-buys", cx))
}
