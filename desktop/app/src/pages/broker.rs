//! Broker (IBKR): what the account is worth vs what went in, the two return
//! figures and what each means, what it holds (and how much sits as cash),
//! NAV over time against the deposits, and the latest trades.

use std::rc::Rc;

use finance_core::{Bundle, Kind, Ledger, broker};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, chart::{BarChart, LineChart}, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::fmt;
use crate::ui::{self, Figures as _, Pt};
use crate::zoom::px;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Overview,
    Monitoring,
    /// the investing analysis (pages::deep), its own view
    DeepDive,
}

/// What the page asks the app to change.
#[derive(Clone, Debug)]
pub enum Event {
    Tab(Tab),
    /// the holding the Monitoring price chart shows
    Symbol(SharedString),
}

pub type OnEvent = Rc<dyn Fn(Event, &mut Window, &mut App)>;

/// The page: a tab switch over the account overview, per-holding monitoring
/// and the deep dive (`deep`, rendered by the app's DeepDive view).
pub fn render(b: &Bundle, ledger: &Ledger, tab: Tab, symbol: Option<&str>, on_event: OnEvent, deep: Option<AnyElement>, cx: &App) -> ui::Sections {
    let t = cx.theme();
    let seg = |label: &'static str, this: Tab| {
        let on = tab == this;
        let on_event = on_event.clone();
        div().id(label).px(px(12.)).py(px(4.)).rounded(px(6.)).text_sm().cursor_pointer().whitespace_nowrap()
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| if !on { on_event(Event::Tab(this), w, cx) })
            .child(label)
    };
    let tabs = h_flex().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx))
        .child(seg("Overview", Tab::Overview))
        .child(seg("Monitoring", Tab::Monitoring))
        .child(seg("Deep dive", Tab::DeepDive));
    ui::Sections::new()
        .gap(16.)
        .child(h_flex().child(tabs))
        .child(match tab {
            Tab::Overview => overview(b, ledger, cx).into_any_element(),
            Tab::Monitoring => monitoring(b, ledger, symbol, on_event.clone(), cx).into_any_element(),
            Tab::DeepDive => deep.unwrap_or_else(|| div().into_any_element()),
        })
}

const RECENT: usize = 10;

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
    div().px_2().rounded_full().bg(color.opacity(0.15)).text_color(color).text_xs().whitespace_nowrap().child(text)
}

