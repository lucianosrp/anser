//! Crypto: what the coins are worth (HKD and USD), what they cost and earned
//! (recurring buys alone, and all coins incl. ones deposited from elsewhere),
//! how fresh the prices are, where the coins sit, and the latest real moves.

use chrono::{Duration, Local};
use finance_core::{Bundle, Kind, Ledger};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, chart::LineChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::fmt;
use crate::ui;
use crate::zoom::px;

const RECENT: usize = 12;
const COINS: [&str; 3] = ["BTC", "ETH", "BNB"];
const STABLE: [&str; 7] = ["USDC", "USDT", "FDUSD", "BUSD", "USD", "EUR", "HKD"];

/// Ledger categories worth showing, and the verb for each. Everything else is
/// the exchange's bookkeeping: Earn rewards of a few satoshi, reconcile
/// adjustments, the cash legs of buys and conversions.
fn verb(category: &str) -> Option<&'static str> {
    Some(match category {
        "auto-invest" | "spot-buy" | "fiat" | "convert-in" | "revolut-btc" => "Buy",
        "spot-sell" | "convert-out" => "Sell",
        "withdraw" => "Withdraw",
        "deposit" => "Deposit",
        "transfer-in" => "Transfer in",
        "trezor-tx" => "Cold wallet",
        "cash-deposit" => "Cash in",
        "cash-withdraw" => "Cash out",
        _ => return None,
    })
}

fn usd(v: f64) -> String {
    format!("US$ {}", fmt::int(v))
}

fn signed_usd(v: f64) -> String {
    format!("{}US$ {}", if v >= 0. { "+" } else { "−" }, fmt::int(v.abs()))
}

/// Coin quantities: enough decimals to see a weekly satoshi buy.
fn qty(v: f64) -> String {
    if fmt::private() {
        return fmt::MASK.into();
    }
    let a = v.abs();
    let s = if a >= 100. {
        fmt::dec2(a)
    } else if a >= 1. {
        format!("{a:.4}")
    } else {
        format!("{a:.6}")
    };
    if v < 0. { format!("−{s}") } else { s }
}

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

fn legend(color: Hsla, label: &str, cx: &App) -> Div {
    h_flex()
        .gap_1()
        .child(div().w(px(10.)).h(px(3.)).rounded_full().bg(color))
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label.to_string()))
}

#[derive(Clone)]
struct Month {
    label: SharedString,
    value: f64,
    invested: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Overview,
    Dca,
}

/// What the page asks the app to change.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Tab(Tab),
    Scope(finance_core::dca::Scope),
}

pub type OnEvent = std::rc::Rc<dyn Fn(Event, &mut Window, &mut App)>;

/// The page: holdings and returns (Overview), or how the BTC DCA is doing.
pub fn render(b: &Bundle, ledger: &Ledger, tab: Tab, scope: finance_core::dca::Scope, on_event: OnEvent, cx: &App) -> ui::Sections {
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
        .child(seg("BTC DCA", Tab::Dca));
    let on_scope: crate::pages::dca::OnScope = std::rc::Rc::new(move |s, w, cx| on_event(Event::Scope(s), w, cx));
    ui::Sections::new()
        .gap(16.)
        .child(h_flex().child(tabs))
        .child(match tab {
            Tab::Overview => overview(b, ledger, cx).into_any_element(),
            Tab::Dca => crate::pages::dca::render(b, ledger, scope, on_scope, cx).into_any_element(),
        })
}

