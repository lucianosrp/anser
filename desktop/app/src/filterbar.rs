//! The filter bar: the active filters as chips (× clears one, a click on a
//! category or account chip flips it between "only" and "not"), "Clear all",
//! and an add button per dimension. Long lists (accounts, categories,
//! merchants, trips) open a searchable picker with a row count per option;
//! short ones (date, amount, direction) are dropdowns.
//!
//! The bar holds no state of its own: it reads a query string and writes a
//! new one through `on_query` (`finance_core::filter` does the editing), so
//! the search box and the chips can never disagree.
//!
//! Saved filters live next to theme.toml (`filters.toml`), or in
//! `$FINANCE_DESKTOP_FILTERS` (dev and screenshots).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use finance_core::Ledger;
use finance_core::filter::{self, DateRange, Dim, Filter, Summary, Term, Trips};
use gpui_kit::assets::{IconName as Lucide, icon_assets};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    command::{Command, CommandGroup, CommandItem, CommandState},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::fmt;
use crate::ui::{self, Figures as _};
use crate::zoom::px;

icon_assets!(pub FilterIcons, [ListFilter, ChartColumn, Bookmark, Store]);

/// Replace the query (the page's search or filter).
pub type OnQuery = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// An add button.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Add {
    /// keep only the picked values
    Only(Dim),
    /// leave the picked values out
    Not(Dim),
    Date,
    Amount,
    Direction,
}

/// The Transactions page: everything.
pub const TRANSACTIONS: &[Add] = &[
    Add::Only(Dim::Account), Add::Only(Dim::Category), Add::Only(Dim::Merchant), Add::Date, Add::Amount,
    Add::Direction, Add::Only(Dim::Currency), Add::Only(Dim::Trip),
];
/// The Spending page: which accounts and categories count.
pub const SPENDING: &[Add] = &[
    Add::Only(Dim::Account), Add::Not(Dim::Account), Add::Not(Dim::Category), Add::Only(Dim::Category), Add::Only(Dim::Currency),
];

/// What the bar reads its options from.
#[derive(Clone)]
pub struct Data {
    pub ledger: Arc<Ledger>,
    pub trips: Arc<Trips>,
}

fn add_label(a: Add) -> String {
    match a {
        Add::Only(Dim::Account) => "Account".into(),
        Add::Only(d) => d.label().into(),
        Add::Not(d) => format!("Exclude {}", d.label().to_lowercase()),
        Add::Date => "Date".into(),
        Add::Amount => "Amount".into(),
        Add::Direction => "In / out".into(),
    }
}

/// An account picker offers whole kinds and sources (src:) next to single accounts.
fn picker_dims(d: Dim) -> Vec<Dim> {
    match d {
        Dim::Account => vec![Dim::Account, Dim::Source],
        d => vec![d],
    }
}

fn amount_label(t: &Term) -> Option<String> {
    Some(match t {
        Term::Above(v) => format!("Amount > {}", fmt::int(*v)),
        Term::Below(v) => format!("Amount < {}", fmt::int(*v)),
        Term::Between(Some(a), Some(b)) if a == b => format!("Amount = {}", fmt::int(*a)),
        Term::Between(Some(a), Some(b)) => format!("Amount {} – {}", fmt::int(*a), fmt::int(*b)),
        Term::Between(Some(a), None) => format!("Amount ≥ {}", fmt::int(*a)),
        Term::Between(None, Some(b)) => format!("Amount ≤ {}", fmt::int(*b)),
        _ => return None,
    })
}

/// What a chip says for term `t`, and whether it excludes.
pub fn chip_label(t: &Term, data: &Data) -> Option<(String, bool)> {
    let today = crate::budgets::today();
    Some(match t {
        Term::Text(_) => return None,
        Term::Clause(c) if c.values.is_empty() => return None,
        Term::Clause(c) => {
            let names: Vec<String> = c.values.iter().map(|v| filter::display(&data.ledger, &data.trips, c.dim, v)).collect();
            let shown = if names.len() > 2 { format!("{}, {} +{}", names[0], names[1], names.len() - 2) } else { names.join(", ") };
            (format!("{}{}: {shown}", if c.negate { "Not " } else { "" }, if c.negate { c.dim.label().to_lowercase() } else { c.dim.label().to_string() }), c.negate)
        }
        Term::Date(r) => (format!("Date: {}", filter::date_label(r, today)), false),
        Term::Direction(true) => ("Money in".into(), false),
        Term::Direction(false) => ("Money out".into(), false),
        t => (amount_label(t)?, false),
    })
}

