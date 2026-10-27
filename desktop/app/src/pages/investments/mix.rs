//! Investments › Allocation › Asset mix: equity, bonds (by kind), cash,
//! crypto and commodities across every account, the bond/stock ratio said
//! out loud, and an optional target mix with the drift from it.
//!
//! The classes come from `finance_core::exposure::class` (a static table by
//! symbol, then the description's words) and are computed once per data
//! load in the [`Model`]. Cash = IBKR's uninvested cash and stablecoins, plus
//! the bank balances when "include bank cash" is on. The target lives in
//! `invest.toml` under `[mix]`. Facts, not advice: the drift is arithmetic
//! against a number the owner chose.

use std::rc::Rc;

use finance_core::exposure::class::{self, AssetClass, BondKind, Mix};
use finance_core::portfolio::Account;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{card_title, head, mix_target, prefs, set_mix_target, set_prefs, stepper, words, Model};
use crate::fmt;
use crate::layout;
use crate::pages::deep::note;
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

fn class_color(c: AssetClass, cx: &App) -> Hsla {
    let t = cx.theme();
    match c {
        AssetClass::Equity => t.chart_1,
        AssetClass::Bond => t.chart_2,
        AssetClass::Cash => t.chart_4,
        AssetClass::Crypto => t.chart_3,
        AssetClass::Commodity => t.chart_5,
        AssetClass::Unclassified => t.muted_foreground.opacity(0.5),
    }
}

/// "100 : 0"
fn ratio(e: f64, b: f64) -> String {
    format!("{e:.0} : {b:.0}")
}

/// The names in a class, biggest first: "VWRA, EQQQ, IBKR cash".
fn members(m: &Model, c: AssetClass, bank: f64) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = m.p.holdings.iter().zip(&m.classes)
        .filter(|(h, k)| k.class == c && h.value_hkd > 0.)
        .map(|(h, _)| (match h.account {
            Some(finance_core::portfolio::Account::Ibkr) | Some(finance_core::portfolio::Account::Crypto) => h.symbol.clone(),
            _ => h.name.clone(),
        }, h.value_hkd))
        .collect();
    if c == AssetClass::Cash {
        if m.broker_cash > 0. {
            v.push(("IBKR cash".into(), m.broker_cash));
        }
        if bank > 0. {
            v.push(("bank cash".into(), bank));
        }
    }
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

