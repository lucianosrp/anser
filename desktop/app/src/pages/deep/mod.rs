//! Broker › Deep dive: the investing analysis in sections. Each section
//! opens with a one-line takeaway in plain words, then the detail.
//!
//! * **DCA**: each holding (and the whole broker, in HKD) as a dollar-cost
//!   averaging plan: the edge, the lump sum, consistency, every buy today.
//! * **Fees**: what set each commission, the order size for a target share,
//!   the yearly projection, IBKR Fixed vs Tiered re-pricing.
//! * **Returns**: value split into money in, market, currency moves and fees.
//! * **Exposure**: currency, venue, type, region and sector look-through,
//!   overlapping funds.
//! * **Risk**: volatility, drawdowns, correlations.
//! * **Targets**: drift from the owner's target weights, and the
//!   rebalancing math for the next contribution.
//! * **Dividends**: income and withholding, or what data is missing.
//!
//! The numbers come from finance_core (`invest`, `fees`, `exposure`,
//! `risk`, `rebalance`, `dca`); this module lays them out. Its own view: it
//! keeps the picked section, holding and scope, and the targets editor.

mod dca;
mod dividends;
pub mod exposure;
mod fees;
mod returns;
mod risk;
mod targets;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use finance_core::fees::Repriced;
use finance_core::invest::{self, Extra, HoldingPlan, MonthSplit, Portfolio, Split};
use finance_core::{Bundle, Ledger};
use gpui_kit::component::{ActiveTheme as _, h_flex, input::InputState, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Section {
    #[default]
    Dca,
    Fees,
    Returns,
    Exposure,
    Risk,
    Targets,
    Dividends,
}

impl Section {
    pub const ALL: [Section; 7] = [Section::Dca, Section::Fees, Section::Returns, Section::Exposure, Section::Risk, Section::Targets, Section::Dividends];

    pub fn label(self) -> &'static str {
        match self {
            Section::Dca => "DCA",
            Section::Fees => "Fees",
            Section::Returns => "Returns",
            Section::Exposure => "Exposure",
            Section::Risk => "Risk",
            Section::Targets => "Targets",
            Section::Dividends => "Dividends",
        }
    }

    /// What the section answers (the Investments page's links).
    pub fn blurb(self) -> &'static str {
        match self {
            Section::Dca => "Is the monthly averaging buying below the average price, after fees?",
            Section::Fees => "What set each commission, and Fixed vs Tiered on your own trades",
            Section::Returns => "Value split into money in, market, currency moves and fees",
            Section::Exposure => "Currencies, venues, regions and sectors, looking through the funds",
            Section::Risk => "Volatility, worst falls and how the holdings move together",
            Section::Targets => "Drift from your target weights; rebalancing math for the next contribution",
            Section::Dividends => "Income per holding and the tax withheld",
        }
    }

    /// `FINANCE_DESKTOP_DEEP=fees` (dev and screenshots)
    fn from_env() -> Option<Section> {
        let v = std::env::var("FINANCE_DESKTOP_DEEP").ok()?;
        Section::ALL.into_iter().find(|s| s.label().eq_ignore_ascii_case(&v))
    }
}

/// Broker only, or every investment (FlexInvest, crypto too).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scope {
    #[default]
    Broker,
    All,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Broker => "Broker",
            Scope::All => "All investments",
        }
    }
}

/// Everything the sections show, computed once per data load.
pub struct Data {
    pub ledger: Arc<Ledger>,
    pub bundle: Arc<Bundle>,
    pub extra: Extra,
    /// the broker snapshot's date (the "today" of the numbers)
    pub as_of: String,
    pub plans: Vec<HoldingPlan>,
    pub portfolio: Portfolio,
    pub repriced: Vec<Repriced>,
    pub split: Vec<Split>,
    pub all: Vec<MonthSplit>,
}

impl Data {
    pub fn new(ledger: Arc<Ledger>, bundle: Arc<Bundle>) -> Self {
        let extra = Extra::load(&finance_core::cache::db_path()).unwrap_or_default().with_ledger(&ledger, &bundle);
        let last_trade = ledger.trades.iter().map(|t| t.date.clone()).max().unwrap_or_default();
        let as_of = bundle.str(&["ibkr", "snapshot", "as_of"]).map(str::to_string).unwrap_or(last_trade);
        let plans = invest::holding_plans(&ledger, &extra.fx, &as_of, &HashMap::new());
        let portfolio = invest::portfolio(&plans, &ledger);
        let repriced = finance_core::fees::reprice(&ledger.trades);
        let split = invest::broker_split(&ledger, &extra.fx, &invest::nav_days(&bundle));
        let all = invest::all_split(&bundle, &split);
        Data { ledger, bundle, extra, as_of, plans, portfolio, repriced, split, all }
    }

    /// The holdings still held, biggest first.
    pub fn held(&self) -> Vec<&HoldingPlan> {
        let mut v: Vec<&HoldingPlan> = self.plans.iter().filter(|h| h.dca.position_qty() > 1e-9).collect();
        v.sort_by(|a, b| b.value_hkd().total_cmp(&a.value_hkd()));
        v
    }
}

