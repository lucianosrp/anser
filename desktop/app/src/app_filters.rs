//! The window's filter and deep-dive wiring: the Transactions filter bar
//! and summary strip, the Spending filter, the table's row menu, and the
//! merchant / category dives. A child module of `app`, so it can reach the
//! view's fields while app.rs stays small.

use std::rc::Rc;

use finance_core::filter::{Dim, Filter, Summary, Trips};
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

use super::FinanceApp;
use crate::dive::{self, Dive, Links, OnDive};
use crate::filterbar::{self, Data, OnQuery};
use crate::pages::spending::OnPick;
use crate::pages::transactions::{RowAction, RowMenu};
use crate::pages::{self, Page};

/// The page filters that aren't a query (Subscriptions, Travel).
#[derive(Default)]
pub(crate) struct PageFilters {
    /// Spending's include/exclude filter, in the query syntax
    pub spend: String,
    pub subs: pages::subscriptions::View,
    pub travel_year: Option<i32>,
}

impl PageFilters {
    /// Dev and screenshot aids: FINANCE_DESKTOP_SPEND_FILTER, _SUBS_ACCOUNT,
    /// _SUBS_STATUS, _TRAVEL_YEAR.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let status = match var("FINANCE_DESKTOP_SUBS_STATUS").as_deref() {
            Some("active") => pages::subscriptions::Status::Active,
            Some("flagged") => pages::subscriptions::Status::Flagged,
            Some("ended") => pages::subscriptions::Status::Ended,
            _ => Default::default(),
        };
        PageFilters {
            spend: var("FINANCE_DESKTOP_SPEND_FILTER").unwrap_or_default(),
            subs: pages::subscriptions::View { account: var("FINANCE_DESKTOP_SUBS_ACCOUNT"), status },
            travel_year: var("FINANCE_DESKTOP_TRAVEL_YEAR").and_then(|y| y.parse().ok()),
        }
    }
}

impl FinanceApp {
    /// What the filter bars read their options from.
    pub(super) fn filter_data(&self) -> Option<Data> {
        self.store.as_ref().map(|s| Data { ledger: s.ledger.clone(), trips: s.trip_index.clone() })
    }

    pub(super) fn trip_index(trips: &[finance_core::travel::Trip]) -> Trips {
        Trips::from_trips(trips)
    }

