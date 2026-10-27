//! Cash flow: a calendar of what's expected to come in and go out, this
//! month or next. The recurring series finance_core::recurring detects are
//! projected forward (salary, the card bill, the student loan, subscriptions);
//! commitments the data can't show yet (a new lease) come from a small config
//! file, editable here. Under the grid: the projected cash balance day by day,
//! with the days it dips under a threshold, and the next 30 days as a list.
//!
//! Its own view (like Upload): it keeps the editor's inputs and the config.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Datelike, Duration, NaiveDate};
use finance_core::recurring::{self, Manual, Origin, Outlook, Planned, Series};
use finance_core::{Bundle, Ledger};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::fmt;
use crate::linechart::{self, Chart, Dot};
use crate::ui::{self, Figures as _};
use crate::zoom::px;

/// A payment at least this big is "the next big payment" on Overview.
pub const BIG_PAYMENT_HKD: f64 = 2_000.;
/// Days listed under the grid, and on the Overview tile.
pub const HORIZON_DAYS: i64 = 30;
/// Items shown in one day cell before "+N more".
const PER_CELL: usize = 3;

fn default_low() -> f64 {
    50_000.
}

/// `~/.config/finance-desktop/calendar.toml` (next to theme.toml).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// flag the days the projected balance dips under this (HKD)
    #[serde(default = "default_low")]
    pub low_balance: f64,
    /// `net_worth.by_source` keys the balance follows (e.g. ["hsbc"]);
    /// empty = all bank cash
    #[serde(default)]
    pub accounts: Vec<String>,
    /// commitments the ledger can't see yet
    #[serde(default, rename = "entry")]
    pub entries: Vec<Manual>,
}

impl Default for Config {
    fn default() -> Self {
        Config { low_balance: default_low(), accounts: Vec::new(), entries: Vec::new() }
    }
}

/// The user_settings key.
pub const KEY: &str = "calendar";

pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("FINANCE_DESKTOP_CALENDAR") {
        return PathBuf::from(p); // dev/screenshots: a calendar file of its own
    }
    dirs::config_dir().unwrap_or_default().join("finance-desktop").join("calendar.toml")
}

const HEADER: &str = "# anser cash-flow calendar (edited by the app; hand edits are fine).\n\
# low_balance: flag days the projected cash dips under this (HKD).\n\
# accounts: by_source keys the balance follows, e.g. [\"my_bank\"]; empty = all bank cash.\n\
# [[entry]]: label, amount (HKD, negative = out), day, from/until (\"YYYY-MM\"),\n\
#   every (\"monthly\" or \"once\"), category (the entry steps aside once an active\n\
#   detected series of that category exists), account (a by_source key).\n\n";

impl Config {
    /// The config, or the default and why it couldn't be read.
    pub fn load() -> (Self, Option<String>) {
        if let Some(v) = finance_core::user_settings::get(KEY) {
            return match serde_json::from_value(v) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(format!("the server's calendar has an error, using defaults: {e}"))),
            };
        }
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(format!("{} has an error, using defaults: {e}", path.display()))),
            },
            Err(_) => (Config::default(), None),
        }
    }

    /// To the server when it keeps settings, else the local file. Returns
    /// where it went.
    pub fn save(&self) -> Result<String, String> {
        if finance_core::user_settings::save(KEY, serde_json::to_value(self).map_err(|e| e.to_string())?) {
            return Ok("the server".into());
        }
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let body = toml::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, format!("{HEADER}{body}")).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }
}

struct Data {
    ledger: Arc<Ledger>,
    bundle: Arc<Bundle>,
    series: Arc<Vec<Series>>,
}

/// A ledger row to show in the detail sheet.
pub struct Pick(pub usize);

