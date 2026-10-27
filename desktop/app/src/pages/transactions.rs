//! Transactions: every row from every source in one virtualized table,
//! narrowed by the search box on each keystroke (finance_core::search).
//! Enter or a double-click opens a row in a side sheet with every field and
//! the same merchant's history.

use std::rc::Rc;
use std::sync::Arc;

use finance_core::{Entry, Ledger, drill};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    sheet::Sheet,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::category;
use crate::fmt;
use crate::ui::{self, Figures as _};
use crate::zoom::px;

/// What the row's right-click menu can do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RowAction {
    Detail,
    MerchantDive,
    CategoryDive,
    OnlyMerchant,
    OnlyCategory,
    ExcludeCategory,
    ExcludeMerchant,
    /// "Change category…": the picker (bank and card rows)
    ChangeCategory,
    /// "Mark as" a home cost: the picker on that category (index into HOME_CATEGORIES)
    MarkAs(usize),
}

/// Run a row action on ledger entry `ix`.
pub type RowMenu = Rc<dyn Fn(RowAction, usize, &mut Window, &mut App)>;

pub struct TxnTable {
    pub ledger: Arc<Ledger>,
    /// indices into ledger.entries, in display order
    pub rows: Vec<usize>,
    columns: Vec<Column>,
    /// the right-click menu's handler (set by the app)
    pub menu: Option<RowMenu>,
}

impl TxnTable {
    pub fn new(ledger: Arc<Ledger>) -> Self {
        TxnTable {
            rows: (0..ledger.entries.len()).collect(),
            menu: None,
            ledger,
            columns: vec![
                Column::new("date", "Date").width(px(100.)).sortable(),
                Column::new("desc", "Description").width(px(330.)),
                Column::new("hkd", fmt::base_ccy()).width(px(120.)).text_right().sortable(),
                Column::new("cat", "Category").width(px(160.)),
                Column::new("amount", "Native").width(px(150.)).text_right().sortable(),
                Column::new("src", "Source").width(px(140.)),
            ],
        }
    }

    pub fn set_rows(&mut self, rows: Vec<usize>) {
        self.rows = rows;
    }

    /// The ledger index shown at table row `row_ix`.
    pub fn entry_at(&self, row_ix: usize) -> Option<usize> {
        self.rows.get(row_ix).copied()
    }
}

