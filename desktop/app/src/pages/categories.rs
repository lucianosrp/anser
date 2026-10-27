//! Categories: three tabs over the server's categories and rules.
//!
//! * Categories: every category with its icon (click: the icon picker),
//!   kind, fixed, reimbursable and hidden; rename in place, merge into
//!   another, delete into another (or back to uncategorized), up/down to
//!   reorder.
//! * Rules: the rules in effective order (your rules, then the starter
//!   rules, each by priority) with an origin badge and an enabled switch;
//!   edit priority, pattern, category, direction and share in place; Test a
//!   description and amount to see which rule wins and why; delete.
//! * Categorize Other: the uncategorized rows clustered by merchant, the
//!   biggest first, each with a proposed rule: accept it in one click, or
//!   edit it in the change-category picker.
//!
//! With almost no rules, the Rules and Categorize tabs offer the starter
//! set. Reads come from the ledger (the replica, or ledger.json); every
//! change goes to the server and its reply comes back as
//! [`CategoriesEvent::Written`], which the app applies to its ledger at
//! once (then syncs in the background).

use std::rc::Rc;
use std::sync::Arc;

use finance_core::categories::{self as cats, Category, Rule, Suggestions, TestResult};
use finance_core::client::Request;
use finance_core::Ledger;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    v_flex, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::category;
use crate::fmt;
use crate::pages::sources::{call, json_of};
use crate::ui::{self, sp, ts, Figures as _};
use crate::zoom::px;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Categories,
    Rules,
    Suggest,
}

impl Tab {
    /// FINANCE_DESKTOP_CATS_TAB=rules|suggest (dev and screenshots;
    /// `?cats_tab=` in the browser).
    fn from_env() -> Self {
        match crate::app::dev_aid("FINANCE_DESKTOP_CATS_TAB").as_deref() {
            Some("rules") => Tab::Rules,
            Some("suggest") | Some("other") => Tab::Suggest,
            _ => Tab::Categories,
        }
    }
}

pub enum CategoriesEvent {
    /// a write's reply: the app patches its ledger and syncs
    Written(Value),
    /// edit a suggestion: the picker on ledger row `ix`, "All like this"
    /// with this pattern and category
    Edit { ix: usize, pattern: String, direction: Option<String>, category: Option<String> },
}

/// Rows shown before "Show all" on the Rules tab.
const RULES_SHOWN: usize = 120;
/// Fewer enabled rules than this: offer the starter set.
const FEW_RULES: usize = 20;

