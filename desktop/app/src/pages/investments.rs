//! Investments: everything invested in one place. FlexInvest, the broker,
//! the coins and the Revolut pot as one portfolio, in six tabs (the app
//! remembers the last one; `FINANCE_DESKTOP_INV_TAB` picks it at start):
//!
//! * **Overview**: the hero against the benchmarks, the accounts, the next
//!   milestones, the links into Broker › Deep dive
//! * **Holdings**: every holding in one table, the week's and month's
//!   movers, where the gain came from (`holdings`)
//! * **Contributions**: money in against the plan, the buying streaks, money
//!   in vs growth (`flows`, `jkb`)
//! * **Income & costs**: what the investments paid, what investing costs
//! * **Buying habits**: the simulations of Nick Maggiulli's *Just Keep
//!   Buying* on the user's own buys
//! * **Allocation**: the asset mix with the bond/stock ratio and a target
//!   (`mix`), then the exposure look-through
//!
//! On a phone the tab switch is a horizontally scrolling segmented control.
//!
//! Every section opens with a one-line takeaway in plain words. Facts and
//! what-ifs, never advice. The numbers come from finance_core (`portfolio`,
//! `jkb`, `invest`, `fees`) and the bundle; they're computed once per data
//! load and cached here (the page is a plain render function, so its few
//! settings live in a thread-local and in `invest.toml`).

mod flows;
mod holdings;
mod jkb;
mod mix;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use finance_core::exposure::class::{Classification, Mix};
use finance_core::invest::{self, Extra, MonthSplit};
use finance_core::jkb as sim;
use finance_core::portfolio::{Account, DipCase, Pause, Periods, Portfolio};
use finance_core::{Bundle, Ledger};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, chart::AreaChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use super::projection::{self, Pace};
use crate::fmt;
use crate::pages::deep::takeaway;
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

/// One month of the portfolio and its S&P 500 shadow.
#[derive(Clone)]
struct Month {
    label: SharedString,
    value: f64,
    spx: f64,
}

/// Flip the hero chart between HK$ and % growth.
pub type OnToggle = Rc<dyn Fn(&mut Window, &mut App)>;
/// Open a section of Broker › Deep dive.
pub type OnDeep = Rc<dyn Fn(crate::pages::deep::Section, &mut Window, &mut App)>;
/// Go to another page (an account's own page).
pub type OnGo = crate::layout::OnGo;
/// Switch tabs (the app keeps the choice).
pub type OnTab = Rc<dyn Fn(Tab, &mut Window, &mut App)>;

/// The page's tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Overview,
    Holdings,
    Contributions,
    Income,
    Habits,
    Allocation,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Overview, Tab::Holdings, Tab::Contributions, Tab::Income, Tab::Habits, Tab::Allocation];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Holdings => "Holdings",
            Tab::Contributions => "Contributions",
            Tab::Income => "Income & costs",
            Tab::Habits => "Buying habits",
            Tab::Allocation => "Allocation",
        }
    }

    /// the `FINANCE_DESKTOP_INV_TAB` value
    pub fn key(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Holdings => "holdings",
            Tab::Contributions => "contributions",
            Tab::Income => "income",
            Tab::Habits => "habits",
            Tab::Allocation => "allocation",
        }
    }

    pub fn parse(s: &str) -> Option<Tab> {
        let s = s.trim().to_ascii_lowercase();
        Tab::ALL.into_iter().find(|t| t.key() == s || t.label().eq_ignore_ascii_case(&s))
            .or(match s.as_str() {
                "costs" => Some(Tab::Income),
                // the tab's old name (saved spots, dev aids)
                "jkb" | "just keep buying" => Some(Tab::Habits),
                "exposure" | "mix" => Some(Tab::Allocation),
                _ => None,
            })
    }

    /// Dev and screenshot aid: `FINANCE_DESKTOP_INV_TAB=holdings` opens on
    /// that tab (keys: overview, holdings, contributions, income, habits,
    /// allocation; the old `jkb` still works); in the browser `?inv_tab=habits`.
    pub fn from_env() -> Tab {
        crate::app::dev_aid("FINANCE_DESKTOP_INV_TAB").and_then(|v| Tab::parse(&v)).unwrap_or_default()
    }
}

// ---------------------------------------------------------------- settings

/// `~/.config/finance-desktop/invest.toml` (`FINANCE_DESKTOP_INVEST`
/// overrides): the assumptions the simulations use, and the monthly plan.
///
/// ```toml
/// return_pct = 6        # the expected annual return (milestones, crossover)
/// buffer_months = 6     # bank cash kept aside, in months of spending
/// [plan]                # HKD a month per account; missing = the median of the last 3 months
/// flexinvest = 10000
/// ibkr = 12000
/// crypto = 1300
/// include_bank_cash = false   # Allocation › Asset mix counts bank cash as cash
/// [mix]                 # optional target asset mix, any numbers, normalized to 100%
/// equity = 80           # keys: equity, bond, cash, crypto, commodity
/// bond = 20
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub return_pct: Option<f64>,
    pub buffer_months: Option<f64>,
    pub include_bank_cash: Option<bool>,
    pub plan: BTreeMap<String, f64>,
    pub mix: BTreeMap<String, f64>,
}

pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("FINANCE_DESKTOP_INVEST") {
        return PathBuf::from(p);
    }
    dirs::config_dir().unwrap_or_default().join("finance-desktop").join("invest.toml")
}

const CONFIG_HEADER: &str = "# anser: the Investments page's assumptions (edited by the app; hand edits are fine).\n\
# return_pct: expected annual return. buffer_months: bank cash kept aside. [plan]: HKD a month per account (flexinvest, ibkr, crypto).\n\
# include_bank_cash: the asset mix counts bank cash. [mix]: target asset mix (equity, bond, cash, crypto, commodity), normalized to 100%.\n\n";

impl Config {
    fn load() -> Config {
        // sealed (.enc) while the local copy is encrypted
        finance_core::replica::read_text(&config_path()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    fn save(&self) {
        if let Ok(s) = toml::to_string(self) {
            let _ = finance_core::replica::write_text(&config_path(), &format!("{CONFIG_HEADER}{s}"));
        }
    }

    fn plan_key(a: Account) -> &'static str {
        match a {
            Account::FlexInvest => "flexinvest",
            Account::Ibkr => "ibkr",
            Account::Crypto => "crypto",
            Account::Revolut => "revolut",
        }
    }
}

/// The default expected return: the Overview projection's conservative rate.
pub const DEFAULT_RETURN_PCT: f64 = 6.;
pub const DEFAULT_BUFFER_MONTHS: f64 = 6.;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sort {
    Name,
    #[default]
    Value,
    Gain,
    M1,
    Ytd,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Prefs {
    pub return_pct: f64,
    pub buffer_months: f64,
    pub sort: Sort,
    pub desc: bool,
    /// Asset mix: count bank cash as cash
    pub bank_cash: bool,
}

thread_local! {
    static PREFS: Cell<Option<Prefs>> = const { Cell::new(None) };
    static MODEL: RefCell<Option<Rc<Model>>> = const { RefCell::new(None) };
    /// the target asset mix (`[mix]` in invest.toml), loaded once
    static MIX_TARGET: RefCell<Option<BTreeMap<String, f64>>> = const { RefCell::new(None) };
}

/// The target asset mix, class key -> weight (empty = none set).
pub fn mix_target() -> BTreeMap<String, f64> {
    MIX_TARGET.with(|m| m.borrow_mut().get_or_insert_with(|| Config::load().mix).clone())
}

/// Change the target asset mix; saved to `invest.toml`.
pub fn set_mix_target(f: impl FnOnce(&mut BTreeMap<String, f64>)) {
    let mut t = mix_target();
    f(&mut t);
    t.retain(|_, v| v.is_finite() && *v > 0.);
    let mut c = Config::load();
    c.mix = t.clone();
    c.save();
    MIX_TARGET.with(|m| *m.borrow_mut() = Some(t));
}

pub fn prefs() -> Prefs {
    PREFS.with(|p| {
        p.get().unwrap_or_else(|| {
            let c = Config::load();
            let v = Prefs {
                return_pct: c.return_pct.unwrap_or(DEFAULT_RETURN_PCT),
                buffer_months: c.buffer_months.unwrap_or(DEFAULT_BUFFER_MONTHS),
                sort: Sort::Value,
                desc: true,
                bank_cash: c.include_bank_cash.unwrap_or(false),
            };
            p.set(Some(v));
            v
        })
    })
}

/// Change a setting; the assumptions are saved to `invest.toml`.
pub fn set_prefs(f: impl FnOnce(&mut Prefs)) {
    let before = prefs();
    let mut p = before;
    f(&mut p);
    PREFS.with(|c| c.set(Some(p)));
    if p.return_pct != before.return_pct || p.buffer_months != before.buffer_months || p.bank_cash != before.bank_cash {
        let mut c = Config::load();
        c.return_pct = Some(p.return_pct);
        c.buffer_months = Some(p.buffer_months);
        c.include_bank_cash = Some(p.bank_cash);
        c.save();
    }
}

// ------------------------------------------------------------------ model

/// Everything the page shows, computed once per data load.
pub struct Model {
    key: (usize, usize, String),
    pub p: Portfolio,
    pub pace: Pace,
    /// every month: value split into money in and market (broker NAV fixed)
    pub split: Vec<MonthSplit>,
    pub dips: Vec<DipCase>,
    pub pause: Option<Pause>,
    /// the broker's best 5 days and BTC's best 5 weeks
    pub broker_best: sim::BestShare,
    pub btc_best: sim::BestShare,
    /// a year of commissions at the last 91 days' pattern
    pub fees_yr: finance_core::fees::Projection,
    /// HKD a month per account: (amount, from the config file)
    pub plan: [(f64, bool); 4],
    /// IBKR cash in the account, HKD
    pub broker_cash: f64,
    /// the broker's holdings today against each one's money all invested at
    /// its first buy, HKD: (DCA value, lump-sum value)
    pub broker_lump: (f64, f64),
    /// the current BTC plan against its lump sum, % (positive = DCA ahead)
    pub btc_vs_lump: Option<f64>,
    /// what each holding is, in `p.holdings` order
    pub classes: Vec<Classification>,
    /// the asset mix: the holdings plus IBKR's cash (bank cash not included)
    pub mix: Mix,
    /// bank balances, HKD (the mix's optional extra cash)
    pub bank_cash: f64,
}

/// "a, b and c" (`conj` "and" or "or"); empty for none.
pub fn words(v: &[&str], conj: &str) -> String {
    match v {
        [] => String::new(),
        [one] => one.to_string(),
        [init @ .., last] => format!("{} {conj} {last}", init.join(", ")),
    }
}

fn today() -> String {
    crate::budgets::today().format("%Y-%m-%d").to_string()
}

impl Model {
    /// Does the user have this account (its source has data)? A tile or
    /// card about one they don't have stays hidden.
    pub fn has(&self, a: Account) -> bool {
        self.p.accounts.iter().any(|x| x.account == a)
    }

    /// The phrases of `of` for the accounts the user has, in order, so a
    /// help text names only their own: `[(Account::Ibkr, "IBKR")]`.
    pub fn mine<'a>(&self, of: &[(Account, &'a str)]) -> Vec<&'a str> {
        of.iter().filter(|(a, _)| self.has(*a)).map(|(_, s)| *s).collect()
    }

    /// "bank to FlexInvest and IBKR, plus crypto": where the monthly
    /// investing goes, from the user's accounts ("your investing" when none).
    pub fn pace_sources(&self) -> String {
        let banked = words(&self.mine(&[(Account::FlexInvest, "FlexInvest"), (Account::Ibkr, "IBKR")]), "and");
        match (banked.is_empty(), self.has(Account::Crypto)) {
            (false, true) => format!("bank to {banked}, plus crypto"),
            (false, false) => format!("bank to {banked}"),
            (true, true) => "crypto".into(),
            (true, false) => "your investing".into(),
        }
    }

    fn build(ledger: &Ledger, b: &Bundle, today: &str) -> Model {
        let x = Extra::load(&finance_core::cache::db_path()).unwrap_or_default().with_ledger(ledger, b);
        let p = Portfolio::build(b, ledger, &x, today);
        let broker = invest::broker_split(ledger, &x.fx, &invest::nav_days(b));
        let split = invest::all_split(b, &broker);
        let dips = p.dip_cases(ledger);
        let pause = p.btc_pause(ledger);
        let broker_best = sim::best_share(&p.broker_daily_pnl().iter().map(|d| d.1).collect::<Vec<_>>(), 5);
        let btc_best = sim::best_share(&p.btc_weekly_pnl().iter().map(|d| d.1).collect::<Vec<_>>(), 5);
        let fees_yr = finance_core::fees::projection(&finance_core::fees::reprice(&ledger.trades), today, 91);
        let cfg = Config::load();
        let done: Vec<&finance_core::portfolio::MonthIn> = p.months_in.iter().filter(|m| m.month.as_str() < &today[..7]).collect();
        let plan = Account::ALL.map(|a| match cfg.plan.get(Config::plan_key(a)) {
            Some(v) => (*v, true),
            None => (sim::median(&done.iter().rev().take(3).map(|m| m.by[a.index()]).collect::<Vec<_>>()), false),
        });
        let live = b.f64(&["crypto", "assets", "BTC", "price"]).or_else(|| b.f64(&["crypto", "price", "BTC"]));
        let btc_vs_lump = finance_core::dca::analyze(ledger, live, finance_core::dca::Scope::Plan).map(|d| d.vs_lump_pct());
        let broker_lump = p.plans.iter().fold((0., 0.), |(v, l), h| (v + h.value_hkd() + h.dca.sold * h.fx_now, l + h.lump_hkd()));
        let classes: Vec<Classification> = p.holdings.iter().map(|h| h.classification()).collect();
        let broker_cash = b.f64(&["ibkr", "snapshot", "cash_base"]).unwrap_or(0.);
        let mut mix = Mix::of(&classes.iter().zip(&p.holdings).map(|(c, h)| (*c, h.value_hkd)).collect::<Vec<_>>());
        if broker_cash > 0. {
            mix.add(&Classification::CASH, broker_cash);
        }
        Model {
            classes,
            mix,
            bank_cash: b.f64(&["net_worth", "banks_hkd"]).unwrap_or(0.).max(0.),
            key: (0, 0, String::new()),
            broker_lump,
            btc_vs_lump,
            pace: projection::pace(b),
            broker_cash,
            p, split, dips, pause, broker_best, btc_best, fees_yr, plan,
        }
    }

    pub fn plan_total(&self) -> f64 {
        self.plan.iter().map(|p| p.0).sum()
    }

    /// What a holding of `p.holdings` is.
    pub fn class_of(&self, h: &finance_core::portfolio::Holding) -> Classification {
        self.p.holdings.iter().position(|x| std::ptr::eq(x, h))
            .and_then(|i| self.classes.get(i).copied())
            .unwrap_or_else(|| h.classification())
    }
}

