//! The window's category wiring (Phase 3): "Change category…" from the
//! detail sheet and the table's menu, the Categories page's events, and a
//! write's reply applied to the ledger at once (then a background sync for
//! the server-computed views). A child module of `app`.

use std::rc::Rc;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    Sizable as _, WindowExt as _,
};
use gpui_kit::*;
use serde_json::Value;

use super::FinanceApp;
use crate::catpicker::{CategoryPicker, PickerEvent};
use crate::pages::categories::{CategoriesEvent, CategoriesView, Tab};
use crate::pages::transactions::{OnChange, OnMark};
use crate::pages::Page;
use crate::zoom::px;

impl FinanceApp {
    /// The Categories page's events: a write to apply, a suggestion to edit.
    pub(super) fn wire_categories(view: &Entity<CategoriesView>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(view, window, |this, _, ev: &CategoriesEvent, window, cx| match ev {
            CategoriesEvent::Written(v) => this.apply_write(v.clone(), cx),
            CategoriesEvent::Edit { ix, pattern, direction, category } => {
                let (p, d, c) = (pattern.clone(), direction.clone(), category.clone());
                this.open_picker(*ix, Some((p, d, c)), window, cx);
            }
        })
    }

    /// What the detail sheet's "Change category…" calls.
    pub(super) fn on_change_category(&self, cx: &mut Context<Self>) -> OnChange {
        let view = cx.entity().downgrade();
        Rc::new(move |ix, window, cx| {
            let _ = view.update(cx, |this, cx| this.open_picker(ix, None, window, cx));
        })
    }

    /// What the detail sheet's "Mark as" calls.
    pub(super) fn on_mark_as(&self, cx: &mut Context<Self>) -> OnMark {
        let view = cx.entity().downgrade();
        Rc::new(move |ix, home, window, cx| {
            let _ = view.update(cx, |this, cx| this.open_picker_marked(ix, home, window, cx));
        })
    }

    /// The change-category picker on ledger row `ix`, in a dialog;
    /// `rule` opens it on "All like this" with (pattern, direction, category).
    pub(super) fn open_picker(&mut self, ix: usize, rule: Option<(String, Option<String>, Option<String>)>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_picker_on(ix, rule, None, window, cx)
    }

    /// "Mark as": the picker on ledger row `ix` with home category `home`
    /// (an index into HOME_CATEGORIES) picked. The category is created only
    /// when the write is saved, and only if the ledger has none of that name.
    pub(super) fn open_picker_marked(&mut self, ix: usize, home: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(h) = finance_core::home::HOME_CATEGORIES.get(home) {
            self.open_picker_on(ix, None, Some(h.name), window, cx);
        }
    }

    fn open_picker_on(&mut self, ix: usize, rule: Option<(String, Option<String>, Option<String>)>, mark: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return };
        let Some(e) = store.ledger.entries.get(ix).cloned() else { return };
        if e.raw_hash.is_empty() {
            return; // crypto and broker rows have no category to change
        }
        let ledger = store.ledger.clone();
        window.close_sheet(cx);
        window.close_dialog(cx);
        // every category counts, hidden ones too: a create would be refused
        let create = mark.and_then(|n| finance_core::home::create_if_missing(n, ledger.categories.iter().map(|c| c.name.as_str()).chain(ledger.entries.iter().map(|e| e.category.as_str()))));
        let picker = cx.new(|cx| CategoryPicker::new(e, ledger, window, cx));
        if let Some(n) = mark {
            picker.update(cx, |p, _| p.start_mark(n, create));
        }
        if let Some((pattern, direction, category)) = rule {
            picker.update(cx, |p, cx| p.start_all(pattern, direction, category, window, cx));
        }
        self._subs.push(cx.subscribe_in(&picker, window, |this, _, ev: &PickerEvent, window, cx| {
            window.close_dialog(cx);
            if let PickerEvent::Written(v) = ev {
                this.apply_write(v.clone(), cx);
            }
        }));
        let p = picker.clone();
        window.open_dialog(cx, move |dialog, _, _| dialog.w(px(720.)).title("Change category").child(p.clone()));
        picker.update(cx, |p, cx| p.focus(window, cx));
    }

    /// A write went through: patch the ledger now (the rows that moved, the
    /// categories and rules), then sync in the background for the views the
    /// server computes (Spending's totals, budgets' history, ...).
    pub(super) fn apply_write(&mut self, reply: Value, cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return };
        let mut ledger = (*store.ledger).clone();
        ledger.apply_write(&reply);
        let bundle = (*store.bundle).clone();
        self.set_store(ledger, bundle, cx);
        self.sync_after_write(cx);
        cx.notify();
    }

    fn sync_after_write(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_family = "wasm")]
        self.refetch_web(cx);
        #[cfg(not(target_family = "wasm"))]
        {
            if matches!(self.sync, super::SyncState::Running) {
                // one more pass once the running one lands
                cx.spawn(async move |this, cx| loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                    let done = this.update(cx, |this, cx| {
                        let busy = matches!(this.sync, super::SyncState::Running);
                        if !busy {
                            this.run_sync(cx);
                        }
                        !busy
                    });
                    if !matches!(done, Ok(false)) {
                        break;
                    }
                })
                .detach();
            } else {
                self.run_sync(cx);
            }
        }
    }

    /// The Categories page on a tab.
    pub(super) fn open_categories(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.cats.update(cx, |c, cx| c.open(tab, cx));
        self.go(Page::Categories, window, cx);
    }

    /// "Categorize Other (N)": a way in from Transactions and Spending, when
    /// some bank or card rows have no category.
    pub(super) fn categorize_button(&self, id: &'static str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let store = self.store.as_ref()?;
        let n = store.ledger.entries.iter().filter(|e| !e.raw_hash.is_empty() && e.category == finance_core::categories::OTHER).count();
        if n == 0 {
            return None;
        }
        let view = cx.entity().downgrade();
        Some(Button::new(id).xsmall().ghost().icon(gpui_kit::assets::IconName::Wand)
            .label(format!("Categorize Other ({})", crate::fmt::count(n as f64)))
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| this.open_categories(Tab::Suggest, window, cx));
            })
            .into_any_element())
    }

    /// The same, as a callback (Spending's "Where it went").
    pub(super) fn on_categorize(&self, cx: &mut Context<Self>) -> Rc<dyn Fn(&mut Window, &mut App)> {
        let view = cx.entity().downgrade();
        Rc::new(move |window, cx| {
            let _ = view.update(cx, |this, cx| this.open_categories(Tab::Suggest, window, cx));
        })
    }

    /// Dev and screenshot aid: FINANCE_DESKTOP_CHANGE=N opens the picker on
    /// the N-th row of the current search (FINANCE_DESKTOP_PICK_ALL=1 on
    /// "All like this"). FINANCE_DESKTOP_MARK_AS=Electricity opens it on the
    /// first row with that home category picked ("Mark as").
    pub(super) fn category_aids_on_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(n) = std::env::var("FINANCE_DESKTOP_CHANGE").ok().and_then(|v| v.parse::<usize>().ok()) {
            if let Some(ix) = self.table.read(cx).delegate().entry_at(n) {
                self.open_picker(ix, None, window, cx);
            }
        }
        if let Some(i) = std::env::var("FINANCE_DESKTOP_MARK_AS").ok().and_then(|v| finance_core::home::HOME_CATEGORIES.iter().position(|c| c.name.eq_ignore_ascii_case(&v))) {
            if let Some(ix) = self.table.read(cx).delegate().entry_at(0) {
                self.open_picker_marked(ix, i, window, cx);
            }
        }
    }
}