fn overview(b: &Bundle, ledger: &Ledger, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let tot = |k: &str| b.f64(&["crypto", "totals", k]);
    let price = |a: &str| b.f64(&["crypto", "price", a]);
    let held_hkd = b.f64(&["net_worth", "crypto_hkd"]);
    let wallets: Vec<&Value> = b.arr(&["net_worth", "by_source"]).iter()
        .filter(|s| s.get("kind").and_then(Value::as_str) == Some("crypto"))
        .collect();
    if held_hkd.is_none() && wallets.is_empty() && tot("value").is_none() {
        return v_flex().child(ui::empty("No crypto data yet — sync first", cx));
    }
    let g = |s: &Value, k: &str| s.get(k).and_then(Value::as_f64).unwrap_or(0.);
    let gs = |s: &Value, k: &str| s.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let held_hkd = held_hkd.unwrap_or_else(|| wallets.iter().map(|s| g(s, "hkd_value")).sum());

    // HKD per USD: the server's current-month rate (bundle `fx`). Older
    // bundles lack it, so fall back to reading it off the BTC holdings
    // (hkd value ÷ coins × BTC price).
    let fx = b.f64(&["fx", "USD"]).filter(|r| *r > 0.).or_else(|| price("BTC").and_then(|p| {
        let (h, n) = wallets.iter().filter(|s| gs(s, "currency") == "BTC")
            .fold((0., 0.), |(h, n), s| (h + g(s, "hkd_value"), n + g(s, "native_amount")));
        (n > 0. && p > 0.).then(|| h / (n * p))
    }));

    // ---- headline tiles ------------------------------------------------
    let rows = b.arr(&["crypto", "rows"]);
    let last = rows.last();
    // BTC average cost of the recurring buys (every buy, spot buys included):
    // the server's per-asset block; older bundles: the last monthly row.
    let btc_avg = b.f64(&["crypto", "assets", "BTC", "avg"]).filter(|a| *a > 0.).or_else(|| last.and_then(|r| {
        let (inv, coins) = (r.get("BTC_invested")?.as_f64()?, r.get("BTC_coins")?.as_f64()?);
        (coins > 0.).then(|| inv / coins)
    }));
    let pf = tot("profit").unwrap_or(0.);
    let pf_all = tot("profit_allin").unwrap_or(0.);

    let cards = h_flex()
        .gap_4()
        .flex_wrap()
        .items_start()
        .child(tile("Holdings", fmt::hkd(held_hkd),
            format!("{}{} wallets and exchanges",
                fx.map(|r| format!("≈ {} · ", usd(held_hkd / r))).unwrap_or_default(), wallets.len()),
            None, cx))
        .child(tile("Gain · recurring buys", signed_usd(pf),
            format!("{} on {} bought (DCA only)",
                fmt::pct(tot("roi").unwrap_or(0.)), usd(tot("invested").unwrap_or(0.))),
            Some(fmt::signed_color(pf)), cx))
        .child(tile("Gain · all coins", signed_usd(pf_all),
            format!("{} on {} · incl. coins deposited from elsewhere",
                fmt::pct(tot("roi_allin").unwrap_or(0.)), usd(tot("invested_allin").unwrap_or(0.))),
            Some(fmt::signed_color(pf_all)), cx))
        .child(tile("BTC avg cost", btc_avg.map(usd).unwrap_or("—".into()),
            match (btc_avg, price("BTC")) {
                (Some(a), Some(p)) if a > 0. => format!("now {} · {} vs avg", usd(p), fmt::pct((p / a - 1.) * 100.)),
                _ => "recurring buys".into(),
            },
            None, cx));

    // ---- prices strip ---------------------------------------------------
    let stale = b.at(&["crypto", "prices_stale"]).as_bool().unwrap_or(false);
    let as_of = b.str(&["crypto", "prices_as_of"]);
    let prices = h_flex()
        .gap_4()
        .flex_wrap()
        .items_center()
        .child(div().text_xs().text_color(t.muted_foreground).child("PRICES"))
        .children(COINS.iter().filter_map(|a| {
            let p = price(a)?;
            Some(h_flex().gap_1()
                .child(div().text_sm().font_semibold().child(a.to_string()))
                .child(div().text_sm().child(usd(p))))
        }))
        .child(if stale {
            chip(format!("stale{}", as_of.map(|d| format!(" · as of {d}")).unwrap_or_default()), t.warning)
        } else {
            chip(format!("live at last sync{}", as_of.map(|d| format!(" · {d}")).unwrap_or_default()), t.muted_foreground)
        });

    // ---- where it sits ----------------------------------------------------
    let mut ws: Vec<&Value> = wallets.clone();
    ws.sort_by(|a, b| g(b, "hkd_value").total_cmp(&g(a, "hkd_value")));
    let bucket = |pred: &dyn Fn(&str) -> bool| -> f64 {
        wallets.iter().filter(|s| pred(&gs(s, "currency"))).map(|s| g(s, "hkd_value")).sum()
    };
    let coins_hkd = bucket(&|c| COINS.contains(&c));
    let stable_hkd = bucket(&|c| STABLE.contains(&c));
    let other_hkd = bucket(&|c| !COINS.contains(&c) && !STABLE.contains(&c));
    let sum_w: f64 = wallets.iter().map(|s| g(s, "hkd_value")).sum();
    let share = |v: f64| if sum_w > 0. { v / sum_w * 100. } else { 0. };
    let wmax = ws.first().map(|s| g(s, "hkd_value")).unwrap_or(0.);

    let num = |w: f32| div().w(px(w)).flex_shrink_0().text_sm().text_right().whitespace_nowrap();
    let wallet_rows = v_flex()
        .children(ws.iter().map(|s| {
            let key = gs(s, "key");
            let cold = key.starts_with("trezor");
            let v = g(s, "hkd_value");
            h_flex()
                .gap_3()
                .py_1()
                .child(h_flex().w(px(if crate::logo::enabled(cx) { 176. } else { 150. })).flex_shrink_0().gap_2().items_center()
                    .when_some(crate::logo::tile(crate::logo::for_symbol(gs(s, "label").split_whitespace().last().unwrap_or("")), 20., cx),
                        |d, l| d.child(l))
                    .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(gs(s, "label")))
                    .when(cold, |d| d.child(chip("cold".into(), t.chart_2))))
                .child(div().flex_1().min_w(px(40.)).h(px(6.)).rounded_full().bg(t.muted).child(
                    div().h_full().rounded_full().bg(t.chart_1)
                        .w(relative(if wmax > 0. { (v / wmax).clamp(0., 1.) as f32 } else { 0. }))))
                .child(num(130.).text_color(t.muted_foreground)
                    .child(format!("{} {}", qty(g(s, "native_amount")), gs(s, "currency"))))
                .child(num(100.).child(fmt::hkd(v)))
                .child(num(44.).text_color(t.muted_foreground).child(format!("{:.0}%", share(v))))
        }))
        .child(div().pt_2().text_xs().text_color(t.muted_foreground).child(format!(
            "Coins (BTC, ETH, BNB) {} · stablecoins and cash parked {} · other coins {}",
            fmt::compact(coins_hkd), fmt::compact(stable_hkd), fmt::compact(other_hkd))));

    // ---- value vs money in, month-end -----------------------------------
    let pts: Vec<Month> = rows.iter().rev().take(36).rev()
        .filter_map(|r| Some(Month {
            label: fmt::month_label(r.get("month")?.as_str()?).into(),
            value: r.get("total_value")?.as_f64()?,
            invested: r.get("total_invested")?.as_f64()?,
        }))
        .collect();
    let ymax = pts.iter().map(|p| p.value.max(p.invested)).fold(0., f64::max) * 1.05;
    let invested_line = LineChart::new(pts.clone()).appear(false)
        .id("crypto-invested")
        .x(|p: &Month| p.label.clone())
        .y(|p: &Month| p.invested)
        .stroke(t.muted_foreground)
        .linear()
        .y_domain(0., ymax.max(1.))
        .y_axis(true)
        .tick_margin(6)
        .y_tick_format(fmt::compact)
        .interactive(false);
    let value_line = LineChart::new(pts).appear(false)
        .id("crypto-value")
        .x(|p: &Month| p.label.clone())
        .y(|p: &Month| p.value)
        .stroke(t.primary)
        .natural()
        .y_domain(0., ymax.max(1.))
        .y_axis(true)
        .tick_margin(6)
        .y_tick_format(fmt::compact)
        .grid(false)
        .tooltip_value(|p, _| format!("{} · cost {}", usd(p.value), usd(p.invested)).into());
    let chart = ui::card(cx)
        .h(px(280.))
        .child(h_flex().gap_3().flex_wrap()
            .child(div().text_sm().font_semibold().child("Value vs money in"))
            .child(div().text_xs().text_color(t.muted_foreground).child("BTC · ETH · BNB, month-end, US$"))
            .child(legend(t.primary, "value", cx))
            .child(legend(t.muted_foreground, "cost of recurring buys", cx)))
        .child(div().flex_1().min_h_0().relative()
            .child(div().absolute().top_0().left_0().size_full().child(invested_line))
            .child(div().absolute().top_0().left_0().size_full().child(value_line)));

    // ---- recent moves: one pass over the newest entries ---------------------
    let cutoff = (Local::now().date_naive() - Duration::days(30)).format("%Y-%m-%d").to_string();
    let (mut buys_30, mut spent_30) = (0usize, 0.);
    let mut recent: Vec<(&finance_core::Entry, &str)> = Vec::with_capacity(RECENT);
    for e in ledger.entries.iter().filter(|e| e.kind == Kind::Crypto) {
        let in_window = e.date >= cutoff;
        if recent.len() >= RECENT && !in_window {
            break;
        }
        let Some(v) = verb(&e.category) else { continue };
        if in_window && v == "Buy" {
            buys_30 += 1;
            spent_30 += e.hkd.unwrap_or(0.).abs();
        }
        if recent.len() < RECENT {
            recent.push((e, v));
        }
    }
    let recent_rows = v_flex().children(recent.iter().map(|(e, v)| {
        let tone = match *v {
            "Buy" | "Deposit" | "Transfer in" | "Cash in" => fmt::money_in(),
            "Sell" | "Withdraw" | "Cash out" => fmt::money_out(),
            _ => t.chart_2,
        };
        let hkd = e.hkd.filter(|h| h.abs() >= 0.01);
        h_flex()
            .gap_3()
            .py_1()
            .border_b_1()
            .border_color(t.border)
            .child(div().w(px(92.)).flex_shrink_0().text_sm().text_color(t.muted_foreground).child(e.date.clone()))
            .child(h_flex().w(px(96.)).flex_shrink_0().child(chip(v.to_string(), tone)))
            .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap()
                .child(format!("{} {}", qty(e.amount), e.currency)))
            .child(div().w(px(80.)).flex_shrink_0().text_xs().text_color(t.muted_foreground).whitespace_nowrap()
                .child(e.source.clone()))
            .child(num(100.).child(hkd.map(|h| fmt::hkd(h.abs())).unwrap_or("—".into())))
    }));
    let recent_caption = if buys_30 > 0 {
        format!("last 30 days: {buys_30} buys, {} · rewards and reconcile noise hidden", fmt::hkd(spent_30))
    } else {
        "no buys in the last 30 days · rewards and reconcile noise hidden".into()
    };

    v_flex()
        .gap_4()
        .child(cards)
        .child(ui::card(cx).child(prices))
        .child(chart)
        .child(
            h_flex()
                .gap_4()
                .items_start()
                .flex_wrap()
                .child(v_flex().flex_1().min_w(crate::layout::col(460.)).gap_2()
                    .child(ui::section("Where it sits", "per wallet, valued at the prices above", cx))
                    .child(ui::card(cx).child(wallet_rows)))
                .child(v_flex().flex_1().min_w(crate::layout::col(460.)).gap_2()
                    .child(ui::section("Recent moves", &recent_caption, cx))
                    .child(ui::card(cx).child(recent_rows))),
        )
}
