//! Investments › Buying habits: the core ideas of Nick Maggiulli's book
//! *Just Keep Buying* as facts and simulations on the user's own buys and
//! prices, never advice.
//!
//! The Buying habits tab:
//! 1. Saving vs investing: contributions against the expected monthly gain.
//! 3. Buy the dip vs DCA: the perfect dip buyer on the same money.
//! 4. Cash drag and lump sum: idle cash, and the lump sum on the own buys.
//! 6. Time in the market: days held, and the best few days' share.
//!
//! The Contributions tab shows the other two ([`streaks`], [`growth`]):
//! 2. Just keep buying: streaks, gaps, what the longest BTC pause cost.
//! 5. Money in vs growth: the value split over time.

use std::rc::Rc;

use finance_core::jkb as sim;
use finance_core::portfolio::Account;
use finance_core::Bundle;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{card_title, head, months_from_now, pct_opt, prefs, return_stepper, set_prefs, stepper, tone_opt, words, Model};
use crate::fmt;
use crate::layout;
use crate::linechart::{self, Chart, Dot, Series};
use crate::pages::deep::{ccy, note, units};
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

pub fn render(b: &Bundle, m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let on = super::shown;
    if !["crossover", "dips", "cash", "time"].iter().any(|k| on(k)) {
        return div();
    }
    v_flex().gap(px(sp::GRID))
        .child(v_flex().gap(px(4.)).mt(px(sp::S))
            .child(div().text_size(px(ts::TITLE + 2.)).font_semibold().child("Buying habits"))
            .child(div().text_size(px(ts::BODY)).line_height(px(19.)).text_color(t.muted_foreground).child(
                "Inspired by Nick Maggiulli's book Just Keep Buying: its core ideas, tested on your own buys and prices: saving vs returns, the dip buyer, idle cash, time in the market. Buying steadily and growth over time are under Contributions. Facts and what-ifs, not advice.")))
        .when(on("crossover"), |d| d.child(crossover(m, cx)))
        .when(on("dips"), |d| d.child(dips(m, cx)))
        .when(on("cash"), |d| d.child(cash(b, m, cx)))
        .when(on("time"), |d| d.child(time_in(m, cx)))
}

/// Two proportional bars: a label, a bar, a value.
fn bar(label: &str, v: f64, max: f64, color: Hsla, cx: &App) -> Div {
    let t = cx.theme();
    let frac = if max > 0. { (v / max).clamp(0., 1.) as f32 } else { 0. };
    v_flex().gap(px(4.))
        .child(h_flex().justify_between().text_size(px(ts::BODY))
            .child(div().text_color(t.muted_foreground).child(label.to_string()))
            .child(div().figures().font_weight(FontWeight::MEDIUM).child(format!("{} /mo", fmt::hkd(v)))))
        .child(div().h(px(10.)).w_full().rounded(px(3.)).bg(t.muted)
            .child(div().h_full().rounded(px(3.)).bg(color).w(relative(frac.max(0.004)))))
}

// ------------------------------------------------------------ 1 crossover

