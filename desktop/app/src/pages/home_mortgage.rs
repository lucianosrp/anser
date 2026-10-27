//! Spending's Home tab in mortgage mode: the Mortgage and Equity cards, the
//! balance over time, each payment split into interest and principal, and
//! the "what if I paid more" input. The maths is finance_core::mortgage;
//! nothing here moves unless the data or the typed amount does.

use chrono::{Datelike, NaiveDate};
use finance_core::home::{self, HomeSetup, LivingCosts, MORTGAGE};
use finance_core::mortgage::{self, Params, Row};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex,
    input::{Input, InputState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, ts, Figures as _};
use crate::zoom::px;

const CHART_H: f32 = 120.;
/// At most this many bars on the balance chart.
const MAX_BARS: usize = 120;

fn long_month(d: NaiveDate) -> String {
    d.format("%b %Y").to_string()
}

fn span(months: usize) -> String {
    let (y, m) = (months / 12, months % 12);
    match (y, m) {
        (0, m) => format!("{m} month{}", if m == 1 { "" } else { "s" }),
        (y, 0) => format!("{y} year{}", if y == 1 { "" } else { "s" }),
        (y, m) => format!("{y} year{} {m} month{}", if y == 1 { "" } else { "s" }, if m == 1 { "" } else { "s" }),
    }
}

fn figure(text: String) -> Div {
    div().text_size(px(ts::FIGURE)).font_weight(FontWeight::SEMIBOLD).figures().whitespace_nowrap().child(text)
}

fn card(cx: &App) -> Div {
    ui::card(cx).flex_1().min_w(crate::layout::col(280.)).gap(px(6.))
}

/// The tab's mortgage sections, in order (the loan's setup, if it can't be
/// worked out yet, is one card saying so).
pub fn sections(s: &HomeSetup, costs: &LivingCosts, today: &str, whatif: &Entity<InputState>, cx: &App) -> Vec<AnyElement> {
    let Ok(now) = NaiveDate::parse_from_str(today, "%Y-%m-%d") else { return vec![] };
    let Some(loan) = s.loan() else {
        return vec![ui::card(cx).gap_2().child(ui::eyebrow("Mortgage", cx))
            .child(ui::caption_wrap("Add the amount borrowed, the term and the loan's start date in Settings › Home to see the balance, the payoff and the equity.", cx))
            .into_any_element()];
    };
    let key = serde_json::to_string(&s.mortgage).unwrap_or_default();
    let rows = crate::memo::get("home.schedule", &key, || mortgage::schedule(&loan));
    let base = fmt::base_ccy();
    let ccy = s.currency.clone().filter(|c| !c.trim().is_empty()).map(|c| c.trim().to_uppercase()).unwrap_or(base.clone());
    let money = {
        let ccy = ccy.clone();
        move |v: f64| fmt::amount_in(&ccy, v)
    };
    let balance = if now < loan.start { loan.principal } else { mortgage::balance_on(&loan, &rows, now) };
    let mut out: Vec<AnyElement> = Vec::new();
    out.push(h_flex().gap_4().flex_wrap().items_stretch()
        .child(mortgage_card(&loan, &rows, balance, costs, now, &ccy == &base, &money, cx))
        .children(equity_card(s, balance, &money, cx))
        .into_any_element());
    out.push(balance_chart(&loan, &rows, now, &money, cx).into_any_element());
    out.push(h_flex().gap_4().flex_wrap().items_stretch()
        .child(split_card(&rows, now, &money, cx))
        .child(whatif_card(&loan, now, whatif, &money, cx))
        .into_any_element());
    out
}