fn chip(id: String, label: String, exclude: bool, flip: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
        remove: Rc<dyn Fn(&mut Window, &mut App)>, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    let (fg, bg, border) = if exclude {
        (fmt::money_out(), fmt::money_out().opacity(0.1), fmt::money_out().opacity(0.35))
    } else {
        (t.foreground, t.primary.opacity(0.12), t.primary.opacity(0.4))
    };
    let tip = if exclude { "Click to keep only these instead" } else { "Click to exclude these instead" };
    h_flex()
        .id(SharedString::from(id.clone()))
        .flex_shrink_0()
        .h(px(24.))
        .pl(px(8.))
        .pr(px(2.))
        .gap_1()
        .rounded(px(6.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(px(12.))
        .text_color(fg)
        .figures()
        .child(div().max_w(px(300.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
        .when_some(flip, |d, flip| {
            d.cursor_pointer()
                .hover(|s| s.border_color(fg.opacity(0.6)))
                .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
                .on_click(move |_, window, cx| flip(window, cx))
        })
        .child(Button::new(SharedString::from(format!("{id}-x"))).xsmall().ghost().icon(IconName::Close)
            .tooltip("Remove this filter")
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                remove(window, cx)
            }))
}

/// The bar: chips for `query`'s filters, then the add buttons in `adds`
/// (and saved filters when `saved`). `id` keeps two bars apart.
pub fn render(id: &'static str, query: &str, adds: &[Add], saved: bool, data: &Data, on_query: OnQuery, cx: &App) -> Div {
    let t = cx.theme();
    let f = Filter::parse(query);
    let mut chips = Vec::new();
    for (i, term) in f.terms.iter().enumerate() {
        let Some((label, exclude)) = chip_label(term, data) else { continue };
        let remove: Rc<dyn Fn(&mut Window, &mut App)> = {
            let (f, oq) = (f.clone(), on_query.clone());
            Rc::new(move |window, cx| {
                let mut g = f.clone();
                g.remove(i);
                oq(g.query(), window, cx)
            })
        };
        let flip: Option<Rc<dyn Fn(&mut Window, &mut App)>> = matches!(term, Term::Clause(c) if c.dim != Dim::Merchant && c.dim != Dim::Trip).then(|| {
            let (f, oq) = (f.clone(), on_query.clone());
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let mut g = f.clone();
                if let Some(Term::Clause(c)) = g.terms.get_mut(i) {
                    c.negate = !c.negate;
                }
                oq(g.query(), window, cx)
            }) as Rc<dyn Fn(&mut Window, &mut App)>
        });
        chips.push(chip(format!("{id}-chip-{i}"), label, exclude, flip, remove, cx));
    }
    let any = !chips.is_empty();
    let clear = any.then(|| {
        let (f, oq) = (f.clone(), on_query.clone());
        Button::new(SharedString::from(format!("{id}-clear"))).xsmall().ghost().label("Clear all")
            .on_click(move |_, window, cx| oq(Filter::parse(&f.text()).query(), window, cx))
    });

    let buttons = adds.iter().enumerate().map(|(k, &a)| add_button(id, k, a, &f, data, on_query.clone())).collect::<Vec<_>>();
    let saved_btn = saved.then(|| saved_button(id, &f, on_query.clone()));

    h_flex()
        .w_full()
        .gap_2()
        .flex_wrap()
        .items_center()
        .child(Icon::new(Lucide::ListFilter).small().text_color(if any { t.primary } else { t.muted_foreground }))
        .children(chips)
        .children(clear)
        .when(any, |d| d.child(div().w(px(1.)).h(px(16.)).bg(ui::hairline(cx))))
        .children(buttons)
        .children(saved_btn)
}

fn add_button(id: &'static str, k: usize, a: Add, f: &Filter, data: &Data, on_query: OnQuery) -> AnyElement {
    let bid = SharedString::from(format!("{id}-add-{k}"));
    let label = add_label(a);
    match a {
        Add::Only(dim) | Add::Not(dim) => {
            let negate = matches!(a, Add::Not(_));
            let (q, data) = (f.query(), data.clone());
            Button::new(bid).xsmall().ghost()
                .icon(if negate { IconName::Minus } else { IconName::Plus })
                .label(label.clone())
                .on_click(move |_, window, cx| open_picker(label.clone(), picker_dims(dim), negate, q.clone(), data.clone(), on_query.clone(), window, cx))
                .into_any_element()
        }
        Add::Date => {
            let f = f.clone();
            Button::new(bid).xsmall().ghost().icon(IconName::Calendar).label(label)
                .dropdown_menu(move |menu, _, _| {
                    let cur = f.date().cloned();
                    let mut menu = menu.min_w(px(200.));
                    for (name, r) in filter::date_presets(crate::budgets::today()) {
                        let (f, oq) = (f.clone(), on_query.clone());
                        let checked = cur.as_ref() == Some(&r);
                        menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(move |_, window, cx| {
                            let mut g = f.clone();
                            g.set_date((!checked).then(|| r.clone()));
                            oq(g.query(), window, cx)
                        }));
                    }
                    let (f2, oq2) = (f.clone(), on_query.clone());
                    let (f3, oq3) = (f.clone(), on_query.clone());
                    menu.separator()
                        .item(PopupMenuItem::new("Custom range…").on_click(move |_, window, cx| {
                            let f = f2.clone();
                            let oq = oq2.clone();
                            let initial = f.date().map(DateRange::query).unwrap_or_default();
                            prompt("Date range", "2026-01..2026-06", "A year (2025), a month (2026-03), a day, or a range: 2026-01..2026-06, 2026-03.. (since), ..2025 (until).",
                                initial, Rc::new(move |v: String, window: &mut Window, cx: &mut App| {
                                    let p = Filter::parse(v.trim());
                                    match p.terms.as_slice() {
                                        [Term::Date(r)] => {
                                            let mut g = f.clone();
                                            g.set_date(Some(r.clone()));
                                            oq(g.query(), window, cx);
                                            Ok(())
                                        }
                                        _ => Err("Not a date or a range. Try 2026-01..2026-06.".into()),
                                    }
                                }), window, cx);
                        }))
                        .when(cur.is_some(), |m| m.item(PopupMenuItem::new("Any time").on_click(move |_, window, cx| {
                            let mut g = f3.clone();
                            g.set_date(None);
                            oq3(g.query(), window, cx)
                        })))
                })
                .into_any_element()
        }
        Add::Amount => {
            let f = f.clone();
            Button::new(bid).xsmall().ghost().icon(IconName::Asterisk).label(label)
                .dropdown_menu(move |menu, _, _| {
                    let presets: [(&str, Term); 6] = [
                        ("Over 100", Term::Above(100.)), ("Over 500", Term::Above(500.)), ("Over 1,000", Term::Above(1000.)),
                        ("Over 5,000", Term::Above(5000.)), ("Under 50", Term::Below(50.)), ("100 to 500", Term::Between(Some(100.), Some(500.))),
                    ];
                    let cur: Vec<Term> = f.terms.iter().filter(|t| matches!(t, Term::Above(_) | Term::Below(_) | Term::Between(..))).cloned().collect();
                    let mut menu = menu.min_w(px(200.)).label(fmt::local("HK$, either direction"));
                    for (name, term) in presets {
                        let (f, oq) = (f.clone(), on_query.clone());
                        let checked = cur.len() == 1 && cur[0] == term;
                        menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(move |_, window, cx| {
                            let mut g = f.clone();
                            g.set_amount((!checked).then(|| term.clone()));
                            oq(g.query(), window, cx)
                        }));
                    }
                    let (f2, oq2) = (f.clone(), on_query.clone());
                    let (f3, oq3) = (f.clone(), on_query.clone());
                    let initial = cur.iter().map(Term::query).collect::<Vec<_>>().join(" ");
                    menu.separator()
                        .item(PopupMenuItem::new("Custom…").on_click(move |_, window, cx| {
                            let (f, oq) = (f2.clone(), oq2.clone());
                            prompt("Amount", "100..500", fmt::local_static("HK$, either direction: >300, <50, or a range 100..500 (also 100.. and ..500)."),
                                initial.clone(), Rc::new(move |v: String, window: &mut Window, cx: &mut App| {
                                    let v = v.trim().replace(' ', "");
                                    let p = Filter::parse(&v);
                                    let term = match p.terms.as_slice() {
                                        [t @ (Term::Above(_) | Term::Below(_) | Term::Between(..))] => Some(t.clone()),
                                        _ => match Filter::parse(&format!("amt:{v}")).terms.as_slice() {
                                            [t @ Term::Between(..)] => Some(t.clone()),
                                            _ => None,
                                        },
                                    };
                                    let Some(term) = term else { return Err("Try >300, <50 or 100..500.".into()) };
                                    let mut g = f.clone();
                                    g.set_amount(Some(term));
                                    oq(g.query(), window, cx);
                                    Ok(())
                                }), window, cx);
                        }))
                        .when(!cur.is_empty(), |m| m.item(PopupMenuItem::new("Any amount").on_click(move |_, window, cx| {
                            let mut g = f3.clone();
                            g.set_amount(None);
                            oq3(g.query(), window, cx)
                        })))
                })
                .into_any_element()
        }
        Add::Direction => {
            let f = f.clone();
            Button::new(bid).xsmall().ghost().icon(IconName::ChevronsUpDown).label(label)
                .dropdown_menu(move |menu, _, _| {
                    let cur = f.direction();
                    let mut menu = menu.min_w(px(160.));
                    for (name, d) in [("Money in", Some(true)), ("Money out", Some(false)), ("Both", None)] {
                        let (f, oq) = (f.clone(), on_query.clone());
                        menu = menu.item(PopupMenuItem::new(name).checked(cur == d).on_click(move |_, window, cx| {
                            let mut g = f.clone();
                            g.set_direction(d);
                            oq(g.query(), window, cx)
                        }));
                    }
                    menu
                })
                .into_any_element()
        }
    }
}