fn model(ledger: &Arc<Ledger>, b: &Arc<Bundle>) -> Rc<Model> {
    let key = (Arc::as_ptr(ledger) as usize, Arc::as_ptr(b) as usize, today());
    MODEL.with(|m| {
        if let Some(cached) = m.borrow().as_ref().filter(|c| c.key == key) {
            return cached.clone();
        }
        let mut built = Model::build(ledger, b, &key.2);
        built.key = key;
        let rc = Rc::new(built);
        *m.borrow_mut() = Some(rc.clone());
        rc
    })
}

// ------------------------------------------------------------ shared bits

/// A section: its title, caption, ⓘ, then the takeaway line.
fn head(title: &str, caption: &str, explain: &str, line: impl Into<SharedString>, cx: &App) -> Div {
    v_flex().gap(px(sp::S))
        .child(ui::section_info(title, caption, explain.to_string(), cx))
        .child(takeaway(line, cx))
}

/// A card title row.
fn card_title(title: &str, right: impl Into<SharedString>, cx: &App) -> Div {
    h_flex().justify_between().gap_2().items_baseline()
        .child(div().text_size(px(ts::TITLE)).font_semibold().whitespace_nowrap().child(title.to_string()))
        .child(ui::caption(right, cx))
}

/// "− 6% +" for a setting.
fn stepper(id: &str, value: String, minus: Rc<dyn Fn()>, plus: Rc<dyn Fn()>, cx: &App) -> Div {
    let t = cx.theme();
    let btn = |suffix: &str, label: &'static str, f: Rc<dyn Fn()>| {
        div().id(SharedString::from(format!("{id}-{suffix}"))).w(px(22.)).h(px(20.)).rounded(px(5.))
            .flex().items_center().justify_center().cursor_pointer()
            .text_size(px(ts::BODY)).text_color(t.muted_foreground)
            .hover(|s| s.bg(t.muted).text_color(t.foreground))
            .on_click(move |_, window, _| {
                f();
                window.refresh();
            })
            .child(label)
    };
    h_flex().items_center().gap(px(2.)).p(px(2.)).rounded(px(7.)).border_1().border_color(ui::hairline(cx))
        .child(btn("minus", "−", minus))
        .child(div().min_w(px(34.)).text_center().text_size(px(ts::CAPTION)).figures().child(value))
        .child(btn("plus", "+", plus))
}

