//! Budgets: a monthly budget per spending category, and how the running
//! month is pacing against it. Spending is the Spending page's definition
//! (`drill::is_spending`, your share of shared rows), computed by
//! `finance_core::budget`. The budgets live in budgets.toml (crate::budgets)
//! and are edited here; "Suggest from history" fills in the median month.
//!
//! Its own view: the edit fields are entities and need a home between frames.

use std::collections::BTreeMap;
use std::sync::Arc;

use finance_core::budget::{self, ByMonth, Line, MonthStatus, Status};
use finance_core::Ledger;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::budgets::{self as store, Budgets};
use crate::category;
use crate::fmt;
use crate::ui::{self, Figures as _, Tile, sp, ts};
use crate::zoom::px;

/// Months the edit list looks back over for categories worth a budget.
const CANDIDATE_MONTHS: usize = 12;

pub struct BudgetsView {
    ledger: Option<Arc<Ledger>>,
    by: Arc<ByMonth>,
    /// category -> its edit field, while editing
    inputs: BTreeMap<String, Entity<InputState>>,
    editing: bool,
    /// the category the history chart shows
    selected: Option<String>,
    note: Option<String>,
    /// dev aid: FINANCE_DESKTOP_BUDGET_EDIT=1 (or =suggest) opens the editor
    edit_on_load: Option<bool>,
    _subs: Vec<Subscription>,
}

impl BudgetsView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        BudgetsView {
            ledger: None,
            by: Arc::default(),
            inputs: BTreeMap::new(),
            editing: false,
            selected: std::env::var("FINANCE_DESKTOP_BUDGET_CAT").ok(),
            note: None,
            edit_on_load: std::env::var("FINANCE_DESKTOP_BUDGET_EDIT").ok().map(|v| v == "suggest"),
            _subs: Vec::new(),
        }
    }

    pub fn set_ledger(&mut self, ledger: Arc<Ledger>, cx: &mut Context<Self>) {
        self.by = Arc::new(budget::by_month(&ledger));
        self.ledger = Some(ledger);
        cx.notify();
    }

    fn month(&self) -> String {
        store::today().format("%Y-%m").to_string()
    }

    /// Categories worth a field: every budgeted one, plus any spent on in the
    /// last CANDIDATE_MONTHS months.
    fn candidates(&self, cx: &App) -> Vec<String> {
        let mut cats: Vec<String> = cx.global::<Budgets>().monthly.keys().cloned().collect();
        let mut window = budget::months_before(&self.month(), CANDIDATE_MONTHS);
        window.push(self.month());
        cats.extend(window.iter().filter_map(|m| self.by.get(m)).flat_map(|c| c.keys().cloned()));
        cats.sort();
        cats.dedup();
        cats
    }

    fn start_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved = cx.global::<Budgets>().monthly.clone();
        self.inputs.clear();
        self._subs.clear();
        for c in self.candidates(cx) {
            let value = saved.get(&c).map(|v| format!("{v:.0}")).unwrap_or_default();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("—").default_value(value));
            self._subs.push(cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.save(cx);
                }
            }));
            self.inputs.insert(c, input);
        }
        self.editing = true;
        self.note = None;
        cx.notify();
    }

    /// Fill every field with its suggestion (fields without one are cleared).
    fn suggest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing {
            self.start_edit(window, cx);
        }
        let s = budget::suggest(&self.by, &self.month());
        for (c, input) in &self.inputs {
            let v = s.get(c).map(|v| format!("{v:.0}")).unwrap_or_default();
            input.update(cx, |i, cx| i.set_value(v, window, cx));
        }
        self.note = Some(format!("Suggested: the median of the last {} complete months, rounded. Save to keep them.", budget::SUGGEST_MONTHS));
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let monthly: BTreeMap<String, f64> = self.inputs.iter()
            .filter_map(|(c, i)| Some((c.clone(), store::parse_amount(&i.read(cx).value())?)))
            .collect();
        let b = Budgets { monthly };
        match b.save() {
            Ok(()) => {
                self.note = Some(format!("Saved {} budgets to {}.", b.monthly.len(), store::saved_where()));
                cx.set_global(b);
                self.editing = false;
                self.inputs.clear();
                self._subs.clear();
            }
            Err(e) => self.note = Some(format!("Could not save: {e:#}")),
        }
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.editing = false;
        self.inputs.clear();
        self._subs.clear();
        self.note = None;
        cx.notify();
    }
}