// ---------- the picker ----------

/// A searchable list of `dims`' values with row counts; Enter or a click
/// toggles one (the dialog stays open for more), Esc closes.
#[allow(clippy::too_many_arguments)]
pub fn open_picker(title: String, dims: Vec<Dim>, negate: bool, query: String, data: Data, on_query: OnQuery, window: &mut Window, cx: &mut App) {
    let filter = Rc::new(RefCell::new(Filter::parse(&query)));
    let groups: Rc<Vec<(Dim, Vec<filter::Choice>)>> = Rc::new(dims.iter()
        .map(|&d| (d, filter::choices(&data.ledger, &filter.borrow(), &data.trips, d)))
        .filter(|(_, c)| !c.is_empty())
        .collect());
    let state = cx.new(|cx| CommandState::new(window, cx));
    let st = state.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let f = filter.borrow();
        let mut cmd = Command::new(&st)
            .bordered(false)
            .placeholder(format!("Search {}…", title.to_lowercase()))
            .max_h(px(460.));
        for (dim, choices) in groups.iter() {
            let on = f.values(*dim, negate);
            let other = f.values(*dim, !negate);
            let heading = match (dim, groups.len()) {
                (Dim::Source, _) => "Kinds and sources".to_string(),
                (Dim::Account, n) if n > 1 => "Accounts".to_string(),
                _ => format!("{} · rows", dim.label()),
            };
            cmd = cmd.group(CommandGroup::new().label(heading).items(choices.iter().map(|c| {
                let note = if other.contains(&c.value) { if negate { " · kept only" } else { " · excluded" } } else { "" };
                CommandItem::new()
                    .label(format!("{} · {}{note}", c.label, fmt::count(c.count as f64)))
                    .checked(on.contains(&c.value))
            })));
        }
        drop(f);
        let (filter2, groups2, oq) = (filter.clone(), groups.clone(), on_query.clone());
        let verb = if negate { "exclude" } else { "keep only" };
        dialog.p_0().w(px(520.)).close_button(false).child(
            cmd.on_confirm(move |ix, window, cx| {
                let Some((dim, choices)) = groups2.get(ix.section) else { return };
                let Some(c) = choices.get(ix.row) else { return };
                let q = {
                    let mut f = filter2.borrow_mut();
                    f.toggle(*dim, negate, &c.value);
                    f.query()
                };
                oq(q, window, cx);
                window.refresh();
            })
            .on_cancel(|window, cx| window.close_dialog(cx))
            .footer(move |_, _, cx| {
                h_flex().px_3().py_2().gap_2().items_center().border_t_1().border_color(ui::hairline(cx))
                    .when(ui::show_shortcuts(), |d| d.child(ui::kbd("enter", cx)))
                    .child(ui::caption(format!("{verb} (toggle; pick several)"), cx))
                    .when(ui::show_shortcuts(), |d| d.child(ui::kbd("esc", cx)).child(ui::caption("done", cx)))
            }),
        )
    });
    state.update(cx, |s, cx| s.focus(window, cx));
}

