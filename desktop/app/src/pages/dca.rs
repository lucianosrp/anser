//! Crypto › BTC DCA: is the dollar-cost averaging working? The numbers come
//! from finance_core::dca (USD throughout, BTC's weekly candles); this page
//! lays them out: the verdict tiles, the price with what you paid on top,
//! value against money in, the monthly rhythm, and every buy's result today.

use std::rc::Rc;

use finance_core::dca::{self, Dca, Scope};
use finance_core::{Bundle, Ledger};
use gpui_kit::component::{ActiveTheme as _, chart::BarChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::linechart::{self, Chart, Dot, Series};
use crate::ui::{self, Pt};
use crate::zoom::px;

pub type OnScope = Rc<dyn Fn(Scope, &mut Window, &mut App)>;

pub const EXPLAIN_EDGE: &str = "A fixed amount on a schedule buys more coins when the price is low and fewer when it is high, so a steady plan should pay less than the average price over the same weeks. This compares the average price you paid with the average weekly close from your first to your last buy. Positive = the averaging worked.";
pub const EXPLAIN_LUMP: &str = "The same total, all invested at the first buy's week, valued today. In a market that mostly rose a lump sum usually wins — DCA trades some of that upside for not having to time the entry. Compare it with the worst drawdown: that is what averaging spared you.";

fn usd(v: f64) -> String {
    if v < 0. { format!("−${}", fmt::int(-v)) } else { format!("${}", fmt::int(v)) }
}

fn usd_compact(v: f64) -> String {
    format!("${}", fmt::compact(v))
}

fn btc(v: f64) -> String {
    if fmt::private() { format!("{} BTC", fmt::MASK) } else { format!("{v:.5} BTC") }
}

/// "2022-08-15" -> "Aug 22"
fn mon(date: &str) -> String {
    fmt::month_label(&date[..date.len().min(7)])
}

pub fn render(b: &Bundle, ledger: &Ledger, scope: Scope, on_scope: OnScope, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let live = b.f64(&["crypto", "assets", "BTC", "price"]).or_else(|| b.f64(&["crypto", "price", "BTC"]));
    let Some(d) = dca::analyze(ledger, live, scope) else {
        return v_flex().child(ui::empty_with(gpui_kit::component::IconName::Globe, "No BTC buys found",
            "The DCA view needs BTC buys in the crypto ledger and BTC's weekly prices — sync first.", cx));
    };

    // ---- scope switch ----
    let seg = |s: Scope| {
        let on = scope == s;
        let on_scope = on_scope.clone();
        div().id(s.label()).px(px(12.)).py(px(4.)).rounded(px(6.)).text_sm().cursor_pointer().whitespace_nowrap()
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|st| st.text_color(t.foreground)))
            .on_click(move |_, w, cx| if !on { on_scope(s, w, cx) })
            .child(s.label())
    };
    let first = &d.buys[0].date;
    let scope_bar = h_flex().gap_3().items_center().flex_wrap()
        .child(h_flex().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx))
            .child(seg(Scope::Plan)).child(seg(Scope::All)))
        .child(ui::caption(format!("{} buys since {} · priced in USD · BTC at {}", d.buys.len(), mon(first), usd(d.price)), cx));

    // ---- the verdict ----
    let edge = d.edge_pct();
    let (active, span) = d.months_active();
    let median_month = {
        let mut m: Vec<f64> = d.months.iter().map(|m| m.cost).collect();
        m.sort_by(f64::total_cmp);
        m.get(m.len() / 2).copied().unwrap_or(0.)
    };
    let tiles = h_flex().gap_4().flex_wrap().items_stretch().children([
        ui::Tile::explained("DCA edge", EXPLAIN_EDGE, fmt::pct(edge))
            .sub(format!("paid {} on average vs {} average close", usd(d.avg_cost()), usd(d.mean_close)))
            .tone(fmt::signed_color(edge)).id("t-edge").render(cx),
        ui::stat("Position", usd(d.value()),
            format!("{} · {} on {} in", fmt::pct(d.gain_pct()), usd(d.gain()), usd(d.bought)), Some(fmt::signed_color(d.gain())), cx),
        ui::Tile::explained("vs lump sum", EXPLAIN_LUMP, fmt::pct(d.vs_lump_pct()))
            .sub(format!("all-in at {} would be {} · worst dip {:.0}%", mon(first), usd(d.lump_value()), d.worst_drawdown_pct()))
            .tone(fmt::signed_color(d.vs_lump_pct())).id("t-lump").render(cx),
        ui::stat("Consistency", format!("{active} of {span} months"),
            format!("median {} a month · {} of {} buys in profit", usd(median_month), d.buys_in_profit(), d.buys.len()), None, cx),
    ].map(|x| x.min_w(px(220.))));

    v_flex()
        .gap_4()
        .child(scope_bar)
        .child(tiles)
        .child(price_chart(&d, cx))
        .child(value_chart(&d, cx))
        .child(h_flex().gap_4().flex_wrap().items_start()
            .child(div().flex_1().min_w(crate::layout::col(380.)).child(monthly_chart(&d, cx)))
            .child(div().flex_1().min_w(crate::layout::col(380.)).child(each_buy_chart(&d, cx))))
        .child(venues(&d, cx))
}