/// Fractional shares: up to 4 decimals, trailing zeros dropped.
fn shares(v: f64) -> String {
    if fmt::private() {
        return fmt::MASK.into();
    }
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[derive(Clone)]
struct Day {
    label: SharedString,
    nav: f64,
    deposits: f64,
}

fn overview(b: &Bundle, ledger: &Ledger, cx: &App) -> Div {
    let t = cx.theme();
    let perf = |k: &str| b.f64(&["ibkr", "performance", k]);
    let snap = |k: &str| b.f64(&["ibkr", "snapshot", k]);
    let nav = perf("nav_base").or(snap("total_base")).or(b.f64(&["ibkr", "value_hkd"]));
    let Some(nav) = nav else {
        return v_flex().child(ui::empty("No IBKR data yet — sync first", cx));
    };
    let ccy = b.str(&["ibkr", "performance", "base_ccy"]).or(b.str(&["ibkr", "snapshot", "base_ccy"])).map(str::to_string).unwrap_or_else(fmt::base_ccy);
    let money = |v: f64| if ccy == fmt::base_ccy() { fmt::hkd(v) } else { format!("{ccy} {}", fmt::int(v)) };
    let signed = |v: f64| if ccy == fmt::base_ccy() { fmt::signed_hkd(v) } else { format!("{}{ccy} {}", if v >= 0. { "+" } else { "−" }, fmt::int(v.abs())) };

    let deposits = perf("net_deposits_base").or(b.f64(&["ibkr", "cost_hkd"]));
    let gain = perf("gain_base").or(deposits.map(|d| nav - d));
    let as_of = b.str(&["ibkr", "performance", "as_of"]).or(b.str(&["ibkr", "snapshot", "as_of"])).unwrap_or("");
    let first = b.str(&["ibkr", "performance", "first_day"]).unwrap_or("");
    let days = perf("days");
    let (twr, mwr, simple) = (perf("twr_pct"), perf("mwr_pct"), perf("simple_pct"));

    let cards = h_flex()
        .gap_4()
        .flex_wrap()
        .items_start()
        .child(tile("Account value", money(nav),
            format!("NAV as of {as_of}{}", days.map(|d| format!(" · {d:.0} days in")).unwrap_or_default()), None, cx))
        .child(tile("Net deposits", deposits.map(money).unwrap_or("—".into()),
            if first.is_empty() { "money in minus money out".to_string() } else { format!("in minus out since {first}") },
            None, cx))
        .child(tile("Gain", gain.map(signed).unwrap_or("—".into()),
            simple.map(|s| format!("{} on net deposits", fmt::pct(s))).unwrap_or_default(),
            gain.map(fmt::signed_color), cx))
        .child(tile("Money-weighted", mwr.map(|m| format!("{}/yr", fmt::pct(m))).unwrap_or("—".into()),
            twr.map(|w| format!("time-weighted {}", fmt::pct(w))).unwrap_or_default(),
            mwr.map(fmt::signed_color), cx));

    // TWR vs MWR in one line, with what the gap says
    let explain = match (twr, mwr) {
        (Some(w), Some(m)) => Some(format!(
            "Time-weighted {} is how the holdings did, ignoring when money arrived (IBKR's headline). \
             Money-weighted {}/yr is what your money earned given when it went in, annualized{}. {}",
            fmt::pct(w), fmt::pct(m),
            days.filter(|d| *d < 365.).map(|d| format!(" from {d:.0} days, so it swings a lot")).unwrap_or_default(),
            if m > w { "Higher MWR: deposits landed before the gains." } else { "Lower MWR: deposits landed before the losses." })),
        _ => None,
    };

    // ---- NAV vs cumulative deposits -------------------------------------------
    let mut cum = 0.;
    let pts: Vec<Day> = b.arr(&["ibkr", "performance", "nav"]).iter()
        .filter_map(|r| {
            cum += r.get("flow").and_then(Value::as_f64).unwrap_or(0.);
            let d = r.get("date")?.as_str()?;
            Some(Day { label: d.get(5..).unwrap_or(d).to_string().into(), nav: r.get("nav")?.as_f64()?, deposits: cum })
        })
        .collect();
    let gain_line = LineChart::new(pts).appear(false)
        .x(|p: &Day| p.label.clone())
        .y(|p: &Day| p.nav - p.deposits)
        .stroke(t.primary)
        .linear()
        .y_axis(true)
        .tick_margin(10)
        .y_tick_format(fmt::compact)
        .tooltip_value(move |p, v| format!("{} · NAV {} · in {}",
            fmt::signed_hkd(v), fmt::compact(p.nav), fmt::compact(p.deposits)).into());
    let chart = ui::chart_card("Gain over time", &format!("NAV minus net deposits, daily, {ccy}"), 260., gain_line, cx);

    // ---- holdings ------------------------------------------------------------
    let mut positions: Vec<&Value> = b.arr(&["ibkr", "snapshot", "positions"]).iter().collect();
    let g = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.);
    let gs = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    positions.sort_by(|a, b| g(b, "value_base").total_cmp(&g(a, "value_base")));
    let cash: Vec<&Value> = b.arr(&["ibkr", "snapshot", "cash"]).iter().collect();
    let cash_base: f64 = snap("cash_base").unwrap_or_else(|| cash.iter().map(|c| g(c, "amount_base")).sum());
    let total = snap("total_base").unwrap_or(nav);
    let share = |v: f64| if total > 0. { v / total * 100. } else { 0. };

    let num = |w: f32| div().w(px(w)).flex_shrink_0().text_sm().text_right().whitespace_nowrap();
    let head = h_flex()
        .gap_3()
        .pb_1()
        .border_b_1()
        .border_color(t.border)
        .text_xs()
        .text_color(t.muted_foreground)
        .child(div().w(px(if crate::logo::enabled(cx) { 92. } else { 64. })).flex_shrink_0().child("SYMBOL"))
        .child(div().flex_1().min_w_0().child("NAME"))
        .child(num(70.).text_xs().child("QTY"))
        .child(num(100.).text_xs().child("PRICE"))
        .child(num(100.).text_xs().child(format!("VALUE {ccy}")))
        .child(num(44.).text_xs().child("SHARE"));
    let line = |sym: String, name: String, q: String, mark: String, value: f64, muted: bool| {
        h_flex()
            .gap_3()
            .py_1()
            .border_b_1()
            .border_color(t.border)
            .child(h_flex().w(px(if crate::logo::enabled(cx) { 92. } else { 64. })).flex_shrink_0().gap_2().items_center()
                .when_some((!muted).then(|| crate::logo::tile(crate::logo::for_symbol(&sym), 20., cx)).flatten(), |d, l| d.child(l))
                .child(div().text_sm().font_semibold().child(sym)))
            .child(div().flex_1().min_w_0().text_sm().text_color(t.muted_foreground)
                .overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
            .child(num(70.).child(q))
            .child(num(100.).text_color(t.muted_foreground).child(mark))
            .child(num(100.).when(muted, |d| d.text_color(t.muted_foreground)).child(fmt::int(value)))
            .child(num(44.).text_color(t.muted_foreground).child(format!("{:.0}%", share(value))))
    };
    let table = v_flex()
        .child(head)
        .children(positions.iter().map(|p| line(
            gs(p, "symbol"),
            gs(p, "description"),
            shares(g(p, "qty")),
            format!("{} {}", fmt::dec2(g(p, "mark_price")), gs(p, "currency")),
            g(p, "value_base"),
            false,
        )))
        .children(cash.iter().map(|c| line(
            "Cash".into(),
            format!("{} {}", gs(c, "currency"), fmt::dec2(g(c, "amount"))),
            String::new(),
            String::new(),
            g(c, "amount_base"),
            true,
        )))
        .child(h_flex().gap_3().pt_2()
            .child(div().flex_1().text_sm().font_semibold().child("Total"))
            .child(num(100.).font_semibold().child(fmt::int(total)))
            .child(num(44.)));
    let cash_pct = share(cash_base);
    let alloc_caption = format!("as of {as_of} · {cash_pct:.0}% uninvested cash");

    // ---- recent trades: newest broker entries, FX conversions aside ----------
    let mut fx_hidden = 0usize;
    let mut trades: Vec<&finance_core::Entry> = Vec::with_capacity(RECENT);
    for e in ledger.entries.iter().filter(|e| e.kind == Kind::Broker) {
        // "Buy 459.99 USD.HKD @ 7.85 HKD": a currency conversion, not a trade
        let symbol = e.description.split_whitespace().nth(2).unwrap_or("");
        if symbol.len() == 7 && symbol.as_bytes()[3] == b'.' && symbol.chars().all(|c| c == '.' || c.is_ascii_uppercase()) {
            fx_hidden += 1;
            continue;
        }
        trades.push(e);
        if trades.len() >= RECENT {
            break;
        }
    }
    let trade_rows = v_flex().children(trades.iter().map(|e| {
        let buy = e.description.starts_with("Buy");
        let hkd = e.hkd.unwrap_or(e.amount);
        h_flex()
            .gap_3()
            .py_1()
            .border_b_1()
            .border_color(t.border)
            .child(div().w(px(92.)).flex_shrink_0().text_sm().text_color(t.muted_foreground).child(e.date.clone()))
            .child(h_flex().w(px(44.)).flex_shrink_0()
                .child(chip(if buy { "Buy" } else { "Sell" }.into(), if buy { fmt::money_in() } else { fmt::money_out() })))
            .when_some(crate::logo::tile(crate::logo::for_entry(e), 20., cx), |d, l| d.child(l))
            .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap()
                .child(e.description.split_once(' ').map(|x| x.1).unwrap_or(&e.description).to_string()))
            .child(num(110.).child(fmt::hkd(hkd.abs())))
    }));
    let trades_caption = if fx_hidden > 0 { "newest first · currency conversions hidden" } else { "newest first" };

    v_flex()
        .gap_4()
        .child(cards)
        .children(explain.map(|s| ui::card(cx).child(div().text_sm().text_color(t.muted_foreground).child(s))))
        .child(chart)
        .child(ui::section("Holdings", &alloc_caption, cx))
        .child(ui::card(cx).child(table))
        .child(ui::section("Recent trades", trades_caption, cx))
        .child(ui::card(cx).child(if trades.is_empty() {
            div().text_sm().text_color(t.muted_foreground).child("No trades in the local replica yet").into_any_element()
        } else {
            trade_rows.into_any_element()
        }))
}