fn crossover(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let r = prefs().return_pct;
    let value = m.p.value();
    let monthly = m.pace.invest;
    let c = sim::crossover(value, monthly, r, 1200);
    let line = if c.ratio >= 1. {
        format!("Returns already move the needle more: at {r:.0}% a year the portfolio's expected gain is about {} a month, more than the {} going in.", fmt::hkd(c.gain_now), fmt::hkd(monthly))
    } else {
        format!("Your saving still moves the needle more than returns: at {r:.0}% a year the portfolio's expected gain is about {} a month, {:.0}% of the {} going in. At this pace returns overtake contributions {}{}.",
            fmt::hkd(c.gain_now), c.ratio * 100., fmt::hkd(monthly),
            c.months.map(|n| months_from_now(n, &m.p.today)).unwrap_or("after more than 100 years".into()),
            c.months.map(|_| format!(", with about {} invested", fmt::compact(c.value_then))).unwrap_or_default())
    };
    let max = monthly.max(c.gain_now);
    let card = ui::card(cx).gap(px(sp::M))
        .child(h_flex().justify_between().items_center().flex_wrap().gap_2()
            .child(div().text_size(px(ts::TITLE)).font_semibold().child("This month"))
            .child(return_stepper("jkb-return", cx)))
        .child(bar("Money you put in", monthly, max, t.chart_2, cx))
        .child(bar(&format!("Expected market gain at {r:.0}%"), c.gain_now, max, t.primary, cx))
        .child(note(format!("Money in is the last 6 months' average (the Overview projection's pace). Expected gain = {} invested × {r:.0}% a year, compounded monthly. The book's point: early on, how much you save matters more than what it earns; later the order flips.", fmt::compact(value)), cx));
    v_flex().gap(px(sp::GRID))
        .child(head("Saving vs investing", "when returns overtake what you put in",
            &format!("Contributions are the average of the last 6 months' investing ({}). The expected gain is today's value times the assumed yearly return, as a monthly rate. The crossover runs the same pace forward until the expected monthly gain reaches the monthly contribution.", m.pace_sources()),
            line, cx))
        .child(card)
}

// --------------------------------------------------------------- 2 streaks

pub fn streaks(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let today = &m.p.today[..7];
    let rows: Vec<(String, sim::Streak)> = [Account::FlexInvest, Account::Ibkr, Account::Crypto].iter()
        .filter_map(|a| Some((a.label().to_string(), sim::streak(m.p.buy_months.get(a)?, today))))
        .filter(|r| r.1.active > 0)
        .chain(std::iter::once(("Any account".to_string(), sim::streak(&m.p.buy_months.values().flatten().cloned().collect::<Vec<_>>(), today))))
        .collect();
    let overall = &rows[rows.len() - 1].1;
    let pause_line = m.pause.as_ref().map(|z| {
        let usd = m.p.usd;
        format!(" BTC paused {}–{} ({} months). Buying the median month ({}) through it, at the actual weekly closes, would have put in {} for {} BTC, worth {} today: the pause {} about {}.",
            fmt::month_label(&z.from), fmt::month_label(&z.to), z.months, ccy(z.monthly_usd, "USD"), ccy(z.money_usd, "USD"), units(z.units), ccy(z.value_usd, "USD"),
            if z.cost_usd() >= 0. { "cost" } else { "saved" }, fmt::hkd(z.cost_usd().abs() * usd))
    }).unwrap_or_default();
    let line = format!("{} months in a row with a buy somewhere, the longest run {}.{pause_line}", overall.current, overall.longest);
    let gap = |s: &sim::Streak| s.longest_gap.as_ref().map(|g| format!("{} mo ({}–{})", g.2, fmt::month_label(&g.0), fmt::month_label(&g.1))).unwrap_or("none".into());
    let table = v_flex()
        .child(h_flex().gap(px(10.)).pb(px(6.)).border_b_1().border_color(ui::hairline(cx))
            .child(div().flex_1().min_w(px(90.)).child(ui::eyebrow("Account", cx)))
            .child(div().w(px(70.)).text_right().child(ui::eyebrow("Streak", cx)))
            .child(div().w(px(70.)).text_right().child(ui::eyebrow("Longest", cx)))
            .child(div().w(px(84.)).text_right().child(ui::eyebrow("Months", cx)))
            .child(div().w(px(150.)).text_right().child(ui::eyebrow("Longest gap", cx))))
        .children(rows.iter().enumerate().map(|(i, (name, s))| {
            h_flex().gap(px(10.)).py(px(6.)).text_size(px(ts::BODY)).figures()
                .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
                .when(i == rows.len() - 1, |d| d.font_weight(FontWeight::MEDIUM))
                .child(div().flex_1().min_w(px(90.)).child(name.clone()))
                .child(div().w(px(70.)).text_right().child(format!("{} mo", s.current)))
                .child(div().w(px(70.)).text_right().text_color(t.muted_foreground).child(format!("{} mo", s.longest)))
                .child(div().w(px(84.)).text_right().text_color(t.muted_foreground).child(format!("{} of {}", s.active, s.span)))
                .child(div().w(px(150.)).text_right().text_color(t.muted_foreground).whitespace_nowrap().child(gap(s)))
        }));
    v_flex().gap(px(sp::GRID))
        .child(head("Just keep buying", "months in a row with a buy",
            &format!("A month counts when it has {}. The running month only breaks a streak once it is over. The pause simulation buys the median month of BTC buying at the first weekly close of each missed month and values it at today's price.",
                words(&m.mine(&[(Account::FlexInvest, "a FlexInvest purchase"), (Account::Ibkr, "an IBKR trade"), (Account::Crypto, "a crypto buy")]), "or")),
            line, cx))
        .child(ui::card(cx).child(div().id("jkb-streaks").w_full().overflow_x_scroll().restrict_scroll_to_axis().child(v_flex().min_w(px(470.)).child(table))))
}

