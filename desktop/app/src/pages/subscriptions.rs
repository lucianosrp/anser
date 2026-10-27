//! Subscriptions: an audit of every recurring charge finance_core::recurring
//! finds (subscriptions, bills, the loan, the gym, office rent). What each
//! costs a year, when it lands next, where it's paid from, and what deserves
//! a look: a price rise, something easy to forget, a duplicate, an ending.
//!
//! A page of its own in the Money group rather than a Spending tab: Spending
//! is one month at a time, while an audit is about "now" and "per year".

use std::collections::HashMap;
use std::rc::Rc;

use finance_core::recurring::{Cadence, Flag, Flow, Series};
use finance_core::{Ledger, recurring};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, chart::BarChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::pages::spending::OnPick;
use crate::ui::{self, Figures as _, Pt};
use crate::zoom::px;

/// How the active list is ordered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sort {
    /// most expensive first
    #[default]
    Yearly,
    Name,
    /// the next one due first
    Next,
}

pub type OnSort = Rc<dyn Fn(Sort, &mut Window, &mut App)>;

/// Which charges the page shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Status {
    #[default]
    All,
    Active,
    /// active and flagged (price up, maybe forgotten, duplicate)
    Flagged,
    Ended,
}

/// The page's filters: one account (`Series::account_key`) and a status.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct View {
    pub account: Option<String>,
    pub status: Status,
}

pub type OnView = Rc<dyn Fn(View, &mut Window, &mut App)>;

/// "HSBC card's active charges", "the ended charges".
fn current_label(view: &View, accounts: &[(String, String, usize)]) -> String {
    let status = match view.status {
        Status::All => "",
        Status::Active => "active ",
        Status::Flagged => "flagged ",
        Status::Ended => "ended ",
    };
    match view.account.as_ref().and_then(|k| accounts.iter().find(|a| &a.0 == k)) {
        Some(a) => format!("{}'s {status}", a.1).trim_end().to_string(),
        None => format!("the {status}").trim_end().to_string(),
    }
}

/// Account and status pickers, as segmented controls and a dropdown.
fn view_bar(view: &View, accounts: &[(String, String, usize)], on_view: OnView, cx: &App) -> Div {
    use gpui_kit::component::{Sizable as _, button::{Button, ButtonVariants as _}, menu::{DropdownMenu as _, PopupMenuItem}};
    let t = cx.theme();
    let seg = |label: &'static str, this: Status| {
        let on = view.status == this;
        let (on_view, v) = (on_view.clone(), View { status: this, ..view.clone() });
        div().id(label).px(px(10.)).py(px(3.)).rounded(px(6.)).text_xs().cursor_pointer().whitespace_nowrap()
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| on_view(v.clone(), w, cx))
            .child(label)
    };
    let status = h_flex().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx))
        .child(seg("All", Status::All))
        .child(seg("Active", Status::Active))
        .child(seg("Worth a look", Status::Flagged))
        .child(seg("Ended", Status::Ended));
    let current = view.account.as_ref().and_then(|k| accounts.iter().find(|a| &a.0 == k)).map(|a| a.1.clone());
    let (accts, v0, ov) = (accounts.to_vec(), view.clone(), on_view.clone());
    let account = Button::new("subs-account").small().outline()
        .icon(gpui_kit::assets::IconName::ListFilter)
        .label(current.clone().unwrap_or_else(|| "All accounts".into()))
        .dropdown_menu(move |menu, _, _| {
            let (v, ov2) = (View { account: None, ..v0.clone() }, ov.clone());
            let mut menu = menu.min_w(px(220.)).item(PopupMenuItem::new("All accounts").checked(v0.account.is_none())
                .on_click(move |_, w, cx| ov2(v.clone(), w, cx)));
            for (key, label, n) in &accts {
                let (v, ov2) = (View { account: Some(key.clone()), ..v0.clone() }, ov.clone());
                menu = menu.item(PopupMenuItem::new(format!("{label} · {n}")).checked(v0.account.as_ref() == Some(key))
                    .on_click(move |_, w, cx| ov2(v.clone(), w, cx)));
            }
            menu
        });
    h_flex().gap_3().flex_wrap().items_center()
        .child(account)
        .child(status)
        .when(*view != View::default(), |d| {
            let ov = on_view.clone();
            d.child(Button::new("subs-clear").xsmall().ghost().label("Clear").on_click(move |_, w, cx| ov(View::default(), w, cx)))
        })
}

