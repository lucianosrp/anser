//! Spending, one month at a time: the month's numbers, where it went, its
//! biggest outflows, against the 12-month picture. ←/→ (or [ and ]) and the
//! arrow buttons move the month; categories and rows drill into the table.

use std::rc::Rc;

use finance_core::filter::{self, Filter};
use finance_core::{Bundle, Entry, Ledger, drill, recurring};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    chart::BarChart,
    h_flex,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::category;
use crate::fmt;
use crate::ui::{self, Pt};
use crate::zoom::px;

/// Open the detail sheet for a ledger index.
pub type OnPick = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// Show the transactions table with a search.
pub type OnSearch = Rc<dyn Fn(String, &mut Window, &mut App)>;

const BIGGEST: usize = 10;

/// Switch tabs (the app keeps the choice).
pub type OnTab = Rc<dyn Fn(Tab, &mut Window, &mut App)>;

/// The page's tabs: the month, and the Home costs (only when some rows are
/// in a home category).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Month,
    Home,
}

impl Tab {
    pub const ALL: [Tab; 2] = [Tab::Month, Tab::Home];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Month => "This month",
            Tab::Home => "Home",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tab::Month => "month",
            Tab::Home => "home",
        }
    }

    pub fn parse(s: &str) -> Option<Tab> {
        let s = s.trim().to_ascii_lowercase();
        Tab::ALL.into_iter().find(|t| t.key() == s || t.label().eq_ignore_ascii_case(&s))
    }

    /// Dev and screenshot aid: `FINANCE_DESKTOP_SPEND_TAB=home` opens on
    /// that tab; in the browser `?spend_tab=home`.
    pub fn from_env() -> Tab {
        crate::app::dev_aid("FINANCE_DESKTOP_SPEND_TAB").and_then(|v| Tab::parse(&v)).unwrap_or_default()
    }
}

/// The tab switch: a segmented control (as Investments', scrolling sideways
/// when it doesn't fit).
fn tab_switch(tab: Tab, on_tab: OnTab, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let compact = crate::layout::compact();
    let seg = |this: Tab| {
        let on = tab == this;
        let on_tab = on_tab.clone();
        div().id(SharedString::from(format!("spend-tab-{}", this.key())))
            .flex_shrink_0().px(px(12.)).rounded(px(6.)).cursor_pointer().whitespace_nowrap()
            .flex().items_center()
            .when(compact, |d| d.h(px(crate::layout::TOUCH - 8.)).text_size(px(ui::ts::BODY + 1.)))
            .when(!compact, |d| d.py(px(4.)).text_sm())
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| if !on { on_tab(this, w, cx) })
            .child(this.label())
    };
    let bar = h_flex().flex_shrink_0().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx))
        .children(Tab::ALL.map(seg));
    h_flex().id("spend-tabs").w_full().overflow_x_scroll().restrict_scroll_to_axis().child(bar)
}

fn month_of(v: &serde_json::Value) -> &str {
    v.get("month").and_then(|m| m.as_str()).unwrap_or("")
}

fn num(v: &serde_json::Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.)
}

/// Every month the trend knows, oldest first (the running one included).
pub fn months(b: &Bundle) -> Vec<String> {
    b.arr(&["spending", "trend"]).iter().map(|t| month_of(t).to_string()).filter(|m| !m.is_empty()).collect()
}

/// The month the page opens on: the last completed one.
pub fn default_month(b: &Bundle) -> Option<String> {
    b.completed_trend().last().map(|t| month_of(t).to_string())
}

/// The month `step` months away from `from` within `months`, clamped.
pub fn step_month(months: &[String], from: &str, step: i32) -> Option<String> {
    let i = months.iter().position(|m| m == from)? as i32;
    let j = (i + step).clamp(0, months.len() as i32 - 1);
    months.get(j as usize).cloned()
}

/// The last completed month of a trend.
fn fallback_month(done: &[&serde_json::Value]) -> Option<String> {
    done.last().map(|t| month_of(t).to_string())
}

/// The page's filter strip: which accounts and categories count.
pub struct Filtering<'a> {
    pub query: &'a str,
    pub data: crate::filterbar::Data,
    pub on_query: crate::filterbar::OnQuery,
    /// "Categorize Other": shown when some rows have no category
    pub on_categorize: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
}

