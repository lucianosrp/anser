//! Investments: money in per account against the plan, the income the
//! investments paid, what investing costs, and the next milestones.

use finance_core::jkb as sim;
use finance_core::portfolio::{Account, MonthIn};
use finance_core::Bundle;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{account_color, card_title, head, months_from_now, prefs, return_stepper, words, Model};
use crate::fmt;
use crate::layout;
use crate::pages::deep::note;
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

/// The accounts that take regular money (the Revolut pot is a one-off).
const PLAN: [Account; 3] = [Account::FlexInvest, Account::Ibkr, Account::Crypto];

/// Stacked monthly bars, one color per account, with the plan as a line.
fn stacked(months: &[&MonthIn], plan: f64, cx: &App) -> Div {
    let t = cx.theme();
    let max = months.iter().map(|m| m.total()).fold(plan, f64::max).max(1.);
    let step = (months.len() / 6).max(1);
    let bars = h_flex().h(px(130.)).items_end().gap(px(3.)).relative()
        .children(months.iter().map(|m| {
            v_flex().flex_1().min_w_0().h_full().justify_end()
                .children(PLAN.iter().rev().map(|a| {
                    let v = m.by[a.index()];
                    div().w_full().h(relative((v / max) as f32)).bg(account_color(*a, cx))
                }))
        }))
        .when(plan > 0., |d| d.child(
            div().absolute().left_0().right_0().bottom(relative((plan / max) as f32)).h(px(1.5)).bg(t.foreground.opacity(0.55))));
    let labels = h_flex().gap(px(3.)).children(months.iter().enumerate().map(|(i, m)| {
        div().flex_1().min_w_0().text_size(px(10.)).text_color(t.muted_foreground).whitespace_nowrap()
            .child(if i % step == 0 { fmt::month_label(&m.month) } else { String::new() })
    }));
    v_flex().gap(px(4.)).child(bars).child(labels)
}

