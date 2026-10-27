//! Deep dive › Exposure: by trading currency, venue and asset type, and,
//! looking through the funds with a small static table of approximate index
//! weights, by region and sector. Flags funds that track the same index.
//! The Investments page reuses `all_positions` and `cards` for its
//! allocation view.

use finance_core::exposure::{self, Exposure, Kind, Position, Venue, WEIGHTS_AS_OF};
use finance_core::Bundle;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, chart::PieChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use super::{note, scope_switch, takeaway, Data, OnScope, Scope};
use crate::fmt;
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

/// The broker: every position at the broker's mark, and its cash.
pub fn broker_positions(b: &Bundle) -> Vec<Position> {
    let mut v: Vec<Position> = b.arr(&["ibkr", "snapshot", "positions"]).iter().filter_map(|p| {
        let desc = p.get("description").and_then(Value::as_str).unwrap_or("");
        Some(Position::broker(p.get("symbol")?.as_str()?, desc, p.get("currency")?.as_str()?, p.get("value_base")?.as_f64()?))
    }).collect();
    let cash = b.f64(&["ibkr", "snapshot", "cash_base"]).unwrap_or(0.);
    if cash.abs() > 0.5 {
        v.push(Position { symbol: "CASH".into(), label: "Broker cash".into(), description: String::new(), currency: fmt::base_ccy(), value_hkd: cash, kind: Kind::Cash, venue: Venue::Account });
    }
    v
}

/// Every investment: the broker, the fund valued through an index proxy
/// (looked through as that index), the coins, and the rest at the bundle's
/// values.
pub fn all_positions(b: &Bundle) -> Vec<Position> {
    let mut v = broker_positions(b);
    let class = |k: &str| b.arr(&["investments", "classes"]).iter()
        .find(|c| c.get("key").and_then(Value::as_str) == Some(k)).and_then(|c| c.get("value")?.as_f64());
    let fund = finance_core::base::get().fund_name();
    if let Some(f) = class(&fund).filter(|x| *x > 0. && !fund.is_empty()) {
        v.push(Position { symbol: finance_core::invest::fund_proxy(b), label: fund.clone(), description: String::new(), currency: fmt::base_ccy(),
            value_hkd: f, kind: Kind::Fund, venue: Venue::FundHouse });
    }
    if let Some(total) = class("Crypto").filter(|x| *x > 0.) {
        // split the class by each coin's share of the USD value
        let assets: Vec<(String, f64)> = b.at(&["crypto", "assets"]).as_object().map(|m| m.iter()
            .filter_map(|(k, a)| Some((k.clone(), a.get("value")?.as_f64()?))).filter(|x| x.1 > 0.).collect()).unwrap_or_default();
        let sum: f64 = assets.iter().map(|a| a.1).sum();
        if sum > 0. {
            for (coin, usd) in assets {
                v.push(Position { symbol: coin.clone(), label: coin, description: String::new(), currency: "USD".into(), value_hkd: total * usd / sum, kind: Kind::Crypto, venue: Venue::CryptoVenue });
            }
        } else {
            v.push(Position { symbol: "CRYPTO".into(), label: "Crypto".into(), description: String::new(), currency: "USD".into(), value_hkd: total, kind: Kind::Crypto, venue: Venue::CryptoVenue });
        }
    }
    for c in b.arr(&["investments", "classes"]) {
        let key = c.get("key").and_then(Value::as_str).unwrap_or("");
        if [fund.as_str(), "Crypto", "IBKR"].contains(&key) {
            continue;
        }
        if let Some(x) = c.get("value").and_then(Value::as_f64).filter(|x| *x > 0.) {
            v.push(Position { symbol: key.into(), label: key.into(), description: String::new(), currency: "unknown".into(), value_hkd: x, kind: Kind::Other, venue: Venue::Unknown });
        }
    }
    v
}

fn palette(cx: &App) -> [Hsla; 6] {
    let t = cx.theme();
    [t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5, t.muted_foreground]
}

#[derive(Clone)]
struct Slice {
    label: SharedString,
    hkd: f64,
    pct: f64,
    color: Hsla,
}