/// The server's monthly trend and categories, or -- with a filter on -- the
/// same shapes worked out from the ledger rows the filter keeps.
fn figures(b: &Bundle, ledger: &Ledger, keep: Option<&dyn Fn(usize, &Entry) -> bool>) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let Some(keep) = keep else {
        return (b.arr(&["spending", "trend"]).to_vec(), b.arr(&["spending", "by_category"]).to_vec());
    };
    let fixed_names: Vec<String> = b.arr(&["spending", "fixed_merchants"]).iter()
        .filter_map(|m| m.get("merchant")?.as_str())
        .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
        .collect();
    let by = filter::month_figures(ledger, keep, &fixed_names);
    let mut trend = Vec::new();
    let mut cats = Vec::new();
    for m in months(b) {
        let f = by.get(&m).cloned().unwrap_or_default();
        trend.push(serde_json::json!({
            "month": m, "income": f.income, "spend": f.spend, "net": f.income - f.spend, "fixed": f.fixed,
            "variable": f.spend - f.fixed, "invested": f.invested, "invested_crypto": 0.0,
        }));
        for (c, v) in &f.by_category {
            cats.push(serde_json::json!({"month": m, "category": c, "hkd": v}));
        }
    }
    (trend, cats)
}

#[allow(clippy::too_many_arguments)]
pub fn render(b: &Bundle, ledger: &Ledger, selected: Option<&str>, tab: Tab, on_tab: OnTab, filtering: Filtering, on_pick: OnPick, on_search: OnSearch,
              on_dive: crate::dive::OnDive, whatif: &Entity<gpui_kit::component::input::InputState>, cx: &App) -> ui::Sections {
    let t = cx.theme();
    // the tab bar only when the Home tab has something to show
    // (a saved home setup shows it too: its cards need no rows)
    let has_home = *crate::memo::get("spending.has_home", &(), || finance_core::home::has_home_rows(ledger))
        || super::spending_home::setup().as_ref().as_ref().is_some_and(finance_core::home::HomeSetup::is_set);
    let tabs = has_home.then(|| tab_switch(tab, on_tab, cx));
    if has_home && tab == Tab::Home {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let (gap, parts) = super::spending_home::render(ledger, b, &today, whatif, on_dive, cx).into_parts();
        return ui::Sections::new().gap(gap).children(tabs).children(parts);
    }
    let flt = Filter::parse(filtering.query);
    let active = flt.has_filters();
    let compiled = flt.compile(&filtering.data.trips);
    let keep_fn = |i: usize, e: &Entry| compiled.matches(i, e);
    let keep: Option<&dyn Fn(usize, &Entry) -> bool> = if active { Some(&keep_fn) } else { None };
    // the figures, the biggest rows, the commitments and the flows are kept
    // per filter (and month) between frames (memo.rs)
    let query = filtering.query.to_string();
    let figs = crate::memo::get("spending.figures", &query, || figures(b, ledger, keep));
    let (trend_owned, cats_owned) = &*figs;
    let trend: &[serde_json::Value] = trend_owned;
    let current = chrono::Utc::now().format("%Y-%m").to_string();
    let done: Vec<&serde_json::Value> = trend.iter().filter(|t| month_of(t) != current).collect();
    let pass = |i: usize, e: &Entry| keep.is_none_or(|k| k(i, e));
    let bar = crate::filterbar::render("spend", filtering.query, crate::filterbar::SPENDING, true, &filtering.data, filtering.on_query.clone(), cx);
    let strip = v_flex().gap_2().child(bar).when(active, |d| {
        let oq = filtering.on_query.clone();
        d.child(h_flex().gap_2().items_center().px_3().py_2().rounded(t.radius_lg)
            .bg(t.warning.opacity(0.12)).border_1().border_color(t.warning.opacity(0.4))
            .child(Icon::new(gpui_kit::assets::IconName::ListFilter).small().text_color(t.warning))
            .child(div().flex_1().min_w_0().text_size(px(12.)).child(
                "Filtered: every number, chart and the Sankey below counts only the bank and card rows the filter keeps, worked out from the ledger (so totals can differ a little from the unfiltered view)."))
            .child(Button::new("spend-filter-off").xsmall().ghost().label("Show everything")
                .on_click(move |_, window, cx| oq(String::new(), window, cx))))
    });
    let fallback = default_month(b);
    let Some(month) = selected.map(str::to_string).or(fallback) else {
        return ui::Sections::new().gap(16.).children(tabs).child(strip).child(ui::empty("No spending data yet — sync first", cx));
    };
    let Some(ix) = trend.iter().position(|t| month_of(t) == month) else {
        return ui::Sections::new().gap(16.).children(tabs).child(strip).child(ui::empty("No spending data for that month", cx));
    };
    let cur = &trend[ix];
    let running = month == chrono::Utc::now().format("%Y-%m").to_string();
    let (income, spend, fixed, variable) = (num(cur, "income"), num(cur, "spend"), num(cur, "fixed"), num(cur, "variable"));
    let net = income - spend;
    let prev_spend = ix.checked_sub(1).map(|p| num(&trend[p], "spend"));

    // ---- month switcher ----
    let label = fmt::month_label(&month);
    let first = ix == 0;
    let last = ix + 1 >= trend.len();
    let nav = h_flex()
        .gap_2()
        .child(Button::new("month-prev").small().ghost().icon(IconName::ChevronLeft).tooltip(ui::keyed("Previous month", "← or ["))
            .disabled(first)
            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::app::PrevMonth), cx)))
        .child(div().min_w(px(84.)).text_center().text_lg().font_semibold().child(label.clone()))
        .child(Button::new("month-next").small().ghost().icon(IconName::ChevronRight).tooltip(ui::keyed("Next month", "→ or ]"))
            .disabled(last)
            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::app::NextMonth), cx)))
        .when(running || ui::show_shortcuts(), |d| d.child(div().text_xs().text_color(t.muted_foreground)
            .child(if running { "month to date — still running" } else { "← → or [ ] to move" })))
        .when(active, |d| d.child(ui::chip("filtered", t.warning, cx)));

    // ---- stat cards ----
    let vs_prev = prev_spend.filter(|p| *p > 0.).map(|p| format!(" · {}% vs prev", fmt::signed((spend - p) / p * 100., 0))).unwrap_or_default();
    // four in a row when wide, two by two at half-screen width
    let cards = h_flex()
        .gap_4()
        .flex_wrap()
        .items_stretch()
        .children([
            ui::stat("Income", fmt::hkd(income), label.clone(), Some(fmt::money_in()), cx),
            ui::stat("Spending", fmt::hkd(spend),
                format!("fixed {} · variable {}{vs_prev}", fmt::compact(fixed), fmt::compact(variable)), Some(fmt::money_out()), cx),
            ui::stat("Saved", fmt::signed_hkd(net),
                if income > 0. { format!("{:.0}% of income", net / income * 100.) } else { String::new() },
                Some(fmt::signed_color(net)), cx),
            ui::stat("Invested", fmt::hkd(num(cur, "invested") + num(cur, "invested_crypto")),
                if active { "sent to investments from these accounts" } else { "funds, broker and crypto" }, None, cx),
        ].map(|d| d.min_w(px(220.))));

    // ---- 12-month chart, the selected month highlighted ----
    // the window ends at the last completed month, or later/earlier to keep
    // the selected one in view
    let last_done = trend.iter().rposition(|t| Some(month_of(t)) == fallback_month(&done).as_deref()).unwrap_or(ix);
    let end = if ix > last_done { ix } else if ix + 12 <= last_done { (ix + 6).min(trend.len() - 1) } else { last_done };
    let start = (end + 1).saturating_sub(12);
    let pts: Vec<Pt> = trend[start..=end].iter()
        .map(|t| Pt { label: fmt::month_label(month_of(t)).into(), v: num(t, "spend") })
        .collect();
    let sel: SharedString = label.clone().into();
    let (on, off) = (t.primary, t.primary.opacity(0.35));
    let chart = BarChart::new(pts).appear(false)
        .band(|p: &Pt| p.label.clone())
        .value(|p: &Pt| p.v)
        .value_axis(true)
        .tick_margin(1)
        .value_tick_format(|v| fmt::compact(v))
        .tooltip_value(|_, v| fmt::hkd(v).into())
        .fill(move |p: &Pt, _, _, _| if p.label == sel { on } else { off });

    // ---- categories of the month, biggest first (click: the category's deep dive) ----
    let mut cats: Vec<(String, f64)> = cats_owned.iter()
        .filter(|c| month_of(c) == month)
        .filter_map(|c| Some((c.get("category")?.as_str()?.to_string(), c.get("hkd")?.as_f64()?)))
        .filter(|(_, v)| *v > 0.5)
        .collect();
    cats.sort_by(|a, b| b.1.total_cmp(&a.1));
    let max = cats.first().map(|c| c.1).unwrap_or(0.);
    let cat_rows = v_flex().children(cats.iter().take(14).enumerate().map(|(i, (c, v))| {
        let (on_dive, c2) = (on_dive.clone(), c.clone());
        div()
            .id(("cat", i))
            .px_1()
            .rounded(t.radius)
            .cursor_pointer()
            .hover(|s| s.bg(t.accent))
            .tooltip(|w, cx| Tooltip::new("Open this category's deep dive").build(w, cx))
            .on_click(move |_, window, cx| on_dive(crate::dive::Dive::Category(c2.clone()), window, cx))
            .child(h_flex().gap_2().items_center()
                .child(category::tile(c, 22., t.chart_1))
                .child(div().flex_1().min_w_0().child(ui::bar_row(c, *v, max, fmt::hkd(*v), t.chart_1, cx))))
    }));
    let cat_card = if cats.is_empty() {
        ui::card(cx).child(div().text_sm().text_color(t.muted_foreground).child("No categorized spending this month"))
    } else {
        ui::card(cx).child(cat_rows)
    };

    // ---- biggest single outflows (click: detail) ----
    let big = crate::memo::get("spending.biggest", &(&query, &month), || drill::biggest_spending_where(ledger, &month, BIGGEST, pass));
    let big_rows = v_flex().children(big.iter().filter_map(|&i| {
        let e = ledger.entries.get(i)?;
        let mine = drill::my_hkd(e).unwrap_or(e.amount);
        let on_pick = on_pick.clone();
        let key = drill::merchant_key(&e.description);
        let on_dive = on_dive.clone();
        let (fg, accent) = (t.foreground, t.primary);
        // the merchant's name opens its deep dive; the rest of the row, the detail
        let name = div().id(("big-merchant", i)).text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap()
            .text_color(fg).child(e.description.clone())
            .when(!key.is_empty(), |d| d.cursor_pointer().hover(move |s| s.text_color(accent).underline())
                .tooltip(|w, cx| Tooltip::new("Open this merchant's deep dive").build(w, cx))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_dive(crate::dive::Dive::Merchant(key.clone()), window, cx)
                }));
        Some(
            h_flex()
                .id(("big", i))
                .gap_3()
                .py_1()
                .px_1()
                .rounded(t.radius)
                .cursor_pointer()
                .hover(|s| s.bg(t.accent))
                .on_click(move |_, window, cx| on_pick(i, window, cx))
                .child(div().w(px(44.)).text_xs().text_color(t.muted_foreground).child(e.date.get(5..).unwrap_or("").to_string()))
                .when_some(crate::logo::tile(crate::logo::for_entry(e), 24., cx), |d, l| d.child(l))
                .child(v_flex().flex_1().min_w_0()
                    .child(h_flex().min_w_0().child(name))
                    .child(category::label_with(&e.category,
                        if e.share < 1. { format!("{} · your {:.0}%", e.category, e.share * 100.) } else { e.category.clone() }, cx).text_xs()))
                .child(div().w(px(104.)).text_sm().text_right().whitespace_nowrap().text_color(fmt::money_out()).child(fmt::hkd(-mine))),
        )
    }));
    let big_card = if big.is_empty() {
        ui::card(cx).child(div().text_sm().text_color(t.muted_foreground).child("No spending rows in the local ledger for this month"))
    } else {
        ui::card(cx).child(big_rows)
    };
    let fixed = fixed_section(b, ledger, &trend[start..=end], &done, &month, &query, &pass, cx);
    let tables = summary_tables(&done, &month, cx);
    let flows = flow_section(trend, ledger, &month, &query, &pass, cx);
    let categorize = filtering.on_categorize.clone().map(|f| Button::new("spend-categorize").xsmall().ghost().icon(gpui_kit::assets::IconName::Wand)
        .label("Categorize Other →").on_click(move |_, window, cx| f(window, cx)));
    let all_q = format!("{} {month} out", flt.query()).trim().to_string();
    let show_all = Button::new("big-all").xsmall().ghost().label("all outflows →")
        .on_click(move |_, window, cx| on_search(all_q.clone(), window, cx));

    ui::Sections::new()
        .gap(16.)
        .children(tabs)
        .child(strip)
        .child(nav)
        .child(cards)
        .child(ui::chart_card("Spending per month", "12 months · selected month highlighted", 260., chart, cx))
        .child(flows)
        .child(fixed)
        .child(
            h_flex()
                .gap_4()
                .items_start()
                .flex_wrap()
                .child(v_flex().flex_1().min_w(crate::layout::col(340.)).gap_2()
                    .child(h_flex().justify_between().items_center().child(ui::section("Where it went", &label, cx)).children(categorize))
                    .child(cat_card))
                .child(v_flex().flex_1().min_w(crate::layout::col(340.)).gap_2()
                    .child(h_flex().justify_between().child(ui::section("Biggest this month", "spending only, your share", cx)).child(show_all))
                    .child(big_card)),
        )
        .child(tables)
}