// ---------- a one-field prompt ----------

type Submit = Rc<dyn Fn(String, &mut Window, &mut App) -> Result<(), String>>;

/// A small dialog with one input: Enter or Apply submits; an error stays open.
pub fn prompt(title: &'static str, placeholder: &'static str, hint: &'static str, initial: String, on_submit: Submit, window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(initial));
    let err: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let submit: Rc<dyn Fn(&mut Window, &mut App)> = {
        let (weak, err) = (input.downgrade(), err.clone());
        Rc::new(move |window, cx| {
            let Some(v) = weak.upgrade().map(|i| i.read(cx).value().to_string()) else { return };
            match on_submit(v, window, cx) {
                Ok(()) => window.close_dialog(cx),
                Err(e) => {
                    *err.borrow_mut() = Some(e);
                    window.refresh();
                }
            }
        })
    };
    let on_enter = submit.clone();
    window.subscribe(&input, cx, move |_, ev: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = ev {
            on_enter(window, cx);
        }
    }).detach();
    let inp = input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let submit = submit.clone();
        dialog.w(px(440.)).title(title).child(
            v_flex().gap_2()
                .child(Input::new(&inp))
                .child(div().text_size(px(12.)).text_color(cx.theme().muted_foreground).child(hint))
                .when_some(err.borrow().clone(), |d, e| d.child(div().text_size(px(12.)).text_color(fmt::money_out()).child(e)))
                .child(h_flex().justify_end().gap_2()
                    .child(Button::new("prompt-cancel").small().ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("prompt-ok").small().primary().label("Apply").on_click(move |_, window, cx| submit(window, cx)))),
        )
    });
    input.update(cx, |s, cx| s.focus(window, cx));
}

