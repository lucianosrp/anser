//! Merchant and category deep dives, in a dialog (Esc closes): the numbers
//! from `finance_core::dive`, a monthly chart, and the rows. A row opens the
//! detail sheet; a category or a merchant inside hops to its own dive.

use std::rc::Rc;
use std::sync::Arc;

use finance_core::filter::{Dim, Filter};
use finance_core::{Ledger, dive, drill};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    chart::{BarChart, LineChart},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::budgets::Budgets;
use crate::category;
use crate::fmt;
use crate::pages::spending::{OnPick, OnSearch};
use crate::ui::{self, Figures as _, Pt};
use crate::zoom::px;

/// What to dive into.
#[derive(Clone, Debug, PartialEq)]
pub enum Dive {
    /// a merchant key (`drill::merchant_key`)
    Merchant(String),
    /// a category name
    Category(String),
}

pub type OnDive = Rc<dyn Fn(Dive, &mut Window, &mut App)>;

/// The Transactions query for a merchant or a category.
pub fn query(d: &Dive) -> String {
    let mut f = Filter::default();
    match d {
        Dive::Merchant(k) => f.set_values(Dim::Merchant, false, std::slice::from_ref(k)),
        Dive::Category(c) => f.set_values(Dim::Category, false, std::slice::from_ref(c)),
    }
    f.query()
}

/// Everything the dialog's links need.
#[derive(Clone)]
pub struct Links {
    pub ledger: Arc<Ledger>,
    pub on_pick: OnPick,
    pub on_search: OnSearch,
    pub on_dive: OnDive,
}

pub fn open(d: Dive, links: Links, window: &mut Window, cx: &mut App) {
    match d {
        Dive::Merchant(k) if !k.is_empty() => open_merchant(k, links, window, cx),
        Dive::Category(c) => open_category(c, links, window, cx),
        _ => {}
    }
}

fn months_chart(pts: Vec<Pt>, color: impl Fn(&Pt) -> Hsla + 'static) -> BarChart<Pt, SharedString, f64> {
    let margin = pts.len().div_ceil(9).max(1);
    BarChart::new(pts).appear(false)
        .tick_margin(margin)
        .band(|p: &Pt| p.label.clone())
        .value(|p: &Pt| p.v)
        .value_axis(true)
        .value_tick_format(fmt::compact)
        .tooltip_value(|_, v| fmt::hkd(v).into())
        .fill(move |p: &Pt, _, _, _| color(p))
}

/// One row of a list: date, description, category, amount; a click opens its detail.
fn row(i: usize, ledger: &Ledger, links: &Links, cx: &App) -> Option<Stateful<Div>> {
    let t = cx.theme();
    let e = ledger.entries.get(i)?;
    let v = drill::my_hkd(e).unwrap_or(e.amount);
    let on_pick = links.on_pick.clone();
    Some(h_flex()
        .id(("dive-row", i))
        .gap_3()
        .py(px(3.))
        .px_1()
        .rounded(t.radius)
        .cursor_pointer()
        .hover(|s| s.bg(t.accent))
        .on_click(move |_, window, cx| {
            window.close_dialog(cx);
            on_pick(i, window, cx)
        })
        .child(div().w(px(84.)).flex_shrink_0().text_xs().text_color(t.muted_foreground).child(e.date.clone()))
        .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.description.clone()))
        .child(category::label(&e.category, cx).w(px(150.)).flex_shrink_0().text_xs())
        .child(div().w(px(110.)).flex_shrink_0().text_sm().text_right().whitespace_nowrap().figures()
            .text_color(fmt::signed_color(v)).child(fmt::signed_hkd(v))))
}

fn actions(d: Dive, rows: Vec<usize>, links: &Links) -> Div {
    let q = query(&d);
    let n = rows.len();
    let (on_search, q1) = (links.on_search.clone(), q.clone());
    let ledger = links.ledger.clone();
    h_flex().gap_2().flex_wrap()
        .child(Button::new("dive-table").small().outline().icon(IconName::Search).label(format!("Show all {} in Transactions", fmt::count(n as f64)))
            .on_click(move |_, window, cx| {
                window.close_dialog(cx);
                on_search(q1.clone(), window, cx)
            }))
        .child(Button::new("dive-plot").small().ghost().icon(Lucide::ChartColumn).label("Plot")
            .on_click(move |_, window, cx| {
                window.close_dialog(cx);
                crate::plot::open(q.clone(), ledger.clone(), rows.clone(), window, cx)
            }))
}

/// "2025-01-13" -> "13 Jan 2025"
fn long_day(d: &str) -> String {
    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_else(|_| d.to_string())
}