// ------------------------------------------------------------------ 3 dips

fn dips(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let cases = &m.dips;
    let ahead = cases.iter().filter(|c| c.r.dca_value() >= c.r.dip_value()).count();
    let btc = cases.iter().find(|c| c.symbol == "BTC");
    let line = if cases.is_empty() {
        "Not enough price history to run the test yet.".to_string()
    } else {
        format!("On your own buys, steady buying ended level with or ahead of a perfect dip buyer in {ahead} of {} cases{}.",
            cases.len(),
            btc.map(|c| format!(". BTC: DCA {} vs {} for the dip buyer, who still holds {} in cash waiting for a dip below the last all-time high",
                ccy(c.r.dca_value(), "USD"), ccy(c.r.dip_value(), "USD"), ccy(c.r.dip_cash, "USD"))).unwrap_or_default())
    };
    let w = [(None, "Asset"), (Some(70.), "Buys"), (Some(110.), "Money in"), (Some(110.), "DCA"), (Some(110.), "Dip buyer"), (Some(100.), "of it cash"), (Some(78.), "DCA ahead")];
    let header = h_flex().gap(px(10.)).pb(px(6.)).border_b_1().border_color(ui::hairline(cx))
        .children(w.iter().map(|(w, l)| match w {
            Some(w) => div().w(px(*w)).flex_shrink_0().text_right().child(ui::eyebrow(*l, cx)),
            None => div().flex_1().min_w(px(90.)).child(ui::eyebrow(*l, cx)),
        }));
    let body = v_flex().children(cases.iter().enumerate().map(|(i, c)| {
        let ahead = c.r.dca_ahead_pct();
        h_flex().gap(px(10.)).py(px(6.)).text_size(px(ts::BODY)).figures()
            .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
            .child(v_flex().flex_1().min_w(px(90.))
                .child(c.symbol.clone())
                .child(ui::caption(format!("{} closes · from {}", c.period, fmt::month_label(&c.first[..7])), cx)))
            .child(div().w(px(70.)).flex_shrink_0().text_right().child(fmt::count(c.buys as f64)))
            .child(div().w(px(110.)).flex_shrink_0().text_right().text_color(t.muted_foreground).child(ccy(c.r.money_in, &c.currency)))
            .child(div().w(px(110.)).flex_shrink_0().text_right().child(ccy(c.r.dca_value(), &c.currency)))
            .child(div().w(px(110.)).flex_shrink_0().text_right().child(ccy(c.r.dip_value(), &c.currency)))
            .child(div().w(px(100.)).flex_shrink_0().text_right().text_color(t.muted_foreground).child(ccy(c.r.dip_cash, &c.currency)))
            .child(div().w(px(78.)).flex_shrink_0().text_right().text_color(fmt::signed_color(ahead)).child(fmt::pct(ahead)))
    }));
    v_flex().gap(px(sp::GRID))
        .child(head("Buy the dip vs DCA", "\"even God can't beat dollar-cost averaging\"",
            "The book's test on your own money: the same amounts arrive on the same dates. DCA buys at once, at that day's (or week's) close. The dip buyer knows the future: they hold the money in cash and buy only at the lowest close between two all-time highs. Money that arrives after the last such dip is still in cash. Both at closes, fees aside. Only holdings with 3+ buys and a year of prices before the first buy.",
            line, cx))
        .child(ui::card(cx).gap(px(sp::S))
            .child(div().id("jkb-dips").w_full().overflow_x_scroll().restrict_scroll_to_axis().child(v_flex().min_w(px(730.)).child(header).child(body)))
            .child(note("A dip buyer can only buy after a new all-time high confirms the bottom. A holding that keeps making new highs (or never got back to its old one) leaves the dip buyer in cash.", cx)))
}