#[cfg(test)]
mod tests {
    use super::step_month;

    #[test]
    fn steps_clamp() {
        let ms: Vec<String> = ["2026-07", "2026-08", "2026-09"].iter().map(|s| s.to_string()).collect();
        assert_eq!(step_month(&ms, "2026-08", -1).as_deref(), Some("2026-07"));
        assert_eq!(step_month(&ms, "2026-07", -1).as_deref(), Some("2026-07"));
        assert_eq!(step_month(&ms, "2026-09", 1).as_deref(), Some("2026-09"));
        assert_eq!(step_month(&ms, "2025-01", 1), None);
    }
}

/// Fixed vs variable: the committed part of each month (rent, loans, bills,
/// subscriptions, steady recurring charges) stacked under everything else,
/// three tiles for the selected month, and the commitments themselves --
/// active ones first, ended ones (`recurring::is_ended`: not seen for
/// ENDED_AFTER_DAYS) folded away.
/// (merchant, category, monthly average, last paid)
type Commitment = (String, String, f64, String);

/// The commitments: when was each last seen in the ledger? (the server picks
/// the merchants; "ended" is the rule the Subscriptions page uses too)
fn commitments(b: &Bundle, ledger: &Ledger, pass: &dyn Fn(usize, &Entry) -> bool) -> (Vec<Commitment>, Vec<Commitment>) {
    let newest = ledger.entries.first().map(|e| e.date.clone()).unwrap_or_default();
    let mut active = Vec::new();
    let mut ended = Vec::new();
    for fm in b.arr(&["spending", "fixed_merchants"]) {
        let Some(name) = fm.get("merchant").and_then(|x| x.as_str()) else { continue };
        let key = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let mut paid = ledger.entries.iter().enumerate().filter(|(_, e)| e.amount < 0. && e.description.to_lowercase().starts_with(&key)).peekable();
        let seen = paid.peek().is_some();
        let last = paid.find(|(i, e)| pass(*i, e)).map(|(_, e)| e.date.clone());
        if seen && last.is_none() {
            continue; // paid only from accounts (or as categories) the filter leaves out
        }
        let row = (name.to_string(), fm.get("category").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                   num(fm, "avg_hkd"), last.unwrap_or_default());
        if recurring::is_ended(&row.3, &newest, recurring::Cadence::Monthly.days()) { ended.push(row) } else { active.push(row) }
    }
    (active, ended)
}