// ---------------------------------------------------------------- monitoring

/// Every holding with real numbers: cost at the rates of the day it was
/// bought (so currency moves are part of the gain), value at the broker's
/// mark, commissions actually paid, and the fund's yearly fee.
fn monitoring(b: &Bundle, ledger: &Ledger, symbol: Option<&str>, on_event: OnEvent, cx: &App) -> Div {
    let t = cx.theme();
    let positions = b.arr(&["ibkr", "snapshot", "positions"]);
    let cash: f64 = b.arr(&["ibkr", "snapshot", "cash"]).iter().filter_map(|c| c.get("amount_base")?.as_f64()).sum();
    let pos = |sym: &str| positions.iter().find(|p| p.get("symbol").and_then(|x| x.as_str()) == Some(sym));
    let g = |v: Option<&Value>, k: &str| v.and_then(|p| p.get(k)).and_then(|x| x.as_f64());
    // yearly fund fee per symbol, from the server's cost model ("IBKR VWRA")
    let ter = |sym: &str| b.arr(&["costs", "running", "holdings"]).iter()
        .find(|h| h.get("holding").and_then(|x| x.as_str()) == Some(&format!("IBKR {sym}")))
        .map(|h| (h.get("ter_pct").and_then(|x| x.as_f64()), h.get("fee_yr_hkd").and_then(|x| x.as_f64())));

    struct Row { h: broker::Holding, price: Option<f64>, value: f64, ter: Option<f64>, fee_yr: Option<f64> }
    let rows: Vec<Row> = broker::holdings(ledger).into_iter()
        .filter(|h| h.qty > 1e-9)
        .map(|h| {
            let p = pos(&h.symbol);
            // the broker's mark wins; without a snapshot row, value at cost
            let value = g(p, "value_base").unwrap_or(h.cost_hkd);
            let (ter, fee_yr) = ter(&h.symbol).unwrap_or((None, None));
            Row { price: g(p, "mark_price"), value, ter, fee_yr, h }
        })
        .collect();
    if rows.is_empty() {
        return v_flex().child(ui::empty("No broker trades in the local replica yet", cx));
    }

    let value: f64 = rows.iter().map(|r| r.value).sum();
    let cost: f64 = rows.iter().map(|r| r.h.cost_hkd).sum();
    let fees: f64 = rows.iter().map(|r| r.h.fees_hkd).sum();
    let realized: f64 = rows.iter().map(|r| r.h.realized_hkd).sum();
    let fee_yr: f64 = rows.iter().filter_map(|r| r.fee_yr).sum();
    let unreal = value - cost;
    let account = value + cash;
    let pct = |a: f64, b: f64| if b.abs() > 1e-9 { a / b * 100. } else { 0. };

    let tiles = h_flex().gap(px(ui::sp::GRID)).flex_wrap().items_start()
        .child(ui::Tile::explained("Market value", fmt::local("Every position at the broker's latest mark, converted to HKD by the broker. Uninvested cash is shown separately."),
            fmt::hkd(value)).sub(format!("{} holdings", rows.len())).render(cx).min_w(px(190.)))
        .child(ui::Tile::explained("Cost basis", fmt::local("What the units you still hold cost, including commissions, converted to HKD at the exchange rate of the day each lot was bought."),
            fmt::hkd(cost)).sub("incl. commissions").render(cx).min_w(px(190.)))
        .child(ui::Tile::explained("Unrealized", "Market value minus cost basis. Because cost is at the day's exchange rate, currency moves are part of this gain.",
            fmt::signed_hkd(unreal)).tone(fmt::signed_color(unreal)).sub(format!("{}% on cost", fmt::signed(pct(unreal, cost), 2)))
            .render(cx).min_w(px(190.)))
        .child(ui::Tile::explained("Commissions paid", "Every IBKR commission charged on these holdings. Small orders hit the minimum per order, so fewer, larger buys cost less.",
            fmt::hkd(fees)).sub(format!("{:.2}% of cost", pct(fees, cost))).render(cx).min_w(px(190.)))
        .child(ui::Tile::explained("Fund fees / yr", "Each fund's yearly fee (TER) at today's value. It is taken inside the fund price, so it never appears as a transaction.",
            format!("{}/yr", fmt::hkd(fee_yr))).sub(format!("{:.2}% of value", pct(fee_yr, value))).render(cx).min_w(px(190.)))
        .child(ui::Tile::explained("Uninvested cash", "Cash sitting in the account, not in any position.",
            fmt::hkd(cash)).sub(if realized.abs() > 0.5 {
                format!("{:.0}% of the account · realized {}", pct(cash, account), fmt::signed_hkd(realized))
            } else {
                format!("{:.0}% of the account", pct(cash, account))
            })
            .render(cx).min_w(px(190.)));

    // unrealized gain per holding
    let pts: Vec<Pt> = rows.iter().map(|r| Pt { label: r.h.symbol.clone().into(), v: r.value - r.h.cost_hkd }).collect();
    let (up, down) = (fmt::money_in(), fmt::money_out());
    let chart = BarChart::new(pts).appear(false)
        .band(|p: &Pt| p.label.clone())
        .value(|p: &Pt| p.v)
        .value_axis(true)
        .value_tick_format(|v| fmt::compact(v))
        .tooltip_value(|_, v| fmt::signed_hkd(v).into())
        .fill(move |p: &Pt, _, _, _| if p.v >= 0. { up } else { down });

    // the table
    const W: [f32; 14] = [96., 76., 96., 96., 92., 92., 92., 64., 76., 64., 56., 72., 56., 84.];
    let cell = |i: usize, txt: String| div().w(px(W[i])).flex_shrink_0().when(i > 0, |d| d.text_right()).whitespace_nowrap().child(txt);
    let head = h_flex().gap_2().py_1().text_xs().text_color(t.muted_foreground).border_b_1().border_color(t.border)
        .children(["Holding", "Qty", "Avg cost", "Price", "Cost HKD", "Value HKD", "P/L HKD", "P/L %", "Fees HKD", "Fees %", "TER", "Fee / yr", "Weight", "Since"]
            .into_iter().enumerate().map(|(i, c)| cell(i, fmt::local(c))));
    let body = v_flex().children(rows.iter().map(|r| {
        let pl = r.value - r.h.cost_hkd;
        let ccy = &r.h.currency;
        h_flex().gap_2().py(px(6.)).text_sm().figures().items_center().border_b_1().border_color(t.border.opacity(0.5))
            .child(h_flex().w(px(W[0])).flex_shrink_0().gap_2().items_center()
                .when_some(crate::logo::tile(crate::logo::for_symbol(&r.h.symbol), 20., cx), |d, l| d.child(l))
                .child(div().font_semibold().child(r.h.symbol.clone())))
            .child(cell(1, shares(r.h.qty)))
            .child(cell(2, format!("{} {ccy}", fmt::dec2(r.h.avg_cost))))
            .child(cell(3, r.price.map(|p| format!("{} {ccy}", fmt::dec2(p))).unwrap_or("—".into())))
            .child(cell(4, fmt::int(r.h.cost_hkd)))
            .child(cell(5, fmt::int(r.value)))
            .child(cell(6, fmt::signed_hkd(pl).replace(&fmt::sym_space(), "")).text_color(fmt::signed_color(pl)))
            .child(cell(7, format!("{}%", fmt::signed(pct(pl, r.h.cost_hkd), 1))).text_color(fmt::signed_color(pl)))
            .child(cell(8, fmt::dec2(r.h.fees_hkd)))
            .child(cell(9, format!("{:.2}%", pct(r.h.fees_hkd, r.h.cost_hkd)))
                .when(pct(r.h.fees_hkd, r.h.cost_hkd) >= 0.75, |d| d.text_color(t.warning)))
            .child(cell(10, r.ter.map(|x| format!("{x:.2}%")).unwrap_or("—".into())))
            .child(cell(11, r.fee_yr.map(|x| fmt::int(x)).unwrap_or("—".into())))
            .child(cell(12, format!("{:.0}%", pct(r.value, account))))
            .child(cell(13, format!("{} · {}×", fmt::month_label(&r.h.first_buy[..r.h.first_buy.len().min(7)]), r.h.buys)))
    }));
    let total = h_flex().gap_2().py(px(6.)).text_sm().figures().font_semibold()
        .child(cell(0, "Total".into()))
        .child(cell(1, String::new())).child(cell(2, String::new())).child(cell(3, String::new()))
        .child(cell(4, fmt::int(cost)))
        .child(cell(5, fmt::int(value)))
        .child(cell(6, fmt::signed_hkd(unreal).replace(&fmt::sym_space(), "")).text_color(fmt::signed_color(unreal)))
        .child(cell(7, format!("{}%", fmt::signed(pct(unreal, cost), 1))).text_color(fmt::signed_color(unreal)))
        .child(cell(8, fmt::dec2(fees)))
        .child(cell(9, format!("{:.2}%", pct(fees, cost))))
        .child(cell(10, String::new()))
        .child(cell(11, fmt::int(fee_yr)))
        .child(cell(12, format!("{:.0}%", pct(value, account))))
        .child(cell(13, String::new()));
    let width: f32 = W.iter().sum::<f32>() + 13. * 8.;
    let table = div().id("monitor-table").w_full().overflow_x_scroll().restrict_scroll_to_axis()
        .child(v_flex().min_w(px(width)).child(head).child(body).child(total));

    // the costly-orders note: commissions above ~0.5% of cost are mostly the per-order minimum
    let heavy: Vec<&Row> = rows.iter().filter(|r| pct(r.h.fees_hkd, r.h.cost_hkd) >= 0.75).collect();
    let note = (!heavy.is_empty()).then(|| {
        ui::card(cx).child(div().text_sm().text_color(t.muted_foreground).child(format!(
            "{} cost{} {} in commissions so far: on small orders the per-order minimum sets the fee. \
             Deep dive › Fees shows each order's fee and the order size that brings it under 0.25%.",
            heavy.iter().map(|r| format!("{} {:.1}%", r.h.symbol, pct(r.h.fees_hkd, r.h.cost_hkd))).collect::<Vec<_>>().join(", "),
            if heavy.len() == 1 { "s" } else { "" },
            "of what you bought",
        )))
    });

    // the price chart for one holding: largest by value unless one was picked
    let picked = symbol.and_then(|s| rows.iter().find(|r| r.h.symbol == s))
        .or_else(|| rows.iter().max_by(|a, b| a.value.total_cmp(&b.value)));
    let picker = h_flex().gap_2().flex_wrap().children(rows.iter().map(|r| {
        let on = picked.is_some_and(|p| p.h.symbol == r.h.symbol);
        let sym: SharedString = r.h.symbol.clone().into();
        let on_event = on_event.clone();
        h_flex().id(SharedString::from(format!("pick-{}", r.h.symbol))).gap_2().items_center()
            .px(px(10.)).py(px(5.)).rounded(px(8.)).border_1().cursor_pointer()
            .border_color(if on { t.primary } else { ui::hairline(cx) })
            .when(on, |d| d.bg(t.secondary))
            .hover(|s| s.bg(t.secondary))
            .on_click(move |_, w, cx| on_event(Event::Symbol(sym.clone()), w, cx))
            .when_some(crate::logo::tile(crate::logo::for_symbol(&r.h.symbol), 18., cx), |d, l| d.child(l))
            .child(div().text_sm().font_semibold().child(r.h.symbol.clone()))
    }));
    let price_card = picked.map(|r| {
        let bars = ledger.bars.get(&r.h.symbol).map(Vec::as_slice).unwrap_or(&[]);
        let buys: Vec<Buy> = ledger.trades.iter().filter(|t| t.symbol == r.h.symbol && t.qty > 0.)
            .map(|t| Buy { date: t.date.clone(), price: t.price, qty: t.qty, cost_hkd: (-t.amount + t.commission) * t.fx_to_base })
            .collect();
        let last = bars.last().map(|b| b.close).or(r.price);
        let vs = last.map(|p| pct(p - r.h.avg_cost, r.h.avg_cost));
        ui::card(cx).gap_3()
            .child(h_flex().justify_between().flex_wrap().gap_2().items_center()
                .child(h_flex().gap_3().items_center()
                    .when_some(crate::logo::tile(crate::logo::for_symbol(&r.h.symbol), 28., cx), |d, l| d.child(l))
                    .child(v_flex()
                        .child(div().text_base().font_semibold().child(r.h.symbol.clone()))
                        .child(ui::caption(positions.iter().find(|p| p.get("symbol").and_then(|x| x.as_str()) == Some(&r.h.symbol))
                            .and_then(|p| p.get("description")?.as_str()).unwrap_or("").to_string(), cx))))
                .child(h_flex().gap_4().items_center().figures()
                    .child(v_flex().items_end()
                        .child(div().text_lg().font_semibold().child(last.map(|p| format!("{} {}", fmt::dec2(p), r.h.currency)).unwrap_or("—".into())))
                        .child(ui::caption("last close", cx)))
                    .child(v_flex().items_end()
                        .child(div().text_lg().font_semibold().text_color(fmt::signed_color(vs.unwrap_or(0.)))
                            .child(vs.map(fmt::pct).unwrap_or("—".into())))
                        .child(ui::caption(format!("vs your avg {}", fmt::dec2(r.h.avg_cost)), cx)))))
            .child(price_candles(bars, &buys, r.h.avg_cost, &r.h.currency, cx))
            .child(ui::caption(format!("{} buys · {} units · cost {} · markers sized by amount; dashed line = your average cost (fees incl.)",
                buys.len(), shares(r.h.qty), fmt::hkd(r.h.cost_hkd)), cx))
    });

    v_flex()
        .gap_4()
        .child(tiles)
        .child(ui::section_info("Price", "real daily market prices, your buys marked",
            "Daily candles from the market price of the listing you hold (open, high, low, close). Each dot is one of your buys, at the price you paid on that day, sized by the amount. The dashed line is your average cost including commissions: above it you are in profit on the price alone (currency moves aside).", cx))
        .child(picker)
        .children(price_card)
        .child(ui::chart_card("Unrealized gain by holding", &fmt::local("HK$, incl. currency moves since each buy"), 220., chart, cx))
        .child(ui::section_info("Holdings", "real cost, value and fees per position",
            fmt::local("Avg cost and Cost include commissions. Cost HKD uses the exchange rate of the day each lot was bought; Value HKD is the broker's mark. Fees % = commissions ÷ cost (amber at 0.75% or more). TER and Fee / yr come from the fund's annual fee; single stocks have none."), cx))
        .child(ui::card(cx).child(table))
        .children(note)
}