impl TableDelegate for TxnTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let t = cx.theme();
        let Some(e) = self.rows.get(row_ix).and_then(|&i| self.ledger.entries.get(i)) else {
            return div();
        };
        match col_ix {
            0 => div().text_color(t.muted_foreground).figures().child(e.date.clone()),
            1 => h_flex()
                .gap_2()
                .items_center()
                .when_some(crate::logo::tile(crate::logo::for_entry(e), 18., cx), |d, l| d.child(l))
                .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.description.clone())),
            2 => {
                let h = e.hkd.unwrap_or(0.);
                let mine = if e.share < 1. { format!(" · {:.0}%", e.share * 100.) } else { String::new() };
                div().w_full().text_right().figures().text_color(fmt::signed_color(h)).child(format!("{}{mine}", fmt::int(h)))
            }
            // the cell's width, so a long name ends in an ellipsis
            3 => category::label(&e.category, cx).w_full(),
            5 => h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .text_color(t.muted_foreground)
                .when_some(crate::logo::tile(crate::logo::for_source(&e.source), 16., cx), |d, l| d.child(l))
                .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(format!("{} · {}", e.kind.label(), e.source))),
            _ => {
                let txt = if e.currency.len() <= 3 && e.currency.chars().all(|c| c.is_ascii_uppercase()) {
                    format!("{} {}", fmt::dec2(e.amount), e.currency)
                } else {
                    format!("{} {}", e.amount, e.currency) // crypto quantities keep full precision
                };
                div().w_full().text_right().figures().text_color(t.muted_foreground).child(txt)
            }
        }
    }

    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, _: &mut Window, _: &mut Context<TableState<Self>>) {
        let e = &self.ledger.entries;
        let key = |i: &usize| -> f64 {
            match col_ix {
                4 => e[*i].amount,
                _ => e[*i].hkd.unwrap_or(0.),
            }
        };
        // column 2 = HKD, 4 = native amount
        match (col_ix, sort) {
            (0, ColumnSort::Ascending) => self.rows.sort_by(|a, b| e[*a].date.cmp(&e[*b].date)),
            (0, _) => self.rows.sort_by(|a, b| e[*b].date.cmp(&e[*a].date)),
            (_, ColumnSort::Ascending) => self.rows.sort_by(|a, b| key(a).total_cmp(&key(b))),
            (_, ColumnSort::Descending) => self.rows.sort_by(|a, b| key(b).total_cmp(&key(a))),
            (_, ColumnSort::Default) => self.rows.sort(), // ledger order = newest first
        }
    }

    fn context_menu(&mut self, row_ix: usize, menu: PopupMenu, window: &mut Window, cx: &mut Context<TableState<Self>>) -> PopupMenu {
        let (Some(ix), Some(cb)) = (self.entry_at(row_ix), self.menu.clone()) else { return menu };
        let Some(e) = self.ledger.entries.get(ix) else { return menu };
        let key = drill::merchant_key(&e.description);
        let name = finance_core::filter::title(&key);
        let item = |label: String, a: RowAction| {
            let cb = cb.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| cb(a, ix, window, cx))
        };
        let mut m = menu.item(item("Open details".into(), RowAction::Detail));
        if !e.raw_hash.is_empty() {
            m = m.item(item("Change category…".into(), RowAction::ChangeCategory));
            let sub = PopupMenu::build(window, cx, |mut sub, _, _| {
                for (i, c) in finance_core::home::HOME_CATEGORIES.iter().enumerate() {
                    sub = sub.item(item(c.name.into(), RowAction::MarkAs(i)));
                }
                sub
            });
            m = m.item(PopupMenuItem::submenu("Mark as", sub));
        }
        if !key.is_empty() {
            m = m.item(item(format!("About {name}…"), RowAction::MerchantDive));
        }
        m = m.item(item(format!("About {}…", e.category), RowAction::CategoryDive)).separator();
        if !key.is_empty() {
            m = m.item(item(format!("Only {name}"), RowAction::OnlyMerchant))
                .item(item(format!("Hide {name}"), RowAction::ExcludeMerchant));
        }
        m.item(item(format!("Only {}", e.category), RowAction::OnlyCategory))
            .item(item(format!("Hide {}", e.category), RowAction::ExcludeCategory))
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        let e = &self.ledger.entries[self.rows[row_ix]];
        match col_ix {
            0 => e.date.clone(),
            1 => e.description.clone(),
            2 if crate::fmt::private() => crate::fmt::MASK.into(),
            2 => e.hkd.map(|h| format!("{h:.2}")).unwrap_or_default(),
            4 if crate::fmt::private() => format!("{} {}", crate::fmt::MASK, e.currency),
            3 => e.category.clone(),
            4 => format!("{} {}", e.amount, e.currency),
            _ => e.source.clone(),
        }
    }
}

// ---------- compact (phone): a list instead of the table ----------

/// Height of one list row: two lines, a comfortable tap target.
const LIST_ROW: f32 = 58.;

/// The search results as a virtualized list: logo, description over
/// category · date, the HKD amount. A tap opens the detail sheet
/// (`on_pick` gets the ledger index).
pub fn compact_list(ledger: Arc<Ledger>, rows: Vec<usize>, on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)>) -> impl IntoElement {
    let n = rows.len();
    uniform_list("txn-list", n, move |range, _, cx| {
        let t = cx.theme();
        range
            .filter_map(|r| {
                let ix = *rows.get(r)?;
                let e = ledger.entries.get(ix)?;
                let h = e.hkd.unwrap_or(0.);
                let pick = on_pick.clone();
                let hover = t.accent.opacity(0.6);
                Some(
                    h_flex()
                        .id(("txn-row", r))
                        .w_full()
                        .overflow_hidden()
                        .h(px(LIST_ROW))
                        .gap(px(12.))
                        .px(px(4.))
                        .border_b_1()
                        .border_color(ui::hairline(cx))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .child(match crate::logo::tile(crate::logo::for_entry(e), 30., cx) {
                            Some(mark) => mark,
                            None => div().flex_shrink_0().size(px(30.)).rounded_full().bg(t.muted).into_any_element(),
                        })
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.))
                                .child(div().text_size(px(14.)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(e.description.clone()))
                                .child(
                                    div().text_size(px(12.)).text_color(t.muted_foreground).whitespace_nowrap().overflow_hidden().text_ellipsis()
                                        .child(format!("{} · {} · {}", e.category, e.date, e.source)),
                                ),
                        )
                        .child(
                            v_flex()
                                .flex_shrink_0()
                                .max_w(px(130.))
                                .overflow_hidden()
                                .items_end()
                                .gap(px(2.))
                                .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(fmt::signed_color(h))
                                    .child(fmt::signed_hkd(h)))
                                .when(e.currency != fmt::base_ccy(), |d| d.child(div().max_w(px(130.)).text_size(px(11.)).text_color(t.muted_foreground)
                                    .whitespace_nowrap().overflow_hidden().text_ellipsis().child(native(e)))),
                        )
                        .on_click(move |_, window, cx| pick(ix, window, cx)),
                )
            })
            .collect::<Vec<_>>()
    })
    .size_full()
}