pub const EXPLAIN: &str = "Found, not tagged: the same merchant charging on a steady rhythm (weekly, monthly, quarterly, yearly) at a steady amount. Subscriptions, utilities, telecom, the gym, rent and loans need less evidence; dining and groceries need a near-perfect pattern, so the coffee shop never shows up. Amounts are your share of shared bills, in HK$ at the latest payment's rate. Something not charged for two months (1.5 periods for longer cadences) is ended.";

/// "€", "£", "HK$" … for a currency code.
pub fn symbol(ccy: &str) -> String {
    match ccy {
        "EUR" => "€".into(),
        "GBP" => "£".into(),
        "HKD" => "HK$ ".into(),
        "USD" => "US$ ".into(),
        "JPY" => "¥".into(),
        other => format!("{other} "),
    }
}

/// "€12.99/mo", "£80.33/yr", "≈ €23.50 a top-up"
fn per_payment(s: &Series) -> String {
    let unit = match s.cadence {
        Cadence::Weekly => "/wk",
        Cadence::Monthly => "/mo",
        Cadence::Quarterly => "/qtr",
        Cadence::Yearly => "/yr",
        Cadence::Irregular => " a top-up",
    };
    format!("{}{}{}{unit}", if s.varies { "≈ " } else { "" }, symbol(&s.currency), fmt::dec2(s.typical))
}

/// "2026-08-25" -> "25 Aug"
pub fn short_day(d: &str) -> String {
    recurring::day(d).map(|d| d.format("%-d %b").to_string()).unwrap_or_default()
}