/// The color a line's status carries.
pub fn status_color(s: Status, cx: &App) -> Hsla {
    match s {
        Status::Over => fmt::money_out(),
        Status::Near => cx.theme().warning,
        Status::OnTrack => fmt::money_in(),
        Status::Unbudgeted => cx.theme().muted_foreground,
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Over => "over",
        Status::Near => "ahead of pace",
        Status::OnTrack => "on track",
        Status::Unbudgeted => "no budget",
    }
}

/// A budget bar: spent against the budget, with a tick where the month is.
/// Over budget fills the bar.
pub fn pace_bar(spent: f64, budget: f64, elapsed: f64, color: Hsla, height: f32, cx: &App) -> Div {
    let t = cx.theme();
    let frac = if budget > 0. { (spent / budget).clamp(0., 1.) } else { 0. } as f32;
    div()
        .relative()
        .w_full()
        .h(px(height))
        .child(div().absolute().inset_0().rounded_full().bg(t.muted).overflow_hidden()
            .child(div().h_full().rounded_full().bg(color).w(relative(frac))))
        .child(div().absolute().top(px(-3.)).h(px(height + 6.)).w(px(2.)).rounded_full()
            .left(relative(elapsed.clamp(0., 1.) as f32)).bg(t.foreground.opacity(0.75)))
}