#[allow(clippy::too_many_arguments)]
fn mortgage_card(loan: &Params, rows: &[Row], balance: f64, costs: &LivingCosts, now: NaiveDate, base: bool, money: &dyn Fn(f64) -> String, cx: &App) -> Div {
    let explain = "From your home setup (Settings › Home): the loan's amount, rate, term and start, with the overpayments and rate changes you listed. \
        The balance is what is owed after the last scheduled payment; interest and payoff come from the same schedule. \
        Paid vs set compares your latest Mortgage row with the schedule's payment.";
    let t = cx.theme();
    let started = now >= loan.start;
    let pay = mortgage::payment_now(rows, now).unwrap_or(0.);
    let rate = mortgage::rate_now(rows, now).unwrap_or(loan.annual_rate_pct);
    let left = mortgage::payments_left(rows, now);
    let year_from = NaiveDate::from_ymd_opt(now.year(), 1, 1).unwrap_or(now);
    let (this_year, to_date, total) = (mortgage::interest_between(rows, year_from, now), mortgage::interest_to(rows, now), mortgage::total_interest(rows));
    let mut lines = vec![
        format!("Payment {} a month at {}% a year", money(pay), rate),
        match mortgage::payoff_date(rows) {
            Some(d) if left > 0 => format!("Paid off {} ({} to go)", long_month(d), span(left)),
            Some(d) => format!("Paid off in {}", long_month(d)),
            None => "No payments scheduled".to_string(),
        },
        format!("Interest {} this year, {} to date, {} over the loan", money(this_year), money(to_date), money(total)),
    ];
    let line = costs.housing.iter().find(|l| l.category == MORTGAGE);
    let last = line.and_then(|l| home::last_rent(l, &costs.months));
    let mut chip = None;
    match &last {
        Some((m, v)) if base => {
            lines.push(format!("Last paid {} ({}) against {} set", fmt::hkd(*v), fmt::month_label(m), money(pay)));
            if (v - pay).abs() > 1. {
                chip = Some(ui::chip("differs from the schedule", t.warning, cx));
            }
        }
        Some((m, v)) => lines.push(format!("Last paid {} ({})", fmt::hkd(*v), fmt::month_label(m))),
        None => lines.push("No Mortgage rows yet: mark the payments as Mortgage in Transactions".to_string()),
    }
    card(cx)
        .child(h_flex().h(px(18.)).child(ui::eyebrow_info("Mortgage", explain, cx)))
        .child(h_flex().gap_2().items_center().flex_wrap().child(figure(money(balance))).children(chip))
        .child(ui::caption(if started { "owed today".to_string() } else { format!("borrowed from {}", long_month(loan.start)) }, cx))
        .children(lines.into_iter().map(|l| ui::caption_wrap(l, cx)))
}

fn equity_card(s: &HomeSetup, balance: f64, money: &dyn Fn(f64) -> String, cx: &App) -> Option<Div> {
    let value = s.mortgage.home_value.filter(|v| *v > 0.)?;
    let (ltv, equity) = mortgage::equity(balance, value)?;
    let explain = "The home's value is the figure you entered (Settings › Home), not a live estimate. Equity is that value less what you owe; \
        loan to value is what you owe as a share of the value.";
    let when = s.mortgage.home_value_date.as_deref().filter(|d| !d.trim().is_empty())
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).map(|d| format!("valued {}", d.format("%-d %b %Y"))).unwrap_or_else(|| "as entered".to_string());
    Some(card(cx)
        .child(h_flex().h(px(18.)).child(ui::eyebrow_info("Equity", explain, cx)))
        .child(figure(money(equity)))
        .child(ui::caption(format!("home {} ({when})", money(value)), cx))
        .child(ui::caption_wrap(format!("Loan to value {ltv:.0}%"), cx)))
}

/// The balance after each payment (every nth when long): paid ones solid,
/// the rest muted.
fn balance_chart(loan: &Params, rows: &[Row], now: NaiveDate, money: &dyn Fn(f64) -> String, cx: &App) -> Div {
    let t = cx.theme();
    let step = rows.len().div_ceil(MAX_BARS).max(1);
    let mut points: Vec<(NaiveDate, f64)> = vec![(loan.start, loan.principal)];
    points.extend(rows.iter().enumerate().filter(|(i, _)| i % step == step - 1 || *i + 1 == rows.len()).map(|(_, r)| (r.date, r.balance)));
    let max = loan.principal.max(1.);
    let bars = points.iter().enumerate().map(|(i, (d, b))| {
        let color = if *d <= now { t.chart_1 } else { t.chart_1.opacity(0.35) };
        let tip: SharedString = format!("{}: {} owed", long_month(*d), money(*b)).into();
        div().id(("mort-bal", i)).flex_1().min_w_0().h_full().flex().items_end()
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .child(div().w_full().h(relative((b / max) as f32)).bg(color))
    });
    let (first, last) = (points.first().map(|p| p.0.year()), points.last().map(|p| p.0.year()));
    ui::card(cx).gap(px(ui::sp::M))
        .child(h_flex().gap_3().justify_between().flex_wrap()
            .child(ui::section("Balance over time", "what is owed after each payment", cx))
            .child(h_flex().gap_3().flex_wrap()
                .child(ui::legend(t.chart_1, "paid so far", "", cx))
                .child(ui::legend(t.chart_1.opacity(0.35), "to come", "", cx))))
        .child(h_flex().gap_1().items_stretch().h(px(CHART_H))
            .child(v_flex().justify_between().items_end().pr_1().text_size(px(ts::EYEBROW)).text_color(t.muted_foreground)
                .child(fmt::compact(max)).child(fmt::compact(0.)))
            .child(h_flex().flex_1().min_w_0().h_full().items_end().gap(px(1.)).children(bars)))
        .child(h_flex().justify_between().pl(px(36.)).text_size(px(ts::EYEBROW)).text_color(t.muted_foreground)
            .child(first.map(|y| y.to_string()).unwrap_or_default()).child(last.map(|y| y.to_string()).unwrap_or_default()))
}