pub fn contributions(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let today = &m.p.today;
    let n = if layout::compact() { 12 } else { 18 };
    let shown: Vec<&MonthIn> = m.p.months_in.iter().rev().take(n).rev().collect();
    let plan = m.plan_total();
    let last_done = m.p.months_in.iter().rev().find(|x| x.month.as_str() < &today[..7]);
    let streaks: Vec<(Account, sim::Streak)> = PLAN.iter()
        .filter_map(|a| Some((*a, sim::streak(m.p.buy_months.get(a)?, &today[..7]))))
        .filter(|(_, s)| s.active > 0).collect();
    let all: Vec<String> = m.p.buy_months.values().flatten().cloned().collect();
    let overall = sim::streak(&all, &today[..7]);
    let from_file = m.plan.iter().any(|p| p.1);
    let line = format!("{}{}",
        last_done.map(|l| format!("{} went in during {} against a plan of about {} a month{}. ",
            fmt::compact(l.total()), fmt::month_label(&l.month), fmt::compact(plan),
            if from_file { "" } else { " (the median of the last 3 months)" })).unwrap_or_default(),
        streaks.iter().map(|(a, s)| if s.missed.is_empty() {
            format!("{} has had a buy every month since {}", a.label(), fmt::month_label(&sim::ym(finance_core::dca::month_index(today) - s.span as i64 + 1)))
        } else {
            format!("{} missed {} of {} months{}", a.label(), s.missed.len(), s.span,
                s.longest_gap.as_ref().map(|g| format!(" (longest gap {} months, {}–{})", g.2, fmt::month_label(&g.0), fmt::month_label(&g.1))).unwrap_or_default())
        }).collect::<Vec<_>>().join("; ") + ".");

    let legend = h_flex().gap(px(sp::M)).flex_wrap().items_center()
        .children(PLAN.iter().filter(|a| m.has(**a)).map(|a| ui::legend(account_color(*a, cx), a.label(), "", cx)))
        .child(h_flex().gap(px(5.)).items_center()
            .child(div().w(px(12.)).h(px(1.5)).bg(t.foreground.opacity(0.55)))
            .child(ui::caption(format!("plan {}", fmt::compact(plan)), cx)));
    let chart = ui::card(cx).flex_1().flex_grow(1.6).min_w(layout::col(340.)).gap(px(sp::S))
        .child(card_title("Money in per month", fmt::local("HK$, by account"), cx))
        .child(legend)
        .child(stacked(&shown, plan, cx));

    let this = m.p.months_in.iter().find(|x| x.month.as_str() == &today[..7]);
    let row = |a: Account| {
        let (amount, file) = m.plan[a.index()];
        let s = streaks.iter().find(|x| x.0 == a).map(|x| &x.1);
        v_flex().gap(px(2.)).py(px(7.)).text_size(px(ts::BODY))
            .child(h_flex().gap(px(8.)).items_center()
                .child(ui::dot(account_color(a, cx), 8.))
                .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(a.label()))
                .child(div().figures().child(format!("{} / mo", fmt::compact(amount))))
                .child(ui::caption(if file { "plan" } else { "recent" }, cx)))
            .child(h_flex().gap_x(px(10.)).gap_y(px(2.)).pl(px(16.)).text_size(px(ts::CAPTION)).text_color(t.muted_foreground).figures().flex_wrap()
                .child(format!("this month {}", fmt::compact(this.map(|x| x.by[a.index()]).unwrap_or(0.))))
                .when_some(s, |d, s| d
                    .child(format!("buys in {} of {} months", s.active, s.span))
                    .child(format!("streak {}", s.current))))
    };
    let side = ui::card(cx).flex_1().min_w(layout::col(240.)).gap(px(2.))
        .child(card_title("Consistency", format!("overall: {} of {} months", overall.active, overall.span), cx))
        .children(PLAN.iter().filter(|a| m.has(**a)).enumerate().map(|(i, a)| row(*a).when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))))
        .child(note(if from_file {
            "The plan is from invest.toml."
        } else {
            fmt::local_static("No plan set: each account's plan is the median of its last 3 complete months. Set one in invest.toml ([plan], one line per account, in HK$ a month).")
        }, cx));
    v_flex().gap(px(sp::GRID))
        .child(head("Contributions", "money in per month, against the plan",
            &format!("Money in = {}. A buy month counts {}. The running month only counts as missed once it is over.",
                words(&m.mine(&[(Account::FlexInvest, "FlexInvest purchases"), (Account::Ibkr, "IBKR deposits (the money sits as cash until it is used to buy)"),
                    (Account::Crypto, "crypto purchases at their USD cost")]), "and"),
                words(&m.mine(&[(Account::FlexInvest, "FlexInvest purchases"), (Account::Ibkr, "IBKR trades"), (Account::Crypto, "crypto buys")]), "and")),
            line, cx))
        // top-aligned: the chart has a fixed height, stretched it left a hole
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start().child(chart).child(side))
}

// ------------------------------------------------------------------ income

