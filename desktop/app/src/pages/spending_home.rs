//! Spending's Home tab: what living costs, worked out from the ledger
//! (finance_core::home): the headline, the breakdown by group, twelve
//! stacked months, and the bills that come back. Rows drill into the
//! category's deep dive.

use chrono::NaiveDate;
use std::rc::Rc;

use finance_core::home::{self, Bill, DepositStatus, HomeSetup, Line, LeaseState, LivingCosts};
use finance_core::{Bundle, Ledger};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, button::{Button, ButtonVariants as _}, h_flex, input::InputState, tooltip::Tooltip, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::category;
use crate::dive::{Dive, OnDive};
use crate::fmt;
use crate::ui::{self, ts, Figures as _};
use crate::zoom::px;

const CHART_H: f32 = 150.;

/// Dev and screenshot aid: `FINANCE_DESKTOP_HOME_DEMO=mortgage` (the
/// browser: `?home_demo=mortgage`) swaps the saved setup for a made-up
/// mortgage, so the mortgage pages can be shot on the rent-mode demo.
fn demo_mortgage() -> Option<HomeSetup> {
    (crate::app::dev_aid("FINANCE_DESKTOP_HOME_DEMO").as_deref() == Some("mortgage")).then(|| {
        let today = chrono::Utc::now().date_naive();
        let start = today.checked_sub_months(chrono::Months::new(26)).unwrap_or(today);
        let d = |d: NaiveDate| Some(d.format("%Y-%m-%d").to_string());
        HomeSetup::parse(&serde_json::json!({
            "mode": "mortgage",
            "mortgage": {
                "principal": 420000.0, "annual_rate_pct": 3.4, "term_months": 300, "start": d(start), "payment_day": 1,
                "lender_match": "HOMELOAN SERVICING",
                "home_value": 560000.0, "home_value_date": d(today.checked_sub_months(chrono::Months::new(2)).unwrap_or(today)),
                "overpayments": [{"date": d(start.checked_add_months(chrono::Months::new(14)).unwrap_or(start)), "amount": 10000.0}],
                "rate_changes": [{"from": d(start.checked_add_months(chrono::Months::new(18)).unwrap_or(start)), "annual_rate_pct": 3.9}],
            },
            "oneoffs": [{"kind": "stamp_duty", "amount": 14000.0, "date": d(start)}, {"kind": "legal_fee", "amount": 2200.0, "date": d(start)}],
        })).unwrap_or_default()
    })
}

/// The saved home setup, or the dev aid's made-up one (unparsed, so the
/// Settings form can show it too).
pub fn saved() -> Option<HomeSetup> {
    // dev aid: FINANCE_DESKTOP_HOME_SETUP=empty (?home_setup=empty) ignores the saved setup
    if crate::app::dev_aid("FINANCE_DESKTOP_HOME_SETUP").as_deref() == Some("empty") {
        return None;
    }
    demo_mortgage().or_else(|| finance_core::user_settings::get(home::SETTING_KEY).as_ref().and_then(HomeSetup::parse))
}

/// The saved home setup (Settings › Home), parsed once per change.
pub fn setup() -> Rc<Option<HomeSetup>> {
    let raw = finance_core::user_settings::get(home::SETTING_KEY);
    let aid = (crate::app::dev_aid("FINANCE_DESKTOP_HOME_DEMO"), crate::app::dev_aid("FINANCE_DESKTOP_HOME_SETUP"));
    crate::memo::get("home.setup", &(raw.as_ref().map(|v| v.to_string()), aid), saved)
}