fn tile(label: &str, value: String, sub: String, tone: Option<Hsla>, cx: &App) -> Div {
    ui::stat(label, value, sub, tone, cx).min_w(px(180.))
}

/// A merchant: totals, visits, the ticket and its trend, months, categories, rows.
fn open_merchant(key: String, links: Links, window: &mut Window, cx: &mut App) {
    let d = Rc::new(dive::merchant(&links.ledger, &key));
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = cx.theme();
        let ledger = links.ledger.clone();
        if d.rows.is_empty() {
            return dialog.w(px(560.)).title(d.name.clone()).child(ui::caption("No rows from this merchant in the local ledger.", cx));
        }
        let every = d.median_gap_days.map(|g| format!(" · every ~{g:.0} d")).unwrap_or_default();
        let tiles = h_flex().gap_3().flex_wrap().items_stretch()
            .child(tile("Spent", fmt::hkd(d.spent),
                if d.received > 0.5 { format!("{} back", fmt::hkd(d.received)) } else { "your share".into() }, Some(fmt::money_out()), cx))
            .child(tile("Visits", fmt::count(d.visits as f64), format!("{:.1}/month{every}", d.per_month), None, cx))
            .child(tile("Average ticket", fmt::hkd(d.avg_ticket), format!("median {}", fmt::hkd(d.median_ticket)), None, cx))
            .child(tile("Price trend", d.price_change.map(fmt::pct).unwrap_or_else(|| "—".into()),
                if d.price_change.is_some() { "median, last 6 mo vs 6 before".into() } else { "too few visits to tell".into() },
                d.price_change.map(|p| if p > 5. { fmt::money_out() } else if p < -5. { fmt::money_in() } else { t.foreground }), cx));

        let pts: Vec<Pt> = d.monthly.iter().rev().take(36).rev().map(|(m, v)| Pt { label: fmt::month_label(m).into(), v: *v }).collect();
        let shown = pts.len();
        let c = t.chart_1;
        let monthly = ui::chart_card("Per month", &format!("spent, last {shown} months of the span"), 200., months_chart(pts, move |_| c), cx);
        let price = (d.price.len() >= 2).then(|| {
            let pts: Vec<Pt> = d.price.iter().map(|(q, v)| Pt { label: q.clone().into(), v: *v }).collect();
            let chart = LineChart::new(pts).appear(false)
                .x(|p: &Pt| p.label.clone())
                .y(|p: &Pt| p.v)
                .stroke(t.chart_2)
                .dot()
                .y_axis(true)
                .y_tick_format(fmt::compact)
                .tick_margin((d.price.len() / 8).max(1));
            ui::chart_card("Ticket over time", "median per quarter", 170., chart, cx)
        });

        let on_dive = links.on_dive.clone();
        let cats = h_flex().gap_2().flex_wrap().children(d.categories.iter().enumerate().map(|(i, (c, n, _))| {
            let (on_dive, c2) = (on_dive.clone(), c.clone());
            h_flex().id(("dive-cat", i)).gap_1().px_2().py(px(2.)).rounded(px(6.)).border_1().border_color(ui::hairline(cx))
                .cursor_pointer().hover(|s| s.bg(t.accent))
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    on_dive(Dive::Category(c2.clone()), window, cx)
                })
                .child(category::label_with(c, format!("{c} · {n}"), cx).text_xs())
        }));
        let accounts = ui::caption(d.accounts.iter().map(|(a, n)| format!("{a} ({n})")).collect::<Vec<_>>().join(" · "), cx);
        let rows = v_flex().children(d.rows.iter().take(12).filter_map(|&i| row(i, &ledger, &links, cx)));

        dialog.w(px(900.)).title(h_flex().gap_2().items_center()
                .when_some(crate::logo::tile(crate::logo::for_text(&d.name), 22., cx), |h, l| h.child(l))
                .child(d.name.clone()))
            .child(v_flex().gap_3()
                .child(ui::caption(format!("matched on \u{201c}{}\u{201d} · {} rows · {} → {} · {} months with a visit",
                    d.key, fmt::count(d.rows.len() as f64), long_day(&d.first), long_day(&d.last), d.months_active), cx))
                .child(tiles)
                .child(monthly)
                .children(price)
                .child(h_flex().gap_4().flex_wrap().items_start()
                    .child(v_flex().gap_1().child(ui::eyebrow("Filed under", cx)).child(cats))
                    .child(v_flex().gap_1().min_w_0().child(ui::eyebrow("Paid from", cx)).child(accounts)))
                .child(ui::eyebrow(format!("Latest {}", d.rows.len().min(12)), cx))
                .child(rows)
                .child(actions(Dive::Merchant(d.key.clone()), d.rows.clone(), &links))
                .child(ui::caption("Esc closes · a row opens its detail · a category opens its own dive", cx)))
    });
}

