//! Deep dive › Returns: what the value is made of. Money in, then what the
//! markets did, what the currencies did against HKD, and what fees took,
//! over time; for the broker (daily) or for every investment (monthly).

use std::rc::Rc;

use finance_core::invest::MonthSplit;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use super::{c, col, flex, note, scope_switch, signed_pct, table, takeaway, tone, Data, OnScope, Scope};
use crate::fmt;
use crate::linechart::{self, Chart, Series};
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

/// Months of the all-investments chart.
const MONTHS: usize = 24;

pub fn render(d: &Data, scope: Scope, on_scope: OnScope, cx: &App) -> Div {
    let head = h_flex().gap_3().items_center().flex_wrap().child(scope_switch(scope, on_scope, cx))
        .child(ui::caption(match scope {
            Scope::Broker => "IBKR, daily statement values",
            Scope::All => "all your investments, month-ends",
        }, cx));
    let body = match scope {
        Scope::Broker => broker(d, cx),
        Scope::All => everything(d, cx),
    };
    v_flex().gap(px(sp::GRID)).child(head).child(body)
}

/// Money in, then each part as a signed bar, then the value.
fn breakdown(parts: &[(&str, f64, Hsla, &str)], money_in: f64, value: f64, cx: &App) -> Div {
    let t = cx.theme();
    let max = parts.iter().map(|p| p.1.abs()).fold(1e-9, f64::max);
    let row = |label: &str, explain: &str, amount: String, bar: Option<(f64, Hsla)>, strong: bool| {
        h_flex().gap_3().py(px(6.)).items_center().text_size(px(ts::BODY))
            .child(v_flex().w(px(150.)).flex_shrink_0()
                .child(div().when(strong, |d| d.font_semibold()).child(label.to_string()))
                .when(!explain.is_empty(), |d| d.child(ui::caption(explain.to_string(), cx))))
            .child(div().flex_1().min_w(px(60.)).h(px(8.)).rounded_full().bg(t.muted.opacity(0.6))
                .when_some(bar, |d, (frac, color)| d.child(div().h_full().rounded_full().bg(color).w(relative(frac.clamp(0.01, 1.) as f32)))))
            .child(div().w(px(110.)).flex_shrink_0().text_right().figures().when(strong, |d| d.font_semibold()).child(amount))
    };
    ui::card(cx).gap_0()
        .child(row("Money in", "deposits minus withdrawals", fmt::hkd(money_in), None, false))
        .children(parts.iter().map(|(label, v, color, explain)| row(label, explain, fmt::signed_hkd(*v), Some((v.abs() / max, *color)), false)))
        .child(div().h(px(1.)).bg(ui::hairline(cx)))
        .child(row("Value now", "", fmt::hkd(value), None, true))
}

fn day_ticks(dates: &[String]) -> Vec<(usize, SharedString)> {
    let n = dates.len();
    let step = (n / 5).max(1);
    (0..n).filter(|i| i % step == 0).map(|i| (i, SharedString::from(super::day(&dates[i])))).collect()
}