/// The tab's sections (`today` is "YYYY-MM-DD"); the page puts the tab
/// switch above them.
pub fn render(ledger: &Ledger, bundle: &Bundle, today: &str, whatif: &Entity<InputState>, on_dive: OnDive, cx: &App) -> ui::Sections {
    let current = today.get(..7).unwrap_or("").to_string();
    let setup = setup();
    let base = fmt::base_ccy();
    let rent = setup.as_ref().as_ref().and_then(|s| s.rent_in(&base));
    let mortgage = setup.as_ref().as_ref().is_some_and(|s| s.mode == "mortgage");
    let pay = setup.as_ref().as_ref().and_then(|s| today.parse().ok().and_then(|t| s.mortgage_payment_in(&base, t)));
    let costs = crate::memo::get("home.costs", &current, || home::living_costs(ledger, &current, home::MONTHS_BACK));
    let bills = crate::memo::get("home.bills", &(today, rent.map(f64::to_bits), pay.map(f64::to_bits)), || home::bills_expecting(ledger, today, &[("Rent", rent), (home::MORTGAGE, pay)]));
    let mut page = ui::Sections::new().gap(16.);
    match setup.as_ref() {
        None => page = page.child(setup_card(cx)),
        Some(s) if s.mode == "mortgage" => {
            for part in super::home_mortgage::sections(s, &costs, today, whatif, cx) {
                page = page.child(part);
            }
        }
        Some(s) => {
            let cards = setup_cards(s, &costs, ledger, bundle, today, cx);
            if !cards.is_empty() {
                page = page.child(h_flex().gap_4().flex_wrap().items_stretch().children(cards));
            }
        }
    }
    if costs.months.is_empty() {
        return page.child(ui::empty("No full month of home costs yet", cx));
    }
    page.child(headline(&costs, cx))
        .child(breakdown(&costs, mortgage, on_dive, cx))
        .child(chart(&costs, cx))
        .when(!bills.is_empty(), |d| d.child(bills_section(&bills, rent.is_some(), cx)))
}

/// "Set up your home": no setup saved yet.
fn setup_card(cx: &App) -> impl IntoElement {
    ui::card(cx).gap_2()
        .child(ui::eyebrow("Set up your home", cx))
        .child(ui::caption_wrap("Add your rent or mortgage to see them here: the lease and the deposit with a reminder before the lease ends, or the balance, the payoff and your equity.", cx))
        .child(h_flex().child(Button::new("home-setup-go").small().primary().label("Set up in Settings")
            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::app::GoHomeSetup), cx))))
}

fn long_day(d: &str) -> String {
    NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_else(|_| d.to_string())
}

/// An amount in the setup's currency (the base one when it names none).
fn money(v: f64, ccy: &str) -> String {
    fmt::amount_in(ccy, v)
}

