//! Investments: the accounts, every holding in one sortable table, where
//! the gain came from, and the week's and month's movers.

use finance_core::portfolio::{Account, Holding};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{account_color, card_title, head, pct_opt, prefs, set_prefs, tone_opt, words, Model, OnGo, Sort};
use crate::fmt;
use crate::layout;
use crate::pages::Page;
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

/// The holding's mark: its own logo, its issuer's, or its asset-class glyph
/// (the Revolut pot, whose contents aren't in the data, gets Revolut's).
fn mark(m: &Model, h: &Holding) -> crate::logo::Mark {
    match h.account {
        Some(Account::FlexInvest) => crate::logo::for_source("hsbc"),
        Some(Account::Revolut) => crate::logo::for_source("revolut"),
        _ => crate::logo::for_class(&h.symbol, &m.class_of(h)),
    }
}

fn logo(m: &Model, h: &Holding, size: f32, cx: &App) -> Option<AnyElement> {
    crate::logo::tile(mark(m, h), size, cx)
}

/// "IBKR · Vanguard · Equity ETF"
fn subtitle(m: &Model, h: &Holding) -> String {
    let c = m.class_of(h);
    let mut parts: Vec<String> = h.account.map(|a| a.label().to_string()).into_iter().collect();
    if let Some(i) = c.issuer.filter(|_| h.account != Some(Account::FlexInvest)) {
        parts.push(i.label().into());
    }
    if c.class != finance_core::exposure::class::AssetClass::Unclassified {
        parts.push(c.describe());
    }
    parts.join(" · ")
}

/// A short name: the ticker for broker and coin holdings.
fn short(h: &Holding) -> String {
    match h.account {
        Some(Account::Ibkr) | Some(Account::Crypto) => h.symbol.clone(),
        _ => h.name.clone(),
    }
}

// ---------------------------------------------------------------- accounts

pub fn accounts(m: &Model, on_go: OnGo, cx: &App) -> Div {
    let t = cx.theme();
    let total = m.p.value();
    let share = |v: f64| if total > 0. { v / total * 100. } else { 0. };
    let mut by_share: Vec<_> = m.p.accounts.iter().filter(|a| a.value > 0.).collect();
    by_share.sort_by(|a, b| b.value.total_cmp(&a.value));
    let best = m.p.accounts.iter().filter(|a| a.gain().abs() > 1.).max_by(|a, b| a.gain().total_cmp(&b.gain()));
    let line = format!("{}{}",
        by_share.iter().map(|a| format!("{} {:.0}%", a.account.label(), share(a.value))).collect::<Vec<_>>().join(", "),
        best.map(|a| format!(". {} has the biggest gain: {} ({}).", a.account.label(), fmt::signed_hkd(a.gain()), pct_opt(a.gain_pct()))).unwrap_or(".".into()));
    let cards = m.p.accounts.iter().filter(|a| a.value > 0. || a.invested > 0.).map(|a| {
        let page = match a.account {
            Account::Ibkr => Some(Page::Broker),
            Account::Crypto => Some(Page::Crypto),
            _ => None,
        };
        let color = account_color(a.account, cx);
        let acct_mark = match a.account {
            Account::FlexInvest => crate::logo::for_source("hsbc"),
            Account::Ibkr => crate::logo::Mark::Icon(gpui_kit::assets::IconName::Building2),
            Account::Crypto => crate::logo::for_symbol("BTC"),
            Account::Revolut => crate::logo::for_source("revolut"),
        };
        // what the account holds, biggest first, as small marks
        let mut inside: Vec<&Holding> = m.p.holdings.iter().filter(|h| h.account == Some(a.account) && h.value_hkd > 0.).collect();
        inside.sort_by(|x, y| y.value_hkd.total_cmp(&x.value_hkd));
        let marks: Vec<AnyElement> = if inside.len() >= 2 {
            inside.iter().take(6).filter_map(|h| crate::logo::tile(mark(m, h), 16., cx)).collect()
        } else {
            Vec::new()
        };
        let more = inside.len().saturating_sub(6);
        let body = v_flex().gap(px(6.))
            .child(h_flex().gap(px(8.)).items_center()
                .when_some(crate::logo::tile(acct_mark, 20., cx), |d, l| d.child(l))
                .child(div().flex_1().min_w_0().text_size(px(ts::TITLE)).font_semibold().whitespace_nowrap().overflow_hidden().text_ellipsis()
                    .child(a.account.label()))
                .child(ui::caption(format!("{:.0}%", share(a.value)), cx))
                .when(page.is_some(), |d| d.child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child("→"))))
            .child(div().text_size(px(ts::FIGURE)).line_height(px(28.)).font_semibold().figures().whitespace_nowrap().child(fmt::hkd(a.value)))
            .when(!marks.is_empty(), |d| d.child(h_flex().gap(px(3.)).items_center().children(marks)
                .when(more > 0, |d| d.child(ui::caption(format!("+{more}"), cx)))))
            .child(h_flex().gap(px(6.)).flex_wrap().text_size(px(ts::CAPTION)).figures()
                .child(div().text_color(fmt::signed_color(a.gain())).child(fmt::signed_hkd(a.gain())))
                .child(div().text_color(tone_opt(a.gain_pct(), cx)).child(pct_opt(a.gain_pct()))))
            // its own line: at half-screen a "· this year" tail wrapped and
            // started a line with the separator
            .children(match (a.account, a.gain_ytd) {
                (Account::Revolut, _) => Some("held at cost".to_string()),
                (_, Some(y)) => Some(format!("this year {}", fmt::signed_hkd(y))),
                (_, None) => None,
            }.map(|s| ui::caption(s, cx).figures()))
            .when(a.spark.iter().filter(|v| **v > 0.).count() >= 2, |d| {
                // from the account's first month with money in it
                let from = a.spark.iter().position(|v| *v > 0.).unwrap_or(0);
                d.child(div().mt_1().h(px(30.)).child(ui::sparkline(SharedString::from(format!("acct-spark-{}", a.account.label())), &a.spark[from..], color)))
            });
        let id = SharedString::from(format!("acct-{}", a.account.label()));
        match page {
            Some(p) => {
                let on_go = on_go.clone();
                ui::card_interactive(id, cx).flex_1().min_w(layout::col(200.)).child(body)
                    .on_click(move |_, w, cx| on_go(p, w, cx)).into_any_element()
            }
            None => ui::card(cx).flex_1().min_w(layout::col(200.)).child(body).into_any_element(),
        }
    });
    v_flex().gap(px(sp::GRID))
        .child(head("Accounts", "where it's held",
            &format!("Each account's value and gain are the server's (the same numbers as the Overview). This year = the market result since 1 January, money put in during the year excluded.{}{}",
                if m.has(Account::Ibkr) { " IBKR's comes from the broker's daily NAV, so it includes its cash." } else { "" },
                if m.has(Account::FlexInvest) { " FlexInvest has no page of its own; it is the fund row in Holdings." } else { "" }),
            line, cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch().children(cards))
}