impl Render for BudgetsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let (Some(suggest), true) = (self.edit_on_load, self.ledger.is_some()) {
            self.edit_on_load = None;
            if suggest { self.suggest(window, cx) } else { self.start_edit(window, cx) }
        }
        let Some(ledger) = self.ledger.clone() else {
            return v_flex().child(ui::empty("No spending data yet — sync first", cx));
        };
        let budgets = cx.global::<Budgets>().monthly.clone();
        let today = store::today();
        let st = budget::month_status(&ledger, &budgets, today);
        let t = cx.theme().clone();

        // ---- header: the month, the actions ----
        let month_name = today.format("%B %Y").to_string();
        let actions = if self.editing {
            h_flex().gap_2()
                .child(Button::new("b-suggest").small().outline().icon(IconName::Star).label("Suggest from history")
                    .tooltip("Each category's median month over the last 6 complete months, rounded")
                    .on_click(cx.listener(|this, _, w, cx| this.suggest(w, cx))))
                .child(Button::new("b-cancel").small().ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| this.cancel(cx))))
                .child(Button::new("b-save").small().primary().label("Save").on_click(cx.listener(|this, _, _, cx| this.save(cx))))
        } else {
            h_flex().gap_2()
                .child(Button::new("b-edit").small().outline().label(if budgets.is_empty() { "Set budgets" } else { "Edit budgets" })
                    .on_click(cx.listener(|this, _, w, cx| this.start_edit(w, cx))))
        };
        let head = h_flex().justify_between().flex_wrap().gap_2()
            .child(h_flex().gap_3().items_baseline()
                .child(div().text_lg().font_semibold().child(month_name))
                .child(ui::caption(format!("day {} of {} · the tick on each bar is today", st.day, st.days), cx)))
            .child(actions);
        let note = self.note.clone().map(|n| ui::caption_wrap(n, cx));

        if self.editing {
            return v_flex().gap_4().child(head).children(note).child(self.edit_list(&st, window, cx));
        }
        if budgets.is_empty() {
            return v_flex().gap_4().child(head).children(note).child(
                ui::empty_with(IconName::ChartPie, "No budgets yet",
                    "Set a monthly budget per category, or let “Suggest from history” start from your median month. They stay on this machine (budgets.toml).", cx)
                    .child(h_flex().gap_2().mt_2()
                        .child(Button::new("b-suggest-empty").small().primary().icon(IconName::Star).label("Suggest from history")
                            .on_click(cx.listener(|this, _, w, cx| this.suggest(w, cx))))
                        .child(Button::new("b-set-empty").small().outline().label("Set by hand")
                            .on_click(cx.listener(|this, _, w, cx| this.start_edit(w, cx))))),
            );
        }

        // ---- totals ----
        let (bt, spent, proj, extra) = (st.budget_total(), st.spent_total(), st.projected_total(), st.unbudgeted_total());
        let total_status = if spent > bt { Status::Over } else if proj > bt * (1. + budget::NEAR_TOLERANCE) { Status::Near } else { Status::OnTrack };
        let tc = status_color(total_status, cx);
        let pace = st.pace().unwrap_or(0.);
        let pace_txt = if (pace - 1.).abs() < 0.05 { "right on pace".to_string() }
            else if pace > 1. { format!("{:.0}% ahead of pace", (pace - 1.) * 100.) }
            else { format!("{:.0}% under pace", (1. - pace) * 100.) };
        let tiles = h_flex().gap(px(sp::GRID)).flex_wrap().items_stretch()
            .child(Tile::explained("Spent", "Spending this month in the categories with a budget: bank and card outflows that are spending (no transfers, investing or card bills), your share of shared rows.", fmt::hkd(spent))
                .sub(format!("of {} · {:.0}% used, {:.0}% of the month gone", fmt::hkd(bt), if bt > 0. { spent / bt * 100. } else { 0. }, st.elapsed() * 100.))
                .tone(tc).render(cx).min_w(px(220.)))
            .child(Tile::explained("Month-end, projected", "What's spent so far plus, per category, the median of what the last 6 complete months spent after today's day of the month. Rent paid on the 1st doesn't count thirty times.", fmt::hkd(proj))
                .sub(pace_txt).tone(status_color(if proj > bt * (1. + budget::NEAR_TOLERANCE) { Status::Over } else { Status::OnTrack }, cx)).render(cx).min_w(px(220.)))
            .child(Tile::new("Remaining", fmt::signed_hkd(bt - spent))
                .sub(format!("{} over · {} ahead of pace · {} on track", st.count(Status::Over), st.count(Status::Near), st.count(Status::OnTrack)))
                .tone(fmt::signed_color(bt - spent)).render(cx).min_w(px(220.)))
            .child(Tile::explained("Unbudgeted", "Spending this month in categories without a budget. It isn't in the totals to the left.", fmt::hkd(extra))
                .sub(format!("{} categories", st.unbudgeted().count())).render(cx).min_w(px(220.)));
        let overall = ui::card(cx).gap_2()
            .child(h_flex().justify_between().child(div().text_sm().font_semibold().child("All budgets"))
                .child(div().text_sm().figures().text_color(t.muted_foreground).child(format!("{} / {}", fmt::int(spent), fmt::hkd(bt)))))
            .child(pace_bar(spent, bt, st.elapsed(), tc, 10., cx));

        // ---- the categories ----
        let selected = self.selected.clone().filter(|s| st.lines.iter().any(|l| &l.category == s))
            .or_else(|| st.budgeted().next().map(|l| l.category.clone()));
        let rows = v_flex().gap(px(2.)).children(st.budgeted().enumerate().map(|(i, l)| {
            self.line_row(i, l, st.elapsed(), selected.as_deref() == Some(l.category.as_str()), cx)
        }));
        let extra_rows = v_flex().children(st.unbudgeted().enumerate().map(|(i, l)| {
            let cat = l.category.clone();
            h_flex().id(("unb", i)).gap_3().py(px(5.)).px_1().rounded(t.radius).cursor_pointer().hover(|s| s.bg(t.accent))
                .on_click(cx.listener(move |this, _, _, cx| { this.selected = Some(cat.clone()); cx.notify(); }))
                .child(category::tile(&l.category, 22., t.muted_foreground))
                .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(l.category.clone()))
                .child(div().text_sm().figures().whitespace_nowrap().child(fmt::hkd(l.spent)))
        }));

        let history = selected.map(|c| self.history_card(&c, &st, cx));
        v_flex()
            .gap_4()
            .child(head)
            .children(note)
            .child(tiles)
            .child(overall)
            .child(
                h_flex().gap_4().items_start().flex_wrap()
                    .child(v_flex().flex_grow(1.6).flex_basis(px(420.)).min_w(crate::layout::col(340.)).gap_2()
                        .child(ui::section("By category", "click one for its last 12 months", cx))
                        .child(ui::card(cx).child(rows)))
                    .child(v_flex().flex_1().flex_basis(px(260.)).min_w(px(260.)).gap_2()
                        .child(ui::section("Without a budget", &fmt::hkd(extra), cx))
                        .child(ui::card(cx).when(st.unbudgeted().next().is_none(), |d| d.child(ui::caption("Nothing spent outside the budgets this month.", cx)))
                            .child(extra_rows))),
            )
            .children(history)
    }
}