pub struct CategoriesView {
    tab: Tab,
    ledger: Option<Arc<Ledger>>,
    busy: bool,
    /// (ok, text) of the last change
    note: Option<(bool, String)>,
    // Categories
    new_name: Entity<InputState>,
    renaming: Option<String>,
    rename: Entity<InputState>,
    // Rules
    filter: Entity<InputState>,
    show_all: bool,
    /// the list as GET /api/rules has it (with hits), and the data_rev it was read at
    live_rules: Option<(i64, Vec<Rule>)>,
    editing: Option<i64>,
    e_priority: Entity<InputState>,
    e_pattern: Entity<InputState>,
    e_share: Entity<InputState>,
    e_category: Option<String>,
    e_direction: Option<String>,
    t_desc: Entity<InputState>,
    t_amount: Entity<InputState>,
    tested: Option<Result<TestResult, String>>,
    // Categorize Other
    suggestions: Option<Result<Suggestions, String>>,
    loading: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<CategoriesEvent> for CategoriesView {}

fn input(window: &mut Window, cx: &mut Context<CategoriesView>, ph: &str) -> Entity<InputState> {
    let ph = ph.to_string();
    cx.new(|cx| InputState::new(window, cx).placeholder(ph))
}

impl CategoriesView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_name = input(window, cx, "New category name");
        let rename = input(window, cx, "Name");
        let filter = input(window, cx, "Filter rules: pattern or category");
        let t_desc = input(window, cx, "A description, e.g. SUSHIRO MOS HK");
        let t_amount = input(window, cx, "-120");
        let subs = vec![
            cx.subscribe_in(&new_name, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.add_category(cx);
                }
            }),
            cx.subscribe_in(&rename, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.finish_rename(cx);
                }
            }),
            cx.subscribe_in(&filter, window, |_, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&t_desc, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.run_test(cx);
                }
            }),
            cx.subscribe_in(&t_amount, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.run_test(cx);
                }
            }),
        ];
        let v = CategoriesView {
            tab: Tab::from_env(),
            ledger: None,
            busy: false,
            note: None,
            new_name,
            renaming: None,
            rename,
            filter,
            show_all: false,
            live_rules: None,
            editing: None,
            e_priority: input(window, cx, "50"),
            e_pattern: input(window, cx, "pattern"),
            e_share: input(window, cx, "1"),
            e_category: None,
            e_direction: None,
            t_desc,
            t_amount,
            tested: None,
            suggestions: None,
            loading: false,
            _subs: subs,
        };
        if let Ok(d) = std::env::var("FINANCE_DESKTOP_CATS_TEST") {
            v.t_desc.update(cx, |s, cx| s.set_value(d, window, cx));
        }
        v
    }

    pub fn set_data(&mut self, ledger: Arc<Ledger>, cx: &mut Context<Self>) {
        let first = self.ledger.is_none();
        self.ledger = Some(ledger);
        if first {
            self.on_tab(cx);
        }
        cx.notify();
    }

    pub fn open(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.on_tab(cx);
        cx.notify();
    }

    /// What a tab needs from the server when it opens.
    fn on_tab(&mut self, cx: &mut Context<Self>) {
        if finance_core::signin::needed() {
            return;
        }
        match self.tab {
            Tab::Rules => self.load_rules(cx),
            Tab::Suggest => self.load_suggestions(cx),
            Tab::Categories => {}
        }
        if self.tab == Tab::Rules && std::env::var_os("FINANCE_DESKTOP_CATS_TEST").is_some() {
            self.run_test(cx);
        }
    }

    fn load_rules(&mut self, cx: &mut Context<Self>) {
        call(Request::new("GET", "/api/rules"), cx, |this, r, _| {
            if let Ok(v) = json_of(r) {
                let rev = v["data_rev"].as_i64().unwrap_or(0);
                if let Ok(rules) = serde_json::from_value::<Vec<Rule>>(v["rules"].clone()) {
                    this.live_rules = Some((rev, rules));
                }
            }
        });
    }

    fn load_suggestions(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        let fut = cats::suggestions();
        cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.suggestions = Some(json_of(r).and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string())));
                cx.notify();
            });
        })
        .detach();
    }

    fn categories(&self) -> Vec<Category> {
        self.ledger.as_ref().map(|l| l.categories.clone()).unwrap_or_default()
    }

    /// The rules: the server's fresh list (with hits) when it is as new as
    /// the ledger, else the ledger's.
    fn rules(&self) -> Vec<Rule> {
        let rev = self.ledger.as_ref().map(|l| l.data_rev).unwrap_or(0);
        match &self.live_rules {
            Some((r, rules)) if *r >= rev => rules.clone(),
            _ => self.ledger.as_ref().map(|l| l.rules.clone()).unwrap_or_default(),
        }
    }

    fn names(&self) -> Vec<String> {
        self.ledger.as_ref().map(|l| crate::catpicker::category_names(l)).unwrap_or_default()
    }

    /// Send a write; its reply goes to the app (and refreshes the open tab).
    fn send(&mut self, what: &str, fut: finance_core::client::BoxFuture, cx: &mut Context<Self>) {
        self.busy = true;
        self.note = None;
        let what = what.to_string();
        cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match json_of(r) {
                    Ok(v) => {
                        let n = v["changed"].as_array().map(|a| a.len()).unwrap_or(0);
                        this.note = Some((true, format!("{what}: {} row{} moved", fmt::count(n as f64), if n == 1 { "" } else { "s" })));
                        if let Ok(rules) = serde_json::from_value::<Vec<Rule>>(v["rules"].clone()) {
                            this.live_rules = Some((v["data_rev"].as_i64().unwrap_or(0), rules));
                        }
                        cx.emit(CategoriesEvent::Written(v));
                        if this.tab == Tab::Suggest {
                            this.load_suggestions(cx);
                        }
                    }
                    Err(e) => this.note = Some((false, e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------- categories ----------

    fn add_category(&mut self, cx: &mut Context<Self>) {
        let name = self.new_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.send(&format!("Added {name}"), cats::create_category(json!({"name": name})), cx);
    }

    fn start_rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.rename.update(cx, |s, cx| {
            s.set_value(name.clone(), window, cx);
            s.focus(window, cx);
        });
        self.renaming = Some(name);
        cx.notify();
    }

    fn finish_rename(&mut self, cx: &mut Context<Self>) {
        let Some(old) = self.renaming.take() else { return };
        let new = self.rename.read(cx).value().trim().to_string();
        if new.is_empty() || new == old {
            cx.notify();
            return;
        }
        self.send(&format!("Renamed {old} to {new}"), cats::patch_category(&old, json!({"name": new})), cx);
    }

    fn patch(&mut self, name: &str, fields: Value, cx: &mut Context<Self>) {
        self.send(&format!("Updated {name}"), cats::patch_category(name, fields), cx);
    }

    fn move_by(&mut self, name: &str, step: i32, cx: &mut Context<Self>) {
        let mut order: Vec<String> = self.categories().into_iter().map(|c| c.name).collect();
        let Some(i) = order.iter().position(|n| n == name) else { return };
        let j = i as i32 + step;
        if j < 0 || j as usize >= order.len() {
            return;
        }
        order.swap(i, j as usize);
        self.send("Reordered", cats::reorder(&order), cx);
    }

    fn category_row(&self, i: usize, c: &Category, n: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let name = c.name.clone();
        let renaming = self.renaming.as_deref() == Some(name.as_str());
        let kind_btn = {
            let name = name.clone();
            let view = cx.entity().downgrade();
            Button::new(("kind", i)).xsmall().outline().label(cats::kind_label(&c.kind)).dropdown_menu(move |mut menu, _, _| {
                for k in cats::KINDS {
                    let (view, name) = (view.clone(), name.clone());
                    menu = menu.item(PopupMenuItem::new(cats::kind_label(k)).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.patch(&name, json!({"kind": k}), cx));
                    }));
                }
                menu
            })
        };
        let fixed_btn = {
            let name = name.clone();
            let view = cx.entity().downgrade();
            let label = match c.fixed.as_str() { "always" => "Fixed", "never" => "Never fixed", _ => "Fixed if steady" };
            Button::new(("fixed", i)).xsmall().ghost().label(label).dropdown_menu(move |mut menu, _, _| {
                for (f, l) in [("auto", "Fixed if steady (6+ months, ±15%)"), ("always", "Always fixed"), ("never", "Never fixed")] {
                    let (view, name) = (view.clone(), name.clone());
                    menu = menu.item(PopupMenuItem::new(l).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.patch(&name, json!({"fixed": f}), cx));
                    }));
                }
                menu
            })
        };
        let other = name == cats::OTHER;
        let names = self.names();
        let view = cx.entity().downgrade();
        let more_btn = {
            let (view, name, names) = (view.clone(), name.clone(), names.clone());
            let (reimb, hidden) = (c.reimbursable, c.hidden);
            let (recurring, placeless, obligation) = (cats::recurring_hint(&c.name), cats::placeless(&c.name), cats::obligation(&c.name));
            Button::new(("more", i)).xsmall().ghost().icon(IconName::Ellipsis).dropdown_menu(move |menu, _, _| {
                let (v1, n1) = (view.clone(), name.clone());
                let (v2, n2) = (view.clone(), name.clone());
                let (v5, n5) = (view.clone(), name.clone());
                let (v6, n6) = (view.clone(), name.clone());
                let (v3, n3, l3) = (view.clone(), name.clone(), names.clone());
                let (v4, n4, l4) = (view.clone(), name.clone(), names.clone());
                let mut menu = menu.min_w(px(220.))
                    .item(PopupMenuItem::new("Reimbursable (money that comes back)").checked(reimb).on_click(move |_, _, cx| {
                        let _ = v1.update(cx, |this, cx| this.patch(&n1, json!({"reimbursable": !reimb}), cx));
                    }))
                    .item(PopupMenuItem::new("Hidden from the pickers").checked(hidden).on_click(move |_, _, cx| {
                        let _ = v2.update(cx, |this, cx| this.patch(&n2, json!({"hidden": !hidden}), cx));
                    }))
                    .item(PopupMenuItem::new("Paid from anywhere (places nobody on a trip)").checked(placeless).on_click(move |_, _, cx| {
                        let _ = v5.update(cx, |this, cx| this.patch(&n5, json!({"placeless": !placeless}), cx));
                    }))
                    .item(PopupMenuItem::new("An obligation (never \"maybe forgotten\")").checked(obligation).on_click(move |_, _, cx| {
                        let _ = v6.update(cx, |this, cx| this.patch(&n6, json!({"obligation": !obligation}), cx));
                    }))
                    .separator();
                for (r, l) in [("strong", "Recurring by nature (a steady cadence is enough)"), ("neutral", "Recurring when cadence and amount are steady"),
                               ("strict", "Everyday spending (only a near-perfect pattern recurs)")] {
                    let (v, n) = (view.clone(), name.clone());
                    menu = menu.item(PopupMenuItem::new(l).checked(recurring == r).on_click(move |_, _, cx| {
                        let _ = v.update(cx, |this, cx| this.patch(&n, json!({"recurring": r}), cx));
                    }));
                }
                menu.separator()
                    .item(PopupMenuItem::new("Merge into…").disabled(other).on_click(move |_, window, cx| {
                        let (view, from) = (v3.clone(), n3.clone());
                        let choices: Vec<String> = l3.iter().filter(|n| **n != from && n.as_str() != cats::OTHER).cloned().collect();
                        choose(format!("Merge {from} into…"), choices, None, Rc::new(move |into, _, cx| {
                            let _ = view.update(cx, |this, cx| this.send(&format!("Merged {from} into {into}"), cats::merge(&from, &into), cx));
                        }), window, cx);
                    }))
                    .item(PopupMenuItem::new("Delete…").disabled(other).on_click(move |_, window, cx| {
                        let (view, from) = (v4.clone(), n4.clone());
                        let choices: Vec<String> = l4.iter().filter(|n| **n != from).cloned().collect();
                        choose(format!("Delete {from}: its rows go to…"), choices,
                            Some(format!("{}: back to uncategorized (its rules go)", cats::OTHER)), Rc::new(move |into, _, cx| {
                            let _ = view.update(cx, |this, cx| this.send(&format!("Deleted {from}"), cats::delete_category(&from, &into), cx));
                        }), window, cx);
                    }))
            })
        };
        let icon_btn = {
            let (view, name) = (view.clone(), name.clone());
            div().id(("icon", i)).cursor_pointer().child(category::tile(&c.name, 28., t.primary))
                .on_click(move |_, window, cx| {
                    let (view, name) = (view.clone(), name.clone());
                    icon_picker(name.clone(), Rc::new(move |icon, _, cx| {
                        let _ = view.update(cx, |this, cx| this.patch(&name, json!({"icon": icon}), cx));
                    }), window, cx);
                })
        };
        let mut flags = vec![format!("{} row{} · {} rule{}", fmt::count(c.rows as f64), if c.rows == 1 { "" } else { "s" },
            fmt::count(c.rules as f64), if c.rules == 1 { "" } else { "s" })];
        if c.reimbursable {
            flags.push("reimbursable".into());
        }
        if c.hidden {
            flags.push("hidden".into());
        }
        let name_el: AnyElement = if renaming {
            h_flex().flex_1().min_w_0().gap_1().child(div().flex_1().child(Input::new(&self.rename).small()))
                .child(Button::new("rename-ok").xsmall().primary().label("Rename").on_click(cx.listener(|this, _, _, cx| this.finish_rename(cx))))
                .into_any_element()
        } else {
            let n2 = name.clone();
            v_flex().flex_1().min_w_0()
                .child(h_flex().id(("name", i)).gap_1().items_center().cursor_pointer().when(!other, |d| d.hover(|s| s.text_color(t.primary)))
                    .child(div().min_w_0().text_size(px(ts::BODY)).font_weight(FontWeight::MEDIUM).overflow_hidden().text_ellipsis().whitespace_nowrap().child(c.name.clone()))
                    .when(!other, |d| d.child(Icon::new(gpui_kit::assets::IconName::Pencil).size(px(11.)).text_color(t.muted_foreground))
                        .on_click(cx.listener(move |this, _, window, cx| this.start_rename(n2.clone(), window, cx)))))
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).figures().overflow_hidden().text_ellipsis().whitespace_nowrap()
                    .child(flags.join(" · ")))
                .into_any_element()
        };
        let (n_up, n_down) = (name.clone(), name.clone());
        h_flex().gap_2().items_center().py(px(5.)).border_b_1().border_color(ui::hairline(cx))
            .child(v_flex()
                .child(Button::new(("up", i)).xsmall().ghost().icon(IconName::ChevronUp).disabled(i == 0)
                    .on_click(cx.listener(move |this, _, _, cx| this.move_by(&n_up, -1, cx))))
                .child(Button::new(("down", i)).xsmall().ghost().icon(IconName::ChevronDown).disabled(i + 1 == n)
                    .on_click(cx.listener(move |this, _, _, cx| this.move_by(&n_down, 1, cx)))))
            .child(icon_btn)
            .child(name_el)
            // fixed columns: the kind chips line up whatever the fixed label says
            .child(div().flex_shrink_0().min_w(px(88.)).flex().justify_end().child(kind_btn))
            .child(div().flex_shrink_0().w(px(108.)).child(fixed_btn))
            .child(more_btn)
            .into_any_element()
    }

    fn categories_tab(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let list = self.categories();
        if list.is_empty() {
            return vec![ui::empty_with(IconName::Inbox, "No categories table yet",
                "An up-to-date server makes it when it starts. Sync after the server is updated.", cx).into_any_element()];
        }
        let n = list.len();
        let rows: Vec<AnyElement> = list.iter().enumerate().map(|(i, c)| {
            let row = self.category_row(i, c, n, cx);
            ui::card_slice(i == 0, i + 1 == n, cx).child(row).into_any_element()
        }).collect();
        let head = v_flex().gap_3().pb(px(sp::GRID))
            .child(h_flex().gap_2().items_center().flex_wrap()
                .child(div().w(px(240.)).child(Input::new(&self.new_name).small()))
                .child(Button::new("cat-add").small().primary().icon(IconName::Plus).label("Add category").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.add_category(cx))))
                )
            .child(div().text_size(px(ts::CAPTION)).text_color(cx.theme().muted_foreground).child("Kind decides what counts as spending; fixed, the Spending page's fixed costs; reimbursable money is never a budget suggestion. Click an icon to change it, a name to rename it."));
        std::iter::once(head.into_any_element()).chain(rows).collect()
    }

    // ---------- rules ----------

    fn start_edit(&mut self, r: &Rule, window: &mut Window, cx: &mut Context<Self>) {
        let set = |i: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| i.update(cx, |s, cx| s.set_value(v, window, cx));
        set(&self.e_priority.clone(), r.priority.to_string(), window, cx);
        set(&self.e_pattern.clone(), r.pattern.clone(), window, cx);
        set(&self.e_share.clone(), r.share.map(|s| format!("{s}")).unwrap_or_default(), window, cx);
        self.e_category = Some(r.category.clone());
        self.e_direction = r.direction.clone();
        self.editing = Some(r.id);
        cx.notify();
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.editing else { return };
        let Some(r) = self.rules().into_iter().find(|r| r.id == id) else { return };
        let mut f = serde_json::Map::new();
        let pr = self.e_priority.read(cx).value().trim().to_string();
        match pr.parse::<i64>() {
            Ok(p) if p != r.priority => {
                f.insert("priority".into(), json!(p));
            }
            Ok(_) => {}
            Err(_) => {
                self.note = Some((false, "the priority is a whole number, 0 to 1000".into()));
                cx.notify();
                return;
            }
        }
        let pat = self.e_pattern.read(cx).value().to_string();
        if pat != r.pattern {
            f.insert("pattern".into(), json!(pat));
        }
        let share = self.e_share.read(cx).value().trim().to_string();
        let share = if share.is_empty() { None } else { share.trim_end_matches('%').parse::<f64>().ok().map(|s| if s > 1. { s / 100. } else { s }) };
        if share != r.share {
            f.insert("share".into(), json!(share));
        }
        if let Some(c) = self.e_category.clone().filter(|c| *c != r.category) {
            f.insert("category".into(), json!(c));
        }
        if self.e_direction != r.direction {
            f.insert("direction".into(), json!(self.e_direction.clone().unwrap_or_else(|| "any".into())));
        }
        self.editing = None;
        if f.is_empty() {
            cx.notify();
            return;
        }
        self.send(&format!("Rule #{id} saved"), cats::patch_rule(id, Value::Object(f)), cx);
    }

    fn run_test(&mut self, cx: &mut Context<Self>) {
        let desc = self.t_desc.read(cx).value().to_string();
        if desc.trim().is_empty() {
            return;
        }
        let amount = self.t_amount.read(cx).value().trim().replace(',', "").parse::<f64>().unwrap_or(-1.);
        let fut = cats::test_rule(&desc, amount);
        cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |this, cx| {
                this.tested = Some(json_of(r).and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string())));
                cx.notify();
            });
        })
        .detach();
    }

    fn rule_row(&self, r: &Rule, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let id = r.id;
        let editing = self.editing == Some(id);
        let origin = match r.origin.as_str() {
            "seed" => ui::chip("starter", t.muted_foreground, cx),
            "suggested" => ui::chip("suggested", t.primary, cx),
            _ => ui::chip("yours", fmt::money_in(), cx),
        };
        let dir = |d: Option<&str>| match d { Some("in") => "in", Some("out") => "out", _ => "both" };
        let cell = |w: f32| div().w(px(w)).flex_shrink_0().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap();
        let enabled = Switch::new(("on", id as usize)).small().checked(r.enabled)
            .on_click(cx.listener(move |this, on: &bool, _, cx| {
                let on = *on;
                this.send(&format!("Rule #{id} {}", if on { "on" } else { "off" }), cats::patch_rule(id, json!({"enabled": on})), cx);
            }));
        let base = h_flex().gap_1().items_center().py(px(4.)).border_b_1().border_color(ui::hairline(cx)).text_size(px(ts::CAPTION))
            .when(!r.enabled, |d| d.opacity(0.55))
            .child(cell(26.).text_color(t.muted_foreground).figures().child(r.rank.map(|k| (k + 1).to_string()).unwrap_or_else(|| "–".into())))
            .child(cell(54.).child(origin))
            .child(cell(34.).child(enabled));
        if editing {
            let names = self.names();
            let view = cx.entity().downgrade();
            let cat_btn = {
                let view = view.clone();
                Button::new("e-cat").xsmall().outline().label(self.e_category.clone().unwrap_or_default()).dropdown_menu(move |mut menu, _, _| {
                    for n in names.iter() {
                        let (view, n2) = (view.clone(), n.clone());
                        menu = menu.item(PopupMenuItem::new(n.clone()).on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| { this.e_category = Some(n2.clone()); cx.notify(); });
                        }));
                    }
                    menu
                })
            };
            let dir_btn = {
                let view = view.clone();
                Button::new("e-dir").xsmall().outline().label(dir(self.e_direction.as_deref())).dropdown_menu(move |mut menu, _, _| {
                    for (l, d) in [("money in", Some("in")), ("money out", Some("out")), ("both", None)] {
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(l).on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| { this.e_direction = d.map(String::from); cx.notify(); });
                        }));
                    }
                    menu
                })
            };
            return base
                .child(div().w(px(40.)).child(Input::new(&self.e_priority).xsmall()))
                .child(div().flex_1().min_w(px(80.)).child(Input::new(&self.e_pattern).xsmall()))
                .child(div().w(px(120.)).child(cat_btn))
                .child(div().w(px(48.)).child(dir_btn))
                .child(div().w(px(44.)).child(Input::new(&self.e_share).xsmall()))
                .child(Button::new("e-save").xsmall().primary().label("Save").on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))))
                .child(Button::new("e-cancel").xsmall().ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| { this.editing = None; cx.notify(); })))
                .into_any_element();
        }
        let r2 = r.clone();
        base.child(cell(36.).figures().child(r.priority.to_string()))
            .child(div().flex_1().min_w(px(80.)).overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(t.mono_font_family.clone())
                .child(format!("\u{201c}{}\u{201d}", r.pattern)))
            .child(cell(120.).child(category::label(&r.category, cx)))
            .child(cell(36.).text_color(t.muted_foreground).child(dir(r.direction.as_deref())))
            .child(cell(44.).text_color(t.muted_foreground).child(r.share.map(|s| format!("{:.1}%", s * 100.)).unwrap_or_default()))
            .child(cell(40.).text_right().figures().text_color(t.muted_foreground).child(if r.hits > 0 { fmt::count(r.hits as f64) } else { String::new() }))
            .child(Button::new(("edit", id as usize)).xsmall().ghost().icon(gpui_kit::assets::IconName::Pencil)
                .on_click(cx.listener(move |this, _, window, cx| this.start_edit(&r2, window, cx))))
            .child(Button::new(("del", id as usize)).xsmall().ghost().icon(IconName::Delete)
                .on_click(cx.listener(move |this, _, _, cx| this.send(&format!("Rule #{id} deleted"), cats::delete_rule(id), cx))))
            .into_any_element()
    }

    fn starter_offer(&self, enabled: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        (enabled < FEW_RULES).then(|| {
            ui::card(cx).child(h_flex().gap_3().items_center().flex_wrap()
                .child(div().flex_shrink_0().size(px(32.)).rounded(px(9.)).bg(cx.theme().primary.opacity(0.14)).flex().items_center().justify_center()
                    .child(Icon::new(gpui_kit::assets::IconName::Sparkles).size(px(18.)).text_color(cx.theme().primary)))
                .child(v_flex().flex_1().min_w(px(240.))
                    .child(div().text_size(px(ts::TITLE)).font_semibold().child("Starting from zero?"))
                    .child(ui::caption(format!("{enabled} rule{} so far. The starter set files common merchants (groceries, transport, dining, \
                        subscriptions, travel…) into a ready category set; your own rules always come first.", if enabled == 1 { "" } else { "s" }), cx)))
                .child(Button::new("starter").small().primary().icon(IconName::Plus).label("Add the starter set").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.send("Starter set added", cats::install_starter(), cx))))
                .child(Button::new("starter-suggest").small().outline().label("Categorize Other")
                    .on_click(cx.listener(|this, _, _, cx| this.open(Tab::Suggest, cx)))))
                .into_any_element()
        })
    }

    fn rules_tab(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = cx.theme().clone();
        let rules = self.rules();
        if rules.is_empty() && self.categories().is_empty() {
            return vec![ui::empty_with(IconName::Inbox, "No managed rules yet",
                "An up-to-date server adds the rule columns when it starts. Sync after the server is updated.", cx).into_any_element()];
        }
        let q = self.filter.read(cx).value().trim().to_lowercase();
        let shown: Vec<&Rule> = rules.iter().filter(|r| q.is_empty() || r.pattern.contains(&q) || r.category.to_lowercase().contains(&q)).collect();
        let total = shown.len();
        let cap = if self.show_all || !q.is_empty() { total } else { total.min(RULES_SHOWN) };
        let rows: Vec<AnyElement> = shown.iter().take(cap).map(|r| self.rule_row(r, cx)).collect();
        let enabled = rules.iter().filter(|r| r.enabled).count();
        let users = rules.iter().filter(|r| r.origin != "seed").count();
        let tested: Option<AnyElement> = self.tested.as_ref().map(|r| match r {
            Err(e) => div().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).child(e.clone()).into_any_element(),
            Ok(tr) => v_flex().gap_1()
                .child(h_flex().gap_2().items_center()
                    .child(category::label(&tr.category, cx).text_color(t.foreground).text_size(px(ts::BODY)))
                    .when_some(tr.winner.as_ref(), |d, w| d.child(ui::caption(format!("rule #{} \u{201c}{}\u{201d}", w.id, w.pattern), cx))))
                .child(ui::caption_wrap(tr.why.clone(), cx))
                .when(!tr.also.is_empty(), |d| d.child(ui::caption_wrap(format!("Also matching, later: {}",
                    tr.also.iter().take(6).map(|a| format!("\u{201c}{}\u{201d} → {}", a.pattern, a.category)).collect::<Vec<_>>().join(", ")), cx)))
                .when(!tr.disabled.is_empty(), |d| d.child(ui::caption(format!("Switched off but matching: {}",
                    tr.disabled.iter().map(|a| format!("\u{201c}{}\u{201d}", a.pattern)).collect::<Vec<_>>().join(", ")), cx)))
                .into_any_element(),
        });
        let header = h_flex().gap_1().py(px(4.)).border_b_1().border_color(t.border).text_size(px(ts::EYEBROW)).text_color(t.muted_foreground)
            .child(div().w(px(26.)).child("#")).child(div().w(px(54.)).child("ORIGIN")).child(div().w(px(34.)).child("ON"))
            .child(div().w(px(36.)).child("PRIO")).child(div().flex_1().min_w(px(80.)).child("CONTAINS"))
            .child(div().w(px(120.)).child("CATEGORY")).child(div().w(px(36.)).child("DIR")).child(div().w(px(44.)).child("SHARE"))
            .child(div().w(px(40.)).text_right().child("ROWS")).child(div().w(px(52.)));
        let more = cap < total;
        let n = rows.len();
        let head = v_flex().gap_3().pb(px(sp::GRID))
            .children(self.starter_offer(enabled, cx))
            .child(ui::card(cx).child(v_flex().gap_2()
                .child(h_flex().gap_2().items_center()
                    .child(div().text_size(px(ts::TITLE)).font_semibold().child("Test"))
                    .child(ui::caption("which rule wins a transaction, and why", cx)))
                .child(h_flex().gap_2().items_center().flex_wrap()
                    .child(div().flex_1().min_w(px(240.)).child(Input::new(&self.t_desc).small()))
                    .child(div().w(px(110.)).child(Input::new(&self.t_amount).small()))
                    .child(Button::new("t-run").small().outline().icon(IconName::Search).label("Test").on_click(cx.listener(|this, _, _, cx| this.run_test(cx)))))
                .children(tested)))
            .child(v_flex().gap_1()
                .child(div().max_w(px(360.)).child(Input::new(&self.filter).small()))
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(format!("{} rules · {} on · {} yours. First match wins: a row you set by hand, then your rules, then the starter rules, each by priority (lower first).",
                    fmt::count(rules.len() as f64), fmt::count(enabled as f64), fmt::count(users as f64)))));
        let mut out = vec![head.into_any_element(), ui::card_slice(true, n == 0 && !more, cx).child(header).into_any_element()];
        out.extend(rows.into_iter().enumerate().map(|(i, r)| ui::card_slice(false, i + 1 == n && !more, cx).child(r).into_any_element()));
        if more {
            out.push(ui::card_slice(false, true, cx).child(h_flex().pt_2().justify_center()
                .child(Button::new("rules-all").small().ghost().label(format!("Show all {}", fmt::count(total as f64)))
                    .on_click(cx.listener(|this, _, _, cx| { this.show_all = true; cx.notify(); })))).into_any_element());
        }
        out
    }

    // ---------- Categorize Other ----------

    fn accept(&mut self, s: &cats::Suggestion, category: String, cx: &mut Context<Self>) {
        let d = cats::RuleDraft {
            pattern: s.pattern.clone(), direction: s.direction.clone(), category: category.clone(),
            priority: Some(s.priority), note: Some("from Categorize Other".into()), ..Default::default()
        };
        self.send(&format!("\u{201c}{}\u{201d} → {category}", s.pattern), cats::create_rule(&d), cx);
    }

    fn edit_suggestion(&mut self, s: &cats::Suggestion, cx: &mut Context<Self>) {
        let Some(l) = &self.ledger else { return };
        let ix = l.entries.iter().position(|e| e.category == cats::OTHER && !e.raw_hash.is_empty()
            && s.samples.first().is_some_and(|d| *d == e.description));
        if let Some(ix) = ix {
            cx.emit(CategoriesEvent::Edit { ix, pattern: s.pattern.clone(), direction: s.direction.clone(),
                category: s.proposal.as_ref().map(|p| p.category.clone()) });
        }
    }

    fn suggestion_card(&self, i: usize, s: &cats::Suggestion, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let names = self.names();
        let view = cx.entity().downgrade();
        let s1 = s.clone();
        let pick_btn = {
            let (view, s) = (view.clone(), s.clone());
            Button::new(("sg-pick", i)).small().outline().label("Pick…").dropdown_menu(move |mut menu, _, _| {
                for n in names.iter().filter(|n| n.as_str() != cats::OTHER) {
                    let (view, s, n2) = (view.clone(), s.clone(), n.clone());
                    menu = menu.item(PopupMenuItem::new(n.clone()).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.accept(&s, n2.clone(), cx));
                    }));
                }
                menu
            })
        };

        let proposal: AnyElement = match &s.proposal {
            Some(p) => {
                let (s3, cat) = (s.clone(), p.category.clone());
                h_flex().gap_2().items_center().flex_wrap()
                    .child(Button::new(("sg-accept", i)).small().primary().icon(IconName::Check).label(format!("{}", p.category)).disabled(self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| this.accept(&s3, cat.clone(), cx))))
                    .child(ui::caption_wrap(format!("{} · {}", p.reason, p.confidence), cx))
                    .into_any_element()
            }
            None => ui::caption("No guess: pick a category, or edit the rule.", cx).into_any_element(),
        };
        let money = fmt::signed_hkd(s.total_hkd);
        ui::card(cx).child(v_flex().gap_2()
            .child(h_flex().gap_3().items_center()
                .child(category::tile(s.proposal.as_ref().map(|p| p.category.as_str()).unwrap_or(cats::OTHER), 30.,
                    if s.proposal.is_some() { t.primary } else { t.muted_foreground }))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().text_size(px(ts::TITLE)).font_semibold().overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.label.clone()))
                    .child(ui::caption(format!("{} row{} · {} to {} · \u{201c}{}\u{201d}{} matches {} row{}", fmt::count(s.count as f64),
                        if s.count == 1 { "" } else { "s" }, s.first, s.last, s.pattern,
                        match s.direction.as_deref() { Some("in") => ", money in,", Some("out") => ", money out,", _ => "" },
                        fmt::count(s.matches as f64), if s.matches == 1 { "" } else { "s" }), cx)))
                .child(div().text_size(px(ts::TITLE)).figures().text_color(fmt::signed_color(s.total_hkd)).child(money)))
            .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap()
                .child(s.samples.join("  ·  ")))
            .child(h_flex().justify_between().items_center().gap_2().flex_wrap()
                .child(proposal)
                .child(h_flex().gap_2()
                    .child(pick_btn)
                    .child(Button::new(("sg-edit", i)).small().ghost().icon(gpui_kit::assets::IconName::Pencil).label("Edit rule…")
                        .on_click(cx.listener(move |this, _, _, cx| this.edit_suggestion(&s1, cx))))
                    )))
            .into_any_element()
    }

    fn suggest_tab(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let refresh = Button::new("sg-refresh").small().ghost().icon(IconName::RotateCw).label("Refresh").loading(self.loading)
            .on_click(cx.listener(|this, _, _, cx| this.load_suggestions(cx)));
        let gap = |e: AnyElement| div().pb(px(sp::GRID)).child(e).into_any_element();
        let mut out = vec![gap(h_flex().justify_end().child(refresh).into_any_element())];
        match &self.suggestions {
            None => out.push(ui::caption(if self.loading { "Clustering the uncategorized rows…" } else { "Nothing loaded yet." }, cx).into_any_element()),
            Some(Err(e)) => out.push(div().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).child(e.clone()).into_any_element()),
            Some(Ok(sg)) => {
                let sg = sg.clone();
                let cards: Vec<AnyElement> = sg.clusters.iter().enumerate().map(|(i, s)| gap(self.suggestion_card(i, s, cx))).collect();
                let empty = cards.is_empty();
                out.push(gap(v_flex().gap_3()
                    .children(self.starter_offer(sg.enabled_rules.max(0) as usize, cx))
                    .child(div().text_size(px(ts::CAPTION)).text_color(cx.theme().muted_foreground).child(format!("{} uncategorized rows, {} in all. The biggest merchants first (spend × rows); accept makes one of your rules.",
                        fmt::count(sg.other_rows as f64), fmt::signed_hkd(sg.other_hkd))))
                    .when(empty, |d| d.child(ui::empty_with(IconName::Check, "Nothing left in Other", "Every row has a category.", cx)))
                    .into_any_element()));
                out.extend(cards);
            }
        }
        out
    }
}