pub struct CashFlow {
    data: Option<Data>,
    config: Config,
    note: Option<String>,
    /// 0 = this month, 1 = next month
    month: i32,
    form_open: bool,
    money_in: bool,
    f_label: Entity<InputState>,
    f_amount: Entity<InputState>,
    f_day: Entity<InputState>,
    f_from: Entity<InputState>,
    f_until: Entity<InputState>,
    f_category: Entity<InputState>,
    f_low: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Pick> for CashFlow {}

impl CashFlow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (config, note) = Config::load();
        let input = |ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let f_low = cx.new(|cx| InputState::new(window, cx).placeholder("50000").default_value(fmt::int(config.low_balance).replace(',', "")));
        let subs = vec![cx.subscribe_in(&f_low, window, |this, _, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.set_low(cx);
            }
        })];
        CashFlow {
            data: None,
            note,
            // dev/screenshot aid: FINANCE_DESKTOP_CASHFLOW_MONTH=next
            month: if std::env::var("FINANCE_DESKTOP_CASHFLOW_MONTH").is_ok_and(|v| v == "next") { 1 } else { 0 },
            form_open: std::env::var_os("FINANCE_DESKTOP_CASHFLOW_FORM").is_some(),
            money_in: false,
            f_label: input("Label, e.g. Rent", window, cx),
            f_amount: input(&fmt::local("Amount, HK$"), window, cx),
            f_day: input("Day (1–31)", window, cx),
            f_from: input("From YYYY-MM", window, cx),
            f_until: input("Until YYYY-MM (optional)", window, cx),
            f_category: input("Category (optional), e.g. Rent", window, cx),
            f_low,
            config,
            _subs: subs,
        }
    }

    pub fn set_data(&mut self, ledger: Arc<Ledger>, bundle: Arc<Bundle>, series: Arc<Vec<Series>>, cx: &mut Context<Self>) {
        self.data = Some(Data { ledger, bundle, series });
        // the server's calendar, shared by every device (user_settings)
        if finance_core::user_settings::server_side() {
            let (config, note) = Config::load();
            self.config = config;
            if note.is_some() {
                self.note = note;
            }
        }
        cx.notify();
    }

    /// Expected in / out over the next HORIZON_DAYS, for the Overview tile.
    pub fn outlook(&self) -> Option<Outlook> {
        let d = self.data.as_ref()?;
        let today = chrono::Local::now().date_naive();
        let p = self.filtered(recurring::plan(&d.series, &self.config.entries, today, today + Duration::days(HORIZON_DAYS)));
        Some(recurring::outlook(&p, BIG_PAYMENT_HKD))
    }

    /// Only the accounts the balance follows (all of them by default).
    fn filtered(&self, plan: Vec<Planned>) -> Vec<Planned> {
        if self.config.accounts.is_empty() {
            return plan;
        }
        plan.into_iter().filter(|p| p.account.is_empty() || self.config.accounts.contains(&p.account) || !p.affects_cash).collect()
    }

    /// Cash today: all bank balances, or the configured accounts'.
    fn cash_today(&self, b: &Bundle) -> f64 {
        if self.config.accounts.is_empty() {
            return b.f64(&["net_worth", "banks_hkd"]).unwrap_or(0.);
        }
        b.arr(&["net_worth", "by_source"]).iter()
            .filter(|s| s.get("key").and_then(|k| k.as_str()).is_some_and(|k| self.config.accounts.iter().any(|a| a == k)))
            .filter_map(|s| s.get("hkd_value").and_then(|v| v.as_f64()))
            .sum()
    }

    fn save(&mut self, what: &str, cx: &mut Context<Self>) {
        self.note = Some(match self.config.save() {
            Ok(p) => format!("{what}. Saved to {p}"),
            Err(e) => format!("Couldn't save: {e}"),
        });
        cx.notify();
    }

    fn set_low(&mut self, cx: &mut Context<Self>) {
        let raw = self.f_low.read(cx).value().replace([',', ' '], "").replace(&fmt::sym(), "");
        match raw.parse::<f64>() {
            Ok(v) if v >= 0. => {
                self.config.low_balance = v;
                self.save(&format!("Threshold set to {}", fmt::hkd(v)), cx);
            }
            _ => {
                self.note = Some(fmt::local("The threshold must be a number of HK$"));
                cx.notify();
            }
        }
    }

    fn add_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let val = |s: &Entity<InputState>, cx: &App| s.read(cx).value().trim().to_string();
        let label = val(&self.f_label, cx);
        let amount = val(&self.f_amount, cx).replace([',', ' ', '-', '+'], "").replace(&fmt::sym(), "").parse::<f64>().ok();
        let day = val(&self.f_day, cx).parse::<u32>().ok().filter(|d| (1..=31).contains(d));
        let ym = |s: String| -> Result<Option<String>, ()> {
            if s.is_empty() {
                return Ok(None);
            }
            NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d").map(|_| Some(s)).map_err(|_| ())
        };
        let from = ym(val(&self.f_from, cx));
        let until = ym(val(&self.f_until, cx));
        let category = val(&self.f_category, cx);
        let err = if label.is_empty() {
            Some("Give it a label")
        } else if amount.is_none_or(|a| a <= 0.) {
            Some("The amount must be a positive number (choose in or out with the switch)")
        } else if day.is_none() {
            Some("The day must be 1–31")
        } else if from.is_err() || until.is_err() {
            Some("Months are YYYY-MM, e.g. 2026-12")
        } else {
            None
        };
        if let Some(e) = err {
            self.note = Some(e.into());
            cx.notify();
            return;
        }
        let amount = amount.unwrap_or(0.) * if self.money_in { 1. } else { -1. };
        self.config.entries.push(Manual {
            label: label.clone(),
            amount,
            day: day.unwrap_or(1),
            from: from.unwrap_or(None),
            until: until.unwrap_or(None),
            every: None,
            category: (!category.is_empty()).then_some(category),
            account: None,
        });
        for s in [&self.f_label, &self.f_amount, &self.f_day, &self.f_from, &self.f_until, &self.f_category] {
            s.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.form_open = false;
        self.save(&format!("Added \"{label}\""), cx);
    }

    fn remove_entry(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.config.entries.len() {
            let gone = self.config.entries.remove(i);
            self.save(&format!("Removed \"{}\"", gone.label), cx);
        }
    }
}