impl BudgetsView {
    fn line_row(&self, i: usize, l: &Line, elapsed: f64, selected: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = cx.theme().clone();
        let s = l.status();
        let c = status_color(s, cx);
        let budget = l.budget.unwrap_or(0.);
        let rem = l.remaining();
        let cat = l.category.clone();
        let tip = format!("{}: spent {} of {} · projected {} by month-end · {}", l.category, fmt::hkd(l.spent), fmt::hkd(budget),
            fmt::hkd(l.projected), status_label(s));
        v_flex()
            .id(("bline", i))
            .gap(px(6.))
            .px(px(sp::S))
            .py(px(sp::S))
            .rounded(t.radius)
            .cursor_pointer()
            .when(selected, |d| d.bg(t.accent))
            .hover(|st| st.bg(t.accent))
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .on_click(cx.listener(move |this, _, _, cx| { this.selected = Some(cat.clone()); cx.notify(); }))
            .child(h_flex().gap_3().items_center()
                .child(category::tile(&l.category, 26., c))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().text_size(px(ts::BODY)).font_weight(FontWeight::MEDIUM).overflow_hidden().text_ellipsis().whitespace_nowrap().child(l.category.clone()))
                    .child(ui::caption(format!("projected {} · {}", fmt::compact(l.projected), status_label(s)), cx)))
                .child(v_flex().items_end().flex_shrink_0()
                    .child(div().text_size(px(ts::BODY)).figures().whitespace_nowrap()
                        .child(format!("{} / {}", fmt::int(l.spent), fmt::int(budget))))
                    .child(div().text_size(px(ts::CAPTION)).figures().whitespace_nowrap().text_color(if rem >= 0. { t.muted_foreground } else { fmt::money_out() })
                        .child(if rem >= 0. { format!("{} left", fmt::int(rem)) } else { format!("{} over", fmt::int(-rem)) }))))
            .child(div().pl(px(38.)).child(pace_bar(l.spent, budget, elapsed, c, 6., cx)))
    }

    /// The last 12 months of one category against its budget (a line).
    fn history_card(&self, cat: &str, st: &MonthStatus, cx: &App) -> Div {
        let t = cx.theme();
        let hist = budget::history(&self.by, cat, &st.month);
        let budget = cx.global::<Budgets>().monthly.get(cat).copied().filter(|b| *b > 0.);
        let max = hist.iter().map(|h| h.1).fold(budget.unwrap_or(0.), f64::max).max(1.) * 1.08;
        const H: f32 = 150.;
        let over = hist.iter().filter(|(m, v)| *m != st.month && budget.is_some_and(|b| *v > b)).count();
        let bars = h_flex().h(px(H)).items_end().gap_1().children(hist.iter().enumerate().map(|(i, (m, v))| {
            let running = *m == st.month;
            let c = match budget {
                Some(b) if *v > b => fmt::money_out(),
                Some(_) => fmt::money_in(),
                None => t.primary,
            };
            let tip = format!("{} · {}{}", fmt::month_label(m), fmt::hkd(*v), if running { " so far" } else { "" });
            v_flex().id(("bh", i)).flex_1().min_w_0().h_full().justify_end().items_center()
                .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                .child(div().w_full().max_w(px(30.)).rounded_t(px(3.)).h(px((*v / max) as f32 * H))
                    .bg(if running { c.opacity(0.45) } else { c.opacity(0.85) }))
        }));
        let labels = h_flex().gap_1().children(hist.iter().map(|(m, _)| {
            div().flex_1().min_w_0().text_center().text_xs().whitespace_nowrap().overflow_hidden()
                .text_color(if *m == st.month { t.foreground } else { t.muted_foreground }).child(fmt::month_label(m))
        }));
        let line = budget.map(|b| {
            let y = (b / max) as f32 * H;
            div().absolute().left_0().right_0().bottom(px(y)).h(px(0.)).border_t_1().border_color(t.foreground.opacity(0.55))
                .child(div().absolute().right_0().bottom(px(2.)).text_xs().figures().text_color(t.muted_foreground).child(format!("budget {}", fmt::int(b))))
        });
        let caption = match budget {
            Some(_) => format!("over budget in {over} of the last 11 months · this month so far, lighter"),
            None => "no budget: the bars are what was spent".into(),
        };
        v_flex().gap_2()
            .child(h_flex().gap_2().items_center()
                .child(category::glyph(cat, 15., t.foreground))
                .child(ui::section(cat, "last 12 months, budget vs actual", cx).mt_0()))
            .child(ui::card(cx).gap_2()
                .child(ui::caption(caption, cx))
                .child(div().relative().mt_2().child(bars).children(line))
                .child(labels))
    }

    /// Every candidate category with its field, the typical month beside it.
    fn edit_list(&self, st: &MonthStatus, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let suggestions = budget::suggest(&self.by, &st.month);
        let window = budget::months_before(&st.month, budget::SUGGEST_MONTHS);
        let median = |c: &str| budget::median(&window.iter().map(|m| self.by.get(m).and_then(|x| x.get(c)).copied().unwrap_or(0.)).collect::<Vec<_>>());
        let rows = v_flex().children(self.inputs.iter().map(|(c, input)| {
            let m = median(c);
            let spent = st.lines.iter().find(|l| &l.category == c).map(|l| l.spent).unwrap_or(0.);
            h_flex().gap_3().py(px(6.)).items_center().border_b_1().border_color(ui::hairline(cx))
                .child(category::tile(c, 24., t.primary))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(c.clone()))
                    .child(ui::caption(format!("median month {} · this month {}{}", fmt::int(m), fmt::int(spent),
                        if suggestions.contains_key(c) { "" } else { " · not suggested" }), cx)))
                .child(div().w(px(130.)).flex_shrink_0().child(Input::new(input).small().prefix(div().text_xs().text_color(t.muted_foreground).child(fmt::sym()))))
        }));
        let total: f64 = self.inputs.values().filter_map(|i| store::parse_amount(&i.read(cx).value())).sum();
        ui::card(cx).gap_2()
            .child(h_flex().justify_between()
                .child(div().text_sm().font_semibold().child("Monthly budget per category"))
                .child(div().text_sm().figures().text_color(t.muted_foreground).child(format!("total {}", fmt::hkd(total)))))
            .child(ui::caption("Empty means no budget. Enter in any field saves. Categories spent on in the last 12 months are listed.", cx))
            .child(rows)
    }
}