impl CategoriesView {
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The page as list sections: the tabs, then each tab's parts and its
    /// card a row at a time, so the app's virtualized list lays out only the
    /// rows on screen (a few hundred rules with menus and switches stutter
    /// when every frame lays them all out).
    pub fn sections(&mut self, cx: &mut Context<Self>) -> ui::Sections {
        let t = cx.theme().clone();
        let tab = self.tab;
        let seg = |label: &'static str, which: Tab, cx: &mut Context<Self>| {
            let on = tab == which;
            div().id(label).px(px(12.)).py(px(4.)).rounded(px(6.)).text_sm().cursor_pointer().whitespace_nowrap()
                .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
                .on_click(cx.listener(move |view, _, _, cx| if !on { view.open(which, cx) }))
                .child(label)
        };
        let tabs = h_flex().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx))
            .child(seg("Categories", Tab::Categories, cx))
            .child(seg("Rules", Tab::Rules, cx))
            .child(seg("Categorize Other", Tab::Suggest, cx));
        let note = self.note.clone().map(|(ok, n)| div().text_size(px(ts::CAPTION)).text_color(if ok { t.muted_foreground } else { fmt::money_out() }).child(n));
        let body = if self.ledger.is_none() {
            vec![ui::empty("No data yet: sync first", cx).into_any_element()]
        } else {
            match self.tab {
                Tab::Categories => self.categories_tab(cx),
                Tab::Rules => self.rules_tab(cx),
                Tab::Suggest => self.suggest_tab(cx),
            }
        };
        ui::Sections::new().gap(0.)
            .child(h_flex().gap_3().items_center().flex_wrap().pb(px(sp::L)).child(tabs).children(note))
            .children(body)
    }
}