/// One thing on a day: expected, or (earlier this month) already paid.
#[derive(Clone)]
struct Item {
    label: String,
    cash: f64,
    /// moved with the card bill, not on the day
    on_card: bool,
    estimate: bool,
    done: bool,
    /// a ledger row for the detail sheet
    row: Option<usize>,
}

fn month_start(today: NaiveDate, offset: i32) -> NaiveDate {
    recurring::add_months(today, offset, 1)
}

impl Render for CashFlow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data) = self.data.as_ref() else {
            return v_flex().child(ui::empty("No data yet — sync first", cx)).into_any_element();
        };
        let t = cx.theme().clone();
        let today = chrono::Local::now().date_naive();
        let start = month_start(today, self.month);
        let end = recurring::add_months(start, 1, 1) - Duration::days(1);
        let horizon = today + Duration::days(HORIZON_DAYS);
        let series = data.series.clone();
        let ledger = data.ledger.clone();

        // what's expected from today to the later of the month's end and the horizon
        let plan = self.filtered(recurring::plan(&series, &self.config.entries, today, end.max(horizon)));
        let cash = self.cash_today(&data.bundle);
        let path = recurring::balance_path(cash, &plan, today, end);
        let low = self.config.low_balance;
        let in_view: Vec<&(NaiveDate, f64)> = path.iter().filter(|(d, _)| *d >= start).collect();
        let low_point = in_view.iter().min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| **x);
        let dips: Vec<NaiveDate> = in_view.iter().filter(|(_, v)| *v < low).map(|(d, _)| *d).collect();
        let bal_on = |d: NaiveDate| path.iter().find(|(x, _)| *x == d).map(|x| x.1);

        // ---- the items of each day ----
        let row_of = |o: &Origin| match o {
            Origin::Series(i) => series.get(*i).and_then(|s| s.rows.first().copied()),
            Origin::Manual(_) => None,
        };
        let mut items: Vec<(NaiveDate, Item)> = plan.iter().filter(|p| p.date >= start && p.date <= end).map(|p| (p.date, Item {
            label: p.label.clone(), cash: p.cash_hkd, on_card: !p.affects_cash, estimate: p.estimate, done: false,
            row: row_of(&p.origin),
        })).collect();
        // earlier this month: the recurring payments that already happened
        for s in series.iter() {
            for &r in &s.rows {
                let e = &ledger.entries[r];
                let Some(d) = recurring::day(&e.date) else { continue };
                if d >= start && d < today && d <= end {
                    items.push((d, Item {
                        label: s.name.clone(), cash: e.hkd.unwrap_or(0.), on_card: s.on_card(), estimate: false,
                        done: true, row: Some(r),
                    }));
                }
            }
        }
        items.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cash.abs().total_cmp(&a.1.cash.abs())));
        let cash_items = |done: bool| items.iter().filter(move |(_, i)| !i.on_card && i.done == done);
        let exp_out: f64 = cash_items(false).filter(|(_, i)| i.cash < 0.).map(|(_, i)| -i.cash).sum();
        let exp_in: f64 = cash_items(false).filter(|(_, i)| i.cash > 0.).map(|(_, i)| i.cash).sum();

        // ---- header: month switch, threshold ----
        let this_label = start.format("%B %Y").to_string();
        let seg = |label: String, m: i32, cx: &mut Context<Self>| {
            let on = self.month == m;
            div().id(("cf-month", m as usize)).px(px(12.)).py(px(4.)).rounded(px(6.)).text_sm().cursor_pointer().whitespace_nowrap()
                .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| { this.month = m; cx.notify(); }))
                .child(label)
        };
        let switch = h_flex().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx))
            .child(seg(format!("This month · {}", month_start(today, 0).format("%b")), 0, cx))
            .child(seg(format!("Next month · {}", month_start(today, 1).format("%b")), 1, cx));
        let threshold = h_flex().gap_2().items_center()
            .child(ui::caption(fmt::local("Flag days under HK$"), cx))
            .child(div().w(px(110.)).child(Input::new(&self.f_low).small()))
            .child(Button::new("cf-low").xsmall().ghost().label("Set").on_click(cx.listener(|this, _, _, cx| this.set_low(cx))));
        let header = h_flex().justify_between().flex_wrap().gap_3().child(switch).child(threshold);

        // ---- tiles ----
        let low_txt = low_point.map(|(d, v)| (fmt::hkd(v), format!("on {}", d.format("%-d %b")))).unwrap_or(("—".into(), String::new()));
        let tiles = h_flex().gap_4().flex_wrap().items_stretch().children([
            ui::stat_info("Expected out", fmt::hkd(exp_out), format!("{} · from today", start.format("%b")), Some(fmt::money_out()),
                "Recurring charges paid from a bank account, the card bill, and your own entries, from today to the end of the month. Card charges ride on the card bill, so they're listed but not counted twice.", cx),
            ui::stat("Expected in", fmt::hkd(exp_in), "salary and repayments", Some(fmt::money_in()), cx),
            ui::stat_info("Low point", low_txt.0, low_txt.1,
                if dips.is_empty() { None } else { Some(t.warning) },
                "The lowest projected end-of-day balance in the month: today's cash plus everything expected until then. Amounts that vary (pay, the card bill) use the median of recent months.", cx),
            ui::stat("Cash today", fmt::hkd(cash),
                if self.config.accounts.is_empty() { "all bank accounts".into() } else { self.config.accounts.join(", ") }, None, cx),
        ].map(|d| d.min_w(px(140.))));

        // ---- the grid ----
        let first_col = start.weekday().num_days_from_monday() as i64;
        let weeks = ((first_col + end.day() as i64) as f64 / 7.).ceil() as i64;
        let head = h_flex().gap_1().children(["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].map(|d| {
            div().flex_1().min_w_0().px_1().text_xs().text_color(t.muted_foreground).child(d)
        }));
        let mut grid = v_flex().gap_1().child(head);
        for w in 0..weeks {
            let mut week = h_flex().gap_1().items_stretch();
            for c in 0..7 {
                let k = w * 7 + c - first_col;
                let d = start + Duration::days(k);
                if k < 0 || d > end {
                    week = week.child(div().flex_1().min_w_0().p(px(5.)).border_1().border_color(transparent_black()));
                    continue;
                }
                let todays: Vec<&Item> = items.iter().filter(|(x, _)| *x == d).map(|(_, i)| i).collect();
                let past = d < today;
                let dip = !past && bal_on(d).is_some_and(|b| b < low);
                let more = todays.len().saturating_sub(PER_CELL);
                let cell = v_flex()
                    .id(("cf-day", k as usize))
                    .flex_1()
                    .min_w_0()
                    .min_h(px(86.))
                    .p(px(5.))
                    .gap(px(3.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if d == today { t.primary } else { ui::hairline(cx) })
                    .bg(if dip { t.warning.opacity(0.12) } else if past { t.background.opacity(0.0) } else { t.secondary })
                    .when(past, |e| e.opacity(0.6))
                    .child(h_flex().justify_between()
                        .child(div().text_xs().font_weight(if d == today { FontWeight::BOLD } else { FontWeight::NORMAL })
                            .text_color(if d == today { t.primary } else { t.muted_foreground }).child(d.day().to_string()))
                        .when(dip, |e| e.child(div().text_size(px(10.)).text_color(t.warning).child("low"))))
                    .children(todays.iter().take(PER_CELL).enumerate().map(|(j, it)| {
                        let color = if it.cash >= 0. { fmt::money_in() } else if it.on_card { t.muted_foreground } else { fmt::money_out() };
                        let amount = format!("{}{}", if it.estimate { "≈" } else { "" }, fmt::compact(it.cash.abs()));
                        let row = it.row;
                        h_flex().id(("cf-it", k as usize * 16 + j)).gap_1().min_w_0().text_size(px(10.5)).rounded(px(3.))
                            .when(row.is_some(), |e| e.cursor_pointer().hover(|s| s.bg(t.accent)))
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| if let Some(r) = row { cx.emit(Pick(r)) }))
                            .child(div().w(px(3.)).h(px(12.)).flex_shrink_0().rounded(px(1.)).bg(color))
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap()
                                .when(it.done, |e| e.text_color(t.muted_foreground)).child(it.label.clone()))
                            .child(div().flex_shrink_0().figures().text_color(color).child(amount))
                    }))
                    .when(more > 0, |e| e.child(div().text_size(px(10.)).text_color(t.muted_foreground).child(format!("+{more} more"))));
                week = week.child(cell);
            }
            grid = grid.child(week);
        }
        let legend = h_flex().gap_4().flex_wrap()
            .child(ui::legend(fmt::money_out(), "out", "", cx))
            .child(ui::legend(fmt::money_in(), "in", "", cx))
            .child(ui::legend(t.muted_foreground, "on the card (paid with the card bill)", "", cx))
            .child(ui::legend(t.warning, format!("projected under {}", fmt::hkd(low)), "", cx));
        let grid_card = ui::card(cx).gap_2().child(grid).child(legend);

        // ---- balance line ----
        let days: Vec<NaiveDate> = (0..=(end - start).num_days()).map(|k| start + Duration::days(k)).collect();
        let ys: Vec<Option<f64>> = days.iter().map(|d| bal_on(*d)).collect();
        let near = low_point.is_some_and(|(_, v)| v < low * 1.5);
        let mut lines = vec![linechart::Series::line("Projected cash", ys, t.chart_2).width(2.)];
        if near {
            lines.push(linechart::Series::line("Threshold", vec![Some(low); days.len()], t.warning).dashed());
        }
        let dots: Vec<Dot> = days.iter().enumerate()
            .filter(|(_, d)| dips.contains(*d))
            .filter_map(|(x, d)| Some(Dot { x: x as f32, y: bal_on(*d)?, r: 3., color: t.warning }))
            .collect();
        let chart = Chart {
            id: format!("cf-balance-{}", self.month).into(),
            labels: days.iter().map(|d| d.format("%a %-d %b").to_string().into()).collect(),
            x_ticks: days.iter().enumerate().filter(|(_, d)| d.day() == 1 || d.day() % 7 == 1 && d.day() < 29)
                .map(|(x, d)| (x, d.format("%-d %b").to_string().into())).collect(),
            series: lines,
            dots,
            log: false,
            height: 190.,
            y_fmt: std::rc::Rc::new(fmt::compact),
        };
        let caption = match (dips.first(), low_point) {
            (Some(d), _) => format!("under {} from {}", fmt::compact(low), d.format("%-d %b")),
            (None, Some((d, v))) => format!("lowest {} on {} · stays above {}", fmt::compact(v), d.format("%-d %b"), fmt::compact(low)),
            _ => String::new(),
        };
        let balance = ui::card(cx).gap_2()
            .child(h_flex().gap_2().items_baseline().flex_wrap()
                .child(div().text_size(px(ui::ts::TITLE)).font_semibold().child("Projected cash"))
                .child(ui::caption(format!("{this_label} · {caption}"), cx)))
            .child(linechart::render(chart, cx));

        // ---- the next 30 days ----
        let upcoming: Vec<&Planned> = plan.iter().filter(|p| p.date <= horizon).collect();
        let list = ui::card(cx).p(px(6.)).gap_0().children(upcoming.iter().enumerate().map(|(k, p)| {
            let row = row_of(&p.origin);
            let color = if p.cash_hkd >= 0. { fmt::money_in() } else if p.affects_cash { fmt::money_out() } else { t.muted_foreground };
            let acct = match p.origin {
                Origin::Series(i) => series.get(i).map(|s| s.account_label()).unwrap_or_default(),
                Origin::Manual(_) => "your entry".into(),
            };
            let amount = format!("{}{}", if p.estimate { "≈ " } else { "" }, fmt::signed_hkd(p.cash_hkd));
            h_flex().id(("cf-up", k)).gap_3().px(px(10.)).py(px(6.)).rounded(t.radius)
                .when(row.is_some(), |e| e.cursor_pointer().hover(|s| s.bg(t.accent)))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| if let Some(r) = row { cx.emit(Pick(r)) }))
                .child(div().w(px(76.)).flex_shrink_0().text_xs().text_color(t.muted_foreground).child(p.date.format("%a %-d %b").to_string()))
                .when_some(crate::logo::tile(crate::logo::for_text(&p.label), 22., cx), |d, l| d.child(l))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().text_size(px(ui::ts::BODY)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(p.label.clone()))
                    .child(ui::caption(format!("{}{}{}", acct, if p.category.is_empty() { String::new() } else { format!(" · {}", p.category) },
                        if p.affects_cash { "" } else { " · paid with the card bill" }), cx)))
                .child(div().flex_shrink_0().text_size(px(ui::ts::BODY)).figures().whitespace_nowrap().text_color(color).child(amount))
        })).when(upcoming.is_empty(), |d| d.p(px(16.)).child(ui::caption("Nothing expected in the next 30 days.", cx)));

        // ---- your entries ----
        let entries = ui::card(cx).gap_2()
            .children(self.config.entries.iter().enumerate().map(|(i, m)| {
                let gone = m.superseded(&series);
                let when = if m.once() {
                    format!("once, {} {}", m.day, m.from.clone().unwrap_or_default())
                } else {
                    format!("monthly on the {}{}{}", m.day,
                        m.from.as_ref().map(|f| format!(" from {f}")).unwrap_or_default(),
                        m.until.as_ref().map(|u| format!(" until {u}")).unwrap_or_default())
                };
                h_flex().gap_3().items_center()
                    .child(v_flex().flex_1().min_w_0()
                        .child(div().text_size(px(ui::ts::BODY)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(m.label.clone()))
                        .child(ui::caption(if gone { format!("{when} · now found in the data, so it's skipped") } else { when }, cx)))
                    .child(div().figures().text_size(px(ui::ts::BODY)).text_color(fmt::signed_color(m.amount)).child(fmt::signed_hkd(m.amount)))
                    .child(Button::new(("cf-del", i)).xsmall().ghost().icon(IconName::Delete).tooltip("Remove")
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_entry(i, cx))))
            }))
            .when(self.config.entries.is_empty(), |d| d.child(ui::caption("None yet. Add what the data can't show: a new lease, a partner's share, a one-off bill.", cx)))
            .child(if self.form_open { self.form(cx).into_any_element() } else {
                h_flex().child(Button::new("cf-add").small().ghost().icon(IconName::Plus).label("Add a commitment")
                    .on_click(cx.listener(|this, _, _, cx| { this.form_open = true; cx.notify(); }))).into_any_element()
            })
            .when_some(self.note.clone(), |d, n| d.child(ui::caption(n, cx)));

        v_flex()
            .gap_4()
            .child(header)
            .child(tiles)
            .child(grid_card)
            .child(balance)
            .child(h_flex().gap_4().items_start().flex_wrap()
                .child(v_flex().flex_1().flex_grow(1.4).min_w(crate::layout::col(340.)).gap_2()
                    .child(ui::section("Next 30 days", "expected, from today", cx))
                    .child(list))
                .child(v_flex().flex_1().min_w(px(300.)).gap_2()
                    .child(ui::section_info("Your entries", "commitments not in the data yet",
                        format!("Saved in {}. An entry with a category steps aside once a matching recurring charge shows up in the data, so it never counts twice.", config_path().display()), cx))
                    .child(entries)))
            .into_any_element()
    }
}

