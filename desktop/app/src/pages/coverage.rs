//! App → Source tracking: a timeline of every data source, one bar per
//! source from its first row to its last (finance_core::coverage). A hole
//! in a bar is a statement gap; a bank behind the broker or an exchange is
//! called out above the chart, since its missing transfers count twice in
//! net worth until the statement is uploaded.
//!
//! The same page on the desktop and in the browser; the range (last 12
//! months or all time) is the app's view state. `FINANCE_DESKTOP_COVERAGE=all`
//! opens it on all time.

use std::rc::Rc;

use chrono::{Datelike, Duration, NaiveDate};
use finance_core::capabilities::Capabilities;
use finance_core::coverage::{self, Lag, Track};
use finance_core::{Bundle, Ledger};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

/// Show all time (true) or the last 12 months.
pub type OnRange = Rc<dyn Fn(bool, &mut Window, &mut App)>;

fn day(d: NaiveDate) -> String {
    d.format("%-d %b %Y").to_string()
}

fn short(d: NaiveDate, today: NaiveDate) -> String {
    if d.year() == today.year() { d.format("%-d %b").to_string() } else { d.format("%b %Y").to_string() }
}

fn ago(days: i64) -> String {
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        n => format!("{n} days ago"),
    }
}

fn group_color(group: &str, cx: &App) -> Hsla {
    let t = cx.theme();
    match group {
        "banks" => t.chart_1,
        "cards" => t.chart_2,
        "funds" => t.chart_3,
        "crypto" => t.chart_4,
        _ => t.chart_5,
    }
}

/// The page: the lag note (if any), then the timeline.
pub fn render(bundle: &Bundle, ledger: &Ledger, all_time: bool, on_range: OnRange, cx: &App) -> ui::Sections {
    let today = crate::budgets::today();
    let tracks = crate::memo::get("coverage", &today, || coverage::tracks(&Capabilities::of(bundle), ledger));
    if tracks.is_empty() {
        return ui::Sections::new().child(ui::empty_with(IconName::Inbox, "No sources with data yet",
            "Upload a bank export or connect an exchange: each source then gets a bar here.", cx));
    }
    let lags = coverage::lags(&tracks, today);
    ui::Sections::new()
        .children((!lags.is_empty()).then(|| lag_card(&lags, cx)))
        .child(timeline(&tracks, &lags, today, all_time, on_range, cx))
}

fn lag_card(lags: &[Lag], cx: &App) -> Div {
    let t = cx.theme();
    let warn = t.warning;
    let first = &lags[0];
    let names = lags.iter().map(|l| l.bank.as_str()).collect::<Vec<_>>().join(", ");
    ui::card(cx)
        .gap(px(sp::S))
        .child(h_flex().gap_2().items_center()
            .child(ui::chip(if lags.len() == 1 { "1 bank behind".to_string() } else { format!("{} banks behind", lags.len()) }, warn, cx))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Net worth may count some money twice")))
        .children(lags.iter().map(|l| ui::caption_wrap(format!(
            "{} stops on {}, {} runs to {}: {} days.", l.bank, day(l.bank_last), l.ahead, day(l.ahead_last), l.days()), cx)
            .text_color(t.foreground)))
        .child(ui::caption_wrap(format!(
            "A transfer from {names} to {} after {} is already in the investments but not yet out of the cash. Upload the newer statement and both sides line up.",
            first.ahead, day(first.bank_last)), cx))
        .child(h_flex().child(Button::new("cov-upload").small().ghost().icon(IconName::ArrowUp).label("Open Upload")
            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::app::GoUpload), cx))))
}

