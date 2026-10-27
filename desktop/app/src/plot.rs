//! Ctrl+Enter on a search: every matching transaction as a cumulative line,
//! in a dialog (ephemeral by design -- Esc closes it, nothing to navigate).
//! Opened from the Transactions search box and from the Ctrl+K palette.

use std::collections::BTreeMap;
use std::sync::Arc;

use finance_core::Ledger;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, chart::{AreaChart, BarChart}, h_flex, v_flex};
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, Figures as _, Pt};
use crate::zoom::px;

/// One day of the running total.
#[derive(Clone)]
struct Day {
    label: SharedString,
    cum: f64,
}

/// Open the plot of `rows` (ledger indices) for `query`.
pub fn open(query: String, ledger: Arc<Ledger>, rows: Vec<usize>, window: &mut Window, cx: &mut App) {
    // per day, oldest first; amounts at the owner's share (a shared rent counts as the owner's part)
    let mut by_day: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_month: BTreeMap<String, f64> = BTreeMap::new();
    let (mut inflow, mut outflow) = (0., 0.);
    for &i in &rows {
        let Some(e) = ledger.entries.get(i) else { continue };
        let v = e.hkd.unwrap_or(0.) * e.share;
        *by_day.entry(e.date.clone()).or_default() += v;
        *by_month.entry(e.date.chars().take(7).collect()).or_default() += v;
        if v >= 0. { inflow += v } else { outflow += v }
    }
    let mut cum = 0.;
    let days: Vec<Day> = by_day.iter().map(|(d, v)| {
        cum += v;
        Day { label: d.clone().into(), cum }
    }).collect();
    let months: Vec<Pt> = by_month.iter().map(|(m, v)| Pt { label: fmt::month_label(m).into(), v: *v }).collect();
    let net = inflow + outflow;
    let span = by_month.len().max(1);
    let (first, last) = (by_day.keys().next().cloned().unwrap_or_default(), by_day.keys().last().cloned().unwrap_or_default());
    let count = rows.len();

    window.open_dialog(cx, move |dialog, _, cx| {
        let t = cx.theme();
        let tone = fmt::signed_color(net);
        let stat = |label: &str, value: String, color: Option<Hsla>| {
            v_flex().flex_1().min_w(px(120.)).gap(px(2.))
                .child(ui::eyebrow(label.to_string(), cx))
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).figures()
                    .text_color(color.unwrap_or(t.foreground)).child(value))
        };
        let line = AreaChart::new(days.clone()).appear(false)
            .x(|d: &Day| d.label.clone())
            .y(|d: &Day| d.cum)
            .stroke(tone)
            .fill(ui::soft_fill(tone, 0.22))
            .natural()
            .y_axis(true)
            .y_tick_count(4)
            .x_tick_count(5)
            .y_tick_format(fmt::compact)
            .tooltip_value(|_, _, v| fmt::signed_hkd(v).into());
        let bars = {
            let (up, down) = (fmt::money_in(), fmt::money_out());
            // one label every few months when there are many
            let margin = (months.len() / 10).max(1);
            BarChart::new(months.clone()).appear(false)
                .tick_margin(margin)
                .band(|p: &Pt| p.label.clone())
                .value(|p: &Pt| p.v)
                .value_axis(true)
                .value_tick_format(fmt::compact)
                .tooltip_value(|_, v| fmt::signed_hkd(v).into())
                .fill(move |p: &Pt, _, _, _| if p.v >= 0. { up } else { down })
        };
        dialog
            .w(px(880.))
            .title(format!("“{query}”"))
            .child(
                v_flex().gap_3()
                    .child(ui::caption(fmt::local(&format!("{count} matching transactions · {first} → {last} · amounts in HK$, your share")), cx))
                    .child(h_flex().gap_4().flex_wrap()
                        .child(stat("Net", fmt::signed_hkd(net), Some(tone)))
                        .child(stat("In", fmt::hkd(inflow), Some(fmt::money_in())))
                        .child(stat("Out", fmt::hkd(-outflow), Some(fmt::money_out())))
                        .child(stat("Per month", fmt::signed_hkd(net / span as f64), None))
                        .child(stat("Months", span.to_string(), None)))
                    .child(div().h(px(240.)).child(line))
                    .child(ui::eyebrow("By month", cx))
                    .child(div().h(px(140.)).child(bars))
                    .child(ui::caption("Esc closes", cx)),
            )
    });
}