pub fn income(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let usd = m.p.usd;
    let total: f64 = m.p.rewards.iter().map(|r| r.usd).sum::<f64>() * usd;
    let ytd: f64 = m.p.rewards.iter().map(|r| r.usd_ytd).sum::<f64>() * usd;
    let count: usize = m.p.rewards.iter().map(|r| r.count).sum();
    let bi = m.p.broker_income;
    let first = m.p.rewards.iter().max_by(|a, b| a.usd.total_cmp(&b.usd));
    // only the accounts the user has: rewards are crypto's, interest and dividends the broker's
    let (crypto, broker) = (m.has(Account::Crypto), m.has(Account::Ibkr));
    let parts: Vec<String> = [
        crypto.then(|| format!("{} in crypto rewards ({} this year){}", fmt::hkd(total), fmt::hkd(ytd),
            first.map(|r| format!(", mostly in {}", r.asset)).unwrap_or_default())),
        broker.then(|| format!("{} of broker interest and {}", fmt::hkd(bi.interest_hkd),
            if bi.dividends_hkd > 0. { format!("{} of dividends", fmt::hkd(bi.dividends_hkd)) } else { "no dividends yet".into() })),
    ].into_iter().flatten().collect();
    let line = if parts.is_empty() {
        "Nothing paid out in cash: a fund's dividends stay inside its price.".to_string()
    } else {
        format!("The investments have paid {}.", parts.join("; "))
    };
    let tiles = h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch()
        .when(crypto, |d| d.child(ui::Tile::explained("Crypto rewards", "Earn and staking rewards on the exchanges, each valued when it arrived where a price exists for that day (BTC's weekly close, stablecoins at US$ 1), otherwise at today's price.",
            fmt::hkd(total)).sub(format!("{} payments · {} this year", fmt::count(count as f64), fmt::hkd(ytd))).render(cx)))
        .when(broker, |d| d.child(ui::Tile::explained("Broker interest", "Interest the broker paid on the account's cash, from the replica's broker cash flows (none in the web app, which has no replica).",
            fmt::hkd(bi.interest_hkd)).sub("credit interest on cash").render(cx))
        .child(ui::Tile::explained("Dividends", "Dividends and the tax withheld on them, from the broker cash flows.",
            fmt::hkd(bi.dividends_hkd - bi.withheld_hkd)).sub(if bi.dividends_hkd > 0. { format!("{} withheld", fmt::hkd(bi.withheld_hkd)) } else { "none recorded".into() }).render(cx)));
    let by_asset = ui::card(cx).flex_1().min_w(layout::col(280.)).gap(px(2.))
        .child(card_title("Rewards by coin", "all time · this year", cx))
        .children(m.p.rewards.iter().filter(|r| r.usd * usd >= 0.5).map(|r| {
            h_flex().gap(px(8.)).py(px(5.)).items_center().text_size(px(ts::BODY)).figures()
                .when_some(crate::logo::tile(crate::logo::for_symbol(&r.asset), 18., cx), |d, l| d.child(l))
                .child(div().flex_1().child(r.asset.clone()))
                .child(div().text_color(t.muted_foreground).child(fmt::count(r.count as f64)))
                .child(div().w(px(76.)).text_right().child(fmt::hkd(r.usd * usd)))
                .child(div().w(px(76.)).text_right().text_color(t.muted_foreground).child(fmt::hkd(r.usd_ytd * usd)))
        }));
    let why = ui::card(cx).flex_1().min_w(layout::col(280.)).gap(px(6.))
        .child(card_title("Why no dividends", "", cx))
        .child(note("The broker data has no dividend rows yet: the Flex query may send deposits only. Distributing funds and single stocks pay out, so their dividends will show here once the query includes Dividends and Withholding Tax. An accumulating fund keeps its dividends inside the fund (they show up in its price), and so does a fund valued at its NAV.", cx));
    v_flex().gap(px(sp::GRID))
        .child(head("Income", "interest, rewards and dividends", "Cash or coins the investments paid out, as opposed to price gains.", line, cx))
        .when(crypto || broker, |d| d.child(tiles))
        .when(crypto || broker, |d| d.child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start()
            .when(crypto, |d| d.child(by_asset))
            .when(broker, |d| d.child(why))))
}

// ------------------------------------------------------------------- costs

