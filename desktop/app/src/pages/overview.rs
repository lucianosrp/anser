//! Overview: the headline (net worth, its trajectory and where it sits), the
//! few numbers that drive decisions (runway, savings rate, cash drag), and the
//! latest movements.

use finance_core::{Bundle, Entry, Kind, Ledger};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, chart::AreaChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, Figures as _, Pt, Tile, sp, ts};
use crate::zoom::px;

/// Percent change from `a` to `b` (None when `a` is ~0).
fn change(a: f64, b: f64) -> Option<f64> {
    (a.abs() > 1e-9).then(|| (b - a) / a.abs() * 100.)
}

/// `next30`: what the Cash flow page expects over the next 30 days.
pub fn render(b: &Bundle, ledger: &Ledger, growth: super::projection::Growth, on_growth: super::projection::OnGrowth,
              next30: Option<finance_core::recurring::Outlook>, cx: &App) -> ui::Sections {
    let t = cx.theme();
    let nw = |k: &str| b.f64(&["net_worth", k]).unwrap_or(0.);
    let total = nw("total_hkd");
    let (cash, inv, crypto) = (nw("banks_hkd"), nw("investments_hkd"), nw("crypto_hkd"));
    // deposits held + the partner's balance (engine views::receivables)
    let owed = nw("receivables_hkd");
    // a mortgage setup (engine views::networth): the loan is a negative line
    // and the home's value a positive one, both inside the total. Shares are
    // of what you own, so the loan doesn't push the others past 100%.
    let (liab, property) = (nw("liabilities_hkd"), nw("property_hkd"));
    let has_home = b.f64(&["net_worth", "liabilities_hkd"]).is_some();
    let gross = total - liab.min(0.);
    let pct_of = |v: f64| if gross > 0. { v / gross * 100. } else { 0. };

    // month-end series (the last row is the current month so far)
    let rows = b.arr(&["overview", "sheet", "rows"]);
    let col = |key: &str| -> Vec<f64> { rows.iter().map(|r| r.get(key).and_then(|v| v.as_f64()).unwrap_or(0.)).collect() };
    let s_total = col("total_hkd");
    let s_inv = col("investments_hkd");
    let s_crypto = col("crypto_hkd");
    let s_owed = col("receivables_hkd");
    let s_cash: Vec<f64> = s_total.iter().zip(&s_inv).zip(&s_crypto).zip(&s_owed).map(|(((t, i), c), o)| t - i - c - o).collect();
    let tail = |s: &[f64], n: usize| s[s.len().saturating_sub(n)..].to_vec();
    let back = |s: &[f64], n: usize| (s.len() > n).then(|| s[s.len() - 1 - n]);
    let d1 = |s: &[f64]| back(s, 1).zip(s.last()).and_then(|(a, b)| change(a, *b));

    // trailing 6 completed months
    let trend = b.completed_trend();
    let last6: Vec<_> = trend.iter().rev().take(6).collect();
    let k = last6.len().max(1) as f64;
    let avg = |key: &str| last6.iter().map(|t| t.get(key).and_then(|v| v.as_f64()).unwrap_or(0.)).sum::<f64>() / k;
    let (spend, income) = (avg("spend"), avg("income"));
    let rate = if income > 0. { (income - spend) / income * 100. } else { 0. };
    let runway = if spend > 0. { cash / spend } else { 0. };
    let drag = b.f64(&["costs", "cash_drag", "mo_hkd"]);
    let g = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.);
    let last12: Vec<_> = trend.iter().rev().take(12).rev().collect();
    let s_rate: Vec<f64> = last12.iter().map(|m| { let i = g(m, "income"); if i > 0. { (i - g(m, "spend")) / i * 100. } else { 0. } }).collect();

    // ---------- hero: net worth, change, allocation, 3-year trajectory ----------
    let (c_cash, c_inv, c_crypto, c_owed, c_home) = (t.chart_3, t.chart_2, t.chart_4, t.chart_5, t.chart_1);
    // only what the server has sources for (finance_core::capabilities)
    let caps = finance_core::capabilities::Capabilities::of(b);
    let (has_cash, has_inv, has_crypto, has_owed) =
        (caps.has("banks"), caps.has("broker") || caps.has("fund_proxy"), caps.has("crypto"), caps.has("receivables"));
    let buckets: Vec<_> = [("Cash", cash, c_cash, has_cash), ("Investments", inv, c_inv, has_inv), ("Crypto", crypto, c_crypto, has_crypto),
                           ("Owed", owed, c_owed, has_owed), ("Home", property, c_home, property > 0.)]
        .into_iter().filter(|b| b.3).map(|(l, v, c, _)| (l, v, c)).collect();
    let alloc_bar = h_flex()
        .w_full()
        .h(px(6.))
        .gap(px(2.))
        .rounded_full()
        .overflow_hidden()
        .children(buckets.iter().copied().filter(|(_, v, _)| *v > 0.).map(|(_, v, c)| {
            div().h_full().flex_basis(px(0.)).flex_grow(pct_of(v).max(0.5) as f32).bg(c)
        }));
    // legend: label over share, one column per bucket (fits a half-screen window)
    let legends = h_flex()
        .gap(px(sp::L))
        .flex_wrap()
        .children(buckets.iter().copied().filter(|(l, v, _)| *l != "Owed" || *v > 0.).map(|(l, v, c)| {
            v_flex()
                .gap(px(2.))
                .child(h_flex().gap(px(6.)).text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(ui::dot(c, 7.)).child(l))
                .child(div().pl(px(13.)).text_size(px(ts::TITLE)).font_weight(FontWeight::MEDIUM).figures().child(format!("{:.0}%", pct_of(v))))
        }));
    let as_of = b.str(&["net_worth", "as_of"]).and_then(|s| s.get(..10))
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_default();
    let headline = v_flex()
        .flex_1()
        .min_w(px(280.))
        .gap(px(10.))
        .child(
            h_flex().gap_2().child(ui::eyebrow("Net worth", cx))
                .when(!as_of.is_empty(), |d| d.child(ui::caption(format!("as of {as_of}"), cx))),
        )
        .child(
            h_flex()
                .items_baseline()
                .gap(px(6.))
                .child(div().text_size(px(16.)).text_color(t.muted_foreground).child(fmt::sym()))
                .child(div().text_size(px(ts::HERO)).line_height(px(40.)).font_semibold().figures().text_color(ui::hero_color(cx)).child(fmt::int(total))),
        )
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .when_some(d1(&s_total), |d, p| d.child(ui::delta_over(p, "this month", cx)))
                .when_some(back(&s_total, 12).and_then(|a| change(a, total - liab - property)), |d, p| d.child(ui::delta_over(p, "in 12 months", cx)))
                // the second figure, in EUR (none when EUR is the base)
                .when(fmt::base_ccy() != "EUR", |d| d.child(ui::caption(format!("≈ € {}", fmt::int(nw("total_eur"))), cx))),
        )
        .when(has_home, |d| d.child(ui::caption_wrap(
            if property > 0. {
                format!("includes your home {} less the mortgage {}", fmt::hkd(property), fmt::hkd(-liab))
            } else {
                format!("includes the mortgage you owe, {}", fmt::hkd(-liab))
            }, cx)))
        .child(div().flex_1().min_h(px(4.)))
        .child(v_flex().gap(px(10.)).child(alloc_bar).child(legends));

    let pts: Vec<Pt> = rows.iter().rev().take(36).rev()
        .filter_map(|r| Some(Pt { label: fmt::month_label(r.get("month")?.as_str()?).into(), v: r.get("total_hkd")?.as_f64()? }))
        .collect();
    let chart = AreaChart::new(pts).appear(false)
        .x(|p: &Pt| p.label.clone())
        .y(|p: &Pt| p.v)
        .stroke(t.primary)
        .fill(ui::soft_fill(t.primary, 0.28))
        .natural()
        .y_axis(true)
        .y_tick_count(4)
        .x_tick_count(4)
        .y_tick_format(|v| fmt::compact(v))
        .tooltip_value(|_, _, v| fmt::hkd(v).into());
    let hero = ui::card(cx)
        .p(px(20.))
        .child(
            h_flex()
                .flex_wrap()
                .items_stretch()
                .gap(px(sp::XL))
                .child(headline)
                .child(
                    v_flex()
                        .flex_1()
                        .flex_grow(1.6)
                        .min_w(px(300.))
                        .h(px(196.))
                        .gap_1()
                        .child(h_flex().justify_between().child(ui::caption("Month-end, last 3 years", cx)))
                        .child(div().flex_1().min_h_0().child(chart)),
                ),
        );

    // ---------- where it sits ----------
    // with the owed tile there are four: 2 + 2 at half-screen, one row when wide
    let owed_tile = super::owed::tile(b, tail(&s_owed, 13), c_owed, cx).filter(|_| has_owed);
    let tile_w = px(if owed_tile.is_some() { 240. } else { 170. });
    let tiles = h_flex()
        .items_stretch()
        .gap(px(sp::GRID))
        .flex_wrap()
        .when(has_cash, |d| d.child(Tile::explained("Cash", "All bank and pocket balances, each converted to HKD at the current rate. Credit-card balances owed are not deducted.", fmt::hkd(cash)).sub("banks, all currencies").id("t-cash")
            .delta_opt(d1(&s_cash), "1m").spark(tail(&s_cash, 13), c_cash).render(cx).min_w(tile_w)))
        .when(has_inv, |d| d.child(Tile::explained("Investments", "Funds and the broker at their latest marks (the broker's valuation, or the fund's NAV). The gain is value minus money put in.", fmt::hkd(inv)).id("t-inv")
            .sub(format!("gain {}", fmt::signed_hkd(nw("investments_gain_hkd"))))
            .delta_opt(d1(&s_inv), "1m").spark(tail(&s_inv, 13), c_inv).render(cx).min_w(tile_w)))
        .when(has_crypto, |d| d.child(Tile::explained("Crypto", "Every exchange and wallet account, at live prices (or the last snapshot when offline), in HKD.", fmt::hkd(crypto)).sub("exchanges + cold storage").id("t-crypto")
            .delta_opt(d1(&s_crypto), "1m").spark(tail(&s_crypto, 13), c_crypto).render(cx).min_w(tile_w)))
        .children(owed_tile.map(|d| d.min_w(tile_w)))
        .when(has_home, |d| d.child(Tile::explained("Home", "Your home's value as you entered it (Settings › Home) less what is left of the mortgage, from the loan's schedule as of today. \
                Both are in net worth: the home as property, the mortgage as a liability. With no home value set it shows only what you owe.",
                fmt::signed_hkd(property + liab)).sub(if property > 0. { format!("home {} − mortgage {}", fmt::compact(property), fmt::compact(-liab)) } else { format!("mortgage owed {}", fmt::compact(-liab)) })
            .id("t-home").render(cx).min_w(tile_w)));

    // ---------- what drives decisions ----------
    let drivers = h_flex()
        .items_stretch()
        .gap(px(sp::GRID))
        .flex_wrap()
        .child(Tile::explained("Runway", "How many months your bank cash alone would last at your average spending of the last 6 completed months, if all income stopped. Investments and crypto are not counted; the Spending page lets you add a new cost like rent.", format!("{runway:.0} months")).id("t-runway")
            .sub(format!("cash ÷ {}/mo spend ({} mo avg)", fmt::compact(spend), last6.len()))
            .render(cx))
        .child(Tile::explained("Savings rate", "(Income − spending) ÷ income over the last 6 completed months. Moves between your own accounts, investing and card-bill payments are neither income nor spending.", format!("{rate:.0}%")).id("t-rate")
            .sub(format!("{} in · {} out a month", fmt::compact(income), fmt::compact(spend)))
            .tone(fmt::signed_color(rate))
            .spark(s_rate, t.muted_foreground).render(cx))
        .child(Tile::explained("Cash drag", "What your bank cash would earn each month at today's 3-month US T-bill yield (for example in IB01). It is an opportunity cost, not a fee you pay.", drag.map(|d| format!("{}/mo", fmt::hkd(d))).unwrap_or("—".into())).id("t-drag")
            .sub(format!("idle cash at {}% T-bill", b.f64(&["costs", "cash_drag", "pct"]).unwrap_or(0.)))
            .tone(fmt::money_out()).render(cx))
        .child(super::budgets::overview_tile(ledger, cx))
        .children(next30.map(|o| super::cashflow::overview_tile(&o, cx)));

    // ---------- latest activity ----------
    // the latest things that happened -- not the exchange's bookkeeping
    // (reconcile adjustments, Earn rewards of a few satoshi)
    let noise = |e: &&Entry| e.kind == Kind::Crypto && (e.category.ends_with("-adjust") || e.category == "reward");
    let recent: Vec<&Entry> = ledger.entries.iter().filter(|e| !noise(e)).take(10).collect();
    let today = chrono::Local::now().date_naive();
    let day_label = |d: &str| -> String {
        match chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
            Ok(x) if x == today => "Today".into(),
            Ok(x) if Some(x) == today.pred_opt() => "Yesterday".into(),
            Ok(x) => x.format("%a %-d %b").to_string(),
            Err(_) => d.to_string(),
        }
    };
    let mut list = v_flex();
    let mut last_day = String::new();
    for e in recent {
        if e.date != last_day {
            last_day = e.date.clone();
            list = list.child(
                div().pt(px(10.)).pb(px(4.)).px(px(sp::L))
                    .child(ui::eyebrow(day_label(&e.date), cx)),
            );
        }
        list = list.child(activity_row(e, cx));
    }
    let latest = ui::card(cx)
        .p_0()
        .pb(px(6.))
        .child(
            h_flex()
                .justify_between()
                .px(px(sp::L))
                .pt(px(14.))
                .child(div().text_size(px(ts::TITLE)).font_semibold().child("Latest activity"))
                .child(h_flex().gap(px(6.)).child(ui::caption("all of it in Transactions", cx)).child(ui::kbd("ctrl-5", cx))),
        )
        .child(list);

    ui::Sections::new()
        .child(hero)
        .child(tiles)
        .child(drivers)
        .children(super::review::overview_card(b, cx))
        .child(div().h(px(sp::XS)))
        .child(super::projection::render(b, growth, on_growth, cx))
        .child(div().h(px(sp::XS)))
        .child(latest)
}