impl Render for CategoriesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sections(cx)
    }
}

// ---------- dialogs ----------

type OnChoose = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// A searchable list of categories; `first` is an extra choice on top (its
/// value: the text before the first ':'). A pick closes the dialog.
pub fn choose(title: String, names: Vec<String>, first: Option<String>, on_pick: OnChoose, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
    let state = cx.new(|cx| CommandState::new(window, cx));
    let st = state.clone();
    let mut values: Vec<String> = Vec::new();
    let mut items: Vec<(String, String)> = Vec::new();
    if let Some(f) = first {
        let v = f.split(':').next().unwrap_or(&f).trim().to_string();
        items.push((v.clone(), f));
        values.push(v);
    }
    for n in names {
        values.push(n.clone());
        items.push((n.clone(), n));
    }
    let items = Rc::new(items);
    window.open_dialog(cx, move |dialog, _, _| {
        let group = CommandGroup::new().label(title.clone()).items(items.iter().map(|(v, l)| {
            CommandItem::new().label(l.clone()).icon(Icon::new(category::icon(v)))
        }));
        let (items2, pick) = (items.clone(), on_pick.clone());
        dialog.p_0().w(px(440.)).close_button(false).child(
            Command::new(&st).bordered(false).placeholder("Search categories…").max_h(px(420.)).group(group)
                .on_confirm(move |ix, window, cx| {
                    if let Some((v, _)) = items2.get(ix.row) {
                        window.close_dialog(cx);
                        pick(v.clone(), window, cx);
                    }
                })
                .on_cancel(|window, cx| window.close_dialog(cx)),
        )
    });
    state.update(cx, |s, cx| s.focus(window, cx));
}