#[allow(clippy::too_many_arguments)]
fn fixed_section(b: &Bundle, ledger: &Ledger, window: &[serde_json::Value], done: &[&serde_json::Value], month: &str,
                 query: &str, pass: &dyn Fn(usize, &Entry) -> bool, cx: &App) -> Div {
    let t = cx.theme();
    let (fixed_c, var_c) = (t.chart_2, t.primary.opacity(0.45));
    let max = window.iter().map(|m| num(m, "fixed") + num(m, "variable")).fold(0., f64::max);
    const H: f32 = 150.;
    let bars = h_flex().h(px(H + 22.)).items_end().gap_1().children(window.iter().enumerate().map(|(i, m)| {
        let (f, v) = (num(m, "fixed"), num(m, "variable"));
        let scale = |x: f64| if max > 0. { (x / max) as f32 * H } else { 0. };
        let selected = month_of(m) == month;
        let tip = format!("{} · fixed {} · variable {} · fixed share {:.0}%", fmt::month_label(month_of(m)),
            fmt::hkd(f), fmt::hkd(v), if f + v > 0. { f / (f + v) * 100. } else { 0. });
        v_flex()
            .id(("fv", i))
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_1()
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(34.))
                    .rounded_t(px(3.))
                    .overflow_hidden()
                    .when(!selected, |d| d.opacity(0.75))
                    .child(div().h(px(scale(v))).bg(var_c))
                    .child(div().h(px(scale(f))).bg(fixed_c)),
            )
            .child(div().text_xs().text_color(if selected { t.foreground } else { t.muted_foreground })
                .whitespace_nowrap().child(fmt::month_label(month_of(m))))
    }));

    let sel = window.iter().find(|m| month_of(m) == month);
    let (f, v) = sel.map(|m| (num(m, "fixed"), num(m, "variable"))).unwrap_or((0., 0.));
    let recent: Vec<_> = done.iter().rev().take(6).collect();
    let avg_fixed = if recent.is_empty() { 0. } else { recent.iter().map(|m| num(m, "fixed")).sum::<f64>() / recent.len() as f64 };
    let tiles = h_flex().gap_4().flex_wrap().items_start()
        .child(ui::stat("Fixed", fmt::hkd(f), fmt::month_label(month), None, cx).min_w(px(200.)))
        .child(ui::stat("Fixed, 6-month average", fmt::hkd(avg_fixed), "completed months", None, cx).min_w(px(200.)))
        .child(ui::stat("Fixed share", if f + v > 0. { format!("{:.0}%", f / (f + v) * 100.) } else { "—".into() },
            format!("variable {}", fmt::hkd(v)), None, cx).min_w(px(200.)));

    let commitments = crate::memo::get("spending.commitments", &query, || commitments(b, ledger, pass));
    let (active, ended) = &*commitments;
    let active_total: f64 = active.iter().map(|r| r.2).sum();
    let list = v_flex()
        .children(active.iter().map(|(name, cat, avg, last)| {
            h_flex().gap_3().py_1().border_b_1().border_color(t.border).items_center()
                .when_some(crate::logo::tile(crate::logo::for_text(name), 20., cx), |d, l| d.child(l))
                .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name.clone()))
                .child(category::label(cat, cx).w(px(130.)).text_xs())
                .child(div().w(px(84.)).text_xs().text_color(t.muted_foreground).child(format!("last {}", last.get(5..).unwrap_or(""))))
                .child(div().w(px(104.)).text_sm().text_right().whitespace_nowrap().child(format!("{}/mo", fmt::hkd(*avg))))
        }))
        .when(active.is_empty(), |d| d.child(div().text_sm().text_color(t.muted_foreground).child("No active fixed commitments found")))
        .when(!ended.is_empty(), |d| d.child(
            div().pt_2().text_xs().text_color(t.muted_foreground).child(format!(
                "Ended: {}",
                ended.iter().take(6).map(|r| r.0.as_str()).collect::<Vec<_>>().join(" · ")
                    + if ended.len() > 6 { " …" } else { "" }
            )),
        ));

    v_flex()
        .gap_3()
        .child(ui::section("Fixed vs variable", "committed before the month starts vs everything else", cx))
        .child(tiles)
        .child(
            ui::card(cx)
                .child(h_flex().gap_4().text_xs().text_color(t.muted_foreground)
                    .child(h_flex().gap_1().child(div().size(px(8.)).rounded_sm().bg(fixed_c)).child("fixed"))
                    .child(h_flex().gap_1().child(div().size(px(8.)).rounded_sm().bg(var_c)).child("variable")))
                .child(bars),
        )
        .child(
            ui::card(cx)
                .child(h_flex().justify_between().pb_1()
                    .child(div().text_sm().font_semibold().child("Active commitments"))
                    .child(div().text_sm().text_color(t.muted_foreground).child(format!("≈ {}/mo", fmt::hkd(active_total)))))
                .child(list),
        )
}