// ---------- saved filters ----------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedFilter {
    pub name: String,
    pub query: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedFilters {
    #[serde(rename = "filter")]
    pub filters: Vec<SavedFilter>,
}

/// The user_settings key.
pub const FILTERS_KEY: &str = "filters";

pub fn saved_path() -> PathBuf {
    if let Some(p) = std::env::var_os("FINANCE_DESKTOP_FILTERS") {
        return PathBuf::from(p);
    }
    dirs::config_dir().unwrap_or_default().join("finance-desktop").join("filters.toml")
}

impl SavedFilters {
    /// The server's saved filters (user_settings), else the local file.
    pub fn load() -> Self {
        if let Some(v) = finance_core::user_settings::get(FILTERS_KEY) {
            return serde_json::from_value(v).unwrap_or_default();
        }
        std::fs::read_to_string(saved_path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if finance_core::user_settings::save(FILTERS_KEY, serde_json::to_value(self)?) {
            return Ok(());
        }
        let p = saved_path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, toml::to_string(self)?)?;
        Ok(())
    }

    /// Add `name`, or replace the filter saved under it (names ignore case).
    pub fn put(&mut self, name: &str, query: &str) {
        let name = name.trim();
        match self.filters.iter_mut().find(|f| f.name.eq_ignore_ascii_case(name)) {
            Some(f) => f.query = query.to_string(),
            None => self.filters.push(SavedFilter { name: name.to_string(), query: query.to_string() }),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.filters.retain(|f| f.name != name);
    }
}

fn saved_button(id: &'static str, f: &Filter, on_query: OnQuery) -> AnyElement {
    let q = f.query();
    let has = f.has_filters();
    Button::new(SharedString::from(format!("{id}-saved"))).xsmall().ghost().icon(Lucide::Bookmark).label("Saved")
        .dropdown_menu(move |menu, _, _| {
            let saved = SavedFilters::load();
            let mut menu = menu.min_w(px(220.)).max_w(px(360.));
            if saved.filters.is_empty() {
                menu = menu.label("No saved filters yet");
            }
            for s in saved.filters {
                let (oq, sq) = (on_query.clone(), s.query.clone());
                menu = menu.item(PopupMenuItem::new(s.name.clone()).checked(s.query == q).on_click(move |_, window, cx| oq(sq.clone(), window, cx)));
            }
            let (oq, q) = (on_query.clone(), q.clone());
            menu.separator().item(PopupMenuItem::new(if has { "Save this filter…" } else { "Manage saved filters…" })
                .on_click(move |_, window, cx| open_saved(q.clone(), oq.clone(), window, cx)))
        })
        .into_any_element()
}

/// Name and save the current filter; apply or delete saved ones.
pub fn open_saved(query: String, on_query: OnQuery, window: &mut Window, cx: &mut App) {
    let list = Rc::new(RefCell::new(SavedFilters::load()));
    let has = Filter::parse(&query).has_filters();
    let name = cx.new(|cx| InputState::new(window, cx).placeholder("A name, e.g. HK dining"));
    let note: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let save: Rc<dyn Fn(&mut Window, &mut App)> = {
        let (weak, list, note, query) = (name.downgrade(), list.clone(), note.clone(), query.clone());
        Rc::new(move |window, cx| {
            let Some(n) = weak.upgrade().map(|i| i.read(cx).value().trim().to_string()) else { return };
            if n.is_empty() {
                *note.borrow_mut() = Some("Give it a name first.".into());
            } else {
                list.borrow_mut().put(&n, &query);
                *note.borrow_mut() = Some(match list.borrow().save() {
                    Ok(()) => format!("Saved as \u{201c}{n}\u{201d}."),
                    Err(e) => format!("Could not save: {e:#}"),
                });
            }
            window.refresh();
        })
    };
    let on_enter = save.clone();
    window.subscribe(&name, cx, move |_, ev: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = ev {
            on_enter(window, cx);
        }
    }).detach();
    let inp = name.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = cx.theme();
        let save = save.clone();
        let rows = v_flex().children(list.borrow().filters.iter().enumerate().map(|(i, s)| {
            let (oq, sq) = (on_query.clone(), s.query.clone());
            let (list, n) = (list.clone(), s.name.clone());
            h_flex()
                .id(("saved-row", i))
                .gap_3()
                .py_1()
                .px_1()
                .rounded(t.radius)
                .cursor_pointer()
                .hover(|st| st.bg(t.accent))
                .on_click(move |_, window, cx| {
                    oq(sq.clone(), window, cx);
                    window.close_dialog(cx);
                })
                .child(Icon::new(Lucide::Bookmark).xsmall().text_color(t.muted_foreground))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().text_sm().font_semibold().child(s.name.clone()))
                    .child(div().text_xs().text_color(t.muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.query.clone())))
                .child(Button::new(("saved-del", i)).xsmall().ghost().icon(IconName::Delete).tooltip("Delete")
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        list.borrow_mut().remove(&n);
                        let _ = list.borrow().save();
                        window.refresh();
                    }))
        }));
        let empty = list.borrow().filters.is_empty();
        dialog.w(px(520.)).title("Saved filters").child(
            v_flex().gap_3()
                .when(has, |d| d.child(v_flex().gap_2()
                    .child(ui::eyebrow("Save the current filter", cx))
                    .child(div().text_xs().text_color(t.muted_foreground).child(query.clone()))
                    .child(h_flex().gap_2()
                        .child(div().flex_1().child(Input::new(&inp)))
                        .child(Button::new("saved-save").small().primary().label("Save").on_click(move |_, window, cx| save(window, cx))))
                    .when_some(note.borrow().clone(), |d, n| d.child(div().text_xs().text_color(t.muted_foreground).child(n)))))
                .child(ui::eyebrow("Saved", cx))
                .child(if empty { ui::caption("Nothing saved yet. Set some filters, then save them here.", cx) } else { rows })
                .child(ui::caption(format!("Stored in {}", saved_path().display()), cx)),
        )
    });
    name.update(cx, |s, cx| s.focus(window, cx));
}