/// Axis ticks: (date, label) inside the window.
fn ticks(start: NaiveDate, end: NaiveDate, compact: bool) -> Vec<(NaiveDate, String)> {
    let months = (end.year() - start.year()) * 12 + end.month() as i32 - start.month() as i32;
    let mut out = Vec::new();
    if months > 36 {
        // yearly: 1 January of each year after the start
        for y in start.year() + 1..=end.year() {
            if let Some(d) = NaiveDate::from_ymd_opt(y, 1, 1) {
                out.push((d, y.to_string()));
            }
        }
        return out;
    }
    // quarterly: on quarter starts, so January (with its year) is one of them
    let step = if compact || months > 18 { 3 } else { 1 };
    let mut d = start;
    while d <= end {
        if (d.month() - 1) % step == 0 && d > start {
            out.push((d, if d.month() == 1 { d.year().to_string() } else { d.format("%b").to_string() }));
        }
        d = d.checked_add_months(chrono::Months::new(1)).unwrap_or(end + Duration::days(1));
    }
    out
}

fn timeline(tracks: &[Track], lags: &[Lag], today: NaiveDate, all_time: bool, on_range: OnRange, cx: &App) -> Div {
    let t = cx.theme().clone();
    let compact = crate::layout::compact();
    let (label_w, right_w) = if compact { (96., 70.) } else { (180., 116.) };
    let from = (!all_time).then(|| today - Duration::days(365));
    let (start, end) = coverage::window(tracks, today, from);
    let span = (end - start).num_days().max(1) as f32;
    let frac = move |d: NaiveDate| ((d - start).num_days() as f32 / span).clamp(0., 1.);
    let ticks = ticks(start, end, compact);
    let grid = ui::hairline(cx);

    // the range switch, as the Year in review's
    let seg = |id: &'static str, label: &'static str, on: bool, all: bool| {
        let on_range = on_range.clone();
        div().id(id).px(px(10.)).py(px(3.)).rounded(px(6.)).text_xs().cursor_pointer().whitespace_nowrap()
            .when(on, |d| d.bg(t.muted).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| on_range(all, w, cx))
            .child(label)
    };
    let switch = h_flex().p(px(2.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(grid)
        .child(seg("cov-12m", "12 months", !all_time, false))
        .child(seg("cov-all", "All time", all_time, true));
    let gaps: usize = tracks.iter().map(|t| t.gaps().len()).sum();
    let note = if gaps > 0 {
        format!("Each bar runs from a source's first row to its last. A break is more than {} days without a statement row.", coverage::GAP_DAYS)
    } else {
        "Each bar runs from a source's first row to its last; an exchange or the broker runs to its last sync.".into()
    };

    // the grid lines behind every track
    let lines = |h: f32| ticks.iter().map(move |(d, _)| div().absolute().top_0().h(px(h)).w(px(1.)).left(relative(frac(*d))).bg(grid)).collect::<Vec<_>>();
    let axis = h_flex()
        .child(div().w(px(label_w)).flex_shrink_0())
        .child(div().relative().flex_1().h(px(16.)).text_size(px(ts::CAPTION)).text_color(t.muted_foreground).figures()
            .children(ticks.iter().map(|(d, l)| div().absolute().top_0().left(relative(frac(*d))).pl(px(3.)).whitespace_nowrap().child(l.clone()))))
        .child(div().w(px(right_w)).flex_shrink_0());

    let mut rows = v_flex().gap(px(6.)).child(axis);
    for (key, title) in coverage::GROUPS.iter().copied().chain([("", "Other")]) {
        let mine: Vec<&Track> = tracks.iter().filter(|t| if key.is_empty() { coverage::GROUPS.iter().all(|(k, _)| *k != t.group) } else { t.group == key }).collect();
        if mine.is_empty() {
            continue;
        }
        rows = rows.child(div().pt(px(sp::S)).child(ui::eyebrow(title, cx)));
        for tr in mine {
            rows = rows.child(track_row(tr, lags, today, start, &frac, label_w, right_w, &lines, cx));
        }
    }

    ui::card(cx)
        .gap(px(sp::S))
        .child(h_flex().justify_between().items_center().gap_2().flex_wrap()
            .child(ui::caption(format!("{} to {}", day(start), day(end)), cx))
            .child(switch))
        .child(rows)
        .child(ui::caption_wrap(note, cx).pt(px(sp::S)))
}

#[allow(clippy::too_many_arguments)]
fn track_row(tr: &Track, lags: &[Lag], today: NaiveDate, start: NaiveDate, frac: &dyn Fn(NaiveDate) -> f32, label_w: f32, right_w: f32,
             lines: &dyn Fn(f32) -> Vec<Div>, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    let color = group_color(&tr.group, cx);
    let lag = lags.iter().find(|l| l.bank == tr.label);
    let age = tr.age(today);
    let (right, right_c) = match () {
        _ if !tr.active(today) => (format!("ended {}", short(tr.last, today)), t.muted_foreground),
        _ if lag.is_some() => (format!("{} behind", lag.map_or(0, Lag::days)), t.warning),
        _ if age > 31 => (ago(age), t.warning),
        _ => (ago(age), t.muted_foreground),
    };
    let rows_txt = format!("{} rows", fmt::count(tr.rows as f64));
    let mut tip = format!("{} · {} – {} · {rows_txt}", tr.label, day(tr.first), day(tr.last));
    if tr.api && tr.last > tr.last_row {
        tip.push_str(&format!(" · last row {}, synced since", day(tr.last_row)));
    }
    for (a, b) in tr.gaps() {
        tip.push_str(&format!("\nno rows {} – {}", day(a), day(b)));
    }
    if let Some(l) = lag {
        tip.push_str(&format!("\n{} has data to {}: {} days ahead", l.ahead, day(l.ahead_last), l.days()));
    }
    const H: f32 = 14.;
    let mut bar = div().relative().flex_1().h(px(H)).children(lines(H));
    let ended_before = tr.last < start;
    if ended_before {
        bar = bar.child(div().absolute().top_0().left_0().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).whitespace_nowrap()
            .child(format!("no data since {}", short(tr.last, today))));
    }
    for (a, b) in tr.runs.iter().copied().filter(|(_, b)| *b >= start) {
        let (x0, x1) = (frac(a), frac(b));
        bar = bar.child(div().absolute().top(px(2.)).h(px(H - 4.)).rounded(px(3.)).bg(color)
            .when(!tr.active(today), |d| d.opacity(0.45))
            .left(relative(x0)).w(relative((x1 - x0).max(0.004))));
    }
    // the stretch it is behind: where the double counting lives
    if let Some(l) = lag {
        let (x0, x1) = (frac(l.bank_last), frac(l.ahead_last));
        bar = bar.child(div().absolute().top(px(2.)).h(px(H - 4.)).rounded(px(3.)).border_1().border_color(t.warning)
            .bg(t.warning.opacity(0.18)).left(relative(x0)).w(relative((x1 - x0).max(0.004))));
    }
    let label = v_flex().w(px(label_w)).flex_shrink_0().pr(px(8.)).min_w_0()
        .child(div().text_sm().whitespace_nowrap().overflow_hidden().text_ellipsis().child(tr.label.clone()))
        .when(!crate::layout::compact(), |d| d.child(ui::caption(format!("since {}", short(tr.first, today)), cx)));
    h_flex()
        .id(SharedString::from(format!("cov-{}", tr.key)))
        .items_center()
        .child(label)
        .child(bar)
        .child(div().w(px(right_w)).flex_shrink_0().text_right().text_size(px(ts::CAPTION)).text_color(right_c).figures()
            .whitespace_nowrap().overflow_hidden().text_ellipsis().child(right))
        .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
}

#[cfg(test)]
mod tests {
    use super::{NaiveDate, ticks};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn ticks_are_years_over_a_long_span_and_months_over_a_short_one() {
        let y = ticks(d("2020-02-01"), d("2026-10-29"), false);
        assert_eq!(y.first().map(|t| t.1.as_str()), Some("2021"));
        assert_eq!(y.len(), 6);
        let m = ticks(d("2025-10-01"), d("2026-10-29"), false);
        assert_eq!(m.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(),
                   ["Nov", "Dec", "2026", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct"]);
        let q = ticks(d("2025-10-01"), d("2026-10-29"), true);
        assert_eq!(q.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(), ["2026", "Apr", "Jul", "Oct"]);
    }
}