    /// Replace the Transactions query (chips and pickers edit it) and re-run it.
    pub(super) fn set_query(&mut self, q: String, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_value(q.clone(), window, cx));
        self.run_search(&q, cx); // set_value emits no Change event
    }

    pub(super) fn summarize(&mut self, rows: &[usize]) {
        self.search_summary = match &self.store {
            Some(s) => finance_core::filter::summarize(&s.ledger, rows),
            None => Summary::default(),
        };
    }

    fn on_query(&self, cx: &mut Context<Self>) -> OnQuery {
        let view = cx.entity().downgrade();
        Rc::new(move |q, window, cx| {
            let _ = view.update(cx, |this, cx| this.set_query(q, window, cx));
        })
    }

    pub(super) fn on_spend_filter(&self, cx: &mut Context<Self>) -> OnQuery {
        let view = cx.entity().downgrade();
        Rc::new(move |q, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.filters.spend = q;
                cx.notify();
            });
        })
    }

    pub(super) fn on_subs_view(&self, cx: &mut Context<Self>) -> pages::subscriptions::OnView {
        let view = cx.entity().downgrade();
        Rc::new(move |v, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.filters.subs = v;
                cx.notify();
            });
        })
    }

    pub(super) fn on_travel_year(&self, cx: &mut Context<Self>) -> pages::travel::OnYear {
        let view = cx.entity().downgrade();
        Rc::new(move |y, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.filters.travel_year = y;
                cx.notify();
            });
        })
    }

    pub(super) fn on_pick(&self, cx: &mut Context<Self>) -> OnPick {
        let view = cx.entity().downgrade();
        Rc::new(move |ix, window, cx| {
            let _ = view.update(cx, |this, cx| this.open_detail(ix, window, cx));
        })
    }

    pub(super) fn on_dive(&self, cx: &mut Context<Self>) -> OnDive {
        let view = cx.entity().downgrade();
        Rc::new(move |d, window, cx| {
            let _ = view.update(cx, |this, cx| this.dive(d, window, cx));
        })
    }

    /// A merchant's or a category's deep dive.
    pub(super) fn dive(&mut self, d: Dive, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return };
        let links = Links { ledger: store.ledger.clone(), on_pick: self.on_pick(cx), on_search: self.on_search(cx), on_dive: self.on_dive(cx) };
        window.close_sheet(cx);
        window.close_dialog(cx);
        self.dive_open = Some(d.clone());
        cx.notify();
        dive::open(d, links, window, cx);
    }

    /// The handler behind the table's right-click menu.
    pub(super) fn row_menu(cx: &mut Context<Self>) -> RowMenu {
        let view = cx.entity().downgrade();
        Rc::new(move |a, ix, window, cx| {
            let _ = view.update(cx, |this, cx| this.row_action(a, ix, window, cx));
        })
    }

    fn row_action(&mut self, a: RowAction, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.store.as_ref().and_then(|s| s.ledger.entries.get(ix)).cloned() else { return };
        let key = finance_core::drill::merchant_key(&e.description);
        let mut f = Filter::parse(&self.search.read(cx).value());
        match a {
            RowAction::Detail => return self.open_detail(ix, window, cx),
            RowAction::ChangeCategory => return self.open_picker(ix, None, window, cx),
            RowAction::MarkAs(i) => return self.open_picker_marked(ix, i, window, cx),
            RowAction::MerchantDive => return self.dive(Dive::Merchant(key), window, cx),
            RowAction::CategoryDive => return self.dive(Dive::Category(e.category), window, cx),
            RowAction::OnlyMerchant => f.set_values(Dim::Merchant, false, &[key]),
            RowAction::ExcludeMerchant => f.toggle(Dim::Merchant, true, &key),
            RowAction::OnlyCategory => f.set_values(Dim::Category, false, &[e.category.to_lowercase()]),
            RowAction::ExcludeCategory => f.toggle(Dim::Category, true, &e.category),
        }
        self.set_query(f.query(), window, cx);
    }

    /// Transactions: the filter bar over the table, and what the matches add up to.
    pub(super) fn render_filters(&self, cx: &mut Context<Self>) -> Option<Div> {
        let data = self.filter_data()?;
        let q = self.search.read(cx).value().to_string();
        let bar = filterbar::render("txn", &q, filterbar::TRANSACTIONS, true, &data, self.on_query(cx), cx);
        let view = cx.entity().downgrade();
        let plot: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, cx| {
            let _ = view.update(cx, |this, cx| {
                let q = this.search.read(cx).value().to_string();
                // an empty query plots everything: give the plot a name
                this.open_plot(if q.trim().is_empty() { "every row".into() } else { q }, window, cx)
            });
        });
        Some(gpui_kit::component::v_flex().gap_2().child(bar).child(filterbar::summary(&self.search_summary, Some(plot), cx)))
    }

    /// Dev and screenshot aids once the data is in: FINANCE_DESKTOP_DIVE
    /// ("merchant:sushiro", "category:Dining") opens a deep dive,
    /// FINANCE_DESKTOP_PICKER ("category", "merchant", "account", …) the
    /// Transactions picker for that dimension, FINANCE_DESKTOP_SAVED=1 the
    /// saved-filters dialog.
    pub(super) fn filter_aids_on_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(d) = std::env::var("FINANCE_DESKTOP_DIVE") {
            let d = match d.split_once(':') {
                Some(("merchant", k)) => Some(Dive::Merchant(k.to_lowercase())),
                Some(("category", c)) => Some(Dive::Category(c.to_string())),
                _ => None,
            };
            if let Some(d) = d {
                self.dive(d, window, cx);
            }
        }
        let q = self.search.read(cx).value().to_string();
        if let (Ok(p), Some(data)) = (std::env::var("FINANCE_DESKTOP_PICKER"), self.filter_data()) {
            let dim = Dim::ALL.into_iter().find(|d| d.label().eq_ignore_ascii_case(&p) || d.key() == p);
            if let Some(dim) = dim {
                let dims = if dim == Dim::Account { vec![Dim::Account, Dim::Source] } else { vec![dim] };
                let on_query = if self.page == Page::Spending { self.on_spend_filter(cx) } else { self.on_query(cx) };
                let q = if self.page == Page::Spending { self.filters.spend.clone() } else { q.clone() };
                filterbar::open_picker(dim.label().to_string(), dims, false, q, data, on_query, window, cx);
            }
        }
        self.category_aids_on_load(window, cx);
        if std::env::var_os("FINANCE_DESKTOP_SAVED").is_some() {
            let on_query = self.on_query(cx);
            filterbar::open_saved(q, on_query, window, cx);
        }
    }
}