// ---------------------------------------------------------------- holdings

fn sorted(m: &Model) -> Vec<&Holding> {
    let p = prefs();
    let mut v: Vec<&Holding> = m.p.holdings.iter().collect();
    let key = |h: &Holding| -> f64 {
        match p.sort {
            Sort::Value | Sort::Name => h.value_hkd,
            Sort::Gain => h.gain_hkd(),
            Sort::M1 => h.ret_1m.unwrap_or(f64::NEG_INFINITY),
            Sort::Ytd => h.ret_ytd.unwrap_or(f64::NEG_INFINITY),
        }
    };
    if p.sort == Sort::Name {
        v.sort_by_key(|h| short(h).to_lowercase());
    } else {
        v.sort_by(|a, b| key(a).total_cmp(&key(b)));
    }
    let desc = if p.sort == Sort::Name { !p.desc } else { p.desc };
    if desc {
        v.reverse();
    }
    v
}

pub fn table(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let total = m.p.value();
    let weight = |h: &Holding| if total > 0. { h.value_hkd / total * 100. } else { 0. };
    let mut by_value: Vec<&Holding> = m.p.holdings.iter().collect();
    by_value.sort_by(|a, b| b.value_hkd.total_cmp(&a.value_hkd));
    let top3: f64 = by_value.iter().take(3).map(|h| weight(h)).sum();
    let (up, down) = {
        let mut r: Vec<&Holding> = m.p.holdings.iter().filter(|h| h.ret_1m.is_some()).collect();
        r.sort_by(|a, b| a.ret_1m.unwrap().total_cmp(&b.ret_1m.unwrap()));
        (r.last().copied(), r.first().copied())
    };
    let line = format!("{} holdings. The biggest is {} at {:.0}% of the total, and the top three make {:.0}%{}",
        m.p.holdings.len(), by_value.first().map(|h| short(h)).unwrap_or_default(), by_value.first().map(|h| weight(h)).unwrap_or(0.), top3,
        match (up, down) {
            (Some(u), Some(d)) if u.symbol != d.symbol => format!(". Over the past month {} did best ({}) and {} worst ({}).", short(u), pct_opt(u.ret_1m), short(d), pct_opt(d.ret_1m)),
            _ => ".".into(),
        });

    let p = prefs();
    let head_cell = |label: &'static str, sort: Option<Sort>, w: Option<f32>| {
        let on = sort == Some(p.sort);
        let arrow = if on { if p.desc ^ (p.sort == Sort::Name) { " ↓" } else { " ↑" } } else { "" };
        let d = div().id(SharedString::from(format!("inv-sort-{label}"))).whitespace_nowrap()
            .text_size(px(ts::EYEBROW)).font_weight(FontWeight::MEDIUM)
            .text_color(if on { t.foreground } else { t.muted_foreground })
            .child(format!("{}{arrow}", label.to_uppercase()));
        let d = match w {
            Some(w) => d.w(px(w)).flex_shrink_0().text_right(),
            None => d.flex_1().min_w(px(110.)),
        };
        match sort {
            Some(s) => d.cursor_pointer().hover(|st| st.text_color(t.foreground)).on_click(move |_, window, _| {
                set_prefs(|p| if p.sort == s { p.desc = !p.desc } else { p.sort = s; p.desc = true });
                window.refresh();
            }),
            None => d,
        }
    };
    let rows = sorted(m);
    let compact = layout::compact();
    let list: Div = if compact {
        // phone: two lines per holding
        v_flex().children(rows.iter().enumerate().map(|(i, h)| {
            v_flex().gap(px(2.)).py(px(8.)).text_size(px(ts::BODY))
                .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
                .child(h_flex().gap(px(8.)).items_center()
                    .when_some(logo(m, h, 20., cx), |d, l| d.child(l))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(short(h)))
                    .child(div().figures().font_weight(FontWeight::MEDIUM).child(fmt::compact(h.value_hkd))))
                .child(h_flex().gap(px(10.)).pl(px(28.)).text_size(px(ts::CAPTION)).figures()
                    .child(div().text_color(t.muted_foreground).child(format!("{:.1}%", weight(h))))
                    .child(div().text_color(fmt::signed_color(h.gain_hkd())).child(format!("{} {}", fmt::compact(h.gain_hkd()), pct_opt(h.gain_pct()))))
                    .child(div().flex_1())
                    .child(div().text_color(tone_opt(h.ret_1m, cx)).child(format!("1M {}", pct_opt(h.ret_1m)))))
        }))
    } else {
        let header = h_flex().gap(px(10.)).pb(px(6.)).border_b_1().border_color(ui::hairline(cx))
            .child(head_cell("Holding", Some(Sort::Name), None))
            .child(head_cell("Value", Some(Sort::Value), Some(84.)))
            .child(head_cell("Weight", None, Some(52.)))
            .child(head_cell("Gain", Some(Sort::Gain), Some(110.)))
            .child(head_cell("1M", Some(Sort::M1), Some(58.)))
            .child(head_cell("YTD", Some(Sort::Ytd), Some(58.)))
            .child(head_cell("3 months", None, Some(72.)));
        let body = v_flex().children(rows.iter().enumerate().map(|(i, h)| {
            let color = h.account.map(|a| account_color(a, cx)).unwrap_or(t.primary);
            h_flex().gap(px(10.)).py(px(7.)).items_center().text_size(px(ts::BODY)).figures()
                .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
                .child(h_flex().flex_1().min_w(px(110.)).gap(px(8.)).items_center()
                    .when_some(logo(m, h, 20., cx), |d, l| d.child(l))
                    .child(v_flex().min_w_0()
                        .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(short(h)))
                        .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap()
                            .child(format!("{}{}", subtitle(m, h),
                                h.proxy.as_deref().map(|p| if p == "held at cost" { format!(" · {p}") } else { format!(" · returns via {p}") }).unwrap_or_default())))))
                .child(div().w(px(84.)).flex_shrink_0().text_right().child(fmt::compact(h.value_hkd)))
                .child(div().w(px(52.)).flex_shrink_0().text_right().text_color(t.muted_foreground).child(format!("{:.1}%", weight(h))))
                .child(div().w(px(110.)).flex_shrink_0().text_right().text_color(fmt::signed_color(h.gain_hkd()))
                    .child(format!("{} {}", fmt::compact(h.gain_hkd()), pct_opt(h.gain_pct()))))
                .child(div().w(px(58.)).flex_shrink_0().text_right().text_color(tone_opt(h.ret_1m, cx)).child(pct_opt(h.ret_1m)))
                .child(div().w(px(58.)).flex_shrink_0().text_right().text_color(tone_opt(h.ret_ytd, cx)).child(pct_opt(h.ret_ytd)))
                .child(div().w(px(72.)).flex_shrink_0().h(px(22.)).when(h.spark.len() >= 2, |d| {
                    d.child(ui::sparkline(SharedString::from(format!("hold-spark-{}", h.symbol)), &h.spark, color))
                }))
        }));
        // min width = the columns plus their gaps: narrower scrolls instead of
        // clipping the last column (it cut "3 months" at half-screen)
        div().child(div().id("inv-holdings").w_full().overflow_x_scroll().restrict_scroll_to_axis().child(v_flex().min_w(px(604.)).child(header).child(body)))
    };
    v_flex().gap(px(sp::GRID))
        .child(head("Holdings", "every fund, ETF, stock and coin",
            &format!("Value at today's prices ({}). Gain = value minus what went in, each buy at its own exchange rate. 1M and YTD are price returns of each holding's own listing{}.{} Click a column to sort.",
                words(&m.mine(&[(Account::Ibkr, "the broker's marks"), (Account::Crypto, "the coins' last price"), (Account::FlexInvest, "FlexInvest units × NAV")]), "and"),
                if m.has(Account::FlexInvest) { ": the FlexInvest fund through MSCI World (URTH), its proxy" } else { "" },
                if m.has(Account::Crypto) { " The coins are split to add up to the server's crypto total." } else { "" }),
            line, cx))
        .child(ui::card(cx).child(list))
}