// ---------- detail sheet ----------

/// Show the transactions table with a search.
pub type OnSearch = Rc<dyn Fn(String, &mut Window, &mut App)>;

const SAME_SHOWN: usize = 10;

/// Everything the sheet shows for one row, computed once when it opens
/// (the sheet's builder re-runs on every frame).
pub struct Detail {
    pub ix: usize,
    pub key: String,
    pub same: Vec<usize>,
    pub stats: drill::MerchantStats,
}

impl Detail {
    pub fn new(ledger: &Ledger, ix: usize) -> Self {
        let key = ledger.entries.get(ix).map(|e| drill::merchant_key(&e.description)).unwrap_or_default();
        let same = drill::same_merchant(ledger, ix);
        let stats = drill::merchant_stats(ledger, &same);
        Detail { ix, key, same, stats }
    }
}

fn native(e: &Entry) -> String {
    if e.currency.len() <= 3 && e.currency.chars().all(|c| c.is_ascii_uppercase()) {
        format!("{} {}", fmt::dec2(e.amount), e.currency)
    } else {
        format!("{} {}", e.amount, e.currency) // crypto quantities keep full precision
    }
}

fn field(label: &str, value: impl Into<SharedString>, cx: &App) -> Div {
    let t = cx.theme();
    h_flex()
        .gap_3()
        .py_1()
        .items_start()
        .border_b_1()
        .border_color(t.border)
        .child(div().w(px(92.)).flex_shrink_0().text_xs().text_color(t.muted_foreground).pt(px(2.)).child(label.to_uppercase()))
        .child(div().flex_1().min_w_0().text_sm().child(value.into()))
}

/// `field` with any element as the value.
fn field_el(label: &str, value: impl IntoElement, cx: &App) -> Div {
    let t = cx.theme();
    h_flex()
        .gap_3()
        .py_1()
        .items_start()
        .border_b_1()
        .border_color(t.border)
        .child(div().w(px(92.)).flex_shrink_0().text_xs().text_color(t.muted_foreground).pt(px(2.)).child(label.to_uppercase()))
        .child(div().flex_1().min_w_0().text_sm().child(value))
}

fn mini(label: &str, value: String, cx: &App) -> Div {
    let t = cx.theme();
    v_flex()
        .child(div().text_xs().text_color(t.muted_foreground).child(label.to_uppercase()))
        .child(div().text_base().font_semibold().child(value))
}

/// "Change category…" on ledger row `ix` (the app opens the picker).
pub type OnChange = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// "Mark as" on ledger row `.0` with home category `.1` (an index into HOME_CATEGORIES).
pub type OnMark = Rc<dyn Fn(usize, usize, &mut Window, &mut App)>;