/// "2026-08-25" -> "25 Aug 2026"
fn long_day(d: &str) -> String {
    recurring::day(d).map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
pub fn render(ledger: &Ledger, all: &[Series], sort: Sort, on_sort: OnSort, view: &View, on_view: OnView, on_pick: OnPick, cx: &App) -> ui::Sections {
    let t = cx.theme();
    let newest = ledger.entries.first().map(|e| e.date.clone()).unwrap_or_default();
    // the flags and the monthly totals are kept between frames (memo.rs)
    let flags = crate::memo::get("subs.flags", &newest, || recurring::flags(all, &newest));
    // charges only: pay and the card bill belong to the cash-flow calendar
    let every: Vec<(usize, &Series)> = all.iter().enumerate().filter(|(_, s)| s.flow == Flow::Charge).collect();
    if every.is_empty() {
        return ui::Sections::new().child(ui::empty_with(gpui_kit::component::IconName::RotateCw, "No recurring charges found yet",
            "They appear once the same merchant has charged a few times on a steady rhythm.", cx));
    }
    // the accounts charges come from, most charges first
    let mut accounts: Vec<(String, String, usize)> = Vec::new();
    for (_, s) in &every {
        match accounts.iter_mut().find(|a| a.0 == s.account_key()) {
            Some(a) => a.2 += 1,
            None => accounts.push((s.account_key(), s.account_label(), 1)),
        }
    }
    accounts.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)));
    let bar = view_bar(view, &accounts, on_view, cx);
    let charges: Vec<(usize, &Series)> = every.into_iter().filter(|(_, s)| view.account.as_ref().is_none_or(|k| *k == s.account_key())).collect();
    let today = chrono::Local::now().date_naive();
    let mut active: Vec<(usize, &Series)> = charges.iter().filter(|(_, s)| s.active).copied().collect();
    let ended: Vec<(usize, &Series)> = charges.iter().filter(|(_, s)| !s.active).copied().collect();
    let flagged_only = view.status == Status::Flagged;
    let (show_active, show_ended) = match view.status {
        Status::All => (true, true),
        Status::Active | Status::Flagged => (true, false),
        Status::Ended => (false, true),
    };
    match sort {
        Sort::Yearly => active.sort_by(|a, b| b.1.yearly_hkd.total_cmp(&a.1.yearly_hkd)),
        Sort::Name => active.sort_by_key(|(_, s)| s.name.to_lowercase()),
        Sort::Next => active.sort_by_key(|(_, s)| s.next_from(today)),
    }
    let listed: Vec<(usize, &Series)> = active.iter()
        .filter(|(i, _)| !flagged_only || flags[*i].iter().any(|f| !matches!(f, Flag::Ended)))
        .copied()
        .collect();

    // ---- tiles ----
    let yearly: f64 = active.iter().map(|(_, s)| s.yearly_hkd).sum();
    let yearly_cash: f64 = active.iter().map(|(_, s)| s.yearly_cash_hkd).sum();
    let flagged = active.iter().filter(|(i, _)| flags[*i].iter().any(|f| !matches!(f, Flag::Ended))).count();
    let biggest = active.iter().max_by(|a, b| a.1.yearly_hkd.total_cmp(&b.1.yearly_hkd));
    let price_ups = active.iter().filter(|(i, _)| flags[*i].contains(&Flag::PriceUp)).count();
    let tiles = h_flex().gap_4().flex_wrap().items_stretch().children([
        ui::stat_info("Per month", fmt::hkd(yearly / 12.), format!("{} active charges", active.len()), Some(fmt::money_out()),
            "Every active recurring charge as a monthly figure: weekly ×52/12, quarterly ÷3, yearly ÷12. Your share of shared bills.", cx),
        ui::stat("Per year", fmt::hkd(yearly),
            if (yearly_cash - yearly).abs() > 1. { format!("{} in cash, shared bills in full", fmt::compact(yearly_cash)) } else { "your share, at today's pace".into() },
            None, cx),
        ui::stat("Worth a look", flagged.to_string(),
            if price_ups > 0 { format!("{price_ups} price rise{} this year", if price_ups == 1 { "" } else { "s" }) } else { "price rises, easy to forget, duplicates".into() },
            if flagged > 0 { Some(t.warning) } else { None }, cx),
        ui::stat("Biggest", biggest.map(|(_, s)| fmt::hkd(s.yearly_hkd)).unwrap_or_default(),
            biggest.map(|(_, s)| format!("{} a year", s.name)).unwrap_or_default(), None, cx),
    ].map(|d| d.min_w(px(140.))));

    // ---- cost per month, 12 months (the running month marked) ----
    let now = chrono::Local::now().date_naive();
    let months: Vec<String> = (0..12).rev()
        .map(|k| recurring::add_months(now, -k, 1).format("%Y-%m").to_string())
        .collect();
    let totals = crate::memo::get("subs.totals", &(&view.account, &months), || {
        let shown: Vec<Series> = charges.iter().map(|(_, s)| (*s).clone()).collect();
        recurring::monthly_totals(ledger, &shown, &months)
    });
    let running = fmt::month_label(&now.format("%Y-%m").to_string());
    let pts: Vec<Pt> = months.iter().zip(totals.iter()).map(|(m, v)| Pt { label: fmt::month_label(m).into(), v: *v }).collect();
    let (on, partial) = (t.chart_2, t.chart_2.opacity(0.4));
    let chart = BarChart::new(pts).appear(false)
        .band(|p: &Pt| p.label.clone())
        .value(|p: &Pt| p.v)
        .value_axis(true)
        .tick_margin(1)
        .value_tick_format(fmt::compact)
        .tooltip_value(|_, v| fmt::hkd(v).into())
        .fill(move |p: &Pt, _, _, _| if p.label.as_ref() == running { partial } else { on });

    // ---- by category, per year ----
    let mut cats: HashMap<&str, f64> = HashMap::new();
    for (_, s) in &active {
        *cats.entry(s.category.as_str()).or_default() += s.yearly_hkd;
    }
    let mut cats: Vec<(&str, f64)> = cats.into_iter().collect();
    cats.sort_by(|a, b| b.1.total_cmp(&a.1));
    let max = cats.first().map(|c| c.1).unwrap_or(0.);
    let by_cat = ui::card(cx).children(cats.iter().map(|(c, v)| {
        ui::bar_row(c, *v, max, format!("{}/yr · {}/mo", fmt::compact(*v), fmt::compact(v / 12.)), t.chart_2, cx)
    }));

    // ---- worth a look ----
    let mut notes: Vec<(usize, Hsla, String, String)> = Vec::new();
    for (i, s) in &active {
        for f in &flags[*i] {
            match f {
                Flag::PriceUp => if let Some(c) = &s.change {
                    notes.push((*i, fmt::money_out(), format!("{} went up {:+.0}%", s.name, c.pct()),
                        format!("{sym}{} → {sym}{} from {} · +{}/yr", fmt::dec2(c.from), fmt::dec2(c.to), long_day(&c.date),
                            fmt::hkd((c.to_hkd - c.from_hkd) * 365.25 / s.period_days), sym = symbol(&s.currency))));
                },
                Flag::MaybeForgotten(why) => notes.push((*i, t.warning, format!("{}: still charging", s.name),
                    format!("{why} · {}/yr · last {}", fmt::hkd(s.yearly_hkd), short_day(&s.last)))),
                Flag::Duplicate(g) => notes.push((*i, t.info, format!("{}: another {g} service", s.name),
                    "two active charges doing the same job".into())),
                Flag::Ended => {}
            }
        }
    }
    let recently_ended: Vec<&(usize, &Series)> = ended.iter()
        .filter(|(_, s)| recurring::day(&s.last).zip(recurring::day(&newest)).is_some_and(|(l, n)| (n - l).num_days() <= 365))
        .collect();
    for (i, s) in &recently_ended {
        notes.push((*i, t.muted_foreground, format!("{} stopped", s.name), format!("last charged {} · was {}/yr", long_day(&s.last), fmt::hkd(s.yearly_hkd))));
    }
    let look = ui::card(cx).p(px(6.)).children(notes.iter().enumerate().map(|(k, (i, c, title, detail))| {
        let (i, on_pick) = (*i, on_pick.clone());
        let row = all[i].rows.first().copied();
        h_flex().id(("note", k)).gap_3().px(px(10.)).py(px(7.)).rounded(t.radius).items_start()
            .cursor_pointer().hover(|s| s.bg(t.accent))
            .on_click(move |_, w, cx| if let Some(r) = row { on_pick(r, w, cx) })
            .child(div().mt(px(5.)).child(ui::dot(*c, 7.)))
            .child(v_flex().flex_1().min_w_0()
                .child(div().text_size(px(ui::ts::BODY)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(title.clone()))
                .child(ui::caption(detail.clone(), cx)))
    })).when(notes.is_empty(), |d| d.p(px(16.)).child(ui::caption("Nothing stands out: no price rises, nothing easy to forget, no duplicates.", cx)));

    // ---- the list ----
    let seg = |label: &'static str, this: Sort| {
        let on = sort == this;
        let on_sort = on_sort.clone();
        div().id(label).px(px(10.)).py(px(3.)).rounded(px(6.)).text_xs().cursor_pointer().whitespace_nowrap()
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| if !on { on_sort(this, w, cx) })
            .child(label)
    };
    let sorter = h_flex().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx))
        .child(seg("Yearly cost", Sort::Yearly))
        .child(seg("Next due", Sort::Next))
        .child(seg("Name", Sort::Name));
    let list = ui::card(cx).p(px(6.)).gap_0().children(listed.iter().map(|(i, s)| row(*i, s, &flags[*i], today, on_pick.clone(), cx)))
        .when(listed.is_empty(), |d| d.p(px(16.)).child(ui::caption("Nothing here with these filters.", cx)));
    let ended_list = ui::card(cx).p(px(6.)).gap_0().children(ended.iter().map(|(i, s)| row(*i, s, &[], today, on_pick.clone(), cx).opacity(0.7)));

    let filtered = *view != View::default();
    ui::Sections::new()
        .gap(16.)
        .child(bar)
        .when(filtered, |d| d.child(ui::caption(format!("Filtered to {} charges: the tiles, the chart and the lists follow.",
            current_label(view, &accounts)), cx)))
        .child(tiles)
        .child(ui::chart_card_with("Recurring charges per month", "12 months · your share · this month so far is faded",
            ui::info_for(ui::ts::TITLE, "info-subs", fmt::local(EXPLAIN), cx), 240., chart, cx))
        .child(
            h_flex().gap_4().items_start().flex_wrap()
                .child(v_flex().flex_1().min_w(px(320.)).gap_2()
                    .child(ui::section("Worth a look", "click for the latest charge", cx))
                    .child(look))
                .child(v_flex().flex_1().min_w(px(320.)).gap_2()
                    .child(ui::section("By category", "active charges, per year", cx))
                    .child(by_cat)),
        )
        .when(show_active, |d| d.child(h_flex().justify_between().flex_wrap().gap_2().items_end()
            .child(ui::section(if flagged_only { "Worth a look" } else { "Active" }, &format!("{} · ≈ {}/mo", listed.len(),
                fmt::hkd(listed.iter().map(|(_, s)| s.yearly_hkd).sum::<f64>() / 12.)), cx))
            .child(sorter))
        .child(list))
        .when(show_ended && !ended.is_empty(), |d| d
            .child(ui::section("Ended", "not charged for two months or more (1.5 periods for longer cadences)", cx))
            .child(ended_list))
}