/// The expected-return stepper (shared by Milestones and Buying habits).
fn return_stepper(id: &str, cx: &App) -> Div {
    let r = prefs().return_pct;
    h_flex().gap(px(6.)).items_center()
        .child(ui::caption("assumed return", cx))
        .child(stepper(id, format!("{r:.0}%/yr"),
            Rc::new(|| set_prefs(|p| p.return_pct = (p.return_pct - 1.).max(0.))),
            Rc::new(|| set_prefs(|p| p.return_pct = (p.return_pct + 1.).min(15.))), cx))
}

/// The account's color, the same everywhere on the page.
fn account_color(a: Account, cx: &App) -> Hsla {
    let t = cx.theme();
    match a {
        Account::FlexInvest => t.chart_1,
        Account::Ibkr => t.chart_2,
        Account::Crypto => t.chart_3,
        Account::Revolut => t.chart_4,
    }
}

/// "+2.3%" or "—"
fn pct_opt(v: Option<f64>) -> String {
    v.map(fmt::pct).unwrap_or("—".into())
}

fn tone_opt(v: Option<f64>, cx: &App) -> Hsla {
    v.map(fmt::signed_color).unwrap_or(cx.theme().muted_foreground)
}

/// "in 7 months (May 27)", "now"
fn months_from_now(m: usize, today: &str) -> String {
    if m == 0 {
        return "now".into();
    }
    let when = fmt::month_label(&sim::ym(finance_core::dca::month_index(today) + m as i64));
    let span = if m == 1 { "1 month".to_string() } else if m < 24 { format!("{m} months") } else { format!("{:.1} years", m as f64 / 12.) };
    format!("in {span} ({when})")
}

// ------------------------------------------------------------------- page

/// Fallback for bundles without the server's `twr_pct`: cumulative
/// time-weighted growth in % per month, each month's return modified Dietz,
/// (V1 - V0 - F) / (V0 + F/2), chained. Months with nothing invested count
/// as 0%. On the TOTAL rows this misreads money moving as market moves (coins
/// sent to a wallet whose receive isn't in the ledger yet read as a loss,
/// IBKR history is held at cost); the server computes it per asset class
/// from data that can measure it, see `server_twr`.
fn twr_series(values: &[f64], flows: &[f64]) -> Vec<f64> {
    let mut growth = 1.0;
    let mut out = Vec::with_capacity(values.len());
    for i in 0..values.len() {
        if i > 0 {
            let base = values[i - 1] + flows[i] / 2.;
            if base > 1. && values[i].is_finite() && values[i - 1].is_finite() {
                growth *= 1. + (values[i] - values[i - 1] - flows[i]) / base;
            }
        }
        out.push((growth - 1.) * 100.);
    }
    out
}

/// The server's cumulative time-weighted growth (`twr_pct` / `spx_twr_pct`,
/// % since the first month of history), rebased to the first row of
/// `window` so the chart starts at 0%. None when any row lacks the field
/// (an older bundle) -- the caller falls back to `twr_series`.
fn server_twr(window: &[&serde_json::Value], key: &str) -> Option<Vec<f64>> {
    let cum: Vec<f64> = window.iter().map(|r| r.get(key).and_then(|x| x.as_f64())).collect::<Option<_>>()?;
    let base = 1. + cum.first()? / 100.;
    (base > 0.).then(|| cum.iter().map(|c| ((1. + c / 100.) / base - 1.) * 100.).collect())
}