// ---------- the summary strip ----------

/// Count, in, out, net and a monthly sparkline for a filtered set, with a
/// plot button (the Ctrl+Enter plot).
pub fn summary(s: &Summary, on_plot: Option<Rc<dyn Fn(&mut Window, &mut App)>>, cx: &App) -> Div {
    let t = cx.theme();
    let stat = |label: &str, value: String, color: Option<Hsla>| {
        v_flex().gap(px(1.)).min_w(px(72.))
            .child(ui::eyebrow(label.to_string(), cx))
            .child(div().text_size(px(14.)).font_semibold().figures().whitespace_nowrap()
                .text_color(color.unwrap_or(t.foreground)).child(value))
    };
    // the side of the money the set is about: out for spending, in for pay
    let outgoing = s.outflow >= s.inflow;
    let vals: Vec<f64> = s.months.iter().rev().take(24).rev().map(|(_, i, o)| if outgoing { *o } else { *i }).collect();
    let color = if outgoing { fmt::money_out() } else { fmt::money_in() };
    let spark_label = format!("{} per month · last {} mo", if outgoing { "out" } else { "in" }, vals.len());
    h_flex()
        .w_full()
        .gap_4()
        .flex_wrap()
        // top-aligned: the sparkline column is taller, its label still lines up
        .items_start()
        .px_3()
        .py_2()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(ui::hairline(cx))
        .bg(t.secondary)
        .child(stat("Rows", fmt::count(s.count as f64), None))
        .child(stat("In", fmt::hkd(s.inflow), Some(fmt::money_in())))
        .child(stat("Out", fmt::hkd(s.outflow), Some(fmt::money_out())))
        .child(stat("Net", fmt::signed_hkd(s.net()), Some(fmt::signed_color(s.net()))))
        .when(vals.len() >= 2, |d| d.child(
            v_flex().gap(px(1.)).flex_1().min_w(px(140.)).max_w(px(320.))
                .child(ui::eyebrow(spark_label, cx))
                .child(div().h(px(26.)).w_full().child(ui::sparkline("filter-spark", &vals, color)))))
        .when_some(on_plot, |d, plot| d.child(div().flex_1().flex().justify_end().child(
            Button::new("filter-plot").small().outline().icon(Lucide::ChartColumn).label("Plot")
                .tooltip(ui::keyed("Plot every match", "Ctrl+Enter in the search"))
                .disabled(s.count == 0)
                .on_click(move |_, window, cx| plot(window, cx)))))
}