pub struct DeepDive {
    data: Option<Rc<Data>>,
    section: Section,
    /// DCA: one holding, or None for the whole broker
    symbol: Option<String>,
    /// Returns and Exposure: broker or everything
    scope: Scope,
    // ---- targets ----
    targets: crate::targets::Targets,
    targets_note: Option<String>,
    contribution: Option<Entity<InputState>>,
    weight_inputs: Vec<(String, Entity<InputState>)>,
    editing: bool,
    _subs: Vec<Subscription>,
}

impl DeepDive {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        let (targets, targets_note) = crate::targets::Targets::load();
        DeepDive {
            data: None,
            section: Section::from_env().unwrap_or_default(),
            symbol: std::env::var("FINANCE_DESKTOP_DEEP_SYMBOL").ok(),
            scope: if std::env::var("FINANCE_DESKTOP_DEEP_SCOPE").is_ok_and(|v| v == "all") { Scope::All } else { Scope::Broker },
            targets,
            targets_note,
            contribution: None,
            weight_inputs: Vec::new(),
            editing: false,
            _subs: Vec::new(),
        }
    }

    pub fn set_data(&mut self, ledger: Arc<Ledger>, bundle: Arc<Bundle>, cx: &mut Context<Self>) {
        self.data = Some(Rc::new(Data::new(ledger, bundle)));
        cx.notify();
    }

    /// Open a section (from the Investments page's links).
    pub fn open(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        cx.notify();
    }

    fn nav(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme();
        let current = self.section;
        h_flex().p(px(3.)).gap(px(2.)).flex_wrap().rounded(px(8.)).border_1().border_color(ui::hairline(cx))
            .children(Section::ALL.into_iter().map(|s| {
                let on = s == current;
                div().id(SharedString::from(format!("deep-{}", s.label()))).px(px(10.)).py(px(3.)).rounded(px(6.))
                    .text_size(px(ts::BODY)).cursor_pointer().whitespace_nowrap()
                    .when(on, |d| d.bg(t.background.opacity(0.6)).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
                    .when(!on, |d| d.text_color(t.muted_foreground).hover(|st| st.text_color(t.foreground)))
                    .on_click(cx.listener(move |this, _, _, cx| { this.section = s; cx.notify(); }))
                    .child(s.label())
            }))
    }
}

impl Render for DeepDive {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(d) = self.data.clone() else {
            return v_flex().child(ui::empty("No data yet — sync first", cx));
        };
        if d.plans.is_empty() && self.section != Section::Dividends {
            return v_flex().gap_4().child(h_flex().child(self.nav(cx)))
                .child(ui::empty_with(gpui_kit::component::IconName::Network, "No broker buys in the local replica yet",
                    "The deep dive needs IBKR trades and the daily prices of what you hold — sync first.", cx));
        }
        let view = cx.entity().downgrade();
        let on_symbol: OnPick = Rc::new(move |s, _, cx| {
            let _ = view.update(cx, |this, cx| { this.symbol = s; cx.notify(); });
        });
        let view = cx.entity().downgrade();
        let on_scope: OnScope = Rc::new(move |s, _, cx| {
            let _ = view.update(cx, |this, cx| { this.scope = s; cx.notify(); });
        });
        let body = match self.section {
            Section::Dca => dca::render(&d, self.symbol.as_deref(), on_symbol, cx).into_any_element(),
            Section::Fees => fees::render(&d, cx).into_any_element(),
            Section::Returns => returns::render(&d, self.scope, on_scope, cx).into_any_element(),
            Section::Exposure => exposure::render(&d, self.scope, on_scope, cx).into_any_element(),
            Section::Risk => risk::render(&d, cx).into_any_element(),
            Section::Targets => self.render_targets(&d, window, cx).into_any_element(),
            Section::Dividends => dividends::render(&d, cx).into_any_element(),
        };
        v_flex().gap(px(sp::GRID)).child(h_flex().child(self.nav(cx))).child(body)
    }
}

pub type OnPick = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;
pub type OnScope = Rc<dyn Fn(Scope, &mut Window, &mut App)>;

// ---------------------------------------------------------------- shared bits

/// The section's one-line answer, first thing under the nav.
pub fn takeaway(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = cx.theme();
    h_flex().items_stretch().gap(px(sp::M)).py(px(2.))
        .child(div().w(px(3.)).flex_shrink_0().rounded_full().bg(t.primary))
        .child(div().flex_1().min_w_0().text_size(px(ts::TITLE)).line_height(px(21.)).text_color(t.foreground).child(text.into()))
}

/// A small muted paragraph (notes, sources, caveats).
pub fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_size(px(ts::CAPTION)).line_height(px(17.)).text_color(cx.theme().muted_foreground).child(text.into())
}