// ------------------------------------------------------------- attribution

/// A row of a diverging bar: positive right, negative left of the middle.
fn diverging(label: String, v: f64, max: f64, color: Hsla, cx: &App) -> Div {
    let t = cx.theme();
    let frac = if max > 0. { (v.abs() / max).clamp(0., 1.) as f32 } else { 0. };
    let bar = |on: bool, right: bool| div().flex_1().h(px(8.)).flex().when(!right, |d| d.justify_end())
        .child(div().h_full().rounded(px(2.)).when(on, |d| d.w(relative(frac)).bg(color)));
    h_flex().gap(px(8.)).py(px(4.)).items_center().text_size(px(ts::BODY))
        .child(div().w(px(124.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
        .child(h_flex().flex_1().min_w(px(60.)).items_center()
            .child(bar(v < 0., false))
            .child(div().w(px(1.)).h(px(14.)).bg(t.border))
            .child(bar(v >= 0., true)))
        .child(div().w(px(100.)).flex_shrink_0().text_right().whitespace_nowrap().figures().text_color(fmt::signed_color(v)).child(fmt::signed_hkd(v)))
}

pub fn attribution(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let since: Vec<(String, f64, Hsla)> = {
        let mut v: Vec<(String, f64, Hsla)> = m.p.holdings.iter().filter(|h| h.gain_hkd().abs() >= 1.)
            .map(|h| (short(h), h.gain_hkd(), h.account.map(|a| account_color(a, cx)).unwrap_or(t.primary))).collect();
        // the broker's account gain beyond its holdings: cash, fees, currency on cash
        if let Some(a) = m.p.accounts.iter().find(|a| a.account == Account::Ibkr) {
            let held: f64 = m.p.holdings.iter().filter(|h| h.account == Some(Account::Ibkr)).map(|h| h.gain_hkd()).sum();
            let rest = a.gain() - held;
            if rest.abs() >= 1. {
                v.push(("IBKR cash, fees".into(), rest, t.muted_foreground));
            }
        }
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v
    };
    let ytd: Vec<(String, f64, Hsla)> = {
        let mut v: Vec<(String, f64, Hsla)> = m.p.holdings.iter()
            .filter_map(|h| Some((short(h), h.gain_ytd_hkd?, h.account.map(|a| account_color(a, cx)).unwrap_or(t.primary))))
            .filter(|x| x.1.abs() >= 1.).collect();
        if let Some(a) = m.p.accounts.iter().find(|a| a.account == Account::Ibkr) {
            let held: f64 = m.p.holdings.iter().filter(|h| h.account == Some(Account::Ibkr)).filter_map(|h| h.gain_ytd_hkd).sum();
            if let Some(rest) = a.gain_ytd.map(|g| g - held).filter(|r| r.abs() >= 1.) {
                v.push(("IBKR cash, fees".into(), rest, t.muted_foreground));
            }
        }
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v
    };
    let missing: Vec<String> = m.p.holdings.iter().filter(|h| h.gain_ytd_hkd.is_none()).map(short).collect();
    let total_since: f64 = since.iter().map(|x| x.1).sum();
    let total_ytd: f64 = ytd.iter().map(|x| x.1).sum();
    let line = match (since.first(), ytd.first()) {
        (Some(s), Some(y)) => format!("Since the start, {} made the most: {} of the {} gain. This year the biggest contributor is {} ({}), out of {} in total.",
            s.0, fmt::signed_hkd(s.1), fmt::signed_hkd(total_since), y.0, fmt::signed_hkd(y.1), fmt::signed_hkd(total_ytd)),
        _ => "Not enough history to split the gain yet.".into(),
    };
    let block = |title: &str, rows: &[(String, f64, Hsla)], total: f64, note: Option<String>| {
        let max = rows.iter().map(|r| r.1.abs()).fold(0., f64::max);
        ui::card(cx).flex_1().min_w(layout::col(320.)).gap(px(sp::S))
            .child(card_title(title, format!("total {}", fmt::signed_hkd(total)), cx))
            .child(v_flex().children(rows.iter().map(|r| diverging(r.0.clone(), r.1, max, r.2, cx))))
            .when_some(note, |d, n| d.child(crate::pages::deep::note(n, cx)))
    };
    v_flex().gap(px(sp::GRID))
        .child(head("Where the gain came from", "by holding, since the start and this year",
            "Since the start: each holding's value minus what went into it. This year: the market result since 1 January (units held × each day's or week's price move), so money put in during the year never counts as gain. The broker's account result also includes its cash, commissions and currency moves on cash, shown as their own row.",
            line, cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start()
            .child(block("Since the start", &since, total_since, None))
            .child(block("This year", &ytd, total_ytd,
                (!missing.is_empty()).then(|| format!("Not measurable this year (no price history in the replica): {}.", missing.join(", "))))))
}

// ------------------------------------------------------------------ movers

pub fn movers(m: &Model, cx: &App) -> Div {
    let ranked = |f: fn(&Holding) -> Option<f64>| -> Vec<(&Holding, f64, f64)> {
        let mut v: Vec<(&Holding, f64, f64)> = m.p.holdings.iter().filter_map(|h| f(h).map(|r| (h, r, h.moved_hkd(r)))).collect();
        v.sort_by(|a, b| b.2.abs().total_cmp(&a.2.abs()));
        v.truncate(4);
        v
    };
    let week = ranked(|h| h.ret_1w);
    let month = ranked(|h| h.ret_1m);
    let net = |v: &[(&Holding, f64, f64)]| v.iter().map(|x| x.2).sum::<f64>();
    let phrase = |v: &[(&Holding, f64, f64)], when: &str| v.first()
        .map(|(h, r, x)| format!("{when} the biggest move was {} ({}, {})", short(h), fmt::pct(*r), fmt::signed_hkd(*x)))
        .unwrap_or_default();
    let line = [phrase(&week, "This week"), phrase(&month, "over the past month")]
        .into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("; ");
    let list = |title: &str, v: &[(&Holding, f64, f64)]| {
        ui::card(cx).flex_1().min_w(layout::col(260.)).gap(px(sp::S))
            .child(card_title(title, format!("these: {}", fmt::signed_hkd(net(v))), cx))
            .child(v_flex().children(v.iter().enumerate().map(|(i, (h, r, x))| {
                h_flex().gap(px(8.)).py(px(6.)).items_center().text_size(px(ts::BODY)).figures()
                    .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
                    .when_some(logo(m, h, 18., cx), |d, l| d.child(l))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(short(h)))
                    .child(div().w(px(60.)).flex_shrink_0().text_right().text_color(fmt::signed_color(*r)).child(fmt::pct(*r)))
                    .child(div().w(px(80.)).flex_shrink_0().text_right().text_color(fmt::signed_color(*x)).child(fmt::signed_hkd(*x)))
            })))
            .when(v.is_empty(), |d| d.child(ui::caption("No prices for this period.", cx)))
    };
    v_flex().gap(px(sp::GRID))
        .child(head("Top movers", &fmt::local("the biggest moves in HK$, this week and this month"),
            &fmt::local(&format!("Each holding's price return over the last 7 and 30 days, and what that move is in HK$ at today's value. Ranked by the size of the HK$ move, up or down. Holdings without daily or weekly prices{} are left out.",
                if m.has(Account::Revolut) { " (such as the Revolut pot)" } else { "" })),
            if line.is_empty() { "No price moves to show.".to_string() } else { format!("{line}.") }, cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start()
            .child(list("This week", &week))
            .child(list("This month", &month)))
}