/// The icons a category can have, in a grid; "From the name" clears the pick.
pub fn icon_picker(name: String, on_pick: Rc<dyn Fn(Option<String>, &mut Window, &mut App)>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = cx.theme().clone();
        let current = finance_core::categories::icon(&name);
        let tiles = category::NAMED.iter().enumerate().map(|(i, (n, icon))| {
            let on = current.as_deref() == Some(*n);
            let (pick, n2) = (on_pick.clone(), n.to_string());
            div().id(("ic", i)).size(px(40.)).flex().items_center().justify_center().rounded(px(9.)).cursor_pointer()
                .border_1().border_color(if on { t.primary } else { t.border })
                .when(on, |d| d.bg(t.primary.opacity(0.14)))
                .hover(|s| s.bg(t.accent))
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    pick(Some(n2.clone()), window, cx);
                })
                .child(Icon::new(*icon).size(px(18.)).text_color(if on { t.primary } else { t.foreground }))
        });
        let reset = on_pick.clone();
        dialog.w(px(560.)).title(format!("An icon for {name}")).child(v_flex().gap_3()
            .child(h_flex().flex_wrap().gap(px(6.)).children(tiles))
            .child(h_flex().justify_between().items_center()
                .child(h_flex().gap_2().items_center().child(ui::caption("From the name:", cx))
                    .child(Icon::new(category::from_name(&name)).size(px(16.)).text_color(t.muted_foreground)))
                .child(Button::new("ic-reset").small().ghost().label("Use the name's icon").on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    reset(None, window, cx);
                }))))
    });
}
