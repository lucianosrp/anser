//! Year in review: one calendar year told as a story, with big numbers and
//! small charts (finance_core::review). Opens on the last complete year; the
//! switcher (or ← →) picks another, and the running year reads "to date".
//!
//! Its own view, so the review is computed once per year and data load
//! rather than on every frame.

use std::rc::Rc;
use std::sync::Arc;

use finance_core::review::{self, Review};
use finance_core::travel::Trip;
use finance_core::{Bundle, Ledger, drill};
use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, h_flex, tooltip::Tooltip, v_flex};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::category;
use crate::fmt;
use crate::pages::spending::{OnPick, OnSearch};
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

struct Data {
    ledger: Arc<Ledger>,
    bundle: Arc<Bundle>,
    trips: Arc<Vec<Trip>>,
}

pub struct ReviewView {
    data: Option<Data>,
    /// None = the default (the last complete year)
    year: Option<i32>,
    cache: Option<(i32, Rc<Review>)>,
    on_pick: OnPick,
    on_search: OnSearch,
}

impl ReviewView {
    pub fn new(on_pick: OnPick, on_search: OnSearch, _cx: &mut Context<Self>) -> Self {
        ReviewView {
            data: None,
            year: std::env::var("FINANCE_DESKTOP_YEAR").ok().and_then(|y| y.parse().ok()), // dev/screenshot aid
            cache: None,
            on_pick,
            on_search,
        }
    }

    pub fn set_data(&mut self, ledger: Arc<Ledger>, bundle: Arc<Bundle>, trips: Arc<Vec<Trip>>, cx: &mut Context<Self>) {
        self.data = Some(Data { ledger, bundle, trips });
        self.cache = None;
        cx.notify();
    }

    fn years(&self) -> Vec<i32> {
        self.data.as_ref().map(|d| review::years(&d.bundle)).unwrap_or_default()
    }

    fn current_year(&self) -> Option<i32> {
        let d = self.data.as_ref()?;
        self.year.filter(|y| self.years().contains(y)).or_else(|| review::default_year(&d.bundle, crate::budgets::today()))
    }

    /// One year older (-1) or newer (+1), within the years there are.
    pub fn shift_year(&mut self, step: i32, cx: &mut Context<Self>) {
        let ys = self.years();
        if let Some(y) = self.current_year() {
            let next = y + step;
            if ys.contains(&next) {
                self.year = Some(next);
                cx.notify();
            }
        }
    }

    fn review(&mut self, year: i32) -> Option<Rc<Review>> {
        if let Some((y, r)) = &self.cache {
            if *y == year {
                return Some(r.clone());
            }
        }
        let d = self.data.as_ref()?;
        let r = Rc::new(review::build(&d.bundle, &d.ledger, &d.trips, year, crate::budgets::today()));
        self.cache = Some((year, r.clone()));
        Some(r)
    }
}

fn pct_change(now: f64, before: f64) -> Option<f64> {
    (before.abs() > 1.).then(|| (now - before) / before.abs() * 100.)
}

/// A change's size: "12%", or "×18.5" once it has more than tripled (a
/// percentage in the thousands reads as noise).
fn change_txt(p: f64) -> String {
    if p >= 200. { format!("×{:.1}", 1. + p / 100.) } else { format!("{:.0}%", p.abs()) }
}

/// "+12% vs 2024" in the right color: spending up is red, income up is green.
fn change_chip(now: f64, before: Option<f64>, up_is_good: bool, year: i32, cx: &App) -> Option<Div> {
    let p = pct_change(now, before?)?;
    let good = (p >= 0.) == up_is_good;
    let color = if p.abs() < 0.5 { cx.theme().muted_foreground } else if good { fmt::money_in() } else { fmt::money_out() };
    Some(ui::chip(format!("{}{} vs {}", if p >= 0. { "▲ " } else { "▼ " }, change_txt(p), year - 1), color, cx))
}