/// A compact label-over-figure pair used inside the hero.
fn kpi(label: &str, explain: impl Into<SharedString>, value: impl Into<SharedString>, tone: Option<Hsla>, sub: impl Into<SharedString>, cx: &App) -> Div {
    v_flex()
        .gap(px(2.))
        .flex_1()
        .min_w(px(130.))
        .child(ui::eyebrow_info(label.to_string(), explain, cx))
        .child(
            div().text_size(px(17.)).font_semibold().figures().whitespace_nowrap()
                .when_some(tone, |d, c| d.text_color(c))
                .child(value.into()),
        )
        .child(ui::caption(sub, cx))
}

#[allow(clippy::too_many_arguments)]
pub fn render(b: &Arc<Bundle>, ledger: &Arc<Ledger>, tab: Tab, pct: bool, on_tab: OnTab, on_toggle: OnToggle, on_deep: OnDeep, on_go: OnGo, cx: &App) -> ui::Sections {
    let m = model(ledger, b);
    let on = shown;
    // the tab switch, then the open tab's blocks: one list section each
    let page = ui::Sections::new().child(tab_switch(tab, on_tab, cx));
    match tab {
        Tab::Overview => page
            .when(on("hero"), |d| d.child(hero(b, &m, pct, on_toggle, cx)))
            .when(on("accounts"), |d| d.child(holdings::accounts(&m, on_go, cx)))
            .when(on("milestones"), |d| d.child(flows::milestones(&m, cx)))
            .when(on("deep"), |d| d.child(deep_links(on_deep, cx))),
        Tab::Holdings => page
            .when(on("holdings"), |d| d.child(holdings::table(&m, cx)))
            .when(on("movers"), |d| d.child(holdings::movers(&m, cx)))
            .when(on("attribution"), |d| d.child(holdings::attribution(&m, cx))),
        Tab::Contributions => page
            .when(on("contributions"), |d| d.child(flows::contributions(&m, cx)))
            .when(on("streaks"), |d| d.child(jkb::streaks(&m, cx)))
            .when(on("growth"), |d| d.child(jkb::growth(&m, cx))),
        Tab::Income => page
            .when(on("income"), |d| d.child(flows::income(&m, cx)))
            .when(on("costs"), |d| d.child(flows::costs(b, &m, cx))),
        Tab::Habits => page.child(jkb::render(b, &m, cx)),
        Tab::Allocation => page
            .when(on("mix"), |d| d.child(mix::render(&m, cx)))
            .when(on("allocation"), |d| d.children(allocation(b, cx))),
    }
}

/// The tab switch: a segmented control, scrolling sideways when it doesn't
/// fit (always on a phone, where the segments are touch height).
fn tab_switch(tab: Tab, on_tab: OnTab, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let compact = crate::layout::compact();
    let seg = |this: Tab| {
        let on = tab == this;
        let on_tab = on_tab.clone();
        div().id(SharedString::from(format!("inv-tab-{}", this.key())))
            .flex_shrink_0().px(px(12.)).rounded(px(6.)).cursor_pointer().whitespace_nowrap()
            .flex().items_center()
            .when(compact, |d| d.h(px(crate::layout::TOUCH - 8.)).text_size(px(ts::BODY + 1.)))
            .when(!compact, |d| d.py(px(4.)).text_sm())
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| if !on { on_tab(this, w, cx) })
            .child(this.label())
    };
    let bar = h_flex().flex_shrink_0().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx))
        .children(Tab::ALL.map(seg));
    // the bar is the scroll container's direct, unshrinkable child, so its
    // full width is the scroll extent (a wrapper would clip it at the
    // container's width and leave nothing to scroll)
    h_flex().id("inv-tabs").w_full().overflow_x_scroll().restrict_scroll_to_axis().child(bar)
}

/// `FINANCE_DESKTOP_INV_SECTIONS=costs,dips` shows only those sections of
/// the current tab (dev and screenshots: a tab can be taller than a
/// window). Keys: hero, accounts, milestones, deep (Overview); holdings,
/// movers, attribution (Holdings); contributions, streaks, growth
/// (Contributions); income, costs; crossover, dips, cash, time (Just Keep
/// Buying); mix, allocation (Allocation).
fn shown(key: &str) -> bool {
    match std::env::var("FINANCE_DESKTOP_INV_SECTIONS") {
        Ok(v) if !v.trim().is_empty() => v.split(',').any(|k| k.trim().eq_ignore_ascii_case(key)),
        _ => true,
    }
}