/// Month-by-month and trailing-average summary tables (completed months; the
/// running month is listed but marked and never averaged). Scrolls sideways at
/// narrow widths instead of squeezing the figures.
fn summary_tables(done: &[&serde_json::Value], selected: &str, cx: &App) -> Div {
    let t = cx.theme();
    let invested = |m: &serde_json::Value| num(m, "invested") + num(m, "invested_crypto");
    let rate = |inc: f64, sp: f64| if inc > 0. { format!("{:.0}%", (inc - sp) / inc * 100.) } else { "—".into() };
    const W: [f32; 8] = [78., 104., 104., 92., 96., 108., 64., 104.];
    let cell = |i: usize, txt: String, color: Option<Hsla>| {
        div().w(px(W[i])).flex_shrink_0().when(i > 0, |d| d.text_right()).whitespace_nowrap()
            .when_some(color, |d, c| d.text_color(c)).child(txt)
    };
    let head = |cols: [&'static str; 8]| {
        h_flex().gap_2().py_1().text_xs().text_color(t.muted_foreground).border_b_1().border_color(t.border)
            .children(cols.into_iter().enumerate().map(|(i, c)| cell(i, c.to_string(), None)))
    };
    let row = |label: String, inc: f64, sp: f64, fx: f64, vr: f64, inv: f64, highlight: bool| {
        let net = inc - sp;
        h_flex().gap_2().py(px(5.)).text_sm().border_b_1().border_color(t.border.opacity(0.5))
            .when(highlight, |d| d.bg(t.accent).rounded(px(4.)))
            .child(cell(0, label, None))
            .child(cell(1, fmt::int(inc), Some(fmt::money_in())))
            .child(cell(2, fmt::int(sp), Some(fmt::money_out())))
            .child(cell(3, fmt::int(fx), None))
            .child(cell(4, fmt::int(vr), None))
            .child(cell(5, fmt::signed_hkd(net).replace(&fmt::sym_space(), ""), Some(fmt::signed_color(net))))
            .child(cell(6, rate(inc, sp), None))
            .child(cell(7, fmt::int(inv), None))
    };

    let monthly = v_flex()
        .child(head(["Month", "Income", "Spending", "Fixed", "Variable", "Saved", "Rate", "Invested"]))
        .children(done.iter().rev().take(12).map(|m| {
            row(fmt::month_label(month_of(m)), num(m, "income"), num(m, "spend"), num(m, "fixed"),
                num(m, "variable"), invested(m), month_of(m) == selected)
        }));

    let windows: [(usize, &str); 6] = [(1, "1 month"), (3, "3 months"), (6, "6 months"), (12, "1 year"), (24, "2 years"), (0, "all")];
    let averages = v_flex()
        .child(head(["Window", "Income", "Spending", "Fixed", "Variable", "Saved", "Rate", "Invested"]))
        .children(windows.into_iter().filter(|(w, _)| *w <= done.len()).map(|(w, label)| {
            let win: Vec<_> = if w == 0 { done.iter().collect() } else { done.iter().rev().take(w).collect() };
            let k = win.len().max(1) as f64;
            let avg = |f: &dyn Fn(&serde_json::Value) -> f64| win.iter().map(|m| f(m)).sum::<f64>() / k;
            let label = if w == 0 { format!("all ({} mo)", win.len()) } else { label.to_string() };
            row(label, avg(&|m| num(m, "income")), avg(&|m| num(m, "spend")), avg(&|m| num(m, "fixed")),
                avg(&|m| num(m, "variable")), avg(&|m| invested(m)), false)
        }));

    let scroll = |id: &'static str, body: Div| div().id(id).w_full().overflow_x_scroll().restrict_scroll_to_axis().child(body.min_w(px(W.iter().sum::<f32>() + 7. * 8.)));
    v_flex()
        .gap_3()
        .child(ui::section_info("Month by month", &fmt::local("last 12 completed months, HK$"),
            "Income and spending exclude moves between your own accounts, investing and card-bill payments. Saved = income − spending; Rate = saved ÷ income. Invested = money sent to funds, the broker and crypto that month.", cx))
        .child(ui::card(cx).child(scroll("tbl-months", monthly)))
        .child(ui::section_info("Averages", "per month, over trailing windows of completed months",
            "Read it like a yield curve: the top rows are your recent pace, the bottom ones the long run. Saved above Invested means cash is piling up; the reverse means you are investing earlier savings.", cx))
        .child(ui::card(cx).child(scroll("tbl-avgs", averages)))
}