/// The last twelve payments (or fewer, early on): interest over principal.
fn split_card(rows: &[Row], now: NaiveDate, money: &dyn Fn(f64) -> String, cx: &App) -> Div {
    let t = cx.theme();
    let paid: Vec<&Row> = rows.iter().filter(|r| r.date <= now).collect();
    let recent = &paid[paid.len().saturating_sub(12)..];
    // the scale is the regular payments: an overpayment is drawn apart, capped
    let max = recent.iter().map(|r| r.interest + r.principal - r.extra).fold(0., f64::max).max(1.);
    let any_extra = recent.iter().any(|r| r.extra > 0.);
    let explain = "Each scheduled payment of the last 12 split into the interest charged and the principal repaid (an overpayment is drawn apart, and its bar is capped so it doesn't flatten the rest). Principal is not spent: it is saving into your equity.";
    let body: AnyElement = if recent.is_empty() {
        ui::caption("No payment yet", cx).into_any_element()
    } else {
        let bars = recent.iter().enumerate().map(|(i, r)| {
            let tip: SharedString = format!("{}: interest {}, principal {}{}", long_month(r.date), money(r.interest), money(r.principal - r.extra),
                if r.extra > 0. { format!(", overpayment {}", money(r.extra)) } else { String::new() }).into();
            let stack = v_flex().w_full().max_w(px(28.)).h_full().justify_end().rounded_t(px(3.)).overflow_hidden()
                .child(div().w_full().h(relative((r.interest / max) as f32)).bg(t.chart_3))
                .child(div().w_full().h(relative(((r.principal - r.extra) / max) as f32)).bg(t.chart_2))
                .when(r.extra > 0., |d| d.child(div().w_full().h(relative((r.extra / max).min(0.35) as f32)).bg(t.chart_4)));
            v_flex().id(("mort-split", i)).flex_1().min_w_0().h_full().items_center().gap_1()
                .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                .child(div().flex_1().w_full().flex().items_end().justify_center().child(stack))
                .child(div().text_size(px(ts::EYEBROW)).text_color(t.muted_foreground).whitespace_nowrap().child(r.date.format("%b").to_string()))
        });
        h_flex().gap_1().items_stretch().h(px(CHART_H - 20.)).children(bars).into_any_element()
    };
    let (i, p) = recent.last().map(|r| (r.interest, r.principal - r.extra)).unwrap_or((0., 0.));
    card(cx)
        .child(h_flex().h(px(18.)).child(ui::eyebrow_info("Interest and principal", explain, cx)))
        .child(h_flex().gap_3().flex_wrap()
            .child(ui::legend(t.chart_3, "interest", money(i), cx))
            .child(ui::legend(t.chart_2, "principal", money(p), cx))
            .when(any_extra, |d| d.child(ui::legend(t.chart_4, "overpayment", "", cx))))
        .child(body)
}

/// "What if": an extra amount every month, live as it is typed.
fn whatif_card(loan: &Params, now: NaiveDate, input: &Entity<InputState>, money: &dyn Fn(f64) -> String, cx: &App) -> Div {
    let explain = "Pays this much extra every month from the next payment, on top of the schedule (and its overpayments and rate changes), \
        and shows how many months sooner the loan ends and how much interest that saves.";
    let typed = input.read(cx).value().to_string();
    let extra = typed.replace([',', ' '], "").replace(&fmt::sym(), "").parse::<f64>().ok().filter(|v| *v > 0.);
    let result = match (extra, typed.trim().is_empty()) {
        (Some(x), _) => {
            let w = crate::memo::get("home.whatif", &(format!("{:?}", loan), x.to_bits(), now.to_string()), || mortgage::what_if(loan, x, now));
            if w.months_saved <= 0 {
                "Nothing sooner: that is too small to move the end date".to_string()
            } else {
                format!("{} sooner (paid off {}), {} less interest", span(w.months_saved as usize), w.payoff.map(long_month).unwrap_or_default(), money(w.interest_saved))
            }
        }
        (None, true) => "Type an extra amount to see what it saves".to_string(),
        (None, false) => "Type a positive number".to_string(),
    };
    card(cx)
        .child(h_flex().h(px(18.)).child(ui::eyebrow_info("Pay a little more", explain, cx)))
        .child(h_flex().gap_2().items_center().child(div().w(px(120.)).child(Input::new(input).small())).child(ui::caption("extra a month", cx)))
        .child(div().text_size(px(ts::BODY)).child(result))
}