/// A category: this month against its budget, 24 months, share of spending,
/// its top merchants, rows.
fn open_category(name: String, links: Links, window: &mut Window, cx: &mut App) {
    let month = crate::budgets::today().format("%Y-%m").to_string();
    let d = Rc::new(dive::category(&links.ledger, &name, &month));
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = cx.theme();
        let ledger = links.ledger.clone();
        let budget = cx.try_global::<Budgets>().and_then(|b| b.monthly.get(&d.category).copied()).filter(|b| *b > 0.);
        let verb = if d.income { "received" } else { "spent" };
        let tone = if d.income { fmt::money_in() } else { fmt::money_out() };
        let this_sub = match budget {
            Some(b) => format!("of {} budget · {:.0}%", fmt::hkd(b), d.this_month / b * 100.),
            None => format!("{verb} so far, {}", fmt::month_label(&month)),
        };
        let tiles = h_flex().gap_3().flex_wrap().items_stretch()
            .child(tile("This month", fmt::hkd(d.this_month), this_sub,
                Some(if budget.is_some_and(|b| d.this_month > b) { fmt::money_out() } else { tone }), cx))
            .child(tile("Last 12 months", fmt::hkd(d.last12), format!("{} a month", fmt::hkd(d.avg12)), None, cx))
            .when_some(d.share12, |h, s| h.child(tile("Share of spending", format!("{s:.1}%"), "last 12 months, your share".into(), None, cx)))
            .when_some(budget, |h, b| h.child(tile("Budget", fmt::hkd(b), format!("average is {:.0}% of it", d.avg12 / b * 100.), None, cx)));

        let pts: Vec<Pt> = d.monthly.iter().map(|(m, v)| Pt { label: fmt::month_label(m).into(), v: *v }).collect();
        let (normal, over) = (if d.income { fmt::money_in() } else { t.chart_1 }, fmt::money_out());
        let cap = format!("{verb} per month, 24 months{}", budget.map(|b| format!(" · red is over the {} budget", fmt::compact(b))).unwrap_or_default());
        let monthly = ui::chart_card("Per month", &cap, 210., months_chart(pts, move |p| if budget.is_some_and(|b| p.v > b) { over } else { normal }), cx);

        let max = d.merchants.first().map(|m| m.3).unwrap_or(0.);
        let on_dive = links.on_dive.clone();
        let merchants = v_flex().children(d.merchants.iter().take(8).enumerate().map(|(i, (key, name, n, v))| {
            let (on_dive, key) = (on_dive.clone(), key.clone());
            let real = !drill::merchant_key(name).is_empty() || !key.contains(' ');
            div().id(("dive-merchant", i)).px_1().rounded(t.radius)
                .when(real, |d| d.cursor_pointer().hover(|s| s.bg(t.accent)).on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    on_dive(Dive::Merchant(key.clone()), window, cx)
                }))
                .child(ui::bar_row(&format!("{name} · {n}"), *v, max, fmt::hkd(*v), t.chart_2, cx))
        }));
        let rows = v_flex().children(d.rows.iter().take(10).filter_map(|&i| row(i, &ledger, &links, cx)));

        dialog.w(px(900.)).title(h_flex().gap_2().items_center()
                .child(category::tile(&d.category, 24., t.chart_1))
                .child(d.category.clone()))
            .child(v_flex().gap_3()
                .child(ui::caption(format!("{} rows since {} · {} {verb} in all · bank and card, your share",
                    fmt::count(d.rows.len() as f64), long_day(&d.first), fmt::hkd(d.total)), cx))
                .child(tiles)
                .child(monthly)
                .child(ui::eyebrow("Top merchants · click one for its dive", cx))
                .child(merchants)
                .child(ui::eyebrow(format!("Latest {}", d.rows.len().min(10)), cx))
                .child(rows)
                .child(actions(Dive::Category(d.category.clone()), d.rows.clone(), &links))
                .child(h_flex().gap_1().child(Icon::new(Lucide::Store).xsmall().text_color(t.muted_foreground))
                    .child(ui::caption("Esc closes · a row opens its detail", cx))))
    });
}

#[cfg(test)]
mod tests {
    use super::{Dive, query};

    #[test]
    fn dive_queries() {
        assert_eq!(query(&Dive::Merchant("sushiro".into())), "merchant:sushiro");
        assert_eq!(query(&Dive::Merchant("city super".into())), "merchant:\"city super\"");
        assert_eq!(query(&Dive::Category("Office (reimbursed)".into())), "cat:\"office (reimbursed)\"");
    }
}