// ------------------------------------------------------------------ 4 cash

fn cash(b: &Bundle, m: &Model, cx: &App) -> Div {
    let p = prefs();
    let rate = b.f64(&["costs", "cash_drag", "pct"]).unwrap_or(0.);
    let banks = b.f64(&["net_worth", "banks_hkd"]).unwrap_or(0.);
    let buffer = m.pace.spend * p.buffer_months;
    let beyond = (banks - buffer).max(0.);
    let ibkr_total = b.f64(&["ibkr", "snapshot", "total_base"]).unwrap_or(0.);
    let ibkr_share = if ibkr_total > 0. { m.broker_cash / ibkr_total * 100. } else { 0. };
    let idle = beyond + m.broker_cash.max(0.);
    let at_bill = idle * rate / 100.;
    let at_return = idle * p.return_pct / 100.;
    let (dca_v, lump_v) = m.broker_lump;
    let broker_vs = (lump_v > 0.).then(|| (dca_v / lump_v - 1.) * 100.);
    // the broker's part only for a user with a broker
    let broker = m.has(Account::Ibkr);
    let line = format!(
        "{} of bank cash sits beyond a {:.0}-month buffer{}. {} {} a year at the {:.2}% T-bill rate, or {} at {:.0}% in the portfolio. Lump sum vs DCA on your own buys: {}.",
        fmt::compact(beyond), p.buffer_months,
        if broker { format!(", and {ibkr_share:.0}% of the IBKR account is uninvested cash") } else { String::new() },
        if broker { "Together that is" } else { "That is" },
        fmt::hkd(at_bill), rate, fmt::hkd(at_return), p.return_pct,
        [broker_vs.map(|v| format!("the broker's DCA is {:.1}% {} investing each holding's money at its first buy", v.abs(), if v >= 0. { "ahead of" } else { "behind" })),
         m.btc_vs_lump.map(|v| format!("the BTC plan is {:.1}% {} a lump sum at its first buy", v.abs(), if v >= 0. { "ahead of" } else { "behind" }))].into_iter().flatten().collect::<Vec<_>>().join("; "));
    let tile = |label: &str, explain: &str, value: String, sub: String| ui::Tile::explained(label.to_string(), explain.to_string(), value).sub(sub).render(cx);
    let buffer_ctl = h_flex().gap(px(6.)).items_center()
        .child(ui::caption("emergency buffer", cx))
        .child(stepper("jkb-buffer", format!("{:.0} mo", p.buffer_months),
            Rc::new(|| set_prefs(|p| p.buffer_months = (p.buffer_months - 1.).max(0.))),
            Rc::new(|| set_prefs(|p| p.buffer_months = (p.buffer_months + 1.).min(36.))), cx));
    v_flex().gap(px(sp::GRID))
        .child(h_flex().justify_between().items_end().flex_wrap().gap_2()
            .child(div().flex_1().min_w(layout::col(300.)).child(head("Cash drag and lump sum", "money waiting to be invested",
                &format!("Bank cash beyond the buffer = the bundle's bank total minus the buffer in months of spending (the last 6 months' average). {}What it would earn: a year at the 3-month US T-bill yield, or at the assumed portfolio return. Lump sum vs DCA: the value today of your actual buys against the same money invested at each holding's first buy (BTC: the current plan). The book cites studies where a lump sum beat averaging in about two thirds of 12-month periods, because markets rise more often than they fall; it is a statement about odds, not every period.",
                    if broker { "The broker's cash is IBKR's uninvested balance. " } else { "" }),
                line, cx)))
            .child(buffer_ctl))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch()
            .child(tile("Beyond the buffer", "Bank cash minus the buffer (months of average spending).", fmt::hkd(beyond),
                format!("{} kept for {:.0} months of {}", fmt::compact(buffer.min(banks)), p.buffer_months, fmt::compact(m.pace.spend))))
            .when(broker, |d| d.child(tile("IBKR cash", "The broker account's uninvested cash, from the latest snapshot.", fmt::hkd(m.broker_cash),
                format!("{ibkr_share:.0}% of the account"))))
            .child(tile("A year at the T-bill rate", "The idle cash above at today's 3-month US T-bill yield.", fmt::hkd(at_bill),
                format!("{rate:.2}% a year")))
            .child(tile("A year in the portfolio", "The idle cash above at the assumed yearly return.", fmt::hkd(at_return),
                format!("{:.0}% a year, assumed", p.return_pct))))
        .child(note("Lump sum vs DCA here is hindsight on one path of prices. Over many periods the book finds lump sum ahead about two thirds of the time; averaging in narrows the range of outcomes, at a cost on average.", cx))
}