/// A donut with its legend: label, share, HKD.
fn donut(id: &'static str, title: &str, split: &[(String, f64, f64)], cx: &App) -> Div {
    let t = cx.theme();
    let colors = palette(cx);
    let slices: Vec<Slice> = split.iter().enumerate().map(|(i, (l, v, p))| Slice {
        label: l.clone().into(), hkd: *v, pct: *p, color: colors[i.min(colors.len() - 1)],
    }).collect();
    const H: f32 = 132.;
    let z = crate::zoom::get();
    let chart = PieChart::new(slices.clone()).appear(false)
        .id(id)
        .value(|s: &Slice| s.hkd.max(0.) as f32)
        .color(|s: &Slice| s.color)
        .inner_radius(H * 0.4 * 0.6 * z)
        .pad_angle(0.02)
        .tooltip_name(|s: &Slice| s.label.clone())
        .tooltip_value(|s: &Slice, _, _| format!("{} · {:.1}%", fmt::hkd(s.hkd), s.pct).into());
    let legend = v_flex().gap(px(4.)).flex_1().min_w(px(150.)).children(slices.iter().map(|s| {
        h_flex().gap(px(6.)).text_size(px(ts::CAPTION)).items_center()
            .child(ui::dot(s.color, 7.))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.label.clone()))
            .child(div().figures().text_color(t.muted_foreground).child(format!("{:.0}%", s.pct)))
            .child(div().w(px(56.)).text_right().figures().child(fmt::compact(s.hkd)))
    }));
    ui::card(cx).flex_1().min_w(px(280.)).gap_2()
        .child(div().text_size(px(ts::TITLE)).font_semibold().child(title.to_string()))
        .child(h_flex().gap_3().items_center().child(div().w(px(H)).h(px(H)).flex_shrink_0().child(chart)).child(legend))
}

/// Look-through bars: label, bar, share.
fn bars(title: &str, caption: &str, split: &[(String, f64, f64)], color: Hsla, cx: &App) -> Div {
    let max = split.iter().map(|s| s.2).fold(1e-9, f64::max);
    ui::card(cx).flex_1().min_w(px(300.)).gap_1()
        .child(h_flex().gap_2().items_baseline().child(div().text_size(px(ts::TITLE)).font_semibold().child(title.to_string())).child(ui::caption(caption.to_string(), cx)))
        .children(split.iter().map(|(l, v, p)| ui::bar_row(l, *p, max, format!("{p:.1}% · {}", fmt::compact(*v)), color, cx)))
}

/// The three donuts and the two look-throughs.
pub fn cards(e: &Exposure, cx: &App) -> Div {
    let t = cx.theme();
    v_flex().gap(px(sp::GRID))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch()
            .child(donut("exp-ccy", "By trading currency", &e.by_currency, cx))
            .child(donut("exp-venue", "By where it trades", &e.by_venue, cx))
            .child(donut("exp-kind", "By asset type", &e.by_kind, cx)))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start()
            .child(bars("Regions, looking through", &format!("approximate, {WEIGHTS_AS_OF} index weights · of {}", fmt::compact(e.looked_through_hkd)), &e.by_region, t.chart_2, cx))
            .child(bars("Sectors, looking through", &format!("approximate, {WEIGHTS_AS_OF}"), &e.by_sector, t.chart_4, cx)))
}