/// One charge: logo, name and flags over category · cadence · account ·
/// dates; the yearly cost over the per-payment amount on the right.
fn row(i: usize, s: &Series, flags: &[Flag], today: chrono::NaiveDate, on_pick: OnPick, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    let chips = flags.iter().filter_map(|f| match f {
        Flag::PriceUp => s.change.as_ref().map(|c| ui::chip(format!("price up {:+.0}%", c.pct()), fmt::money_out(), cx)),
        Flag::MaybeForgotten(_) => Some(ui::chip("maybe forgotten", t.warning, cx)),
        Flag::Duplicate(g) => Some(ui::chip(format!("duplicate · {g}"), t.info, cx)),
        Flag::Ended => None,
    });
    let reimbursed = s.category.to_lowercase().contains("reimburs");
    let next = if s.active { s.next_from(today).map(|d| format!(" · next {}", d.format("%-d %b"))).unwrap_or_default() } else { String::new() };
    let meta = format!("{} · {} · {} · last {}{next}", s.category, s.cadence.label(), s.account_label(), short_day(&s.last));
    let first = s.rows.first().copied();
    h_flex()
        .id(("sub", i))
        .gap_3()
        .px(px(10.))
        .py(px(8.))
        .rounded(t.radius)
        .cursor_pointer()
        .hover(|d| d.bg(t.accent))
        .on_click(move |_, w, cx| if let Some(r) = first { on_pick(r, w, cx) })
        .child(crate::logo::tile(crate::logo::for_text(&s.name), 30., cx).unwrap_or_else(|| ui::dot(t.chart_2, 8.).into_any_element()))
        .child(v_flex().flex_1().min_w_0().gap(px(2.))
            .child(h_flex().gap_2().min_w_0()
                .child(div().text_size(px(ui::ts::BODY)).font_weight(FontWeight::MEDIUM).overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.name.clone()))
                .children(chips)
                .when(reimbursed, |d| d.child(ui::tag("reimbursed", cx)))
                .when(!s.active, |d| d.child(ui::tag("ended", cx))))
            .child(ui::caption(meta, cx)))
        .child(v_flex().flex_shrink_0().items_end().gap(px(2.))
            .child(div().text_size(px(ui::ts::BODY)).font_semibold().figures().whitespace_nowrap().child(format!("{}/yr", fmt::hkd(s.yearly_hkd))))
            .child(ui::caption(if s.share < 0.999 {
                format!("{} · your {:.0}%", per_payment(s), s.share * 100.)
            } else {
                per_payment(s)
            }, cx)))
}
