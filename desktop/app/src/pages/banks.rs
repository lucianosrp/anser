//! Banks: where the cash sits right now (per bank, pockets grouped, native
//! currency and HKD), whether each bank's export is fresh enough to trust, the
//! currency mix, and how the cash pile and the monthly in/out move over time.

use std::collections::BTreeMap;

use chrono::{Datelike, Local, NaiveDate};
use finance_core::Bundle;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, chart::{BarChart, LineChart}, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::fmt;
use crate::ui::{self, Pt};
use crate::zoom::px;

/// A bank whose newest row is older than this needs a fresh export.
const STALE_DAYS: i64 = 45;
/// No movement for this long: a dead account, shown apart.
const INACTIVE_DAYS: i64 = 365;
/// A pocket untouched this long gets a quiet "idle since" note.
const IDLE_DAYS: i64 = 90;

struct Account {
    group: String,
    name: String,
    currency: String,
    native: f64,
    hkd: f64,
    last: Option<NaiveDate>,
}

struct Group {
    name: String,
    accounts: Vec<Account>,
}

impl Group {
    fn hkd(&self) -> f64 {
        self.accounts.iter().map(|a| a.hkd).sum()
    }
    fn last(&self) -> Option<NaiveDate> {
        self.accounts.iter().filter_map(|a| a.last).max()
    }
}

#[derive(Clone)]
struct Flow {
    label: SharedString,
    inflow: f64,
    outflow: f64,
    net: f64,
}

fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn days_since(d: Option<NaiveDate>, today: NaiveDate) -> i64 {
    d.map(|d| (today - d).num_days()).unwrap_or(i64::MAX)
}

/// "€ 31,985.53", "HK$ 2,205", "¥ 27"
fn native(ccy: &str, v: f64) -> String {
    match ccy {
        "HKD" => fmt::hkd(v),
        "EUR" => format!("€ {}", fmt::dec2(v)),
        "GBP" => format!("£ {}", fmt::dec2(v)),
        "USD" => format!("US$ {}", fmt::dec2(v)),
        "JPY" => format!("¥ {}", fmt::int(v)),
        _ => format!("{} {ccy}", fmt::dec2(v)),
    }
}

/// "Feb 26"
fn short_day(d: NaiveDate) -> String {
    fmt::month_label(&format!("{:04}-{:02}", d.year(), d.month()))
}

/// `ui::stat` that keeps the figure on one line and packs two to a row at
/// half-screen width.
fn tile(label: &str, value: impl Into<SharedString>, sub: impl Into<SharedString>, tone: Option<Hsla>, cx: &App) -> Div {
    let t = cx.theme();
    ui::card(cx)
        .flex_1()
        .min_w(px(220.))
        .child(div().text_xs().text_color(t.muted_foreground).child(label.to_uppercase()))
        .child(div().text_2xl().font_semibold().whitespace_nowrap().when_some(tone, |d, c| d.text_color(c)).child(value.into()))
        .child(div().text_xs().text_color(t.muted_foreground).child(sub.into()))
}

fn chip(text: String, color: Hsla) -> Div {
    div()
        .px_2()
        .rounded_full()
        .bg(color.opacity(0.15))
        .text_color(color)
        .text_xs()
        .whitespace_nowrap()
        .child(text)
}