/// One node of the money-flow Sankey.
#[derive(Clone)]
struct FlowNode {
    name: SharedString,
    value: f64,
    color: Hsla,
}

/// Where the month's money went: income sources -> Income -> spending by
/// category, invested, kept as cash. Spending more than came in shows as a
/// "From savings" source, so the picture always balances.
fn flow_section(trend: &[serde_json::Value], ledger: &Ledger, month: &str, query: &str, pass: &dyn Fn(usize, &Entry) -> bool, cx: &App) -> Div {
    use gpui_kit::component::chart::SankeyChart;
    use gpui_kit::component::plot::shape::SankeyLink;
    const TOP: usize = 6;
    // flows smaller than this share of the month are grouped, so labels never pile up
    const MIN_SHARE: f64 = 0.03;
    let t = cx.theme();
    let flows = crate::memo::get("spending.flows", &(query, month), || drill::month_flows_where(ledger, month, pass));
    let (income, spend) = (*flows).clone();
    let group = |rows: Vec<(String, f64)>, total: f64, other: &str| -> Vec<(String, f64)> {
        let (mut keep, mut rest, mut n) = (Vec::new(), 0., 0);
        for (i, (c, v)) in rows.into_iter().enumerate() {
            if i < TOP && v >= total * MIN_SHARE { keep.push((c, v)) } else { rest += v; n += 1 }
        }
        if rest > 0.5 {
            keep.push((format!("{other} ({n})"), rest));
        }
        keep
    };
    let invested = trend.iter().find(|m| month_of(m) == month)
        .map(|m| num(m, "invested") + num(m, "invested_crypto")).unwrap_or(0.).max(0.);
    let total_in: f64 = income.iter().map(|x| x.1).sum();
    let total_out: f64 = spend.iter().map(|x| x.1).sum();
    if total_in <= 0. && total_out <= 0. {
        return v_flex();
    }
    let kept = total_in - total_out - invested;
    let scale = total_in.max(total_out + invested);
    let income = group(income, scale, "Other income");
    let spend = group(spend, scale, "Other spending");

    let palette = [t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5];
    let mut nodes: Vec<FlowNode> = Vec::new();
    let mut links: Vec<SankeyLink> = Vec::new();
    // income sources (and savings drawn down) on the left
    for (cat, v) in &income {
        nodes.push(FlowNode { name: cat.clone().into(), value: *v, color: fmt::money_in() });
    }
    if kept < 0. {
        nodes.push(FlowNode { name: "From savings".into(), value: -kept, color: t.warning });
    }
    let hub = nodes.len();
    let hub_value = total_in + (-kept).max(0.);
    nodes.push(FlowNode { name: "Income".into(), value: hub_value, color: t.foreground.opacity(0.7) });
    for i in 0..hub {
        links.push(SankeyLink::new(i, hub, nodes[i].value));
    }
    // where it went, on the right
    let push_out = |name: String, v: f64, color: Hsla, nodes: &mut Vec<FlowNode>, links: &mut Vec<SankeyLink>| {
        if v > 0.5 {
            nodes.push(FlowNode { name: name.into(), value: v, color });
            links.push(SankeyLink::new(hub, nodes.len() - 1, v));
        }
    };
    for (k, (cat, v)) in spend.iter().enumerate() {
        let color = if cat.starts_with("Other spending") { t.muted_foreground } else { palette[k % palette.len()].opacity(0.9) };
        push_out(cat.clone(), *v, color, &mut nodes, &mut links);
    }
    push_out("Invested".into(), invested, t.primary, &mut nodes, &mut links);
    push_out("Kept as cash".into(), kept, fmt::money_in().opacity(0.6), &mut nodes, &mut links);

    let chart = SankeyChart::new(nodes, links).appear(false)
        .node_color(|n: &FlowNode| n.color)
        .node_label(|n: &FlowNode| n.name.clone())
        .value_label(|_, v| fmt::compact(v).into())
        .tooltip_name(|n: &FlowNode| n.name.clone())
        .tooltip_value(|_, v| fmt::hkd(v).into())
        .node_width(10.)
        .node_padding(10.)
        .link_opacity(0.28);
    let caption = format!("{} in · {} spent · {} invested · {} {}", fmt::compact(total_in), fmt::compact(total_out),
        fmt::compact(invested), if kept >= 0. { "kept" } else { "drawn from savings" }, fmt::compact(kept.abs()));
    v_flex()
        .gap_2()
        .child(ui::section_info("Where the money went", &format!("{} · {caption}", fmt::month_label(month)),
            "Income by source flows into one pot, then out to spending by category (your share of shared bills), to investing (funds, broker, crypto), and what stayed as cash. When spending and investing exceed income, the difference comes from savings and appears as a source on the left. Transfers between your own accounts are left out.", cx))
        .child(ui::card(cx).h(px(380.)).child(div().flex_1().min_h_0().child(chart)))
}