// ---------------------------------------------------------------- 5 growth

pub fn growth(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let rows: Vec<_> = m.split.iter().skip_while(|s| s.money_in <= 0.).collect();
    let Some(last) = rows.last() else { return div() };
    let monthly = m.pace.invest;
    let cross = rows.iter().position(|s| s.value - s.money_in >= monthly && monthly > 0.);
    let growth_now = last.value - last.money_in;
    let share = if last.value > 0. { growth_now / last.value * 100. } else { 0. };
    let line = format!("Of the {} invested, {} is money you put in and {} is market growth ({share:.0}%). {}",
        fmt::compact(last.value), fmt::compact(last.money_in), fmt::signed_hkd(growth_now),
        match cross {
            Some(i) => format!("Growth first passed one month of today's contributions ({}) in {}.", fmt::compact(monthly), fmt::month_label(&rows[i].month)),
            None => format!("Growth hasn't yet passed one month of today's contributions ({}).", fmt::compact(monthly)),
        });
    let up = fmt::money_in();
    let ticks: Vec<(usize, SharedString)> = rows.iter().enumerate()
        .filter(|(i, s)| s.month.ends_with("-01") || *i == 0)
        .map(|(i, s)| (i, s.month[..4].to_string().into())).collect();
    let chart = Chart {
        id: "jkb-growth".into(),
        labels: rows.iter().map(|s| fmt::month_label(&s.month).into()).collect(),
        x_ticks: ticks,
        series: vec![
            Series::line("Value", rows.iter().map(|s| Some(s.value)).collect(), up).width(1.8).fill(up.opacity(0.16)),
            Series::line("Money in", rows.iter().map(|s| Some(s.money_in)).collect(), t.muted_foreground).width(1.5).fill(t.muted_foreground.opacity(0.12)),
        ],
        dots: cross.map(|i| vec![Dot { x: i as f32, y: rows[i].value, r: 4., color: t.primary }]).unwrap_or_default(),
        log: false,
        height: 220.,
        y_fmt: Rc::new(fmt::compact),
    };
    v_flex().gap(px(sp::GRID))
        .child(head("Money in vs growth", "the portfolio, split into what you put in and what the market added",
            "Month-ends of every investment (the bundle's rows, with the broker at its statement NAV). The gap between the two areas is market growth. The dot marks the first month that growth reached one month of today's contributions.",
            line, cx))
        .child(ui::card(cx).gap(px(sp::S))
            .child(h_flex().gap(px(sp::M)).flex_wrap()
                .child(ui::legend(up, "Value", fmt::compact(last.value), cx))
                .child(ui::legend(t.muted_foreground, "Money in", fmt::compact(last.money_in), cx))
                .when(cross.is_some(), |d| d.child(ui::legend(t.primary, "growth ≥ a month of contributions", "", cx))))
            .child(linechart::render(chart, cx)))
}

// ---------------------------------------------------------------- 6 time in