fn broker(d: &Data, cx: &App) -> Div {
    let t = cx.theme();
    let Some(s) = d.split.last() else {
        return v_flex().child(ui::empty("No IBKR statement days in the bundle yet — sync first", cx));
    };
    let first = d.split.first().map(|x| x.date.clone()).unwrap_or_default();
    let line = format!(
        "Of the broker's {} gain since {}, the market added {}, currency moves {}, and commissions took {}; {} is cash interest, conversion spreads and pricing differences.",
        fmt::signed_hkd(s.gain()), super::day(&first), fmt::signed_hkd(s.market), fmt::signed_hkd(s.fx), fmt::hkd(s.fees), fmt::signed_hkd(s.other));
    let (cm, cf, cfee, co) = (fmt::money_in(), t.chart_3, fmt::money_out(), t.muted_foreground);
    let parts = [
        ("Market", s.market, cm, "prices, in each holding's currency"),
        ("Currency", s.fx, cf, fmt::local_static("USD and GBP against HKD since each buy")),
        ("Fees", -s.fees, cfee, "commissions"),
        ("Other", s.other, co, "interest, FX spread, marks vs closes"),
    ];
    let dates: Vec<String> = d.split.iter().map(|x| x.date.clone()).collect();
    let chart = Chart {
        id: "ret-broker".into(),
        labels: dates.iter().map(|x| SharedString::from(x.clone())).collect(),
        x_ticks: day_ticks(&dates),
        series: vec![
            Series::line("Gain", d.split.iter().map(|x| Some(x.gain())).collect(), t.foreground).width(2.),
            Series::line("Market", d.split.iter().map(|x| Some(x.market)).collect(), cm).width(1.5),
            Series::line("Currency", d.split.iter().map(|x| Some(x.fx)).collect(), cf).width(1.5),
            Series::line("Fees", d.split.iter().map(|x| Some(-x.fees)).collect(), cfee).width(1.5),
            Series::line("Other", d.split.iter().map(|x| Some(x.other)).collect(), co).dashed().width(1.2),
        ],
        dots: vec![],
        log: false,
        height: 240.,
        y_fmt: Rc::new(fmt::compact),
    };
    let legend = h_flex().gap_3().flex_wrap()
        .child(ui::legend(t.foreground, "gain", "", cx)).child(ui::legend(cm, "market", "", cx))
        .child(ui::legend(cf, "currency", "", cx)).child(ui::legend(cfee, "fees", "", cx)).child(ui::legend(co, "other", "", cx));
    v_flex().gap(px(sp::GRID))
        .child(takeaway(line, cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start()
            .child(div().flex_1().min_w(px(320.)).child(breakdown(&parts, s.money_in, s.value, cx)))
            .child(div().flex_1().flex_grow(1.4).min_w(px(340.)).child(
                ui::chart_card_with("The gain, split", &fmt::local("HKD · daily"), legend, 320., linechart::render(chart, cx), cx))))
        .child(note(fmt::local("Holdings are valued at each day's close in their own currency (the same as the broker's marks within a few HK$), cost at the price and exchange rate of each buy. Market = price moves at the buy's rate; Currency = what USD and GBP did against HKD on those units since. Rates between months are interpolated; each trade's rate and the snapshot's are exact."), cx))
}

fn everything(d: &Data, cx: &App) -> Div {
    let t = cx.theme();
    let rows: Vec<&MonthSplit> = d.all.iter().filter(|m| m.money_in > 0. || m.value > 0.).collect();
    let rows: Vec<&MonthSplit> = rows[rows.len().saturating_sub(MONTHS)..].to_vec();
    let Some(m) = rows.last().copied() else {
        return v_flex().child(ui::empty("No investment history in the bundle yet — sync first", cx));
    };
    let gain = m.value - m.money_in;
    let line = format!(
        "All investments are worth {}: {} put in, {} from the markets (funds, coins and the broker's holdings together), {} from the broker's currency moves, and {} in broker commissions.",
        fmt::hkd(m.value), fmt::hkd(m.money_in), fmt::signed_hkd(m.market), fmt::signed_hkd(m.fx), fmt::hkd(m.fees));
    let (cm, cf, cfee) = (fmt::money_in(), t.chart_3, fmt::money_out());
    let parts = [
        ("Market", m.market, cm, "funds, coins, broker prices"),
        ("Currency", m.fx, cf, "the broker's USD and GBP lines"),
        ("Fees", -m.fees, cfee, "broker commissions"),
    ];
    let months: Vec<String> = rows.iter().map(|r| r.month.clone()).collect();
    let step = (months.len() / 6).max(1);
    let ticks: Vec<(usize, SharedString)> = (0..months.len()).filter(|i| i % step == 0).map(|i| (i, SharedString::from(fmt::month_label(&months[i])))).collect();
    let labels: Vec<SharedString> = months.iter().map(|x| SharedString::from(fmt::month_label(x))).collect();
    let value_chart = Chart {
        id: "ret-all-value".into(),
        labels: labels.clone(),
        x_ticks: ticks.clone(),
        series: vec![
            Series::line("Value", rows.iter().map(|r| Some(r.value)).collect(), t.primary).width(1.8).fill(t.primary.opacity(0.12)),
            Series::line("Money in", rows.iter().map(|r| Some(r.money_in)).collect(), t.muted_foreground).dashed().width(1.5),
        ],
        dots: vec![],
        log: false,
        height: 220.,
        y_fmt: Rc::new(fmt::compact),
    };
    let parts_chart = Chart {
        id: "ret-all-parts".into(),
        labels,
        x_ticks: ticks,
        series: vec![
            Series::line("Gain", rows.iter().map(|r| Some(r.value - r.money_in)).collect(), t.foreground).width(2.),
            Series::line("Market", rows.iter().map(|r| Some(r.market)).collect(), cm).width(1.5),
            Series::line("Currency", rows.iter().map(|r| Some(r.fx)).collect(), cf).width(1.5),
            Series::line("Fees", rows.iter().map(|r| Some(-r.fees)).collect(), cfee).width(1.5),
        ],
        dots: vec![],
        log: false,
        height: 220.,
        y_fmt: Rc::new(fmt::compact),
    };

    // by class, from the bundle
    let classes = d.bundle.arr(&["investments", "classes"]);
    let g = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.);
    let class_rows: Vec<Vec<super::Cell>> = classes.iter().map(|k| vec![
        c(k.get("label").and_then(Value::as_str).unwrap_or("?").to_string()),
        c(fmt::int(g(k, "invested"))),
        c(fmt::int(g(k, "value"))),
        tone(fmt::signed_hkd(g(k, "gain")).replace(&fmt::sym_space(), ""), fmt::signed_color(g(k, "gain"))),
        signed_pct(g(k, "roi_pct"), 1),
    ]).collect();
    let class_cols = [flex("Class"), col("Money in", 90.), col("Value", 90.), col("Gain", 84.), col("Return", 64.)];
    let platform = d.bundle.f64(&["costs", "paid", "flexinvest_platform_hkd"]);

    v_flex().gap(px(sp::GRID))
        .child(takeaway(line, cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start()
            .child(div().flex_1().min_w(px(320.)).child(breakdown(&parts, m.money_in, m.value, cx)))
            .child(div().flex_1().min_w(px(320.)).child(ui::card(cx).gap_2()
                .child(ui::section("By class", &format!("gain {} · HKD", fmt::signed_hkd(gain)), cx).mt_0())
                .child(table("ret-classes", &class_cols, class_rows, cx)))))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap()
            .child(div().flex_1().min_w(px(320.)).child(ui::chart_card_with("Value vs money in", &fmt::local("HKD · month-ends"),
                h_flex().gap_3().child(ui::legend(t.primary, "value", "", cx)).child(ui::legend(t.muted_foreground, "money in", "", cx)),
                290., linechart::render(value_chart, cx), cx)))
            .child(div().flex_1().min_w(px(320.)).child(ui::chart_card_with("The gain, split", &fmt::local("HKD · month-ends"),
                h_flex().gap_3().flex_wrap().child(ui::legend(cm, "market", "", cx)).child(ui::legend(cf, "currency", "", cx)).child(ui::legend(cfee, "fees", "", cx)),
                290., linechart::render(parts_chart, cx), cx))))
        .child(note(format!(
            "Only the broker has a currency split here: a fund priced in HKD has its currency moves inside its price, and crypto is priced in USD, which the HKD is pegged to. Past broker months use the broker's month-end value (the bundle holds them at cost). {}",
            platform.map(|p| format!("FlexInvest's platform fee ({} so far) is paid from the bank, outside these numbers.", fmt::hkd(p))).unwrap_or_default()), cx))
}

