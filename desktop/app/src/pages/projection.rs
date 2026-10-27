//! Net worth projection: where today's money and today's habits lead in five
//! years, with a choice of how investments grow. Shown on the Overview.
//!
//! Model, monthly: savings = average income - spending of the last 6 completed
//! months; of that, the average monthly investing moves into the invested pool
//! (funds, broker, crypto), which grows at the chosen rate; the rest stays as
//! cash at 0% (the cash-drag tile is the cost of that). A deliberately simple,
//! transparent model -- every assumption is printed under the chart.

use std::rc::Rc;

use finance_core::Bundle;
use gpui_kit::component::{ActiveTheme as _, chart::AreaChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, Figures as _, Tile, sp, ts};
use crate::zoom::px;

const MONTHS: usize = 60;
/// A long-run equity assumption, below the historical ~7-9%.
const CONSERVATIVE_PCT: f64 = 6.;
/// Your own annualized return is clamped to this band before projecting: a
/// short, lucky (or unlucky) history should not compound for five years.
const HISTORY_CLAMP: (f64, f64) = (-5., 12.);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Growth {
    None,
    #[default]
    Conservative,
    History,
}

pub type OnGrowth = Rc<dyn Fn(Growth, &mut Window, &mut App)>;

#[derive(Clone)]
struct Point {
    label: SharedString,
    projected: f64,
    savings_only: f64,
}

/// (projected totals, savings-only totals), month 0 = today. The invested
/// pool compounds as `finance_core::jkb::grow` (the Investments page's
/// milestones and crossover use the same path).
fn simulate(cash: f64, pool: f64, save: f64, invest: f64, annual_pct: f64) -> (Vec<f64>, Vec<f64>) {
    let pools = finance_core::jkb::grow(pool, invest, annual_pct, MONTHS);
    let projected = pools.iter().enumerate().map(|(m, p)| cash + (save - invest) * m as f64 + p).collect();
    let flat = (0..=MONTHS).map(|m| cash + pool + save * m as f64).collect();
    (projected, flat)
}

/// Today's money and the habits the projection runs on.
pub struct Pace {
    pub cash: f64,
    /// investments and crypto
    pub pool: f64,
    /// average monthly saving (income - spend), last 6 completed months
    pub save: f64,
    /// average monthly investing (bank to funds / broker, plus crypto), same months
    pub invest: f64,
    /// average monthly spend, same months
    pub spend: f64,
}

pub fn pace(b: &Bundle) -> Pace {
    let nw = |k: &str| b.f64(&["net_worth", k]).unwrap_or(0.);
    // money owed to you (deposits, partner balance) comes back as cash at 0%,
    // so it starts in the cash bucket and the projection starts at net worth
    // (a mortgage setup's home and loan are left out on purpose: the home isn't
    // cash to spend or invest, and the loan's payments are already in the spending
    // and saving habits the projection runs on)
    let (cash, pool) = (nw("banks_hkd") + nw("receivables_hkd"), nw("investments_hkd") + nw("crypto_hkd"));
    let done = b.completed_trend();
    let last6: Vec<_> = done.iter().rev().take(6).collect();
    let k = last6.len().max(1) as f64;
    let avg = |f: &dyn Fn(&serde_json::Value) -> f64| last6.iter().map(|m| f(m)).sum::<f64>() / k;
    let g = |m: &serde_json::Value, key: &str| m.get(key).and_then(|v| v.as_f64()).unwrap_or(0.);
    Pace {
        cash,
        pool,
        save: avg(&|m| g(m, "income") - g(m, "spend")),
        invest: avg(&|m| g(m, "invested") + g(m, "invested_crypto")).max(0.),
        spend: avg(&|m| g(m, "spend")),
    }
}