/// A segmented switch for the scope.
pub fn scope_switch(scope: Scope, on: OnScope, cx: &App) -> Div {
    let t = cx.theme();
    h_flex().p(px(2.)).gap(px(2.)).rounded(px(7.)).border_1().border_color(ui::hairline(cx))
        .children([Scope::Broker, Scope::All].into_iter().map(|s| {
            let active = s == scope;
            let on = on.clone();
            div().id(SharedString::from(format!("scope-{}", s.label()))).px(px(10.)).py(px(2.)).rounded(px(5.))
                .text_size(px(ts::CAPTION)).cursor_pointer().whitespace_nowrap()
                .when(active, |d| d.bg(t.background.opacity(0.6)).text_color(t.foreground))
                .when(!active, |d| d.text_color(t.muted_foreground).hover(|st| st.text_color(t.foreground)))
                .on_click(move |_, w, cx| if !active { on(s, w, cx) })
                .child(s.label())
        }))
}

/// A table column: title, width (None = takes the rest); fixed-width
/// columns are right-aligned unless `left`.
pub struct Col {
    pub title: &'static str,
    pub w: Option<f32>,
    pub left: bool,
}

pub const fn col(title: &'static str, w: f32) -> Col {
    Col { title, w: Some(w), left: false }
}

/// A fixed-width column of text (left-aligned).
pub const fn lcol(title: &'static str, w: f32) -> Col {
    Col { title, w: Some(w), left: true }
}

pub const fn flex(title: &'static str) -> Col {
    Col { title, w: None, left: true }
}

/// One cell: text and an optional color.
pub type Cell = (String, Option<Hsla>);

pub fn c(s: impl Into<String>) -> Cell {
    (s.into(), None)
}

pub fn tone(s: impl Into<String>, color: Hsla) -> Cell {
    (s.into(), Some(color))
}

/// A plain table that scrolls sideways when the window is narrow. Numbers
/// right-aligned (every fixed-width column), tabular figures.
pub fn table(id: &str, cols: &[Col], rows: Vec<Vec<Cell>>, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    let min_w: f32 = cols.iter().map(|c| c.w.unwrap_or(140.)).sum::<f32>() + cols.len() as f32 * 10.;
    let cell = |c: &Col, text: String| {
        let d = div().whitespace_nowrap().overflow_hidden().text_ellipsis();
        match c.w {
            Some(w) if c.left => d.w(px(w)).flex_shrink_0(),
            Some(w) => d.w(px(w)).flex_shrink_0().text_right(),
            None => d.flex_1().min_w(px(110.)),
        }
        .child(text)
    };
    let head = h_flex().gap(px(10.)).pb(px(6.)).border_b_1().border_color(ui::hairline(cx))
        .text_size(px(ts::EYEBROW)).font_weight(FontWeight::MEDIUM).text_color(t.muted_foreground)
        .children(cols.iter().map(|c| cell(c, c.title.to_uppercase())));
    let body = v_flex().children(rows.into_iter().enumerate().map(|(i, r)| {
        h_flex().gap(px(10.)).py(px(6.)).text_size(px(ts::BODY)).figures()
            .when(i > 0, |d| d.border_t_1().border_color(ui::hairline(cx).opacity(0.6)))
            .children(cols.iter().zip(r).map(|(c, (text, color))| cell(c, text).when_some(color, |d, col| d.text_color(col))))
    }));
    div().id(SharedString::from(id.to_string())).w_full().overflow_x_scroll().restrict_scroll_to_axis()
        .child(v_flex().min_w(px(min_w)).child(head).child(body))
}

/// Money in a trading currency: "USD 1,234.56" (masked in privacy mode).
pub fn ccy(v: f64, currency: &str) -> String {
    format!("{currency} {}", fmt::dec2(v))
}

/// Fractional shares: up to 4 decimals (masked in privacy mode).
pub fn units(v: f64) -> String {
    if fmt::private() {
        return fmt::MASK.into();
    }
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// "2026-07-13" -> "13 Jul"
pub fn day(date: &str) -> String {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map(|d| d.format("%-d %b").to_string()).unwrap_or_else(|_| date.to_string())
}

/// Rounded to `decimals`, with no negative zero ("-0.00%").
pub fn tidy(v: f64, decimals: usize) -> f64 {
    let k = 10f64.powi(decimals as i32);
    let r = (v * k).round() / k;
    if r == 0. { 0. } else { r }
}

/// Percent with a sign and two decimals for small numbers.
pub fn pct2(v: f64) -> String {
    format!("{}%", fmt::signed(tidy(v, 2), 2))
}

/// The color of a signed number as shown with `decimals`: neutral at zero.
pub fn sign_color(v: f64, decimals: usize) -> Hsla {
    let v = tidy(v, decimals);
    if v == 0. { gpui_kit::hsla(0., 0., 0.6, 1.) } else { fmt::signed_color(v) }
}

/// A colored percent: green up, red down, neutral at zero.
pub fn signed_pct(v: f64, decimals: usize) -> Cell {
    let v = tidy(v, decimals);
    (format!("{v:+.decimals$}%"), Some(if v == 0. { gpui_kit::hsla(0., 0., 0.6, 1.) } else { fmt::signed_color(v) }))
}