/// Overview's tile: how many categories are over or ahead of pace, and the total pace.
pub fn overview_tile(ledger: &Ledger, cx: &App) -> AnyElement {
    let budgets = &cx.global::<Budgets>().monthly;
    let open = |w: &mut Window, cx: &mut App| w.dispatch_action(Box::new(crate::app::GoBudgets), cx);
    if budgets.is_empty() {
        return ui::card_interactive("t-budgets", cx).flex_1().min_w(px(170.))
            .on_click(move |_, w, cx| open(w, cx))
            .child(ui::eyebrow("Budgets", cx))
            .child(div().text_size(px(ts::FIGURE)).line_height(px(28.)).font_semibold().text_color(cx.theme().muted_foreground).child("Not set"))
            .child(ui::caption("suggest them from your history →", cx))
            .into_any_element();
    }
    let today = store::today();
    let key = (today, budgets.iter().map(|(c, v)| (c.as_str(), v.to_bits())).collect::<Vec<_>>());
    let st = crate::memo::get("budgets.tile", &key, || budget::month_status(ledger, budgets, today));
    let (bt, spent) = (st.budget_total(), st.spent_total());
    let (over, near) = (st.count(Status::Over), st.count(Status::Near));
    let tone = if over > 0 { fmt::money_out() } else if near > 0 { cx.theme().warning } else { fmt::money_in() };
    let headline = match (over, near) {
        (0, 0) => "All on track".to_string(),
        (o, 0) => format!("{o} over"),
        (0, n) => format!("{n} ahead of pace"),
        (o, n) => format!("{o} over · {n} ahead"),
    };
    let elapsed = st.elapsed();
    let card = Tile::explained("Budgets", "This month's spending in budgeted categories against their budgets. Over = spent more than the budget already; ahead of pace = on course to end the month over it. The tick is how much of the month has gone.", headline)
        .sub(format!("{} of {} · {:.0}% used, {:.0}% of month", fmt::compact(spent), fmt::compact(bt), if bt > 0. { spent / bt * 100. } else { 0. }, elapsed * 100.))
        .tone(tone)
        .id("t-budgets")
        .render(cx)
        .child(div().mt_1().child(pace_bar(spent, bt, elapsed, tone, 6., cx)));
    div().id("t-budgets-link").flex().flex_1().min_w(px(170.)).cursor_pointer()
        .on_click(move |_, w, cx| open(w, cx)).child(card.w_full())
        .into_any_element()
}