pub fn costs(b: &Bundle, m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let cost = |p: &[&str]| b.f64(&[&["costs"], p].concat());
    let running = cost(&["running", "total_yr_hkd"]).unwrap_or(0.);
    let commissions = m.fees_yr.actual_yr_hkd;
    let fees_yr = running + commissions;
    let value = m.p.value();
    let rate = cost(&["cash_drag", "pct"]).unwrap_or(0.);
    let bank_drag = cost(&["cash_drag", "yr_hkd"]).unwrap_or(0.);
    let broker_drag = m.broker_cash.max(0.) * rate / 100.;
    let paid = cost(&["paid", "total_hkd"]).unwrap_or(0.);
    let line = format!(
        "Investing costs about {} a year in fees ({:.2}% of what's invested): {} in fund and platform fees and {} in commissions at the last 3 months' pattern. Cash not invested forgoes about {} a year at the {:.2}% T-bill rate{}.",
        fmt::hkd(fees_yr), if value > 0. { fees_yr / value * 100. } else { 0. }, fmt::hkd(running), fmt::hkd(commissions),
        fmt::hkd(bank_drag + broker_drag), rate,
        if fees_yr > 0. { format!(", about {:.0} times the fees", (bank_drag + broker_drag) / fees_yr) } else { String::new() });
    let row = |label: &str, sub: String, v: f64, strong: bool, color: Option<Hsla>| {
        h_flex().gap(px(8.)).py(px(7.)).items_center().text_size(px(ts::BODY))
            .child(v_flex().flex_1().min_w_0()
                .child(div().when(strong, |d| d.font_weight(FontWeight::MEDIUM)).child(label.to_string()))
                .when(!sub.is_empty(), |d| d.child(ui::caption(sub, cx))))
            .child(div().figures().whitespace_nowrap().when(strong, |d| d.font_weight(FontWeight::SEMIBOLD))
                .when_some(color, |d, c| d.text_color(c)).child(format!("{} /yr", fmt::hkd(v))))
    };
    let hr = || div().h(px(1.)).bg(ui::hairline(cx));
    // rows for platforms the server has (finance_core::capabilities)
    let caps = finance_core::capabilities::Capabilities::of(b);
    let (has_fund, has_broker) = (caps.has("fund_proxy"), caps.has("broker"));
    let fees_sub = if has_fund {
        format!("TER on each fund at today's value, FlexInvest platform ({:.2}%)", cost(&["running", "pct"]).unwrap_or(0.))
    } else {
        "TER on each fund at today's value".to_string()
    };
    let summary = ui::card(cx).flex_1().min_w(layout::col(320.)).gap(px(0.))
        .child(card_title("What investing costs you", format!("{} paid so far", fmt::hkd(paid)), cx))
        .child(row("Fund and platform fees", fees_sub, running, false, None))
        .when(has_broker, |d| d.child(hr()).child(row("Commissions", format!("{} buys in the last {} days, scaled to a year", m.fees_yr.trades_in_window, m.fees_yr.window_days), commissions, false, None)))
        .child(hr())
        .child(row("Fees, total", "what leaves your money".into(), fees_yr, true, Some(fmt::money_out())))
        .child(div().h(px(sp::M)))
        .child(row("Bank cash at the T-bill rate", format!("{} in the banks at {rate:.2}%", fmt::compact(cost(&["cash_drag", "cash_hkd"]).unwrap_or(0.))), bank_drag, false, None))
        .when(has_broker, |d| d.child(hr())
            .child(row("IBKR cash at the T-bill rate", format!("{} waiting in the broker account", fmt::compact(m.broker_cash)), broker_drag, false, None)))
        .child(hr())
        .child(row("Cash drag, total", "an opportunity cost, not a fee".into(), bank_drag + broker_drag, true, Some(t.muted_foreground)));

    let holdings = b.arr(&["costs", "running", "holdings"]);
    let per = ui::card(cx).flex_1().min_w(layout::col(280.)).gap(px(2.))
        .child(card_title("Fees per holding", "TER · per year", cx))
        .children(holdings.iter().enumerate().filter_map(|(i, h)| {
            let name = h.get("holding")?.as_str()?;
            let ter = h.get("ter_pct").and_then(|x| x.as_f64());
            let fee = h.get("fee_yr_hkd").and_then(|x| x.as_f64());
            Some(h_flex().gap(px(8.)).py(px(6.)).items_center().text_size(px(ts::BODY)).figures()
                .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
                .when_some(crate::logo::tile(
                    if name.starts_with("HSBC") { crate::logo::for_source("hsbc") }
                    else { crate::logo::for_symbol(name.split_whitespace().last().unwrap_or("")) }, 18., cx), |d, l| d.child(l))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name.to_string()))
                .child(div().w(px(52.)).text_right().text_color(t.muted_foreground).child(ter.map(|x| format!("{x:.2}%")).unwrap_or("unknown".into())))
                .child(div().w(px(64.)).text_right().child(fee.map(fmt::int).unwrap_or("—".into()))))
        }));
    v_flex().gap(px(sp::GRID))
        .child(head("Costs", "fees, and what idle cash forgoes",
            &format!("Fees: {}. Cash drag: what bank cash{} would earn at today's 3-month US T-bill yield. It is an opportunity cost, not a fee.",
                words(&[Some("each fund's yearly charge (TER/OCF) at today's value"), has_fund.then_some("the FlexInvest platform fee"),
                    has_broker.then_some("IBKR commissions at the last 91 days' buying, scaled to a year (Broker › Deep dive › Fees has the detail)")]
                    .into_iter().flatten().collect::<Vec<_>>(), "and"),
                if has_broker { " and the broker's uninvested cash" } else { "" }),
            line, cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start().child(summary).child(per))
}

