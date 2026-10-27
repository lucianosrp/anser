//! "Change category…" (the detail sheet, the table's right-click menu): a
//! searchable list of the categories with their icons, "New category…",
//! and a scope: This row (an override, PUT /api/txn/…/category) or All
//! like this (a rule). All like this proposes a pattern from the row's
//! merchant (drill::merchant_key, else its merchant text), editable, and
//! the direction from the row's sign, then shows the rule's dry run (how
//! many rows match, what they add up to, how many would change, how many
//! are pinned by an override, and which rule decides each sample row now)
//! before Save.
//!
//! The same view on the desktop and in the browser. A save emits
//! [`PickerEvent::Written`] with the server's reply: the app patches its
//! ledger at once and syncs in the background.

use std::sync::Arc;

use finance_core::categories::{self as cats, Preview, RuleDraft};
use finance_core::{drill, Entry, Ledger};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::category;
use crate::fmt;
use crate::pages::sources::json_of;
use crate::ui::{self, ts, Figures as _};
use crate::zoom::px;

pub enum PickerEvent {
    /// a write went through: the server's reply (rows moved, data_rev, ...)
    Written(Value),
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Row,
    All,
}

pub struct CategoryPicker {
    entry: Entry,
    /// the categories, in the manager's order (the table's, else the ledger's)
    names: Vec<String>,
    search: Entity<InputState>,
    pattern: Entity<InputState>,
    selected: Option<String>,
    /// "Mark as": the body that creates the home category picked from the
    /// menu, sent just before the write (and only if it is still the pick)
    create: Option<(String, Value)>,
    scope: Scope,
    /// "out", "in", or None for both
    direction: Option<String>,
    preview: Option<Result<Preview, String>>,
    /// bumps on every preview asked for: a late answer to an older one is dropped
    preview_gen: u64,
    busy: bool,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PickerEvent> for CategoryPicker {}

/// The categories a picker offers: the table's (minus hidden ones), else
/// every category the bank and card rows use. "Other" last.
pub fn category_names(ledger: &Ledger) -> Vec<String> {
    let mut v: Vec<String> = if ledger.categories.is_empty() {
        let mut v: Vec<String> = ledger.entries.iter()
            .filter(|e| matches!(e.kind, finance_core::Kind::Bank | finance_core::Kind::Card))
            .map(|e| e.category.clone())
            .collect();
        v.sort_by_key(|a| a.to_lowercase());
        v.dedup();
        v
    } else {
        ledger.categories.iter().filter(|c| !c.hidden).map(|c| c.name.clone()).collect()
    };
    v.retain(|n| n != cats::OTHER);
    v.push(cats::OTHER.into());
    v
}

/// The pattern "All like this" starts from: the merchant key when the
/// description has it as is, else the merchant words, else the whole
/// description (lowercase, as the rules match).
pub fn proposed_pattern(desc: &str) -> String {
    let lower = desc.to_lowercase();
    let key = drill::merchant_key(desc);
    if key.chars().count() >= 3 && lower.contains(&key) {
        return key;
    }
    let text = drill::merchant_text(desc);
    if text.chars().count() >= 3 && lower.contains(&text) {
        return text;
    }
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl CategoryPicker {
    pub fn new(entry: Entry, ledger: Arc<Ledger>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search or name a new category…"));
        let pattern = cx.new(|cx| InputState::new(window, cx).default_value(proposed_pattern(&entry.description)));
        let mut subs = vec![cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => {
                let q = this.query(cx);
                let pick = this.filtered(&q).first().cloned().or_else(|| (!q.is_empty()).then_some(q));
                if let Some(p) = pick {
                    this.pick(p, cx);
                }
            }
            _ => {}
        })];
        subs.push(cx.subscribe_in(&pattern, window, |this, _, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                this.run_preview(cx);
            }
        }));
        let direction = Some(if entry.amount < 0. { "out" } else { "in" }.to_string());
        let mut p = CategoryPicker {
            names: category_names(&ledger),
            entry,
            search,
            pattern,
            selected: None,
            create: None,
            scope: Scope::Row,
            direction,
            preview: None,
            preview_gen: 0,
            busy: false,
            error: None,
            _subs: subs,
        };
        // dev/screenshot aids: FINANCE_DESKTOP_PICK=Dining, FINANCE_DESKTOP_PICK_ALL=1
        if let Ok(c) = std::env::var("FINANCE_DESKTOP_PICK") {
            p.selected = Some(c);
        }
        if std::env::var_os("FINANCE_DESKTOP_PICK_ALL").is_some() {
            p.scope = Scope::All;
            p.run_preview(cx);
        }
        p
    }

    /// Open with a home category picked ("Mark as"); `create` is the body
    /// that makes it when the ledger has no such category yet. Nothing is
    /// created until the write is saved.
    pub fn start_mark(&mut self, name: &str, create: Option<Value>) {
        if let Some(body) = create {
            // the list offers it like any other until it exists
            let at = self.names.len().saturating_sub(1);
            self.names.insert(at, name.to_string());
            self.create = Some((name.to_string(), body));
        }
        self.selected = Some(name.to_string());
    }

    /// Open on "All like this" with a rule already drafted (a suggestion's).
    pub fn start_all(&mut self, pattern: String, direction: Option<String>, category: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.pattern.update(cx, |s, cx| s.set_value(pattern, window, cx));
        self.direction = direction;
        if category.is_some() {
            self.selected = category;
        }
        self.scope = Scope::All;
        self.run_preview(cx);
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_string()
    }

    fn filtered(&self, q: &str) -> Vec<String> {
        let l = q.to_lowercase();
        self.names.iter().filter(|n| l.is_empty() || n.to_lowercase().contains(&l)).cloned().collect()
    }

    fn pick(&mut self, name: String, cx: &mut Context<Self>) {
        self.selected = Some(name);
        self.error = None;
        if self.scope == Scope::All {
            self.run_preview(cx);
        }
        cx.notify();
    }

    fn set_scope(&mut self, s: Scope, cx: &mut Context<Self>) {
        self.scope = s;
        self.error = None;
        if s == Scope::All {
            self.run_preview(cx);
        }
        cx.notify();
    }

    fn set_direction(&mut self, d: Option<&str>, cx: &mut Context<Self>) {
        self.direction = d.map(String::from);
        self.run_preview(cx);
        cx.notify();
    }

    fn draft(&self, cx: &App) -> RuleDraft {
        RuleDraft {
            pattern: self.pattern.read(cx).value().to_string(),
            direction: self.direction.clone(),
            category: self.selected.clone().unwrap_or_else(|| self.entry.category.clone()),
            ..Default::default()
        }
    }

    /// The dry run of "All like this" for what is picked now.
    fn run_preview(&mut self, cx: &mut Context<Self>) {
        if self.scope != Scope::All || self.pattern.read(cx).value().trim().is_empty() {
            return;
        }
        self.preview_gen += 1;
        let asked = self.preview_gen;
        let fut = cats::preview(&self.draft(cx));
        cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |this, cx| {
                if this.preview_gen != asked {
                    return; // an older question
                }
                this.preview = Some(json_of(r).and_then(|v| serde_json::from_value::<Preview>(v).map_err(|e| e.to_string())));
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(cat) = self.selected.clone() else { return };
        self.busy = true;
        self.error = None;
        let create = self.create.clone().filter(|(n, _)| *n == cat).map(|(_, body)| body);
        let write: Box<dyn FnOnce() -> finance_core::client::BoxFuture> = match self.scope {
            Scope::Row => {
                let share = (self.entry.share < 1.).then_some(self.entry.share);
                let (source, hash) = (self.entry.source.clone(), self.entry.raw_hash.clone());
                Box::new(move || cats::set_row(&source, &hash, Some(&cat), share, None))
            }
            Scope::All => {
                let mut d = self.draft(cx);
                d.priority = self.preview.as_ref().and_then(|p| p.as_ref().ok()).map(|p| p.priority);
                Box::new(move || cats::create_rule(&d))
            }
        };
        self.send(create, write, cx);
    }

    /// "Use the rules again": drop this row's override.
    fn unpin(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        let (source, hash) = (self.entry.source.clone(), self.entry.raw_hash.clone());
        self.send(None, Box::new(move || cats::set_row(&source, &hash, None, None, None)), cx);
    }

    /// Run the write, after creating the category first when `create` says so
    /// (the write is only built once that went through).
    fn send(&mut self, create: Option<Value>, write: Box<dyn FnOnce() -> finance_core::client::BoxFuture>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let r = match create {
                Some(body) => match json_of(cats::create_category(body).await) {
                    Ok(_) => write().await,
                    Err(e) => Err(e),
                },
                None => write().await,
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match json_of(r) {
                    Ok(v) => cx.emit(PickerEvent::Written(v)),
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn row_summary(&self, cx: &App) -> Div {
        let t = cx.theme();
        let e = &self.entry;
        let v = drill::my_hkd(e).unwrap_or(e.amount);
        h_flex().gap_3().items_center().px(px(ui::sp::M)).py(px(ui::sp::S)).rounded(t.radius).bg(t.muted.opacity(0.5))
            .child(match crate::logo::tile(crate::logo::for_entry(e), 28., cx) {
                Some(l) => l,
                None => category::tile(&e.category, 28., t.muted_foreground).into_any_element(),
            })
            .child(v_flex().flex_1().min_w_0()
                .child(div().text_size(px(ts::BODY)).font_weight(FontWeight::MEDIUM).overflow_hidden().text_ellipsis().whitespace_nowrap()
                    .child(e.description.clone()))
                .child(h_flex().gap_2().text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
                    .child(e.date.clone()).child("·").child(category::label(&e.category, cx))))
            .child(div().text_size(px(ts::BODY)).figures().text_color(fmt::signed_color(v)).child(fmt::signed_hkd(v)))
    }

    fn list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let q = self.query(cx);
        let shown = self.filtered(&q);
        let exact = shown.iter().any(|n| n.eq_ignore_ascii_case(&q));
        let mut items: Vec<AnyElement> = shown.iter().enumerate().map(|(i, n)| {
            let sel = self.selected.as_deref() == Some(n.as_str());
            let current = *n == self.entry.category;
            let kind = cats::get(n).filter(|c| c.kind != "spending").map(|c| cats::kind_label(&c.kind));
            let name = n.clone();
            h_flex().id(("cat-item", i)).gap_2().items_center().px(px(ui::sp::S)).py(px(5.)).rounded(t.radius).cursor_pointer()
                .when(sel, |d| d.bg(t.accent))
                .hover(|s| s.bg(t.accent.opacity(0.7)))
                .on_click(cx.listener(move |this, _, _, cx| this.pick(name.clone(), cx)))
                .child(category::tile(n, 24., if sel { t.primary } else { t.muted_foreground }))
                .child(div().flex_1().min_w_0().text_size(px(ts::BODY)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(n.clone()))
                .when_some(kind, |d, k| d.child(ui::tag(k, cx)))
                .when(current, |d| d.child(ui::caption("now", cx)))
                .when(sel, |d| d.child(gpui_kit::component::Icon::new(IconName::Check).size(px(14.)).text_color(t.primary)))
                .into_any_element()
        }).collect();
        if !q.is_empty() && !exact {
            let name = q.clone();
            let sel = self.selected.as_deref() == Some(q.as_str());
            items.push(h_flex().id("cat-new").gap_2().items_center().px(px(ui::sp::S)).py(px(5.)).rounded(t.radius).cursor_pointer()
                .when(sel, |d| d.bg(t.accent))
                .hover(|s| s.bg(t.accent.opacity(0.7)))
                .on_click(cx.listener(move |this, _, _, cx| this.pick(name.clone(), cx)))
                .child(div().size(px(24.)).flex().items_center().justify_center().rounded(px(7.)).bg(t.primary.opacity(0.14))
                    .child(gpui_kit::component::Icon::new(IconName::Plus).size(px(14.)).text_color(t.primary)))
                .child(div().text_size(px(ts::BODY)).child(format!("New category \u{201c}{q}\u{201d}…")))
                .into_any_element());
        } else if q.is_empty() {
            items.push(ui::caption("New category…: type its name above", cx).px(px(ui::sp::S)).py(px(4.)).into_any_element());
        }
        div().id("cat-list").max_h(px(230.)).overflow_y_scroll().child(v_flex().gap(px(1.)).children(items))
    }

    fn segmented(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let row = Button::new("scope-row").small().label("This row").when(self.scope == Scope::Row, |b| b.primary()).when(self.scope != Scope::Row, |b| b.outline())
            .on_click(cx.listener(|this, _, _, cx| this.set_scope(Scope::Row, cx)));
        let all = Button::new("scope-all").small().label("All like this").when(self.scope == Scope::All, |b| b.primary()).when(self.scope != Scope::All, |b| b.outline())
            .on_click(cx.listener(|this, _, _, cx| this.set_scope(Scope::All, cx)));
        h_flex().gap_1().child(row).child(all)
    }

    fn rule_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let dir = self.direction.clone();
        let dbtn = |id: &'static str, label: &'static str, d: Option<&'static str>, cx: &mut Context<Self>| {
            let on = dir.as_deref() == d;
            Button::new(id).xsmall().label(label).when(on, |b| b.primary()).when(!on, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| this.set_direction(d, cx)))
        };
        let preview: AnyElement = match &self.preview {
            None => ui::caption("Pick a category to see what the rule would do.", cx).into_any_element(),
            Some(Err(e)) => div().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).child(e.clone()).into_any_element(),
            Some(Ok(p)) => {
                let mut facts = vec![format!("{} row{} match", fmt::count(p.matches as f64), if p.matches == 1 { "" } else { "s" }),
                                     format!("{} in all", fmt::signed_hkd(p.total_hkd)),
                                     format!("{} would change", fmt::count(p.would_change as f64))];
                if p.pinned > 0 {
                    facts.push(format!("{} pinned by hand (kept)", fmt::count(p.pinned as f64)));
                }
                if p.shadowed > 0 {
                    facts.push(format!("{} stay with a stronger rule", fmt::count(p.shadowed as f64)));
                }
                let rows = p.sample.iter().take(20).map(|r| {
                    let now = r.rule_now.as_ref().map(|w| format!("rule \u{201c}{}\u{201d}", w.pattern)).unwrap_or_else(|| "no rule".into());
                    let why = if r.pinned { "pinned".to_string() } else { now };
                    let moves = r.category != r.category_after;
                    h_flex().gap_2().py(px(3.)).text_size(px(ts::CAPTION)).border_b_1().border_color(ui::hairline(cx))
                        .child(div().w(px(76.)).flex_shrink_0().text_color(t.muted_foreground).child(r.date.clone()))
                        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(r.description.clone()))
                        .child(div().w(px(84.)).flex_shrink_0().text_right().figures().text_color(fmt::signed_color(r.hkd)).child(fmt::signed_hkd(r.hkd)))
                        .child(div().w(px(170.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap()
                            .text_color(if moves { t.foreground } else { t.muted_foreground })
                            .child(if moves { format!("{} → {}", r.category, r.category_after) } else { format!("{} (stays)", r.category) }))
                        .child(div().w(px(120.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(t.muted_foreground).child(why))
                }).collect::<Vec<_>>();
                v_flex().gap_2()
                    .child(div().text_size(px(ts::BODY)).child(facts.join(" · ")))
                    .child(ui::caption(format!("Saved as your rule at priority {}: your rules come before the starter rules, then by priority.", p.priority), cx))
                    .child(div().id("preview-rows").max_h(px(220.)).overflow_y_scroll().child(v_flex().children(rows)))
                    .into_any_element()
            }
        };
        v_flex().gap_2().p(px(ui::sp::M)).rounded(t.radius).border_1().border_color(t.border)
            .child(h_flex().gap_2().items_center()
                .child(div().w(px(64.)).text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child("CONTAINS"))
                .child(div().flex_1().child(Input::new(&self.pattern).small())))
            .child(h_flex().gap_2().items_center()
                .child(div().w(px(64.)).text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child("MONEY"))
                .child(dbtn("dir-out", "Out", Some("out"), cx))
                .child(dbtn("dir-in", "In", Some("in"), cx))
                .child(dbtn("dir-any", "Both", None, cx))
                .child(ui::caption("=text matches the whole description only", cx)))
            .child(preview)
    }
}

impl Render for CategoryPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let ready = self.selected.is_some() && !self.busy && match self.scope {
            Scope::Row => true,
            Scope::All => matches!(&self.preview, Some(Ok(p)) if p.matches > 0),
        };
        let save_label = match (self.scope, &self.selected) {
            (_, None) => "Pick a category".to_string(),
            (Scope::Row, Some(c)) => format!("Move to {c}"),
            (Scope::All, Some(c)) => match &self.preview {
                Some(Ok(p)) => format!("Save rule: {} → {c}", fmt::count(p.matches as f64)),
                _ => format!("Save rule → {c}"),
            },
        };
        v_flex().gap_3().w_full()
            .child(self.row_summary(cx))
            .child(Input::new(&self.search).small().prefix(gpui_kit::component::Icon::new(IconName::Search).size(px(14.)).text_color(t.muted_foreground)))
            .child(self.list(cx))
            .child(h_flex().justify_between().items_center().gap_2()
                .child(self.segmented(cx))
                .child(ui::caption(match self.scope {
                    Scope::Row => "only this transaction (an override: rules can't move it)",
                    Scope::All => "a rule: these rows and every new one like them",
                }, cx)))
            .when(self.scope == Scope::All, |d| d.child(self.rule_editor(cx)))
            .when_some(self.error.clone(), |d, e| d.child(div().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).child(e)))
            .child(h_flex().justify_between().items_center().gap_2()
                .child(Button::new("pick-unpin").small().ghost().label("Use the rules again").disabled(self.busy || self.scope != Scope::Row)
                    .tooltip("Drop this row's hand-picked category: the rules decide it again")
                    .on_click(cx.listener(|this, _, _, cx| this.unpin(cx))))
                .child(h_flex().gap_2()
                    .child(Button::new("pick-cancel").small().ghost().label("Cancel").on_click(cx.listener(|_, _, _, cx| cx.emit(PickerEvent::Cancel))))
                    .child(Button::new("pick-save").small().primary().label(save_label).loading(self.busy).disabled(!ready)
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))))))
    }
}

#[cfg(test)]
mod tests {
    use super::proposed_pattern;

    #[test]
    fn the_pattern_starts_from_the_merchant() {
        assert_eq!(proposed_pattern("POS MDC (17JUN24)P BBMSL*042 SUSHIRO MOS"), "sushiro");
        assert_eq!(proposed_pattern("CITY SUPER LIMITED SHA TIN HK"), "city super");
        assert_eq!(proposed_pattern("12 34"), "12 34");
    }
}