fn time_in(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let today = chrono::NaiveDate::parse_from_str(&m.p.today, "%Y-%m-%d").ok();
    let days = |d: &str| today.zip(chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).map(|(a, b)| (a - b).num_days());
    let mut held: Vec<_> = m.p.holdings.iter().filter_map(|h| Some((h, days(h.first_buy.as_deref()?)?))).collect();
    held.sort_by(|a, b| b.1.cmp(&a.1));
    let (bb, wb) = (m.broker_best, m.btc_best);
    let best = |s: sim::BestShare, unit: &str, what: &str| (s.periods > s.n).then(|| format!(
        "{what}: over {} {unit}s the market result was {}; the best {} {unit}s made {}, so without them it would be {}",
        fmt::count(s.periods as f64), fmt::signed_hkd(s.total), s.n, fmt::signed_hkd(s.best), fmt::signed_hkd(s.without())));
    let parts: Vec<String> = [best(wb, "week", "BTC"), best(bb, "trading day", "The broker")].into_iter().flatten().collect();
    let line = if parts.is_empty() { "Not enough history yet.".to_string() } else { format!("{}.", parts.join(". ")) };
    let span = |d: i64| if d >= 730 { format!("{:.1} years", d as f64 / 365.25) } else if d >= 60 { format!("{} months", d / 30) } else { format!("{d} days") };
    let list = ui::card(cx).flex_1().min_w(layout::col(300.)).gap(px(2.))
        .child(card_title("Held for", "since the first buy · return", cx))
        .children(held.iter().enumerate().map(|(i, (h, d))| {
            h_flex().gap(px(8.)).py(px(6.)).text_size(px(ts::BODY)).figures()
                .when(i > 0, |x| x.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap()
                    .child(if matches!(h.account, Some(Account::Ibkr) | Some(Account::Crypto)) { h.symbol.clone() } else { h.name.clone() }))
                .child(div().w(px(90.)).text_right().text_color(t.muted_foreground).child(span(*d)))
                .child(div().w(px(70.)).text_right().text_color(tone_opt(h.gain_pct(), cx)).child(pct_opt(h.gain_pct())))
        }));
    let bars = |s: sim::BestShare, title: &str, unit: &str| {
        let max = s.total.abs().max(s.best.abs()).max(s.without().abs()).max(1.);
        let row = |label: String, v: f64, color: Hsla| v_flex().gap(px(3.))
            .child(h_flex().justify_between().text_size(px(ts::BODY))
                .child(div().text_color(t.muted_foreground).child(label))
                .child(div().figures().text_color(fmt::signed_color(v)).child(fmt::signed_hkd(v))))
            .child(div().h(px(8.)).w_full().rounded(px(3.)).bg(t.muted)
                .child(div().h_full().rounded(px(3.)).bg(color).w(relative((v.abs() / max).clamp(0.004, 1.) as f32))));
        ui::card(cx).flex_1().min_w(layout::col(260.)).gap(px(sp::S))
            .child(card_title(title, format!("{} {unit}s held", fmt::count(s.periods as f64)), cx))
            .child(row("Whole market result".into(), s.total, t.primary))
            .child(row(format!("Best {} {unit}s", s.n), s.best, fmt::money_in()))
            .child(row(format!("Without those {} {unit}s", s.n), s.without(), if s.without() >= 0. { t.muted_foreground } else { fmt::money_out() }))
    };
    v_flex().gap(px(sp::GRID))
        .child(head("Time in the market", "days held, and how much the best few days made",
            &fmt::local("Market result = the units held going into each day (broker, daily closes, HK$ at today's rates) or week (BTC, weekly closes) times that period's price move; money put in never counts. The best periods are the few with the biggest gains. Missing them, by being out of the market on those days, would have changed the result by that much: the book's argument for staying invested."),
            line, cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start()
            .child(list)
            .when(wb.periods > wb.n, |d| d.child(bars(wb, "BTC", "week")))
            .when(bb.periods > bb.n, |d| d.child(bars(bb, "Broker", "day"))))
}