fn hero(b: &Bundle, m: &Model, pct: bool, on_toggle: OnToggle, cx: &App) -> Div {
    let t = cx.theme();
    let tot = |k: &str| b.f64(&["investments", "totals", k]).unwrap_or(0.);
    let (value, invested, gain) = (tot("value"), tot("invested"), tot("gain"));
    let irr = b.f64(&["investments", "totals", "irr_pct"]);
    let spx_irr = b.f64(&["investments", "spx", "irr_pct"]);
    let spx_value = b.f64(&["investments", "spx", "value"]);

    // ---------- the chart ----------
    let rows = b.arr(&["investments", "rows"]);
    let window: Vec<&serde_json::Value> = rows.iter().rev().take(24).rev().collect();
    let num = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_f64());
    let flows: Vec<f64> = window.iter().map(|r| num(r, "flow").unwrap_or(0.)).collect();
    let vals: Vec<f64> = window.iter().map(|r| num(r, "value").unwrap_or(f64::NAN)).collect();
    let spxs: Vec<f64> = window.iter().map(|r| num(r, "spx_value").unwrap_or(f64::NAN)).collect();
    let you_pct = server_twr(&window, "twr_pct").unwrap_or_else(|| twr_series(&vals, &flows));
    let spx_pct = server_twr(&window, "spx_twr_pct").unwrap_or_else(|| twr_series(&spxs, &flows));
    let months: Vec<Month> = window.iter().enumerate()
        .filter_map(|(i, r)| Some(Month {
            label: fmt::month_label(r.get("month")?.as_str()?).into(),
            value: if pct { you_pct[i] } else { vals[i] },
            spx: if pct { if spxs[i].is_finite() { spx_pct[i] } else { f64::NAN } } else { spxs[i] },
        }))
        .collect();
    let growth_now = (you_pct.last().copied(), spx_pct.last().copied());
    let has_spx = months.iter().any(|m| m.spx.is_finite());
    let (c_you, c_spx) = (t.primary, t.muted_foreground);
    let mut chart = AreaChart::new(months).appear(false)
        .x(|m: &Month| m.label.clone())
        .y(|m: &Month| m.value)
        .stroke(c_you)
        .fill(ui::soft_fill(c_you, 0.26))
        .name("Portfolio");
    if has_spx {
        chart = chart
            .y(|m: &Month| if m.spx.is_finite() { m.spx } else { m.value })
            .stroke(c_spx.opacity(0.8))
            .fill(c_spx.opacity(0.))
            .name(fmt::local("S&P 500, same flows"));
    }
    let chart = chart
        .natural()
        .y_axis(true)
        .y_tick_count(4)
        .x_tick_count(4)
        .y_tick_format(move |v| if pct { format!("{v:+.0}%") } else { fmt::compact(v) })
        .tooltip_value(move |_, _, v| if pct { fmt::pct(v).into() } else { fmt::hkd(v).into() });
    let seg = |label: &'static str, on: bool, id: &'static str| {
        let on_toggle = on_toggle.clone();
        div().id(id).px(px(8.)).py(px(2.)).rounded(px(5.)).text_size(px(ts::CAPTION)).cursor_pointer()
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| if !on { on_toggle(w, cx) })
            .child(label)
    };
    let toggle = h_flex().p(px(2.)).gap(px(2.)).rounded(px(7.)).border_1().border_color(ui::hairline(cx))
        .child(seg(fmt::local_static("HK$"), !pct, "inv-hkd"))
        .child(seg("%", pct, "inv-pct"));

    let headline = v_flex()
        .flex_1()
        .min_w(px(260.))
        .gap(px(10.))
        .child(ui::eyebrow("Worth now", cx))
        .child(
            h_flex()
                .items_baseline()
                .gap(px(6.))
                .child(div().text_size(px(16.)).text_color(t.muted_foreground).child(fmt::sym()))
                .child(div().text_size(px(ts::HERO)).line_height(px(40.)).font_semibold().figures().text_color(ui::hero_color(cx)).child(fmt::int(value))),
        )
        .child(
            h_flex().gap_2().flex_wrap()
                .child(ui::delta_over(tot("roi_pct"), "total return", cx))
                .child(ui::caption(format!("{} put in", fmt::hkd(invested)), cx)),
        );
    let kpis = h_flex()
        .gap(px(sp::XL))
        .flex_wrap()
        .mt(px(sp::L))
        .pt(px(sp::L))
        .border_t_1()
        .border_color(ui::hairline(cx))
        .child(kpi("Gain", gain_info(m),  fmt::signed_hkd(gain), Some(fmt::signed_color(gain)), format!("{} of the money in", fmt::pct(tot("roi_pct"))), cx))
        .child(kpi("Money-weighted", fmt::local_static("Money-weighted return (IRR): one yearly rate that accounts for when each deposit was made. The S&P 500 figure applies your same deposits on the same dates."), irr.map(|v| format!("{} /yr", fmt::pct(v))).unwrap_or("—".into()), irr.map(fmt::signed_color),
            spx_irr.map(|s| fmt::local(&format!("S&P 500: {} /yr", fmt::pct(s)))).unwrap_or_default(), cx))
        .child(kpi(fmt::local_static("vs S&P 500"), fmt::local_static("Your portfolio's value minus what the same deposits, made on the same dates, would be worth in the S&P 500 index today."), spx_value.map(|s| fmt::signed_hkd(value - s)).unwrap_or("—".into()),
            spx_value.map(|s| fmt::signed_color(value - s)),
            spx_value.map(|s| format!("the index: {}", fmt::hkd(s))).unwrap_or_default(), cx));

    // ---------- time-weighted returns against the benchmarks ----------
    let first = rows.first().and_then(|r| r.get("month")?.as_str()).map(fmt::month_label).unwrap_or_default();
    let mut bench: Vec<(&str, Periods)> = vec![("Your portfolio", m.p.twr), (fmt::local_static("S&P 500"), m.p.spx)];
    if let Some(w) = m.p.msci {
        bench.push(("MSCI World (URTH)", w));
    }
    let cell = |v: Option<f64>, strong: bool| div().w(px(88.)).flex_shrink_0().text_right().figures()
        .when(strong, |d| d.font_weight(FontWeight::MEDIUM))
        .text_color(tone_opt(v, cx)).child(pct_opt(v));
    let grid = v_flex().gap(px(4.)).text_size(px(ts::BODY))
        .child(h_flex().gap(px(sp::S))
            .child(div().flex_1().min_w(px(110.)).child(ui::eyebrow("Time-weighted", cx)))
            .child(div().w(px(88.)).flex_shrink_0().text_right().child(ui::eyebrow("Month", cx)))
            .child(div().w(px(88.)).flex_shrink_0().text_right().child(ui::eyebrow("YTD", cx)))
            .child(div().w(px(88.)).flex_shrink_0().text_right().child(ui::eyebrow(format!("Since {first}"), cx))))
        .children(bench.iter().enumerate().map(|(i, (name, p))| h_flex().gap(px(sp::S))
            .child(div().flex_1().min_w(px(110.)).overflow_hidden().text_ellipsis().whitespace_nowrap()
                .text_color(if i == 0 { t.foreground } else { t.muted_foreground }).child(name.to_string()))
            .child(cell(p.month, i == 0)).child(cell(p.ytd, i == 0)).child(cell(p.since, i == 0))));
    let ytd_gap = m.p.twr.ytd.zip(m.p.spx.ytd).map(|(a, s)| a - s);
    let sym = fmt::sym();
    let bench_name = finance_core::base::get().benchmark_label();
    let line = format!(
        "{sym} {} is invested, {sym} {} above the {sym} {} put in. Money-weighted, that is {} a year against {} for the {bench_name} on the same dates{}.",
        fmt::compact(value), fmt::compact(gain), fmt::compact(invested),
        irr.map(fmt::pct).unwrap_or("—".into()), spx_irr.map(fmt::pct).unwrap_or("—".into()),
        ytd_gap.map(|g| format!("; this year, time-weighted, {} points {} it", format!("{:.1}", g.abs()), if g >= 0. { "ahead of" } else { "behind" })).unwrap_or_default());

    v_flex().gap(px(sp::S))
        .child(takeaway(line, cx))
        .child(ui::card(cx).p(px(20.)).child(
            h_flex()
                .flex_wrap()
                .items_stretch()
                .gap(px(sp::XL))
                .child(headline)
                .child(
                    v_flex()
                        .flex_1()
                        .flex_grow(1.6)
                        .min_w(crate::layout::col(300.))
                        .min_h(px(176.))
                        .gap_1()
                        .child(
                            h_flex().gap(px(sp::M)).flex_wrap().items_center()
                                .child(ui::legend(c_you, "Portfolio",
                                    if pct { growth_now.0.map(fmt::pct).unwrap_or_default() } else { String::new() }, cx))
                                .when(has_spx, |d| d.child(ui::legend(c_spx, fmt::local("S&P 500, same flows"),
                                    if pct { growth_now.1.map(fmt::pct).unwrap_or_default() } else { String::new() }, cx)))
                                .child(div().flex_1())
                                .when(pct, |d| d.child(ui::caption("time-weighted, deposits removed · 2 years", cx))
                                    .child(ui::info_for(ui::ts::CAPTION, "twr-info", fmt::local(&format!("Time-weighted growth: each month's market return with deposits, withdrawals and moves between your own wallets removed{}, chained together — what HK$ 100 held from the start would have become. It judges how the investments did, independent of how much you deposited. The S&P 500 line gets the same deposits.",
                                    { let v = words(&m.mine(&[(Account::Crypto, "crypto from coin prices"), (Account::Ibkr, "the broker from its daily NAV"), (Account::FlexInvest, "funds from units × NAV")]), "and");
                                      if v.is_empty() { String::new() } else { format!(" ({v})") } })), cx)))
                                .child(toggle),
                        )
                        // fixed chart height: at half-screen width the legend row wraps
                        // and must push the card taller, not squash the chart
                        .child(div().h(px(140.)).child(chart)),
                ),
        ).child(kpis)
        .child(div().mt(px(sp::L)).pt(px(sp::M)).border_t_1().border_color(ui::hairline(cx))
            .child(div().id("inv-bench").w_full().overflow_x_scroll().restrict_scroll_to_axis().child(div().min_w(px(340.)).child(grid))))
        .child(div().mt(px(6.)).child(crate::pages::deep::note(
            fmt::local("Time-weighted strips out the timing of deposits: it is how HK$ 100 held from the start did. The S&P 500 row gets your same deposits; MSCI World is URTH's price return (dividends aside), shown when the replica has its prices."), cx))))
}