pub fn render(m: &Model, cx: &App) -> Div {
    let t = cx.theme();
    let p = prefs();
    let bank = if p.bank_cash { m.bank_cash } else { 0. };
    let mix: Mix = if bank > 0. { m.mix.plus(AssetClass::Cash, bank) } else { m.mix.clone() };
    let target = mix_target();
    let drift = class::drift(&mix, &target);
    let shown: Vec<AssetClass> = AssetClass::ALL.into_iter().filter(|c| mix.hkd(*c) > 0.5 || target.get(c.key()).is_some_and(|w| *w > 0.)).collect();
    let bonds = mix.hkd(AssetClass::Bond);
    let sb = mix.stock_bond();

    // ---------- the takeaway ----------
    let mut by_size: Vec<AssetClass> = shown.iter().copied().filter(|c| mix.hkd(*c) > 0.5).collect();
    by_size.sort_by(|a, b| mix.hkd(*b).total_cmp(&mix.hkd(*a)));
    let parts = by_size.iter().map(|c| format!("{} {:.0}%", c.label().to_lowercase(), mix.pct(*c))).collect::<Vec<_>>().join(", ");
    let ratio_line = match sb {
        Some((e, b)) if b > 0. => {
            let kinds = BondKind::ALL.iter().filter(|k| mix.by_bond[**k as usize] > 0.5)
                .map(|k| format!("{} {}", k.label().to_lowercase(), fmt::compact(mix.by_bond[*k as usize]))).collect::<Vec<_>>().join(", ");
            format!("Stock : bond is {} ({kinds}).", ratio(e, b))
        }
        Some(_) => "Stock : bond is 100 : 0, with no bond or T-bill holdings.".to_string(),
        None => "Nothing held in equity or bonds.".to_string(),
    };
    let drift_line = drift.iter().filter(|d| d.0 != AssetClass::Unclassified).max_by(|a, b| a.3.abs().total_cmp(&b.3.abs()))
        .filter(|d| d.3.abs() >= 0.5)
        .map(|d| format!(" Against the target, the widest gap is {}: {} points ({} {}).", d.0.label().to_lowercase(), fmt::signed(d.3, 1),
            fmt::hkd(d.4.abs()), if d.4 >= 0. { "above" } else { "below" }))
        .unwrap_or_default();
    let line = format!("{}{}. {ratio_line}{drift_line}",
        if parts.is_empty() { "Nothing invested yet".to_string() } else { capitalize(&parts) },
        if bank > 0. { ", bank cash included" } else { "" });

    // ---------- controls ----------
    let bank_toggle = {
        let on = p.bank_cash;
        div().id("mix-bank-cash").flex().items_center().gap(px(6.)).px(px(8.)).py(px(3.)).rounded(px(7.))
            .border_1().border_color(ui::hairline(cx)).cursor_pointer()
            .text_size(px(ts::CAPTION)).text_color(if on { t.foreground } else { t.muted_foreground })
            .hover(|s| s.text_color(t.foreground))
            .on_click(|_, window, _| {
                set_prefs(|p| p.bank_cash = !p.bank_cash);
                window.refresh();
            })
            .child(div().size(px(12.)).rounded(px(3.)).border_1()
                .border_color(if on { t.primary } else { t.muted_foreground })
                .when(on, |d| d.bg(t.primary)))
            .child(format!("include bank cash ({})", fmt::compact(m.bank_cash)))
    };

    // ---------- the ratio and the bar ----------
    let bar = h_flex().w_full().h(px(14.)).rounded(px(4.)).overflow_hidden().bg(t.muted)
        .children(shown.iter().filter(|c| mix.pct(**c) > 0.).map(|c| div().h_full().w(relative((mix.pct(*c) / 100.) as f32)).bg(class_color(*c, cx))));
    let legend = h_flex().gap(px(sp::M)).flex_wrap()
        .children(shown.iter().filter(|c| mix.hkd(**c) > 0.5).map(|c| ui::legend(class_color(*c, cx), c.label(), format!("{:.0}%", mix.pct(*c)), cx)));
    let bond_kinds = v_flex().gap(px(2.)).when(bonds > 0.5, |d| d
        .child(ui::eyebrow("Bonds by kind", cx))
        .children(BondKind::ALL.iter().filter(|k| mix.by_bond[**k as usize] > 0.5).map(|k| {
            let v = mix.by_bond[*k as usize];
            h_flex().gap(px(8.)).text_size(px(ts::BODY)).figures()
                .child(div().flex_1().child(k.label()))
                .child(div().text_color(t.muted_foreground).child(format!("{:.0}% of bonds", v / bonds * 100.)))
                .child(div().w(px(84.)).text_right().child(fmt::compact(v)))
        })));
    let ratio_card = ui::card(cx).flex_1().min_w(layout::col(320.)).gap(px(sp::S))
        .child(h_flex().items_baseline().gap(px(10.)).flex_wrap()
            .child(div().text_size(px(ts::FIGURE)).line_height(px(28.)).font_semibold().figures().whitespace_nowrap()
                .child(sb.map(|(e, b)| ratio(e, b)).unwrap_or("—".into())))
            .child(ui::caption("stock : bond", cx)))
        .child(ui::caption(match sb {
            Some(_) => format!("equity {} · bonds {}", fmt::compact(mix.hkd(AssetClass::Equity)), fmt::compact(bonds)),
            None => String::new(),
        }, cx))
        .child(div().mt(px(4.)).child(bar))
        .child(legend)
        .child(bond_kinds)
        .when(bonds <= 0.5 && sb.is_some(), |d| d.child(note(
            "No bond, gilt or T-bill fund is held, so the ratio reads 100 : 0. Bond funds are recognised by symbol (IB01, BIL and SGOV as T-bills; IGLT as gilts; AGG and AGGU as aggregate) or by a description saying Treasury, T-Bill, Gilt or Bond.", cx)));

    // ---------- the table ----------
    let has_target = !drift.is_empty();
    let weight_sum: f64 = AssetClass::ALL.iter().filter_map(|c| target.get(c.key())).sum();
    let hcell = |label: &str, w: Option<f32>| {
        let d = div().text_size(px(ts::EYEBROW)).font_weight(FontWeight::MEDIUM).text_color(t.muted_foreground).whitespace_nowrap().child(label.to_uppercase());
        match w {
            Some(w) => d.w(px(w)).flex_shrink_0().text_right(),
            None => d.flex_1().min_w(px(140.)),
        }
    };
    let header = h_flex().gap(px(10.)).pb(px(6.)).border_b_1().border_color(ui::hairline(cx))
        .child(hcell("Class", None))
        .child(hcell(&fmt::sym(), Some(76.)))
        .child(hcell("Share", Some(50.)))
        .when(has_target, |d| d.child(hcell("Target", Some(96.))).child(hcell("Drift", Some(110.))));
    let rows = v_flex().children(shown.iter().enumerate().map(|(i, c)| {
        let names = members(m, *c, bank);
        let names_line = {
            let mut s = names.iter().take(5).map(|n| n.0.clone()).collect::<Vec<_>>().join(", ");
            if names.len() > 5 {
                s.push_str(&format!(" +{}", names.len() - 5));
            }
            s
        };
        let d = drift.iter().find(|d| d.0 == *c);
        let key = c.key();
        let weight = target.get(key).copied().unwrap_or(0.);
        let target_cell = if *c == AssetClass::Unclassified {
            div().w(px(96.)).flex_shrink_0().text_right().text_color(t.muted_foreground).child("—").into_any_element()
        } else {
            h_flex().w(px(96.)).flex_shrink_0().justify_end().child(stepper(&format!("mix-{key}"), format!("{weight:.0}"),
                Rc::new(move || set_mix_target(|m| { let v = m.entry(key.to_string()).or_insert(0.); *v = (*v - 5.).max(0.); })),
                Rc::new(move || set_mix_target(|m| { let v = m.entry(key.to_string()).or_insert(0.); *v = (*v + 5.).min(100.); })), cx))
                .into_any_element()
        };
        h_flex().gap(px(10.)).py(px(7.)).items_center().text_size(px(ts::BODY)).figures()
            .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
            .child(h_flex().flex_1().min_w(px(140.)).gap(px(8.)).items_center()
                .child(ui::dot(class_color(*c, cx), 8.))
                .child(v_flex().min_w_0()
                    .child(div().whitespace_nowrap().child(c.label()))
                    .when(!names_line.is_empty(), |d| d.child(ui::caption(names_line, cx)))))
            .child(div().w(px(76.)).flex_shrink_0().text_right().child(fmt::compact(mix.hkd(*c))))
            .child(div().w(px(50.)).flex_shrink_0().text_right().text_color(t.muted_foreground).child(format!("{:.1}%", mix.pct(*c))))
            .when(has_target, |row| row.child(target_cell).child(
                div().w(px(110.)).flex_shrink_0().text_right().whitespace_nowrap().child(match d {
                    Some(d) if d.3.abs() >= 0.05 => v_flex().items_end()
                        .child(div().child(format!("{} pts", fmt::signed(d.3, 1))))
                        .child(ui::caption(format!("{} {}", fmt::compact(d.4.abs()), if d.4 >= 0. { "above" } else { "below" }), cx))
                        .into_any_element(),
                    Some(_) => div().text_color(t.muted_foreground).child("on target").into_any_element(),
                    None => div().text_color(t.muted_foreground).child("—").into_any_element(),
                })))
    }));
    let set_button = |id: &'static str, label: &'static str, f: Rc<dyn Fn()>| {
        div().id(id).px(px(10.)).py(px(4.)).rounded(px(7.)).border_1().border_color(ui::hairline(cx))
            .text_size(px(ts::CAPTION)).cursor_pointer().text_color(t.muted_foreground)
            .hover(|s| s.text_color(t.foreground).bg(t.muted))
            .on_click(move |_, window, _| {
                f();
                window.refresh();
            })
            .child(label)
    };
    let seed: Vec<(&'static str, f64)> = AssetClass::ALL.into_iter().filter(|c| *c != AssetClass::Unclassified)
        .map(|c| (c.key(), (mix.pct(c) / 5.).round() * 5.)).filter(|x| x.1 > 0.).collect();
    let table_card = ui::card(cx).flex_1().min_w(layout::col(420.)).gap(px(sp::S))
        .child(card_title("By class", if has_target { format!("target weights sum to {weight_sum:.0}{}", if (weight_sum - 100.).abs() > 0.5 { ", shown normalized to 100%" } else { "" }) } else { "no target set".into() }, cx))
        .child(div().id("mix-table").w_full().overflow_x_scroll().restrict_scroll_to_axis().child(v_flex().min_w(px(if has_target { 520. } else { 320. })).child(header).child(rows)))
        .child(h_flex().gap_2().pt(px(4.)).flex_wrap()
            .when(!has_target, |d| d.child(set_button("mix-set", "Set a target mix", Rc::new(move || {
                let seed = seed.clone();
                set_mix_target(|m| {
                    m.clear();
                    m.extend(seed.iter().map(|(k, v)| (k.to_string(), *v)));
                });
            }))))
            .when(has_target, |d| d.child(set_button("mix-clear", "Clear the target", Rc::new(|| set_mix_target(|m| m.clear()))))));

    let cash_own = words(&[m.has(Account::Ibkr).then_some("IBKR's uninvested cash"), m.has(Account::Crypto).then_some("dollar stablecoins")]
        .into_iter().flatten().collect::<Vec<_>>(), "and");
    let cash_own = if cash_own.is_empty() { "the bank balances when included".to_string() } else { format!("{cash_own}, plus the bank balances when included") };
    v_flex().gap(px(sp::GRID))
        .child(h_flex().justify_between().items_end().flex_wrap().gap_2()
            .child(div().flex_1().min_w(layout::col(300.)).child(head("Asset mix", "equity, bonds, cash, crypto",
                &fmt::local(&format!("Every holding classified by a small table of known symbols (common index ETFs, widely held stocks, T-bill, gilt, aggregate and corporate bond funds, gold), else by its description's words (Treasury, T-Bill, Gilt, Bond, Gold). {}Cash is {}. {}Stock : bond compares equity with bonds only. The target is yours, in invest.toml under [mix]; drift is the share now minus the target, and the HK$ it is above or below at today's total.",
                    if m.has(Account::FlexInvest) { "The FlexInvest fund is equity (an MSCI World tracker). " } else { "" },
                    cash_own,
                    if m.has(Account::Revolut) { "The Revolut pot is not classified: its contents aren't in the data. " } else { "" })),
                line, cx)))
            .child(bank_toggle))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_start().child(ratio_card).child(table_card))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{capitalize, ratio};

    #[test]
    fn phrases() {
        assert_eq!(ratio(100., 0.), "100 : 0");
        assert_eq!(ratio(79.6, 20.4), "80 : 20");
        assert_eq!(capitalize("equity 70%"), "Equity 70%");
    }
}