pub fn render(b: &Bundle, cx: &App) -> ui::Sections {
    let t = cx.theme();
    let today = Local::now().date_naive();
    let this_month = today.format("%Y-%m").to_string();
    let last_txn = b.at(&["banks", "last_txn"]);

    // ---- current balances, grouped by bank ------------------------------
    let mut groups: Vec<Group> = Vec::new();
    for s in b.arr(&["net_worth", "by_source"]) {
        if s.get("kind").and_then(Value::as_str) != Some("bank") {
            continue;
        }
        let key = s.get("key").and_then(Value::as_str).unwrap_or("");
        let label = s.get("label").and_then(Value::as_str).unwrap_or(key);
        let (group, name) = match label.split_once(" · ") {
            Some((g, n)) => (g.to_string(), n.to_string()),
            None => (label.to_string(), String::new()),
        };
        let acc = Account {
            group,
            name,
            currency: s.get("currency").and_then(Value::as_str).unwrap_or("").to_string(),
            native: s.get("native_amount").and_then(Value::as_f64).unwrap_or(0.),
            hkd: s.get("hkd_value").and_then(Value::as_f64).unwrap_or(0.),
            last: last_txn.get(key).and_then(Value::as_str).and_then(date),
        };
        if let Some(g) = groups.iter_mut().find(|g| g.name == acc.group) {
            g.accounts.push(acc);
        } else {
            groups.push(Group { name: acc.group.clone(), accounts: vec![acc] });
        }
    }
    // a whole bank with no movement for a year is dead (an idle pocket at a
    // live bank is not: it still holds money the owner can see and use)
    let (mut groups, dead): (Vec<Group>, Vec<Group>) =
        groups.into_iter().partition(|g| days_since(g.last(), today) <= INACTIVE_DAYS);
    let inactive: Vec<Account> = dead.into_iter().flat_map(|g| g.accounts).collect();
    if groups.is_empty() && inactive.is_empty() {
        return ui::Sections::new().child(ui::empty("No bank data yet — sync first", cx));
    }
    for g in &mut groups {
        g.accounts.sort_by(|a, b| b.hkd.total_cmp(&a.hkd));
    }
    groups.sort_by(|a, b| b.hkd().total_cmp(&a.hkd()));

    let active_hkd: f64 = groups.iter().map(Group::hkd).sum();
    let inactive_hkd: f64 = inactive.iter().map(|a| a.hkd).sum();
    let cash = b.f64(&["net_worth", "banks_hkd"]).unwrap_or(active_hkd + inactive_hkd);
    let share = |v: f64| if cash > 0. { v / cash * 100. } else { 0. };
    let n_accounts: usize = groups.iter().map(|g| g.accounts.len()).sum();

    // ---- cash over time: the bank columns of the monthly sheet (HKD) -----
    let cols: Vec<&str> = b.arr(&["banks", "sheet", "columns"]).iter()
        .filter_map(|c| c.get("key")?.as_str())
        .collect();
    let series: Vec<(String, f64)> = b.arr(&["banks", "sheet", "rows"]).iter()
        .filter_map(|r| {
            let m = r.get("month")?.as_str()?.to_string();
            Some((m, cols.iter().filter_map(|k| r.get(*k)?.as_f64()).sum()))
        })
        .collect();
    let back = |n: usize| series.len().checked_sub(n + 1).map(|i| series[i].1);
    let delta3 = back(3).map(|v| cash - v);

    // ---- monthly in/out, completed months ------------------------------
    let flows: Vec<Flow> = b.arr(&["banks", "cashflows"]).iter()
        .filter(|f| f.get("month").and_then(Value::as_str) != Some(this_month.as_str()))
        .filter_map(|f| Some(Flow {
            label: fmt::month_label(f.get("month")?.as_str()?).into(),
            inflow: f.get("inflow")?.as_f64()?,
            outflow: f.get("outflow")?.as_f64()?.abs(),
            net: f.get("net")?.as_f64()?,
        }))
        .collect();
    let last12: Vec<Flow> = flows.iter().rev().take(12).rev().cloned().collect();
    let k = last12.len().max(1) as f64;
    let avg_net = last12.iter().map(|f| f.net).sum::<f64>() / k;
    let avg_in = last12.iter().map(|f| f.inflow).sum::<f64>() / k;
    let avg_out = last12.iter().map(|f| f.outflow).sum::<f64>() / k;
    let last_flow = flows.last();

    let stale: Vec<(&str, i64)> = groups.iter()
        .map(|g| (g.name.as_str(), days_since(g.last(), today)))
        .filter(|(_, d)| *d > STALE_DAYS)
        .collect();

    let cards = h_flex()
        .gap_4()
        .flex_wrap()
        .items_start()
        .child(tile("Cash", fmt::hkd(cash),
            format!("{n_accounts} accounts at {} banks", groups.len()), None, cx))
        .child(tile("Last 3 months", delta3.map(fmt::signed_hkd).unwrap_or("—".into()),
            delta3.zip(back(3)).filter(|(_, v)| *v > 0.)
                .map(|(d, v)| format!("{} vs 3 months ago", fmt::pct(d / v * 100.))).unwrap_or_default(),
            delta3.map(fmt::signed_color), cx))
        .child(tile("Last month net", last_flow.map(|f| fmt::signed_hkd(f.net)).unwrap_or("—".into()),
            last_flow.map(|f| format!("{} · in {} · out {}", f.label, fmt::compact(f.inflow), fmt::compact(f.outflow)))
                .unwrap_or_default(),
            last_flow.map(|f| fmt::signed_color(f.net)), cx))
        .child(tile("12-month average", format!("{}/mo", fmt::signed_hkd(avg_net)),
            format!("in {} · out {} per month", fmt::compact(avg_in), fmt::compact(avg_out)),
            Some(fmt::signed_color(avg_net)), cx));

    // banner when any bank's export is old: the balances above are then stale
    let banner = (!stale.is_empty()).then(|| {
        let names = stale.iter().map(|(n, d)| format!("{n} ({d} days)")).collect::<Vec<_>>().join(", ");
        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(t.radius)
            .bg(t.warning.opacity(0.12))
            .border_1()
            .border_color(t.warning.opacity(0.4))
            .child(div().text_sm().text_color(t.warning).font_semibold().child("Old exports"))
            .child(div().text_sm().child(format!(
                "{names}: balances may be out of date. Download a fresh statement and ingest it.")))
    });

    // ---- accounts list ---------------------------------------------------
    let compact = crate::layout::compact();
    let num = |w: f32| div().w(px(w)).flex_shrink_0().text_sm().text_right().whitespace_nowrap();
    let account_row = |a: &Account| {
        let idle = days_since(a.last, today);
        h_flex()
            .gap_3()
            .py_1()
            .pl_4()
            .child(h_flex().flex_1().min_w_0().gap_2()
                .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(a.name.clone()))
                .when(idle > IDLE_DAYS && idle != i64::MAX, |d| d.child(
                    div().text_xs().text_color(t.muted_foreground).whitespace_nowrap()
                        .child(format!("idle since {}", short_day(a.last.unwrap_or(today)))))))
            // compact: no native column, the name keeps the room
            .when(!compact, |d| d.child(num(130.).text_color(t.muted_foreground)
                .when(a.currency != fmt::base_ccy(), |d| d.child(native(&a.currency, a.native)))))
            .child(num(110.).child(fmt::hkd(a.hkd)))
            .child(num(48.).text_color(t.muted_foreground).child(format!("{:.0}%", share(a.hkd))))
    };

    let group_block = |g: &Group| {
        let hkd = g.hkd();
        let age = days_since(g.last(), today);
        let single = g.accounts.len() == 1 && g.accounts[0].name.is_empty();
        let fresh = match g.last() {
            Some(_) if age > STALE_DAYS => chip(format!("export {age} days old"), t.warning),
            Some(_) => chip(format!("updated {}", if age == 0 { "today".into() } else { format!("{age} d ago") }),
                t.muted_foreground),
            None => chip("no export date".into(), t.warning),
        };
        let header = h_flex()
            .gap_3()
            .py_1()
            .child(h_flex().flex_1().min_w_0().gap_2().items_center()
                .when_some(crate::logo::tile(crate::logo::for_source(&g.name), 22., cx), |d, l| d.child(l))
                .child(div().text_sm().font_semibold().whitespace_nowrap().child(g.name.clone()))
                .child(fresh))
            .when(!compact, |d| d.child(num(130.).text_color(t.muted_foreground).when(single, |d| {
                let a = &g.accounts[0];
                d.when(a.currency != fmt::base_ccy(), |d| d.child(native(&a.currency, a.native)))
            })))
            .child(num(110.).font_semibold().child(fmt::hkd(hkd)))
            .child(num(48.).text_color(t.muted_foreground).child(format!("{:.0}%", share(hkd))));
        let bar = div().h(px(4.)).rounded_full().bg(t.muted).child(
            div().h_full().rounded_full().bg(t.primary).w(relative((share(hkd) / 100.).clamp(0., 1.) as f32)));
        v_flex()
            .py_2()
            .border_b_1()
            .border_color(t.border)
            .child(header)
            .child(bar)
            .when(!single, |d| d.child(v_flex().pt_1().children(g.accounts.iter().map(account_row))))
    };

    let inactive_line = (!inactive.is_empty()).then(|| {
        let what = inactive.iter()
            .map(|a| {
                let name = if a.name.is_empty() { a.group.clone() } else { format!("{} {}", a.group, a.name) };
                let since = a.last.map(|d| format!(", last activity {}", short_day(d))).unwrap_or_default();
                format!("{name}: {}{since}", native(&a.currency, a.native))
            })
            .collect::<Vec<_>>()
            .join(" · ");
        div().pt_2().text_xs().text_color(t.muted_foreground)
            .child(format!("Inactive, no activity for over a year ({} still counted in Cash): {what}",
                fmt::hkd(inactive_hkd)))
    });

    let accounts = ui::card(cx)
        .gap_0()
        .children(groups.iter().map(group_block))
        .children(inactive_line);

    // ---- currency mix now ---------------------------------------------
    let mut by_ccy: BTreeMap<&str, f64> = BTreeMap::new();
    for a in groups.iter().flat_map(|g| g.accounts.iter()).chain(inactive.iter()) {
        *by_ccy.entry(a.currency.as_str()).or_default() += a.hkd;
    }
    let mut mix: Vec<(&str, f64)> = by_ccy.into_iter().collect();
    mix.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mmax = mix.first().map(|m| m.1).unwrap_or(0.);
    let foreign = 100. - mix.iter().find(|m| m.0 == fmt::base_ccy()).map(|m| share(m.1)).unwrap_or(0.);
    let mix_rows = v_flex()
        .children(mix.iter().map(|(c, v)| {
            ui::bar_row(c, *v, mmax, format!("{:.1}% · {}", share(*v), fmt::compact(*v)), t.chart_1, cx)
        }))
        .child(div().pt_2().text_xs().text_color(t.muted_foreground)
            .child(fmt::local(&format!("{foreign:.0}% of cash is outside HKD and moves with FX"))));

    // ---- charts ----------------------------------------------------------
    let pts: Vec<Pt> = series.iter().rev().take(36).rev()
        .map(|(m, v)| Pt { label: fmt::month_label(m).into(), v: *v })
        .collect();
    let cash_chart = LineChart::new(pts).appear(false)
        .x(|p: &Pt| p.label.clone())
        .y(|p: &Pt| p.v)
        .stroke(t.primary)
        .natural()
        .y_axis(true)
        .tick_margin(6)
        .y_tick_format(fmt::compact)
        .tooltip_value(|_, v| fmt::hkd(v).into());

    let (green, red) = (fmt::money_in(), fmt::money_out());
    let flow_chart = BarChart::new(last12).appear(false)
        .band(|f: &Flow| f.label.clone())
        .value(|f: &Flow| f.net)
        .value_axis(true)
        .tick_margin(2)
        .value_tick_format(fmt::compact)
        .fill(move |f: &Flow, _, _, _| if f.net >= 0. { green } else { red })
        .tooltip_value(|f, _| format!("net {} · in {} · out {}",
            fmt::signed_hkd(f.net), fmt::compact(f.inflow), fmt::compact(f.outflow)).into());

    ui::Sections::new()
        .gap(16.)
        .child(cards)
        .children(banner)
        .child(
            h_flex()
                .gap_4()
                .items_start()
                .flex_wrap()
                .child(v_flex().flex_1().min_w(crate::layout::col(460.)).gap_2()
                    .child(ui::section("Accounts", &fmt::local("balance now · native · HKD · share of cash"), cx))
                    .child(accounts))
                .child(v_flex().flex_1().min_w(px(300.)).gap_2()
                    .child(ui::section("Currency mix", &fmt::local("share of cash, HKD value"), cx))
                    .child(ui::card(cx).child(mix_rows))
                    .when_some(super::owed::card(b, cx).filter(|_| finance_core::capabilities::Capabilities::of(b).has("receivables")), |d, c| d.child(div().h(px(ui::sp::S))).child(c))),
        )
        .child(
            h_flex()
                .gap_4()
                .items_start()
                .flex_wrap()
                .child(div().flex_1().min_w(crate::layout::col(420.))
                    .child(ui::chart_card("Cash over time", &fmt::local("all bank accounts, month-end, HKD"), 260., cash_chart, cx)))
                .child(div().flex_1().min_w(crate::layout::col(420.))
                    .child(ui::chart_card("Net cashflow", &fmt::local("in − out per completed month, HKD"), 260., flow_chart, cx))),
        )
}