/// Every investment in one allocation and exposure view: the broker, the
/// fund valued through an index proxy (looked through as that index), the
/// coins.
fn allocation(b: &Bundle, cx: &App) -> Option<Div> {
    use crate::pages::deep::exposure as x;
    let positions = x::all_positions(b);
    if positions.is_empty() {
        return None;
    }
    let e = finance_core::exposure::exposure(&positions);
    let share = |label: &str, s: &[(String, f64, f64)]| s.iter().find(|r| r.0 == label).map(|r| r.2).unwrap_or(0.);
    let fund = positions.iter().filter(|p| p.kind == finance_core::exposure::Kind::Fund).map(|p| p.value_hkd).sum::<f64>();
    let base = fmt::base_ccy();
    let name = finance_core::base::get().fund_name();
    let line = format!(
        "Across everything invested, about {:.0}% sits in US companies once the funds are looked through, {:.0}% is quoted in {base}{}.",
        share("United States", &e.by_region), share(&base, &e.by_currency),
        if fund > 0. && !name.is_empty() { format!(", and the {name} fund is {:.0}% of the total", if e.total_hkd > 0. { fund / e.total_hkd * 100. } else { 0. }) } else { String::new() });
    let funds = if name.is_empty() { String::new() } else { format!(" (the {name} fund as its index proxy)") };
    Some(v_flex().gap(px(sp::GRID))
        .child(head("Allocation and exposure", "every account together",
            &format!("Trading currency is what each holding is quoted in; the region and sector bars look through the funds with approximate index weights{funds}. Broker cash counts as cash. Broker › Deep dive › Exposure has the detail and the overlap check."), line, cx))
        .child(x::cards(&e, cx)))
}