// -------------------------------------------------------------- milestones

/// Round numbers worth a line.
const MARKS: [f64; 13] = [50e3, 100e3, 250e3, 500e3, 750e3, 1e6, 1.5e6, 2e6, 3e6, 4e6, 5e6, 7.5e6, 10e6];
const HORIZON: usize = 600;

/// (target, months at the rate, months with no growth)
pub fn next_marks(value: f64, monthly: f64, annual_pct: f64, k: usize) -> Vec<(f64, Option<usize>, Option<usize>)> {
    let grown = sim::grow(value, monthly, annual_pct, HORIZON);
    let flat = sim::grow(value, monthly, 0., HORIZON);
    MARKS.iter().filter(|m| **m > value).take(k).map(|target| {
        (*target, grown.iter().position(|v| v >= target), flat.iter().position(|v| v >= target))
    }).collect()
}

pub fn milestones(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let r = prefs().return_pct;
    let value = m.p.value();
    let monthly = m.pace.invest;
    let marks = next_marks(value, monthly, r, 3);
    let line = match marks.first() {
        Some((target, Some(g), flat)) => format!("At {} a month and {r:.0}% a year, the investments pass {} {}{}.",
            fmt::hkd(monthly), fmt::hkd(*target), months_from_now(*g, &m.p.today),
            flat.filter(|f| f > g).map(|f| format!(", {} months sooner than with no growth", f - g)).unwrap_or_default()),
        _ => "No milestone within 50 years at the current pace.".into(),
    };
    let tiles = h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch().children(marks.iter().map(|(target, g, flat)| {
        ui::card(cx).flex_1().min_w(layout::col(180.)).gap(px(4.))
            .child(ui::eyebrow(fmt::hkd(*target), cx))
            .child(div().text_size(px(17.)).font_semibold().whitespace_nowrap().child(
                g.map(|g| months_from_now(g, &m.p.today).trim_start_matches("in ").to_string()).unwrap_or("50+ years".into())))
            .child(ui::caption(match (g, flat) {
                (Some(g), Some(f)) => format!("{} with no growth", if *f == *g { "the same".to_string() } else { months_from_now(*f, &m.p.today).trim_start_matches("in ").to_string() }),
                _ => "not reached without growth".into(),
            }, cx))
    }));
    v_flex().gap(px(sp::GRID))
        .child(h_flex().justify_between().items_end().flex_wrap().gap_2()
            .child(div().flex_1().min_w(layout::col(300.)).child(head("Milestones", "the next round numbers at the current pace",
                &format!("The Overview projection's model on the investments alone: today's value grows at the assumed yearly rate, compounded monthly, and each month the average of the last 6 months' investing goes in ({}). The no-growth date is the same money with 0% return.", m.pace_sources()),
                line, cx)))
            .child(return_stepper("ms-return", cx)))
        .child(tiles)
        .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(
            format!("Pace: {} a month, the last 6 months' average. Value now: {}.", fmt::hkd(monthly), fmt::hkd(value))))
}

#[cfg(test)]
mod tests {
    use super::next_marks;

    #[test]
    fn marks_and_months() {
        let v = next_marks(222_000., 22_000., 6., 3);
        assert_eq!(v.iter().map(|x| x.0).collect::<Vec<_>>(), vec![250e3, 500e3, 750e3]);
        // 28k to go at 22k a month: 2 months either way
        assert_eq!((v[0].1, v[0].2), (Some(2), Some(2)));
        // growth gets there sooner further out
        assert!(v[2].1.unwrap() < v[2].2.unwrap());
        assert!(next_marks(1e9, 0., 6., 3).is_empty());
    }
}