/// A big number with a label above and a line below.
fn big(label: &str, value: String, sub: String, tone: Option<Hsla>, cx: &App) -> Div {
    v_flex()
        .flex_1()
        .min_w(px(170.))
        .gap(px(4.))
        .child(ui::eyebrow(label, cx))
        .child(div().text_size(px(30.)).line_height(px(36.)).font_semibold().figures().whitespace_nowrap()
            .when_some(tone, |d, c| d.text_color(c)).child(value))
        .child(ui::caption(sub, cx))
}

/// A fun-fact card: icon, a big figure, what it means.
fn fact(icon: Lucide, color: Hsla, value: String, what: String, cx: &App) -> Div {
    ui::card(cx)
        .flex_1()
        .min_w(px(200.))
        .gap(px(sp::S))
        .child(div().size(px(30.)).rounded(px(9.)).bg(color.opacity(0.14)).flex().items_center().justify_center()
            .child(Icon::new(icon).size(px(16.)).text_color(color)))
        .child(div().text_size(px(ts::FIGURE)).line_height(px(28.)).font_semibold().figures().whitespace_nowrap()
            .overflow_hidden().text_ellipsis().child(value))
        .child(div().text_size(px(ts::CAPTION)).text_color(cx.theme().muted_foreground).child(what))
}

fn day(d: &str) -> String {
    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|d| d.format("%-d %b").to_string()).unwrap_or_else(|_| d.to_string())
}

fn month_name(ym: &str) -> String {
    chrono::NaiveDate::parse_from_str(&format!("{ym}-01"), "%Y-%m-%d").map(|d| d.format("%B").to_string()).unwrap_or_else(|_| ym.to_string())
}

