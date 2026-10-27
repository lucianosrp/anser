//! Money owed to you (the bundle's `receivables` section, computed by the
//! engine's views::receivables): deposits held, the partner's balance, and
//! reimbursable spend shown as information. Shared by Overview (a tile) and
//! Banks (the full card).

use finance_core::Bundle;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::fmt;
use crate::ui::{self, Figures as _, Tile, sp, ts};
use crate::zoom::px;

pub const EXPLAIN: &str = "Money that left your accounts but is still yours: deposits held by a landlord (until refunded) and the partner's unpaid part of shared bills. It counts in net worth, not in cash. Office costs the employer repays inside salary are not included, because the data can't tell which are still outstanding.";

/// The receivables section, if the bundle has a usable one.
pub fn section(b: &Bundle) -> Option<&Value> {
    let r = b.at(&["receivables"]);
    (r.is_object() && r.get("error").is_none()).then_some(r)
}

fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.)
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// "2026-09-22" -> "22 Sep 2026"
fn day(d: &str) -> String {
    chrono::NaiveDate::parse_from_str(d.get(..10).unwrap_or(d), "%Y-%m-%d")
        .map(|d| d.format("%-d %b %Y").to_string())
        .unwrap_or_else(|_| d.to_string())
}

/// Overview tile: the total, and what it is made of.
pub fn tile(b: &Bundle, spark: Vec<f64>, color: Hsla, cx: &App) -> Option<Div> {
    let r = section(b)?;
    let deposits = r.get("deposits").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let partner = r.get("partner").cloned().unwrap_or(Value::Null);
    let mut parts = Vec::new();
    if !deposits.is_empty() {
        parts.push(format!("{} deposit{}", deposits.len(), if deposits.len() == 1 { "" } else { "s" }));
    }
    let p = f(&partner, "amount_hkd");
    if p.abs() >= 0.5 {
        parts.push(format!("partner {}", fmt::compact(p)));
    }
    let since = deposits.iter().filter_map(|d| s(d, "since")).chain(s(&partner, "since")).min();
    let sub = match (parts.is_empty(), since) {
        (true, _) => "nothing outstanding".to_string(),
        (false, Some(d)) => format!("{} · since {}", parts.join(" + "), day(d)),
        (false, None) => parts.join(" + "),
    };
    Some(Tile::explained("Owed to you", EXPLAIN, fmt::hkd(f(r, "total_hkd"))).sub(sub).id("t-owed").spark(spark, color).render(cx))
}

/// One line of the card: title over caption, amount on the right.
fn line(title: String, caption: String, amount: String, tone: Option<Hsla>, cx: &App) -> Div {
    h_flex()
        .gap(px(sp::M))
        .py(px(8.))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_size(px(ts::BODY)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                .child(ui::caption(caption, cx)),
        )
        .child(
            div().flex_shrink_0().text_size(px(ts::BODY)).font_weight(FontWeight::MEDIUM).figures().whitespace_nowrap()
                .when_some(tone, |d, c| d.text_color(c))
                .child(amount),
        )
}

/// Banks page card: every deposit held and the partner balance with their
/// since-dates, the total, and the reimbursement information line.
pub fn card(b: &Bundle, cx: &App) -> Option<Div> {
    let r = section(b)?;
    let t = cx.theme();
    let mut rows: Vec<Div> = Vec::new();
    for d in r.get("deposits").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
        let label = s(d, "label").unwrap_or("deposit");
        let title = s(d, "note").map(str::to_string).unwrap_or_else(|| format!("Deposit · {label}"));
        let n = d.get("payments").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        let base = fmt::base_ccy();
        let ccy = s(d, "currency").unwrap_or(&base);
        let native = if ccy == base { String::new() } else { format!(" · {} {ccy}", fmt::dec2(f(d, "native_amount"))) };
        let caption = format!("deposit to {label} · since {}{}{native}",
            s(d, "since").map(day).unwrap_or_default(),
            if n > 1 { format!(" · {n} payments") } else { String::new() });
        rows.push(line(title, caption, fmt::hkd(f(d, "amount_hkd")), None, cx));
    }
    let p = r.get("partner").cloned().unwrap_or(Value::Null);
    let pa = f(&p, "amount_hkd");
    let p_caption = match s(&p, "since") {
        Some(since) => format!("since {} · their part {} · repaid {}", day(since),
            fmt::compact(f(&p, "shared_hkd")), fmt::compact(f(&p, "repaid_hkd"))),
        None => s(&p, "note").unwrap_or("starts with the first shared bill").to_string(),
    };
    let p_title = if pa < -0.5 { "You owe your partner" } else { "Partner's share of shared bills" };
    rows.push(line(p_title.into(), p_caption, if s(&p, "since").is_some() { fmt::hkd(pa.abs()) } else { "—".into() },
        (pa < -0.5).then(fmt::money_out), cx));

    let reimb = r.get("reimbursements").cloned().unwrap_or(Value::Null);
    let reimb_line = (f(&reimb, "t12m_hkd") > 0.).then(|| {
        h_flex()
            .gap(px(6.))
            .pt(px(10.))
            .child(ui::info_for(ts::CAPTION, "info-reimb", s(&reimb, "note").map(|n| format!("{}: {n}.", s(&reimb, "category").unwrap_or("Reimbursed")))
                .unwrap_or_default(), cx))
            .child(ui::caption(format!("Reimbursed office costs: {} this year · {}/mo average · repaid in salary, not counted",
                fmt::hkd(f(&reimb, "ytd_hkd")), fmt::compact(f(&reimb, "monthly_avg_hkd"))), cx))
    });

    let n = rows.len();
    let list = v_flex().children(rows.into_iter().enumerate().map(|(i, row)| {
        row.when(i + 1 < n, |d| d.border_b_1().border_color(ui::hairline(cx)))
    }));
    Some(
        ui::card(cx)
            .gap_0()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .pb(px(4.))
                    .child(ui::with_info(ui::title("Owed to you"), ts::TITLE, "info-owed", EXPLAIN, cx))
                    .child(div().text_size(px(ts::TITLE)).font_semibold().figures().child(fmt::hkd(f(r, "total_hkd")))),
            )
            .child(ui::caption("in net worth, not in cash", cx).text_color(t.muted_foreground))
            .child(list)
            .children(reimb_line),
    )
}