fn days(n: i64) -> String {
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

/// The Lease and Deposit cards a setup gives.
fn setup_cards(s: &HomeSetup, costs: &LivingCosts, ledger: &Ledger, bundle: &Bundle, today: &str, cx: &App) -> Vec<AnyElement> {
    let base = fmt::base_ccy();
    let ccy = s.currency.clone().filter(|c| !c.trim().is_empty()).map(|c| c.trim().to_uppercase()).unwrap_or_else(|| base.clone());
    let mut cards = Vec::new();
    if s.mode == "rent" {
        cards.push(lease_card(s, &ccy, &base, costs, today, cx).into_any_element());
    }
    if let Some(d) = s.deposit() {
        let held: Vec<(String, String)> = bundle.arr(&["receivables", "deposits"]).iter()
            .filter_map(|v| Some((v.get("since")?.as_str()?.to_string(), v.get("currency")?.as_str()?.to_string())))
            .collect();
        let status = home::deposit_status(d, &ccy, &held, ledger);
        cards.push(deposit_card(d, &ccy, &status, cx).into_any_element());
    }
    cards
}

fn lease_card(s: &HomeSetup, ccy: &str, base: &str, costs: &LivingCosts, today: &str, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let lease = &s.rent.lease;
    let explain = "From your home setup (Settings › Home). The renewal reminder shows from 60 days before the lease ends. \
        Rent changes are read from the Rent rows: a month that differs from the last month paid. The last paid is the Rent category's latest month.";
    let (figure, sub, chip) = match lease.state(today) {
        Some(LeaseState::Active { days_left, renew }) => (
            days(days_left),
            format!("left, ends {}", long_day(lease.end.as_deref().unwrap_or(""))),
            renew.then(|| ui::chip("renewal due", t.warning, cx)),
        ),
        Some(LeaseState::Ended { days_ago }) => (
            "Ended".to_string(),
            format!("{} ago, on {}", days(days_ago), long_day(lease.end.as_deref().unwrap_or(""))),
            Some(ui::chip("ended", fmt::money_out(), cx)),
        ),
        None => ("No end date".to_string(), "add the lease's dates in Settings › Home".to_string(), None),
    };
    let rent = costs.housing.iter().find(|l| l.category == "Rent");
    let last = rent.and_then(|r| home::last_rent(r, &costs.months));
    let changes = rent.map(|r| home::rent_changes(r, &costs.months)).unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    if s.rent.monthly > 0. {
        let set = format!("Rent set at {} a month", money(s.rent.monthly, ccy));
        lines.push(match &last {
            Some((m, v)) => format!("{set}; last paid {} ({})", fmt::hkd(*v), fmt::month_label(m)),
            None => set,
        });
    } else if let Some((m, v)) = &last {
        lines.push(format!("Last rent paid {} ({})", fmt::hkd(*v), fmt::month_label(m)));
    }
    if rent.is_some() {
        lines.push(if changes.is_empty() {
            format!("No rent change in the last {} months", costs.months.len())
        } else {
            let shown: Vec<String> = changes.iter().rev().take(3).rev()
                .map(|c| format!("{}: {} to {}", fmt::month_label(&c.month), fmt::hkd(c.from), fmt::hkd(c.to))).collect();
            format!("Rent changed {}", shown.join("; "))
        });
    }
    let differs = ccy.eq_ignore_ascii_case(base) && s.rent.monthly > 0. && last.as_ref().is_some_and(|(_, v)| (v - s.rent.monthly).abs() > 1.);
    ui::card(cx).flex_1().min_w(crate::layout::col(280.)).gap(px(6.))
        .child(h_flex().h(px(18.)).child(ui::eyebrow_info("Lease", explain, cx)))
        .child(h_flex().gap_2().items_center().flex_wrap()
            .child(div().text_size(px(ts::FIGURE)).font_weight(FontWeight::SEMIBOLD).figures().whitespace_nowrap().child(figure))
            .children(chip)
            .when(differs, |d| d.child(ui::chip("rent differs from the setup", t.warning, cx))))
        .child(ui::caption(sub, cx))
        .children(lines.into_iter().map(|l| ui::caption_wrap(l, cx)))
}

fn deposit_card(d: &home::OneOff, ccy: &str, status: &DepositStatus, cx: &App) -> impl IntoElement {
    let explain = "From your home setup. Returned when you set a returned date, or when the bank rows show the deposit paid and since refunded (Owed to you no longer lists it). \
        A deposit that isn't in the bank rows can't be read, so it shows as held, as entered.";
    let (chip, note) = match status {
        DepositStatus::Held => (ui::chip("held", cx.theme().warning, cx), "Still held by the landlord".to_string()),
        DepositStatus::Returned { on: Some(on), by_setting } => (
            ui::chip(format!("returned {}", long_day(on)), fmt::money_in(), cx),
            if *by_setting { "Returned on the date in your setup".to_string() } else { "The bank rows show the refund".to_string() },
        ),
        DepositStatus::Returned { on: None, .. } => (ui::chip("returned", fmt::money_in(), cx), "The bank rows show it refunded".to_string()),
    };
    ui::card(cx).flex_1().min_w(crate::layout::col(280.)).gap(px(6.))
        .child(h_flex().h(px(18.)).child(ui::eyebrow_info("Deposit", explain, cx)))
        .child(h_flex().gap_2().items_center().flex_wrap()
            .child(div().text_size(px(ts::FIGURE)).font_weight(FontWeight::SEMIBOLD).figures().whitespace_nowrap().child(money(d.amount, ccy)))
            .child(chip))
        .child(ui::caption(match d.date.as_deref().filter(|x| !x.trim().is_empty()) {
            Some(x) => format!("paid {}", long_day(x)),
            None => "no paid date in your setup".to_string(),
        }, cx))
        .child(ui::caption_wrap(note, cx))
}

fn pct(f: Option<f64>) -> String {
    f.map(|f| format!("{:.0}%", f * 100.)).unwrap_or_else(|| "no income".into())
}

fn headline(c: &LivingCosts, cx: &App) -> impl IntoElement {
    let (last, avg) = c.headline();
    let (share_last, share_avg) = c.income_share();
    let month = c.months.last().map(|m| fmt::month_label(m)).unwrap_or_default();
    let explain = "Housing, groceries and the other essentials (categories that are fixed or an obligation), at your share. \
        Income is what the bank and card rows of the same months brought in: pay, interest, refunds.";
    h_flex().gap_4().flex_wrap().items_stretch().children([
        ui::stat_info("Cost of living", fmt::hkd(last), format!("{month}, the last full month"), None, explain, cx),
        ui::stat("12-month average", fmt::hkd(avg), format!("a month, over {} months", c.months.len()), None, cx),
        ui::stat("Share of income", pct(share_last), format!("average {}", pct(share_avg)), None, cx),
    ].map(|d| d.min_w(px(220.))))
}

/// One breakdown row: tile, name, last month, average. `on_dive` makes it
/// a link to the category.
fn row(id: impl Into<SharedString>, name: &str, last: f64, avg: f64, indent: bool, strong: bool, dive: Option<(OnDive, String)>, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let w = if crate::layout::compact() { 78. } else { 110. };
    let id: SharedString = id.into();
    let name_cell = h_flex().gap_2().items_center().flex_1().min_w_0()
        .when(indent, |d| d.pl(px(ui::sp::L)))
        .child(category::tile(name, 20., t.muted_foreground))
        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap()
            .when(strong, |d| d.font_weight(FontWeight::SEMIBOLD)).child(name.to_string()));
    let amount = |v: f64| div().w(px(w)).flex_shrink_0().text_right().whitespace_nowrap().figures().child(fmt::hkd(v));
    let hover = t.accent;
    h_flex().id(id).gap_3().py(px(5.)).px_1().rounded(t.radius).text_size(px(ts::BODY))
        .child(name_cell)
        .child(amount(last))
        .child(amount(avg).text_color(t.muted_foreground))
        .when_some(dive, |d, (on_dive, cat)| d.cursor_pointer().hover(move |s| s.bg(hover))
            .tooltip(|w, cx| Tooltip::new("Open this category's deep dive").build(w, cx))
            .on_click(move |_, window, cx| on_dive(Dive::Category(cat.clone()), window, cx)))
}