/// One movement: kind icon, description over category · source, amount.
fn activity_row(e: &Entry, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let hkd = e.hkd.unwrap_or(e.amount);
    let icon = match e.kind {
        Kind::Bank => IconName::Building2,
        Kind::Card => IconName::Inbox,
        Kind::Crypto => IconName::Globe,
        Kind::Broker => IconName::ChartPie,
    };
    let hover = t.accent.opacity(0.6);
    h_flex()
        .gap(px(sp::M))
        .px(px(sp::L))
        .py(px(7.))
        .hover(move |s| s.bg(hover))
        .child(match crate::logo::tile(crate::logo::for_entry(e), 30., cx) {
            Some(mark) => mark,
            None => div().flex_shrink_0().size(px(30.)).rounded_full().bg(t.muted).flex().items_center().justify_center()
                .child(Icon::new(icon).small().text_color(t.muted_foreground))
                .into_any_element(),
        })
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_size(px(ts::BODY)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.description.clone()))
                .child(ui::caption(format!("{} · {}", e.category, e.source), cx)),
        )
        .child(
            div().flex_shrink_0().text_size(px(ts::BODY)).font_weight(FontWeight::MEDIUM).figures()
                .text_color(fmt::signed_color(hkd))
                .child(fmt::signed_hkd(hkd)),
        )
}