impl Render for ReviewView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(year) = self.current_year() else {
            return v_flex().child(ui::empty("No spending data yet — sync first", cx));
        };
        let Some(r) = self.review(year) else { return v_flex() };
        let Some(d) = self.data.as_ref() else { return v_flex() };
        let (ledger, trips) = (d.ledger.clone(), d.trips.clone());
        let t = cx.theme().clone();
        let this_year = crate::budgets::today().format("%Y").to_string().parse::<i32>().unwrap_or(year);

        // ---- year switcher ----
        let seg = |y: i32, cx: &mut Context<Self>| {
            let on = y == year;
            let label = if y == this_year { format!("{y} to date") } else { y.to_string() };
            div().id(("year", y as usize)).px(px(12.)).py(px(4.)).rounded(px(6.)).text_sm().cursor_pointer().whitespace_nowrap()
                .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
                .on_click(cx.listener(move |this, _, _, cx| { this.year = Some(y); cx.notify(); }))
                .child(label)
        };
        let mut years = self.years();
        years.reverse();
        let switcher = h_flex().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx)).flex_wrap()
            .children(years.into_iter().map(|y| seg(y, cx)));
        let span = if r.ytd { format!("1 Jan – {} {year}, to date", day(&r.through)) } else { format!("January – December {year}") };
        let head = h_flex().gap_3().flex_wrap().items_center().justify_between()
            .child(switcher)
            .child(ui::caption(format!("{span} · ← → to change year"), cx));

        // ---- the hero: earned, spent, kept ----
        let rate = r.savings_rate().unwrap_or(0.);
        let sentence = format!(
            "{} came in and {} went out{}. You kept {:.0}% of it{}.",
            fmt::hkd(r.income), fmt::hkd(r.spending),
            if r.ytd { " so far" } else { "" },
            rate.max(0.),
            if r.invested() > 0. { format!(", and put {} to work in funds, the broker and crypto", fmt::hkd(r.invested())) } else { String::new() },
        );
        let max_m = r.months.iter().map(|m| m.income.max(m.spend)).fold(1., f64::max);
        const MH: f32 = 90.;
        let month_bars = h_flex().h(px(MH + 16.)).items_end().gap(px(6.)).children(r.months.iter().enumerate().map(|(i, m)| {
            let tip = format!("{} · in {} · out {}", fmt::month_label(&m.month), fmt::hkd(m.income), fmt::hkd(m.spend));
            v_flex().id(("rm", i)).flex_1().min_w_0().items_center().gap(px(3.))
                .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                .child(h_flex().h(px(MH)).w_full().max_w(px(26.)).items_end().gap(px(2.))
                    .child(div().flex_1().rounded_t(px(2.)).h(px((m.income / max_m) as f32 * MH)).bg(fmt::money_in().opacity(0.8)))
                    .child(div().flex_1().rounded_t(px(2.)).h(px((m.spend / max_m) as f32 * MH)).bg(fmt::money_out().opacity(0.8))))
                .child(div().text_size(px(10.)).text_color(t.muted_foreground).child(fmt::month_label(&m.month).chars().next().unwrap_or(' ').to_string()))
        }));
        let hero = ui::card(cx).p(px(20.)).gap(px(sp::L))
            .child(h_flex().gap_2().items_center()
                .child(Icon::new(Lucide::CalendarRange).size(px(16.)).text_color(t.primary))
                .child(ui::eyebrow(format!("{year} in review"), cx)))
            .child(div().text_size(px(18.)).line_height(px(26.)).max_w(px(760.)).child(sentence))
            .child(h_flex().gap(px(sp::XL)).flex_wrap().items_start()
                .child(big("Earned", fmt::hkd(r.income), String::new(), Some(fmt::money_in()), cx)
                    .children(change_chip(r.income, r.prev_income, true, year, cx).map(|c| h_flex().child(c))))
                .child(big("Spent", fmt::hkd(r.spending), format!("{} a month on average", fmt::hkd(r.avg_month_spend())), Some(fmt::money_out()), cx)
                    .children(change_chip(r.spending, r.prev_spending, false, year, cx).map(|c| h_flex().child(c))))
                .child(big("Saved", fmt::signed_hkd(r.saved()), format!("{rate:.0}% savings rate"), Some(fmt::signed_color(r.saved())), cx))
                .child(v_flex().flex_1().min_w(px(240.)).gap_1()
                    .child(h_flex().gap_3().child(ui::legend(fmt::money_in(), "in", "", cx)).child(ui::legend(fmt::money_out(), "out", "", cx)))
                    .child(month_bars)));

        // ---- net worth ----
        let nw = r.net_worth_change().map(|(a, b)| {
            let vals: Vec<f64> = r.net_worth.iter().map(|x| x.1).collect();
            let delta = b - a;
            ui::card(cx).gap(px(sp::M))
                .child(h_flex().justify_between().child(ui::eyebrow("Net worth", cx))
                    .children(pct_change(b, a).map(|p| ui::delta_over(p, if r.ytd { "so far" } else { "in the year" }, cx))))
                .child(h_flex().gap_3().items_baseline().flex_wrap()
                    .child(div().text_size(px(ts::FIGURE)).text_color(t.muted_foreground).figures().child(fmt::hkd(a)))
                    .child(div().text_size(px(ts::FIGURE)).text_color(t.muted_foreground).child("→"))
                    .child(div().text_size(px(30.)).line_height(px(36.)).font_semibold().figures().child(fmt::hkd(b))))
                .child(ui::caption(format!("{} {} · {} → {}", fmt::signed_hkd(delta), if delta >= 0. { "added" } else { "lost" },
                    r.net_worth.first().map(|x| fmt::month_label(&x.0)).unwrap_or_default(),
                    r.net_worth.last().map(|x| fmt::month_label(&x.0)).unwrap_or_default()), cx))
                .child(div().h(px(54.)).child(ui::sparkline("rv-nw", &vals, t.primary)))
        });

        // ---- categories ----
        let cmax = r.categories.first().map(|c| c.hkd).unwrap_or(1.);
        let cats = v_flex().gap(px(2.)).children(r.categories.iter().enumerate().map(|(i, c)| {
            let q = format!("cat:{} {year} out", c.category.split_whitespace().next().unwrap_or("").to_lowercase());
            let on_search = self.on_search.clone();
            let share = if r.spending > 0. { c.hkd / r.spending * 100. } else { 0. };
            h_flex().id(("rc", i)).gap_3().py(px(5.)).px_1().rounded(t.radius).cursor_pointer().hover(|s| s.bg(t.accent))
                .on_click(move |_, w, cx| on_search(q.clone(), w, cx))
                .child(category::tile(&c.category, 24., t.chart_1))
                .child(v_flex().flex_1().min_w_0().gap(px(4.))
                    .child(h_flex().justify_between().gap_2()
                        .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(c.category.clone()))
                        .child(h_flex().gap_2().flex_shrink_0()
                            .children(c.prev.and_then(|p| {
                                if p < 1. { return Some(ui::tag("new", cx)); }
                                change_chip(c.hkd, Some(p), false, year, cx).map(|_| {
                                    let pc = (c.hkd - p) / p * 100.;
                                    ui::chip(match pc { x if x >= 200. => change_txt(x), x if x >= 0. => format!("+{}", change_txt(x)), x => format!("−{}", change_txt(x)) }, if pc > 0. { fmt::money_out() } else { fmt::money_in() }, cx)
                                })
                            }))
                            .child(div().text_sm().figures().whitespace_nowrap().child(fmt::hkd(c.hkd)))))
                    .child(h_flex().gap_2().items_center()
                        .child(div().flex_1().h(px(5.)).rounded_full().bg(t.muted)
                            .child(div().h_full().rounded_full().bg(t.chart_1).w(relative((c.hkd / cmax).clamp(0., 1.) as f32))))
                        .child(div().w(px(34.)).text_right().text_xs().text_color(t.muted_foreground).figures().child(format!("{share:.0}%")))))
        }));
        let top_cat = r.categories.first().map(|c| format!("{} led the way", c.category)).unwrap_or_default();

        // ---- merchants ----
        let merchants = v_flex().children(r.merchants.iter().enumerate().map(|(i, m)| {
            let q = m.key.clone();
            let on_search = self.on_search.clone();
            h_flex().id(("rmc", i)).gap_3().py(px(6.)).px_1().rounded(t.radius).cursor_pointer().hover(|s| s.bg(t.accent))
                .on_click(move |_, w, cx| on_search(q.clone(), w, cx))
                .child(div().w(px(16.)).text_xs().text_color(t.muted_foreground).figures().child(format!("{}", i + 1)))
                .child(crate::logo::tile(crate::logo::for_text(&m.name), 24., cx)
                    .unwrap_or_else(|| div().size(px(24.)).into_any_element()))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(m.name.clone()))
                    .child(ui::caption(format!("{} visit{}", m.count, if m.count == 1 { "" } else { "s" }), cx)))
                .child(div().text_sm().figures().whitespace_nowrap().child(fmt::hkd(m.hkd)))
        }));

        // ---- months: the priciest and the cheapest ----
        let (hi, lo) = (r.priciest_month().cloned(), r.cheapest_month().cloned());
        let smax = r.months.iter().map(|m| m.spend).fold(1., f64::max);
        const SH: f32 = 120.;
        let spend_bars = h_flex().h(px(SH)).items_end().gap(px(6.)).children(r.months.iter().enumerate().map(|(i, m)| {
            let c = if hi.as_ref().is_some_and(|h| h.month == m.month) { fmt::money_out() }
                else if lo.as_ref().is_some_and(|l| l.month == m.month) { fmt::money_in() }
                else { t.primary.opacity(0.35) };
            let tip = format!("{} · {}", fmt::month_label(&m.month), fmt::hkd(m.spend));
            div().id(("rs", i)).flex_1().min_w_0().h_full().flex().items_end()
                .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                .child(div().w_full().rounded_t(px(3.)).h(px((m.spend / smax) as f32 * SH)).bg(c))
        }));
        let spend_labels = h_flex().gap(px(6.)).children(r.months.iter().map(|m| {
            div().flex_1().min_w_0().text_center().text_xs().text_color(t.muted_foreground).whitespace_nowrap().overflow_hidden()
                .child(fmt::month_label(&m.month).get(..3).unwrap_or("").to_string())
        }));
        let month_card = ui::card(cx).gap_2()
            .child(h_flex().gap(px(sp::XL)).flex_wrap()
                .children(hi.as_ref().map(|h| v_flex()
                    .child(ui::eyebrow("Most expensive", cx))
                    .child(div().text_size(px(ts::FIGURE)).font_semibold().text_color(fmt::money_out()).child(month_name(&h.month)))
                    .child(ui::caption(fmt::hkd(h.spend), cx))))
                .children(lo.as_ref().map(|l| v_flex()
                    .child(ui::eyebrow("Cheapest", cx))
                    .child(div().text_size(px(ts::FIGURE)).font_semibold().text_color(fmt::money_in()).child(month_name(&l.month)))
                    .child(ui::caption(fmt::hkd(l.spend), cx)))))
            .child(div().mt_2().child(spend_bars))
            .child(spend_labels);

        // ---- the biggest single purchases ----
        let biggest = v_flex().children(r.biggest.iter().filter_map(|&i| {
            let e = ledger.entries.get(i)?;
            let v = -drill::my_hkd(e).unwrap_or(e.amount);
            let on_pick = self.on_pick.clone();
            Some(h_flex().id(("rb", i)).gap_3().py(px(6.)).px_1().rounded(t.radius).cursor_pointer().hover(|s| s.bg(t.accent))
                .on_click(move |_, w, cx| on_pick(i, w, cx))
                .child(div().w(px(46.)).text_xs().text_color(t.muted_foreground).child(day(&e.date)))
                .child(category::tile(&e.category, 24., t.chart_3))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.description.clone()))
                    .child(ui::caption(e.category.clone(), cx)))
                .child(div().text_sm().figures().whitespace_nowrap().text_color(fmt::money_out()).child(fmt::hkd(v))))
        }));

        // ---- travel, investing, BTC ----
        let places: Vec<String> = r.trips.iter().filter_map(|&i| trips.get(i)).map(|t| t.label()).collect();
        let travel = ui::card(cx).flex_1().min_w(px(260.)).gap(px(sp::S))
            .child(h_flex().gap_2().child(Icon::new(Lucide::Plane).size(px(15.)).text_color(t.chart_2)).child(ui::eyebrow("Travel", cx)))
            .child(div().text_size(px(30.)).line_height(px(36.)).font_semibold().figures()
                .child(format!("{} trip{}", r.trips.len(), if r.trips.len() == 1 { "" } else { "s" })))
            .child(ui::caption(format!("{} days away · {} spent travelling", r.trip_days, fmt::hkd(r.travel_hkd)), cx))
            .child(h_flex().gap_1().flex_wrap().children(places.iter().take(8).map(|p| ui::tag(p.clone(), cx))));
        let inv_parts = [("Funds", r.funds_hkd, t.chart_2), ("Broker", r.broker_hkd, t.chart_4), ("Crypto", r.crypto_hkd, t.chart_5)];
        let inv_total = r.invested().max(1.);
        let investing = ui::card(cx).flex_1().min_w(px(260.)).gap(px(sp::S))
            .child(h_flex().gap_2().child(Icon::new(Lucide::TrendingUp).size(px(15.)).text_color(t.chart_4)).child(ui::eyebrow("Invested", cx)))
            .child(div().text_size(px(30.)).line_height(px(36.)).font_semibold().figures().child(fmt::hkd(r.invested())))
            .child(h_flex().w_full().h(px(6.)).gap(px(2.)).rounded_full().overflow_hidden().bg(t.muted)
                .children(inv_parts.iter().filter(|p| p.1 > 0.5).map(|(_, v, c)| div().h_full().flex_basis(px(0.)).flex_grow((v / inv_total) as f32).bg(*c))))
            .child(h_flex().gap_3().flex_wrap().children(inv_parts.iter().map(|(l, v, c)| ui::legend(*c, *l, fmt::compact(v.max(0.)), cx))));
        let btc = &r.btc;
        let btc_card = ui::card(cx).flex_1().min_w(px(260.)).gap(px(sp::S))
            .child(h_flex().gap_2().child(Icon::new(Lucide::Bitcoin).size(px(15.)).text_color(t.chart_5)).child(ui::eyebrow("Bitcoin bought", cx)))
            .child(div().text_size(px(30.)).line_height(px(36.)).font_semibold().figures().child(format!("₿ {:.5}", btc.btc)))
            .child(ui::caption(if btc.buys > 0 {
                format!("{} buys · US$ {} · average US$ {}", btc.buys, fmt::int(btc.usd), fmt::int(btc.usd / btc.btc.max(1e-12)))
            } else { "no BTC buys this year".into() }, cx));

        // ---- fun facts ----
        let mut facts: Vec<Div> = Vec::new();
        if r.coffee_count > 0 {
            facts.push(fact(Lucide::Coffee, t.chart_3, format!("{} coffees", r.coffee_count),
                format!("{} on coffee, {} a cup", fmt::hkd(r.coffee_hkd), fmt::hkd(r.coffee_hkd / r.coffee_count as f64)), cx));
        }
        facts.push(fact(Lucide::Receipt, t.chart_1, fmt::int(r.txn_count as f64),
            format!("bank and card transactions, {} of them spending", fmt::int(r.spending_count as f64)), cx));
        if let Some((name, _, n)) = &r.most_visited {
            facts.push(fact(Lucide::Repeat, t.chart_2, name.clone(), format!("most visited: {n} times, about every {} days",
                (if r.ytd { chrono::NaiveDate::parse_from_str(&r.through, "%Y-%m-%d").map(|d| chrono::Datelike::ordinal(&d)).unwrap_or(365) } else { 365 } as usize / (*n).max(1)).max(1)), cx));
        }
        if let Some(s) = &r.no_spend {
            facts.push(fact(Lucide::PiggyBank, fmt::money_in(), format!("{} days", s.days),
                format!("longest stretch without spending: {} → {}", day(&s.start), day(&s.end)), cx));
        }

        v_flex()
            .gap_4()
            .child(head)
            .child(hero)
            .children(nw)
            .child(
                h_flex().gap_4().items_start().flex_wrap()
                    .child(v_flex().flex_1().min_w(crate::layout::col(340.)).gap_2()
                        .child(ui::section("Where it went", &top_cat, cx))
                        .child(ui::card(cx).child(cats)))
                    .child(v_flex().flex_1().min_w(px(300.)).gap_2()
                        .child(ui::section("Favourite places", "by money spent", cx))
                        .child(ui::card(cx).child(merchants))),
            )
            .child(
                h_flex().gap_4().items_start().flex_wrap()
                    .child(v_flex().flex_1().min_w(crate::layout::col(340.)).gap_2()
                        .child(ui::section("Month by month", "spending; the priciest in red, the cheapest in green", cx))
                        .child(month_card))
                    .child(v_flex().flex_1().min_w(px(300.)).gap_2()
                        .child(ui::section("Biggest purchases", "your share · click for details", cx))
                        .child(ui::card(cx).child(biggest))),
            )
            .child(ui::section("Away and invested", "", cx))
            .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch().child(travel).child(investing).child(btc_card))
            .child(ui::section("Fun facts", "", cx))
            .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch().children(facts))
    }
}

/// Overview's way in: a card that opens the year in review.
pub fn overview_card(b: &Bundle, cx: &App) -> Option<Stateful<Div>> {
    let t = cx.theme();
    let year = review::default_year(b, crate::budgets::today())?;
    Some(ui::card_interactive("ov-review", cx)
        .on_click(|_, w, cx| w.dispatch_action(Box::new(crate::app::GoReview), cx))
        .child(h_flex().gap_3().items_center()
            .child(div().size(px(34.)).rounded(px(10.)).bg(t.primary.opacity(0.14)).flex().items_center().justify_center()
                .child(Icon::new(Lucide::CalendarRange).size(px(17.)).text_color(t.primary)))
            .child(v_flex().flex_1().min_w_0()
                .child(div().text_size(px(ts::TITLE)).font_semibold().child(format!("Your {year} in review")))
                .child(ui::caption("income, spending, net worth, trips, favourite places and a few fun facts", cx)))
            .child(h_flex().gap_1().child(ui::kbd("ctrl-y", cx)))))
}