fn breakdown(c: &LivingCosts, mortgage: bool, on_dive: OnDive, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let w = if crate::layout::compact() { 78. } else { 110. };
    let head = h_flex().gap_3().px_1().pb(px(4.)).border_b_1().border_color(ui::hairline(cx))
        .child(div().flex_1())
        .child(div().w(px(w)).text_right().child(ui::caption(c.months.last().map(|m| fmt::month_label(m)).unwrap_or_default(), cx)))
        .child(div().w(px(w)).text_right().child(ui::caption("average", cx)));
    let mut rows: Vec<AnyElement> = Vec::new();
    let group = |id: &str, name: &str, total: &[f64], lines: &[Line], rows: &mut Vec<AnyElement>| {
        rows.push(row(format!("home-g-{id}"), name, total.last().copied().unwrap_or(0.), total.iter().sum::<f64>() / total.len().max(1) as f64, false, true, None, cx).into_any_element());
        for l in lines {
            rows.push(row(format!("home-c-{}", l.category), &l.category, l.last(), l.avg(), true, false, Some((on_dive.clone(), l.category.clone())), cx).into_any_element());
        }
    };
    group("housing", "Housing", &c.housing_total(), &c.housing, &mut rows);
    rows.push(row("home-c-groceries", &c.groceries.category, c.groceries.last(), c.groceries.avg(), false, true,
        Some((on_dive.clone(), c.groceries.category.clone())), cx).into_any_element());
    if !c.essentials.is_empty() {
        group("essentials", "Other essentials", &c.essentials_total(), &c.essentials, &mut rows);
    }
    rows.push(div().h(px(1.)).my(px(4.)).bg(ui::hairline(cx)).into_any_element());
    rows.push(row("home-other", "Everything else", c.other.last(), c.other.avg(), false, false, None, cx).into_any_element());
    let explain = format!("Housing is the home categories (Rent, Mortgage, Electricity, ...); Other essentials is every other category that is fixed or an obligation. \
        Everything else is the rest of your spending, for context.{}", if mortgage {
        " Mortgage payments count in full, interest and principal alike, because both leave your account; the principal part is saving into your equity, not a cost."
    } else { "" });
    v_flex().gap_2()
        .child(ui::section_info("Where living costs go", if crate::layout::compact() { "" } else { "a month, last full month and average" }, explain, cx))
        .child(ui::card(cx).gap_1().child(head).children(rows))
        .text_color(t.foreground)
}