/// One of your buys, for the price chart.
struct Buy {
    date: String,
    price: f64,
    qty: f64,
    cost_hkd: f64,
}

/// Daily candles drawn by hand (the library candlestick has no markers):
/// each column is one trading day -- wick, body, and a dot where you bought,
/// plus a dashed line at your average cost. Window: from about two months
/// before the first buy (at most ~6 months of days).
fn price_candles(all: &[finance_core::Bar], buys: &[Buy], avg: f64, ccy: &str, cx: &App) -> Div {
    const H: f32 = 260.;
    const GUTTER: f32 = 56.;
    const MAX_DAYS: usize = 130;
    let t = cx.theme();
    if all.is_empty() {
        return div().h(px(80.)).flex().items_center().justify_center()
            .child(ui::caption("No market prices cached for this symbol yet", cx));
    }
    let first_buy = buys.iter().map(|b| b.date.as_str()).min().unwrap_or("");
    let start_ix = all.iter().position(|b| b.date.as_str() >= first_buy).unwrap_or(all.len() - 1).saturating_sub(42);
    let start_ix = start_ix.max(all.len().saturating_sub(MAX_DAYS));
    let bars = &all[start_ix..];

    // scale on the candle bodies (plus your buys and average): a single bad
    // print -- Yahoo has the odd 30% "low" on London ETFs -- may stretch a
    // wick but must not flatten every candle into a line. Wicks beyond a
    // quarter of the body range are clipped at the plot edge.
    let mut lo = bars.iter().map(|b| b.open.min(b.close)).fold(f64::MAX, f64::min);
    let mut hi = bars.iter().map(|b| b.open.max(b.close)).fold(f64::MIN, f64::max);
    for p in buys.iter().map(|b| b.price).chain(std::iter::once(avg)).filter(|p| *p > 0.) {
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let span = (hi - lo).max(hi.abs() * 0.01);
    let wick_lo = bars.iter().map(|b| b.low).fold(f64::MAX, f64::min).max(lo - span * 0.25);
    let wick_hi = bars.iter().map(|b| b.high).fold(f64::MIN, f64::max).min(hi + span * 0.25);
    let (lo, hi) = (lo.min(wick_lo), hi.max(wick_hi));
    let pad = (hi - lo) * 0.05;
    let (lo, hi) = (lo - pad, hi + pad);
    let y = move |v: f64| (((hi - v) / (hi - lo)) as f32 * H).clamp(0., H);
    let max_buy = buys.iter().map(|b| b.cost_hkd).fold(0., f64::max);
    let (up, down, mark) = (fmt::money_in(), fmt::money_out(), t.primary);

    // buys that fall on a shown day (a buy on a non-trading day snaps to the next bar)
    let buy_at = |ix: usize| -> Vec<&Buy> {
        let d = &bars[ix].date;
        let prev = if ix == 0 { "" } else { bars[ix - 1].date.as_str() };
        buys.iter().filter(|b| b.date.as_str() <= d.as_str() && b.date.as_str() > prev).collect()
    };

    let columns = h_flex().h(px(H)).flex_1().min_w_0().gap(px(1.)).children(bars.iter().enumerate().map(|(i, b)| {
        let color = if b.close >= b.open { up } else { down };
        let (top, bot) = (b.open.max(b.close), b.open.min(b.close));
        let here = buy_at(i);
        let mut tip = format!("{}  O {}  H {}  L {}  C {} {ccy}", b.date, fmt::dec2(b.open), fmt::dec2(b.high), fmt::dec2(b.low), fmt::dec2(b.close));
        for bb in &here {
            tip.push_str(&format!("\nYou bought {} @ {} {ccy} — {}", (bb.qty * 10000.).round() / 10000., fmt::dec2(bb.price), fmt::hkd(bb.cost_hkd)));
        }
        div().id(("candle", i)).flex_1().min_w(px(1.)).h_full().relative()
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            .child(div().absolute().left(relative(0.5)).w(px(1.)).top(px(y(b.high))).h(px((y(b.low) - y(b.high)).max(1.))).bg(color.opacity(0.8)))
            .child(div().absolute().left(relative(0.18)).right(relative(0.18)).top(px(y(top))).h(px((y(bot) - y(top)).max(1.5))).rounded(px(1.)).bg(color))
            .children(here.into_iter().map(move |bb| {
                let r = 5. + 5. * if max_buy > 0. { (bb.cost_hkd / max_buy).sqrt() as f32 } else { 0.5 };
                div().absolute().left(relative(0.5)).ml(px(-r)).top(px(y(bb.price) - r)).size(px(2. * r)).rounded_full()
                    .bg(mark.opacity(0.85)).border_2().border_color(t.background)
            }))
    }));

    // y labels, the average-cost line and its label
    let ticks = 4;
    let ylabels = (0..=ticks).map(|k| {
        let v = hi - (hi - lo) * k as f64 / ticks as f64;
        div().absolute().right(px(6.)).top(px(y(v) - 7.)).text_size(px(10.)).text_color(t.muted_foreground).figures()
            .child(fmt::dec2(v))
    });
    let avg_line = (avg > 0.).then(|| {
        div().absolute().left(px(GUTTER)).right_0().top(px(y(avg))).h(px(0.)).border_t_1()
            .border_color(t.foreground.opacity(0.45))
            .child(div().absolute().right_0().top(px(-18.)).px_1().rounded(px(3.)).bg(t.secondary)
                .text_size(px(10.)).text_color(t.foreground).figures().child(format!("avg {}", fmt::dec2(avg))))
    });
    let step = (bars.len() / 6).max(1);
    let xlabels = h_flex().pl(px(GUTTER)).children(bars.iter().enumerate().map(|(i, b)| {
        // labels overflow their (narrow) day column on purpose
        div().flex_1().min_w(px(1.)).text_size(px(10.)).text_color(t.muted_foreground).whitespace_nowrap()
            .when(i % step == 0, |d| d.child(
                chrono::NaiveDate::parse_from_str(&b.date, "%Y-%m-%d").map(|x| x.format("%-d %b").to_string()).unwrap_or_else(|_| b.date.clone()),
            ))
    }));
    v_flex().gap_1()
        .child(div().relative().h(px(H)).w_full()
            .child(h_flex().h_full().w_full().child(div().w(px(GUTTER)).h_full().relative().children(ylabels)).child(columns))
            .children(avg_line))
        .child(xlabels)
}