pub fn render(b: &Bundle, mode: Growth, on_mode: OnGrowth, cx: &App) -> Div {
    let t = cx.theme();
    let Pace { cash, pool, save, invest, .. } = pace(b);

    let history = b.f64(&["investments", "totals", "irr_pct"]);
    let rate = match mode {
        Growth::None => 0.,
        Growth::Conservative => CONSERVATIVE_PCT,
        Growth::History => history.unwrap_or(0.).clamp(HISTORY_CLAMP.0, HISTORY_CLAMP.1),
    };
    let (proj, flat) = simulate(cash, pool, save, invest, rate);

    let now = chrono::Utc::now().date_naive();
    let label_at = |m: usize| {
        let total = now.format("%Y").to_string().parse::<i32>().unwrap_or(2026) * 12 + now.format("%m").to_string().parse::<i32>().unwrap_or(1) - 1 + m as i32;
        fmt::month_label(&format!("{:04}-{:02}", total / 12, total % 12 + 1))
    };
    let points: Vec<Point> = (0..=MONTHS).step_by(3)
        .map(|m| Point { label: label_at(m).into(), projected: proj[m], savings_only: flat[m] })
        .collect();
    let (c_main, c_flat) = (t.primary, t.muted_foreground);
    let chart = AreaChart::new(points).appear(false)
        .x(|p: &Point| p.label.clone())
        .y(|p: &Point| p.projected)
        .stroke(c_main)
        .fill(ui::soft_fill(c_main, 0.22))
        .name("Projected")
        .y(|p: &Point| p.savings_only)
        .stroke(c_flat.opacity(0.8))
        .fill(c_flat.opacity(0.))
        .name("Savings only")
        .natural()
        .y_axis(true)
        .y_tick_count(4)
        .x_tick_count(5)
        .y_tick_format(|v| fmt::compact(v))
        .tooltip_value(|_, _, v| fmt::hkd(v).into());

    // milestones
    let at = |m: usize| proj[m.min(MONTHS)];
    let growth_part = |m: usize| proj[m] - flat[m];
    let next_million = ((proj[0] / 1e6).floor() + 1.) * 1e6;
    let cross = proj.iter().position(|v| *v >= next_million);
    let tile = |label: &str, m: usize| {
        let sub = if rate != 0. { format!("{} from growth", fmt::signed_hkd(growth_part(m))) } else { "savings only".into() };
        Tile::new(label.to_string(), fmt::hkd(at(m))).sub(sub).render(cx).min_w(px(170.))
    };
    let tiles = h_flex().gap(px(sp::GRID)).flex_wrap().items_start()
        .child(tile("In 1 year", 12))
        .child(tile("In 3 years", 36))
        .child(tile("In 5 years", 60))
        .child(
            Tile::new(format!("Next {}", fmt::compact(next_million)),
                cross.map(|m| label_at(m)).unwrap_or("beyond 5 years".into()))
                .sub(cross.map(|m| format!("in {} months", m)).unwrap_or_default())
                .render(cx).min_w(px(170.)),
        );

    // the growth choice
    let seg = |label: String, g: Growth| {
        let on = mode == g;
        let on_mode = on_mode.clone();
        div().id(SharedString::from(format!("proj-{g:?}"))).px(px(9.)).py(px(3.)).rounded(px(5.))
            .text_size(px(ts::CAPTION)).cursor_pointer().whitespace_nowrap()
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| if !on { on_mode(g, w, cx) })
            .child(label)
    };
    let hist_label = match history {
        Some(h) if h.clamp(HISTORY_CLAMP.0, HISTORY_CLAMP.1) != h => format!("Your history {:.0}% → {:.0}%", h, h.clamp(HISTORY_CLAMP.0, HISTORY_CLAMP.1)),
        Some(h) => format!("Your history {h:.1}%"),
        None => "Your history —".into(),
    };
    let toggle = h_flex().p(px(2.)).gap(px(2.)).rounded(px(7.)).border_1().border_color(ui::hairline(cx)).flex_wrap()
        .child(seg("Savings only".into(), Growth::None))
        .child(seg(format!("{CONSERVATIVE_PCT:.0}% a year"), Growth::Conservative))
        .child(seg(hist_label, Growth::History));

    let assumptions = format!(
        "Starts from {} today ({} cash, {} invested). Each month: saves {} and invests {} of it (6-month averages); cash earns 0%; invested grows {}.",
        fmt::hkd(proj[0]), fmt::compact(cash), fmt::compact(pool), fmt::hkd(save), fmt::hkd(invest),
        if rate == 0. { "0%".to_string() } else { format!("{rate:.1}% a year") },
    );

    v_flex()
        .gap(px(sp::GRID))
        .child(
            h_flex().justify_between().flex_wrap().gap_2().items_center()
                .child(ui::section_info("Projection", "net worth over the next five years",
                    "A simple model, not a forecast: today's balances plus your recent average saving and investing, with the invested part growing at the rate you pick. It ignores new costs (like rent that has not started yet), pay changes and market swings. 'Your history' is your annualized money-weighted return, clamped to −5…12% so a short, lucky stretch does not compound for five years.", cx))
                .child(toggle),
        )
        .child(tiles)
        .child(
            ui::card(cx).h(px(270.)).gap_2()
                .child(h_flex().gap(px(sp::M)).flex_wrap()
                    .child(ui::legend(c_main, "Projected", "", cx))
                    .child(ui::legend(c_flat, "Savings only", "", cx)))
                .child(div().flex_1().min_h_0().child(chart))
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).figures().child(assumptions)),
        )
}

#[cfg(test)]
mod tests {
    use super::simulate;

    #[test]
    fn zero_growth_is_savings_only() {
        let (p, f) = simulate(100., 50., 10., 4., 0.);
        assert!((p[60] - f[60]).abs() < 1e-6);
        assert!((p[60] - (150. + 600.)).abs() < 1e-6);
    }

    #[test]
    fn growth_adds_on_top() {
        let (p, f) = simulate(0., 1000., 0., 0., 12.);
        assert!((p[12] - 1120.).abs() < 1e-6);
        assert!((f[12] - 1000.).abs() < 1e-9);
    }
}