impl CashFlow {
    fn form(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let dir = |label: &'static str, money_in: bool, cx: &mut Context<Self>| {
            let on = self.money_in == money_in;
            div().id(label).px(px(10.)).py(px(3.)).rounded(px(6.)).text_xs().cursor_pointer()
                .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(t.muted_foreground))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| { this.money_in = money_in; cx.notify(); }))
                .child(label)
        };
        let field = |s: &Entity<InputState>, w: f32| div().flex_1().min_w(px(w)).child(Input::new(s).small());
        v_flex().gap_2().pt_2().border_t_1().border_color(ui::hairline(cx))
            .child(h_flex().gap_2().flex_wrap()
                .child(h_flex().p(px(2.)).gap(px(2.)).rounded(px(7.)).border_1().border_color(ui::hairline(cx))
                    .child(dir("Money out", false, cx)).child(dir("Money in", true, cx)))
                .child(field(&self.f_label, 140.))
                .child(field(&self.f_amount, 100.)))
            .child(h_flex().gap_2().flex_wrap()
                .child(field(&self.f_day, 70.))
                .child(field(&self.f_from, 100.))
                .child(field(&self.f_until, 120.))
                .child(field(&self.f_category, 120.)))
            .child(h_flex().gap_2()
                .child(Button::new("cf-save").small().primary().label("Add").on_click(cx.listener(|this, _, w, cx| this.add_entry(w, cx))))
                .child(Button::new("cf-cancel").small().ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| { this.form_open = false; cx.notify(); }))))
    }
}