pub fn render(d: &Data, scope: Scope, on_scope: OnScope, cx: &App) -> Div {
    let t = cx.theme();
    let positions = match scope {
        Scope::Broker => broker_positions(&d.bundle),
        Scope::All => all_positions(&d.bundle),
    };
    if positions.is_empty() {
        return v_flex().child(ui::empty("No positions in the bundle yet — sync first", cx));
    }
    let e = exposure::exposure(&positions);
    let share = |label: &str, s: &[(String, f64, f64)]| s.iter().find(|x| x.0 == label).map(|x| x.2).unwrap_or(0.);
    let us = share("United States", &e.by_region);
    let tech = share("Information technology", &e.by_sector);
    let cash = share("Cash", &e.by_kind);
    let invested: f64 = positions.iter().filter(|p| p.kind != Kind::Cash).map(|p| p.value_hkd).sum();
    let overlap: f64 = e.overlaps.iter().map(|o| o.2).sum();
    let mut line = format!("Looking through the funds, about {us:.0}% of what's invested sits in US companies and {tech:.0}% in information technology.");
    if !e.overlaps.is_empty() {
        line.push_str(&format!(" {} track (nearly) the same index: together {}, {:.0}% of the invested money.",
            e.overlaps.iter().map(|o| format!("{} and {}", o.0, o.1)).collect::<Vec<_>>().join(", and "), fmt::hkd(overlap),
            if invested > 0. { overlap / invested * 100. } else { 0. }));
    }
    if cash >= 10. {
        line.push_str(&format!(" {cash:.0}% is uninvested cash."));
    }

    let callout = (!e.overlaps.is_empty()).then(|| {
        ui::card(cx).border_color(t.warning.opacity(0.6)).gap_2()
            .child(h_flex().gap_2().items_center().child(ui::chip("Overlap", t.warning, cx))
                .child(div().text_size(px(ts::TITLE)).font_semibold().child("Funds that hold the same thing")))
            .children(e.overlaps.iter().map(|(a, b, v, why)| {
                let held = |s: &str| positions.iter().filter(|p| p.symbol == s).map(|p| p.value_hkd).sum::<f64>();
                v_flex().gap(px(2.)).py(px(4.))
                    .child(h_flex().gap_2().flex_wrap().text_size(px(ts::BODY))
                        .child(div().font_semibold().child(format!("{a} ≈ {b}")))
                        .child(div().figures().text_color(t.muted_foreground).child(format!("{} {} + {} {} = {}", a, fmt::compact(held(a)), b, fmt::compact(held(b)), fmt::hkd(*v)))))
                    .child(note(why.to_string(), cx))
            }))
            .child(note("Holding both lines of one index adds no spread of risk; the trade-offs between them are the fund's domicile (withholding tax on dividends), its fee, and the commission at its venue.", cx))
    });
    let twice = e.held_twice.iter().map(|h| {
        let total = h.direct_hkd + h.in_funds_hkd;
        ui::card(cx).gap_1()
            .child(h_flex().gap_2().items_baseline().child(div().text_size(px(ts::TITLE)).font_semibold().child(format!("{}, held twice", h.name)))
                .child(ui::caption("single-name look-through, approximate", cx)))
            .child(div().text_size(px(ts::BODY)).child(format!("{} directly plus about {} inside the funds = {}, {:.1}% of what's invested.",
                fmt::hkd(h.direct_hkd), fmt::hkd(h.in_funds_hkd), fmt::hkd(total),
                if invested > 0. { total / invested * 100. } else { 0. })))
    }).collect::<Vec<_>>();

    // the look-through table, shown with its date: each position looked
    // through (a fund priced through an index proxy as that index), and the
    // share of the biggest stock held twice
    let top_stock = e.held_twice.first().map(|h| h.symbol.clone());
    let mut seen: Vec<&str> = Vec::new();
    let table_rows: Vec<Vec<super::Cell>> = positions.iter().filter_map(|p| {
        if seen.contains(&p.symbol.as_str()) {
            return None;
        }
        let l = p.look()?;
        seen.push(p.symbol.as_str());
        let domicile = if p.venue == Venue::FundHouse { format!("{}, as {}", p.label, l.index) } else { l.domicile.clone() };
        let mut row = vec![
            super::c(p.symbol.clone()),
            super::c(l.index.clone()),
            super::c(domicile),
            super::c(l.regions.iter().find(|r| r.0 == exposure::Region::Us).map(|r| format!("{:.0}%", r.1)).unwrap_or("0%".into())),
            super::c(l.sectors.iter().find(|r| r.0 == exposure::Sector::Tech).map(|r| format!("{:.0}%", r.1)).unwrap_or("0%".into())),
        ];
        if let Some(sym) = &top_stock {
            let w = if p.symbol.eq_ignore_ascii_case(sym) { 100. } else { l.top.iter().find(|t| t.0.eq_ignore_ascii_case(sym)).map(|t| t.1).unwrap_or(0.) };
            row.push(super::c(format!("{w:.1}%")));
        }
        Some(row)
    }).collect();
    let mut cols = vec![super::lcol("Symbol", 56.), super::flex("Tracks"), super::lcol("Domicile", 110.), super::col("US", 44.), super::col("Tech", 44.)];
    if let Some(sym) = &top_stock {
        // every name a fund's `top` lists is a known company
        cols.push(super::col(exposure::company(sym).map(|c| c.name).unwrap_or("Stock"), 56.));
    }
    let static_table = ui::card(cx).gap_2()
        .child(ui::section("The look-through table", &format!("approximate index weights, {WEIGHTS_AS_OF}"), cx).mt_0())
        .child(super::table("exp-static", &cols, table_rows, cx))
        .child(note(format!("Rounded weights in the style of the index factsheets ({WEIGHTS_AS_OF}); they drift daily, so read them as \"roughly\" and check the issuer's factsheet. {}Crypto and cash have no region or sector{}.",
            positions.iter().find(|p| p.venue == Venue::FundHouse).and_then(|p| Some(format!("{} is looked through as {} (its {} proxy). ", p.label, p.look()?.index, p.symbol))).unwrap_or_default(),
            if e.unknown_hkd > 0.5 { format!("; {} has no table row and is left out of the look-through", fmt::hkd(e.unknown_hkd)) } else { String::new() }), cx));

    v_flex().gap(px(sp::GRID))
        .child(h_flex().gap_3().items_center().flex_wrap().child(scope_switch(scope, on_scope, cx))
            .child(ui::caption(format!("{} in {} positions · trading currency is what a holding is quoted in, not what it owns", fmt::hkd(e.total_hkd), positions.len()), cx)))
        .child(takeaway(line, cx))
        .children(callout)
        .child(cards(&e, cx))
        .children(twice)
        .child(static_table)
        .when(scope == Scope::Broker, |d| d.child(note("Switch to All investments to add the funds outside the broker and the coins.", cx)))
}