/// Links into Broker › Deep dive, one card per section.
fn deep_links(on_deep: OnDeep, cx: &App) -> Div {
    use crate::pages::deep::Section;
    let t = cx.theme();
    v_flex().gap(px(sp::GRID))
        .child(ui::section("Deep dive", "the broker holdings, one question per section", cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch().children(Section::ALL.into_iter().map(|s| {
            let on_deep = on_deep.clone();
            ui::card_interactive(SharedString::from(format!("deep-link-{}", s.label())), cx)
                .flex_1().min_w(crate::layout::col(220.)).gap(px(4.))
                .on_click(move |_, w, cx| on_deep(s, w, cx))
                .child(h_flex().justify_between().items_center()
                    .child(div().text_size(px(ts::TITLE)).font_semibold().child(s.label()))
                    .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child("→")))
                .child(div().text_size(px(ts::CAPTION)).line_height(px(17.)).text_color(t.muted_foreground).child(s.blurb()))
        })))
}

#[cfg(test)]
mod tests {
    use super::{months_from_now, server_twr, twr_series, Config, Tab};

    #[test]
    fn tabs_from_keys() {
        for t in Tab::ALL {
            assert_eq!(Tab::parse(t.key()), Some(t));
            assert_eq!(Tab::parse(t.label()), Some(t));
        }
        assert_eq!(Tab::parse("Exposure"), Some(Tab::Allocation));
        assert_eq!(Tab::parse("nope"), None);
        assert_eq!((Tab::parse("jkb"), Tab::parse("Just Keep Buying")), (Some(Tab::Habits), Some(Tab::Habits)), "the old name");
    }

    #[test]
    fn mix_target_in_the_config() {
        let c: Config = toml::from_str("include_bank_cash = true\n[mix]\nequity = 80\nbond = 20\n").unwrap();
        assert_eq!(c.include_bank_cash, Some(true));
        assert_eq!(c.mix.get("bond"), Some(&20.));
        assert_eq!(toml::from_str::<Config>(&toml::to_string(&c).unwrap()).unwrap(), c);
    }

    #[test]
    fn server_twr_rebases_to_window() {
        let rows: Vec<serde_json::Value> = [10., 21., 32.]
            .iter().map(|c| serde_json::json!({ "twr_pct": c })).collect();
        let w: Vec<&serde_json::Value> = rows.iter().collect();
        let g = server_twr(&w, "twr_pct").unwrap();
        assert!(g[0].abs() < 1e-9 && (g[1] - 10.).abs() < 1e-9 && (g[2] - 20.).abs() < 1e-9);
        // a row without the field (older bundle) -> fall back
        let old = [serde_json::json!({ "twr_pct": 1. }), serde_json::json!({})];
        assert!(server_twr(&old.iter().collect::<Vec<_>>(), "twr_pct").is_none());
    }

    #[test]
    fn deposits_are_not_growth() {
        // 100 in, then 100 more with no market move: 0% growth
        assert!(twr_series(&[100., 200.], &[100., 100.])[1].abs() < 1e-9);
        // a 10% month after the deposit
        let g = twr_series(&[100., 110.], &[100., 0.]);
        assert!((g[1] - 10.).abs() < 1e-9);
    }

    #[test]
    fn config_file() {
        let c: Config = toml::from_str("return_pct = 5\n[plan]\nflexinvest = 10000\n").unwrap();
        assert_eq!(c.return_pct, Some(5.));
        assert_eq!(c.buffer_months, None);
        assert_eq!(c.plan.get("flexinvest"), Some(&10000.));
        assert_eq!(toml::from_str::<Config>(&toml::to_string(&c).unwrap()).unwrap(), c);
    }

    #[test]
    fn month_phrases() {
        assert_eq!(months_from_now(0, "2026-10-03"), "now");
        assert_eq!(months_from_now(3, "2026-10-03"), "in 3 months (Jan 27)");
        assert_eq!(months_from_now(1, "2026-10-03"), "in 1 month (Nov 26)");
        assert_eq!(months_from_now(30, "2026-10-03"), "in 2.5 years (Apr 29)");
    }
}

/// The Gain tile's explainer, naming the accounts the user has.
fn gain_info(m: &Model) -> String {
    let across = words(&m.mine(&[(Account::FlexInvest, "FlexInvest"), (Account::Ibkr, "IBKR"), (Account::Crypto, "crypto"), (Account::Revolut, "the Revolut pot")]), "and");
    format!("Value now minus everything put in (net of withdrawals){}. Includes gains not yet sold.",
        if across.is_empty() { String::new() } else { format!(", across {across}") })
}