/// The three groups of the chart, bottom to top.
fn groups(c: &LivingCosts, cx: &App) -> [(&'static str, Hsla, Vec<f64>); 3] {
    let t = cx.theme();
    [
        ("Housing", t.chart_1, c.housing_total()),
        ("Groceries", t.chart_2, c.groceries.values.clone()),
        ("Other essentials", t.chart_3, c.essentials_total()),
    ]
}

/// Twelve months stacked: housing, groceries, other essentials, one value
/// axis (drawn here: the chart kit's bars don't stack).
fn chart(c: &LivingCosts, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let groups = groups(c, cx);
    let totals = c.living_total();
    let max = totals.iter().copied().fold(0., f64::max).max(1.);
    let bars = c.months.iter().enumerate().map(|(i, m)| {
        let label = fmt::month_label(m);
        let tip: SharedString = format!("{label}: {}", fmt::hkd(totals[i])).into();
        let stack = v_flex().w_full().max_w(px(28.)).h_full().justify_end().rounded_t(px(3.)).overflow_hidden()
            .children(groups.iter().rev().map(|(_, color, v)| div().w_full().h(relative((v[i] / max) as f32)).bg(*color)));
        v_flex().id(("home-bar", i)).flex_1().min_w_0().h_full().items_center().gap_1()
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .child(div().flex_1().w_full().flex().items_end().justify_center().child(stack))
            .child(div().text_size(px(ts::EYEBROW)).text_color(t.muted_foreground).whitespace_nowrap()
                .child(label.split_whitespace().next().unwrap_or("").to_string()))
    });
    let legend = h_flex().gap_3().flex_wrap().children(groups.iter().map(|(n, color, v)| ui::legend(*color, *n, fmt::compact(v.last().copied().unwrap_or(0.)), cx)));
    ui::card(cx).gap(px(ui::sp::M))
        .child(h_flex().gap_3().justify_between().flex_wrap()
            .child(h_flex().gap_2().items_baseline().child(ui::section("Living costs per month", "stacked, by group", cx)))
            .child(legend))
        .child(h_flex().gap_1().items_stretch().h(px(CHART_H))
            .child(v_flex().justify_between().items_end().pr_1().pb(px(ts::EYEBROW + 5.)).text_size(px(ts::EYEBROW)).text_color(t.muted_foreground)
                .child(fmt::compact(max)).child(fmt::compact(0.)))
            .children(bars))
}

fn day_label(d: &str) -> String {
    NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|d| d.format("%b %-d").to_string()).unwrap_or_else(|_| d.to_string())
}

fn bills_section(bills: &[Bill], rent_set: bool, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let explain = "What comes back: each home category with at least two months of payments. Typical is the usual amount before the last one; \
        missing means the next payment is a week overdue; high means the last one was over 1.5 times the typical.";
    let rows = bills.iter().map(|b| {
        let due = if b.missing { format!("was due {}", day_label(&b.next)) } else { format!("next {}", day_label(&b.next)) };
        h_flex().gap_3().py(px(6.)).px_1().items_center().text_size(px(ts::BODY))
            .child(category::tile(&b.category, 22., t.muted_foreground))
            .child(v_flex().flex_1().min_w_0()
                .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(b.category.clone()))
                .child(ui::caption(if rent_set && b.category == "Rent" {
                    format!("{} · {} set in your home setup", home::cadence(b.every), fmt::hkd(b.typical))
                } else {
                    format!("{} · typically {}", home::cadence(b.every), fmt::hkd(b.typical))
                }, cx)))
            .when(b.missing, |d| d.child(ui::chip("missing", fmt::money_out(), cx)))
            .when(b.high, |d| d.child(ui::chip("high", t.warning, cx)))
            .child(v_flex().items_end().flex_shrink_0()
                .child(div().figures().whitespace_nowrap().child(fmt::hkd(b.last_amount)))
                .child(ui::caption(format!("last {} · {due}", day_label(&b.last_date)), cx)))
    });
    v_flex().gap_2()
        .child(ui::section_info("Bills", "rent first, then what repeats", explain, cx))
        .child(ui::card(cx).gap_1().children(rows))
}