pub fn detail_sheet(sheet: Sheet, ledger: &Ledger, d: &Detail, on_search: OnSearch, on_dive: crate::dive::OnDive, on_change: OnChange, on_mark: OnMark, cx: &App) -> Sheet {
    let t = cx.theme();
    let Some(e) = ledger.entries.get(d.ix) else { return sheet };
    let mine = drill::my_hkd(e);
    let headline = mine.unwrap_or(e.amount);
    // on a phone the buttons are tap-sized
    let tap = |b: Button| if crate::layout::compact() { b.small().h(px(crate::layout::TOUCH)) } else { b };
    let change = (!e.raw_hash.is_empty()).then(|| {
        let ix = d.ix;
        tap(Button::new("detail-change").xsmall().outline().icon(gpui_kit::assets::IconName::Pencil).label("Change category…"))
            .on_click(move |_, window, cx| on_change(ix, window, cx))
    });
    // the same home categories as the row menu's "Mark as", for where there is no right-click
    let mark = (!e.raw_hash.is_empty()).then(|| {
        let ix = d.ix;
        tap(Button::new("detail-mark").xsmall().outline().icon(gpui_kit::assets::IconName::House).label("Mark as").dropdown_caret(true)).dropdown_menu(move |mut menu, _, _| {
            for (i, c) in finance_core::home::HOME_CATEGORIES.iter().enumerate() {
                let on_mark = on_mark.clone();
                menu = menu.item(PopupMenuItem::new(c.name).on_click(move |_, window, cx| on_mark(ix, i, window, cx)));
            }
            menu
        })
    });

    let cat_link = {
        let (on_dive, c) = (on_dive.clone(), e.category.clone());
        let accent = t.primary;
        h_flex().id("detail-cat").gap_2().cursor_pointer()
            .hover(move |s| s.text_color(accent))
            .on_click(move |_, window, cx| {
                window.close_sheet(cx);
                on_dive(crate::dive::Dive::Category(c.clone()), window, cx)
            })
            .child(category::label(&e.category, cx).text_color(t.foreground))
            .child(div().text_xs().text_color(t.muted_foreground).child("deep dive →"))
    };
    let mut fields = v_flex()
        .child(field("Date", e.date.clone(), cx))
        .child(field("Description", e.description.clone(), cx))
        .child(field_el("Category", h_flex().gap_3().items_center().flex_wrap().child(cat_link).children(change).children(mark), cx))
        .child(field("Source", format!("{} · {}", e.kind.label(), e.source), cx));
    if !e.account.is_empty() {
        fields = fields.child(field("Account", e.account.clone(), cx));
    }
    fields = fields
        .child(field("Native", native(e), cx))
        .child(field(&fmt::base_ccy(), e.hkd.map(|h| format!("{} at the month's rate", fmt::dec2(h))).unwrap_or("no rate for this month".into()), cx));
    if e.share < 1. {
        fields = fields.child(field("Your share", format!(
            "{:.0}% = {}. A shared row: only your part counts in spending and savings.",
            e.share * 100., mine.map(|m| fmt::hkd(m.abs())).unwrap_or_default()), cx));
    }
    if e.amount < 0. && matches!(e.kind, finance_core::Kind::Bank | finance_core::Kind::Card) && !drill::is_spending(e) {
        fields = fields.child(field("Counted", "not as spending (a transfer, investment or settlement)", cx));
    }

    // the same merchant over time
    let s = &d.stats;
    let others = v_flex().children(d.same.iter().take(SAME_SHOWN).filter_map(|&i| {
        let x = ledger.entries.get(i)?;
        let v = drill::my_hkd(x).unwrap_or(x.amount);
        Some(h_flex()
            .gap_3()
            .py(px(3.))
            .when(i == d.ix, |r| r.font_semibold())
            .child(div().w(px(84.)).text_xs().text_color(t.muted_foreground).child(x.date.clone()))
            .child(div().flex_1().min_w_0().text_xs().text_color(t.muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap()
                .child(x.description.clone()))
            .child(div().w(px(104.)).text_sm().text_right().whitespace_nowrap().text_color(fmt::signed_color(v)).child(fmt::signed_hkd(v))))
    }));
    let dive_btn = (!d.key.is_empty()).then(|| {
        let (on_dive, k) = (on_dive.clone(), d.key.clone());
        Button::new("same-dive").small().primary().icon(gpui_kit::assets::IconName::Store).label("Merchant deep dive")
            .on_click(move |_, window, cx| {
                window.close_sheet(cx);
                on_dive(crate::dive::Dive::Merchant(k.clone()), window, cx)
            })
    });
    let same = if d.key.is_empty() || s.count <= 1 {
        v_flex().gap_2().child(div().text_sm().text_color(t.muted_foreground).child("No other rows from this merchant."))
            .children(dive_btn)
    } else {
        let q = crate::dive::query(&crate::dive::Dive::Merchant(d.key.clone()));
        let shown = d.same.len().min(SAME_SHOWN);
        v_flex()
            .gap_2()
            .child(h_flex().gap_6().flex_wrap()
                .child(mini("Rows", s.count.to_string(), cx))
                .child(mini("Total", fmt::signed_hkd(s.total_hkd), cx))
                .child(mini(&format!("Per month · {} mo", s.months), fmt::signed_hkd(s.monthly_avg), cx)))
            .child(div().text_xs().text_color(t.muted_foreground)
                .child(format!("{} to {} · matched on \u{201c}{}\u{201d} · latest {shown}", s.first, s.last, d.key)))
            .child(others)
            .child(h_flex().gap_2().flex_wrap()
                .children(dive_btn)
                .child(Button::new("same-all").small().outline().icon(IconName::Search)
                    .label(format!("Show all {} in table", s.count))
                    .on_click(move |_, window, cx| on_search(q.clone(), window, cx))))
    };

    sheet
        .size(crate::layout::sheet_width(460.))
        .title(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.description.clone()))
        .child(
            v_flex()
                .gap_4()
                .pb_6()
                .child(v_flex().gap_1()
                    .child(div().text_3xl().font_bold().text_color(fmt::signed_color(headline)).child(fmt::signed_hkd(headline)))
                    .child(h_flex().gap_2().text_sm().text_color(t.muted_foreground)
                        .child(e.date.clone()).child("·").child(category::label(&e.category, cx))))
                .child(fields)
                .child(ui::section("Same merchant", "", cx))
                .child(same)
                .child(div().text_xs().text_color(t.muted_foreground).child("Esc closes")),
        )
}