/// Ticks at each January (or every month when the span is short).
fn year_ticks(dates: &[String]) -> Vec<(usize, SharedString)> {
    let mut out = Vec::new();
    let mut last = String::new();
    for (i, d) in dates.iter().enumerate() {
        let y = &d[..4];
        if y != last {
            if !last.is_empty() {
                out.push((i, y.to_string().into()));
            }
            last = y.to_string();
        }
    }
    out
}

/// Weekly close (log) with the running average paid and every buy as a dot
/// sized by its amount.
fn price_chart(d: &Dca, cx: &App) -> Div {
    let t = cx.theme();
    let dates: Vec<String> = d.points.iter().map(|w| w.date.clone()).collect();
    let max_usd = d.buys.iter().map(|b| b.cost).fold(1., f64::max);
    let buy_color = fmt::money_in();
    let dots = d.buys.iter().filter_map(|b| {
        // the buy's position inside its week, so dots don't stack on the candle date
        let i = dates.partition_point(|w| w.as_str() <= b.date.as_str()).checked_sub(1)?;
        Some(Dot { x: i as f32, y: b.price(), r: 2. + 4. * (b.cost / max_usd).sqrt() as f32, color: buy_color.opacity(0.75) })
    }).collect();
    let chart = Chart {
        id: "dca-price".into(),
        labels: d.points.iter().map(|w| format!("week of {}", w.date).into()).collect(),
        x_ticks: year_ticks(&dates),
        series: vec![
            Series::line("BTC", d.points.iter().map(|w| Some(w.close)).collect(), t.foreground.opacity(0.55)).width(1.4),
            Series::line("You paid (avg)", d.points.iter().map(|w| (w.avg_cost > 0.).then_some(w.avg_cost)).collect(), t.primary).dashed().width(1.8),
        ],
        dots,
        log: true,
        height: 300.,
        y_fmt: Rc::new(usd_compact),
    };
    let legend = h_flex().gap_3()
        .child(ui::legend(t.foreground.opacity(0.55), "BTC weekly close", usd(d.price), cx))
        .child(ui::legend(t.primary, "your average", usd(d.avg_cost()), cx))
        .child(ui::legend(buy_color, "buys (size = amount)", d.buys.len().to_string(), cx));
    ui::chart_card_with("Price and what you paid", "log scale · hover for a week", legend, 370., linechart::render(chart, cx), cx)
}

/// What the plan is worth each week against the money put in.
fn value_chart(d: &Dca, cx: &App) -> Div {
    let t = cx.theme();
    let dates: Vec<String> = d.points.iter().map(|w| w.date.clone()).collect();
    let up = fmt::money_in();
    let chart = Chart {
        id: "dca-value".into(),
        labels: d.points.iter().map(|w| format!("week of {}", w.date).into()).collect(),
        x_ticks: year_ticks(&dates),
        series: vec![
            Series::line("Value", d.points.iter().map(|w| Some(w.value())).collect(), up).width(1.8).fill(up.opacity(0.14)),
            Series::line("Money in", d.points.iter().map(|w| Some(w.invested)).collect(), t.muted_foreground).dashed().width(1.5),
        ],
        dots: vec![],
        log: false,
        height: 240.,
        y_fmt: Rc::new(usd_compact),
    };
    let legend = h_flex().gap_3()
        .child(ui::legend(up, "value", usd(d.value()), cx))
        .child(ui::legend(t.muted_foreground, "money in", usd(d.bought - d.sold), cx))
        .child(ui::legend(t.muted_foreground, "in profit", format!("{:.0}% of weeks", d.points_in_profit_pct()), cx));
    ui::chart_card_with("Value vs money in", "the plan's coins at each week's close", legend, 310., linechart::render(chart, cx), cx)
}