/// The Overview tile: out and in over the next 30 days, and the next big payment.
pub fn overview_tile(o: &Outlook, cx: &App) -> Div {
    let t = cx.theme();
    let figure = |label: &str, v: f64, c: Hsla| v_flex().gap(px(2.))
        .child(ui::caption(label.to_string(), cx))
        .child(div().text_size(px(17.)).line_height(px(24.)).font_semibold().figures().whitespace_nowrap().text_color(c).child(fmt::compact(v)));
    let next = o.next_big.as_ref().map(|p| format!("next big: {} {} on {}", p.label, fmt::hkd(-p.cash_hkd), p.date.format("%-d %b")))
        .unwrap_or_else(|| "nothing big expected".into());
    ui::card(cx)
        .flex_1()
        .min_w(px(170.))
        .gap(px(6.))
        .child(h_flex().h(px(18.))
            .child(ui::eyebrow_info("Next 30 days", "What the Cash flow page expects from today: recurring charges paid from a bank account, the card bill, salary, and your own entries. Card charges ride on the card bill. Amounts that vary use recent medians.", cx)))
        .child(h_flex().gap(px(ui::sp::L)).flex_wrap()
            .child(figure("out", o.out_hkd, fmt::money_out()))
            .child(figure("in", o.in_hkd, fmt::money_in())))
        .child(div().text_size(px(ui::ts::CAPTION)).text_color(t.muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap().child(next))
}