#[cfg(test)]
mod tests {
    use super::{SavedFilters, chip_label, Data};
    use finance_core::filter::{Filter, Trips};
    use std::sync::Arc;

    #[test]
    fn saved_file_format() {
        let mut s = SavedFilters::default();
        s.put("HK dining", "cat:dining ccy:hkd");
        s.put("hk dining", "cat:dining");
        s.put("Not rent", "-cat:rent");
        assert_eq!(s.filters.len(), 2);
        let txt = toml::to_string(&s).unwrap();
        assert!(txt.contains("[[filter]]"));
        let back: SavedFilters = toml::from_str(&txt).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.filters[0].query, "cat:dining");
        s.remove("Not rent");
        assert_eq!(s.filters.len(), 1);
    }

    #[test]
    fn chips_read_naturally() {
        let data = Data { ledger: Arc::new(Default::default()), trips: Arc::new(Trips::default()) };
        let f = Filter::parse("coffee cat:dining,groceries,travel -acct:hsbc_card out amt:100..500");
        let labels: Vec<(String, bool)> = f.terms.iter().filter_map(|t| chip_label(t, &data)).collect();
        assert_eq!(labels, vec![
            ("Category: dining, groceries +1".to_string(), false),
            ("Not account: hsbc_card".to_string(), true),
            ("Money out".to_string(), false),
            ("Amount 100 – 500".to_string(), false),
        ]);
    }
}