/// USD bought per month, every month of the span (gaps show pauses).
fn monthly_chart(d: &Dca, cx: &App) -> Div {
    let t = cx.theme();
    let (Some(a), Some(z)) = (d.months.first(), d.months.last()) else { return div() };
    let mut pts = Vec::new();
    let (mut y, mut m) = (a.month[..4].parse::<i32>().unwrap_or(2000), a.month[5..7].parse::<u32>().unwrap_or(1));
    loop {
        let key = format!("{y:04}-{m:02}");
        let v = d.months.iter().find(|x| x.month == key).map_or(0., |x| x.cost);
        pts.push(Pt { label: fmt::month_label(&key).into(), v });
        if key >= z.month {
            break;
        }
        (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    }
    let margin = (pts.len() / 8).max(1);
    let color = t.primary;
    let chart = BarChart::new(pts).appear(false)
        .band(|p: &Pt| p.label.clone())
        .value(|p: &Pt| p.v)
        .value_axis(true)
        .tick_margin(margin)
        .value_tick_format(|v| usd_compact(v))
        .tooltip_value(|_, v| usd(v).into())
        .fill(move |_: &Pt, _, _, _| color);
    ui::chart_card("Bought per month", "USD · empty months are pauses", 260., chart, cx)
}

/// Every buy's result at today's price, oldest first.
fn each_buy_chart(d: &Dca, cx: &App) -> Div {
    let pts: Vec<Pt> = d.buys.iter().enumerate().map(|(i, b)| Pt {
        // bands must be unique: invisible zero-width spaces keep same-month buys
        // apart while the axis shows just "Mar 23"
        label: format!("{}{}", mon(&b.date), "\u{200B}".repeat(i)).into(),
        v: (d.price / b.price() - 1.) * 100.,
    }).collect();
    let (up, down) = (fmt::money_in(), fmt::money_out());
    let margin = (pts.len() / 6).max(1);
    let chart = BarChart::new(pts).appear(false)
        .band(|p: &Pt| p.label.clone())
        .value(|p: &Pt| p.v)
        .value_axis(true)
        .tick_margin(margin)
        .value_tick_format(|v| format!("{v:.0}%"))
        .tooltip_value(|_, v| fmt::pct(v).into())
        .fill(move |p: &Pt, _, _, _| if p.v >= 0. { up } else { down });
    ui::chart_card("Every buy today", "return of each buy at today's price", 260., chart, cx)
}

/// Where the buys happened: count, money, coins, average price.
fn venues(d: &Dca, cx: &App) -> Div {
    let t = cx.theme();
    let mut rows: Vec<(String, usize, f64, f64)> = Vec::new();
    for b in &d.buys {
        let key = format!("{} · {}", b.venue, b.kind);
        match rows.iter_mut().find(|r| r.0 == key) {
            Some(r) => { r.1 += 1; r.2 += b.cost; r.3 += b.qty; }
            None => rows.push((key, 1, b.cost, b.qty)),
        }
    }
    rows.sort_by(|a, b| b.2.total_cmp(&a.2));
    let cell = |s: String, w: f32| div().w(px(w)).text_sm().text_right().whitespace_nowrap().child(s);
    let head = h_flex().gap_3().pb_1().text_xs().text_color(t.muted_foreground)
        .child(div().flex_1().child("Where"))
        .child(div().w(px(60.)).text_right().child("Buys"))
        .child(div().w(px(100.)).text_right().child("Paid"))
        .child(div().w(px(120.)).text_right().child("Coins"))
        .child(div().w(px(100.)).text_right().child("Avg price"));
    let body = v_flex().children(rows.into_iter().map(|(k, n, u, c)| {
        h_flex().gap_3().py(px(6.)).border_t_1().border_color(ui::hairline(cx))
            .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(k))
            .child(cell(n.to_string(), 60.))
            .child(cell(usd(u), 100.))
            .child(cell(btc(c), 120.))
            .child(cell(usd(u / c), 100.))
    }));
    let extra = (d.rewards_qty > 0. || d.sold_qty > 0.).then(|| ui::caption(format!(
        "Rewards added {} for free; sells took out {} for {}.", btc(d.rewards_qty), btc(d.sold_qty), usd(d.sold)), cx).pt_2());
    v_flex().gap_2()
        .child(ui::section("Where you bought", "recurring and one-off buys", cx))
        .child(ui::card(cx).child(head).child(body).children(extra))
}
