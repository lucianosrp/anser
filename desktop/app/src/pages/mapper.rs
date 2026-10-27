//! "Map columns…": describe a CSV or spreadsheet no source reads, once, as a
//! format definition (engine `sources::format`). Opened from an Upload row
//! that says *unknown format*, in a dialog.
//!
//! The dialog shows the file's first rows, one dropdown per role (date,
//! amount or money out + money in, description, balance, account,
//! currency), the date pattern (detected, with an override), the decimal
//! separator, the sign convention, and a live preview of the rows as the
//! definition reads them, with each unreadable row in red. *Save as format*
//! names it, writes the TOML to this machine's formats folder and emits
//! [`Saved`]; the Upload page then sends it to the server and reads the file
//! again, now as a known source.

use std::path::PathBuf;

use finance_engine::sources::format::{self, Draft, FormatDef, FormatKind, Names, RowOut, Sign};
use finance_engine::sources::grid::{ReadOpts, Val};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, Figures as _, sp, ts};
use crate::zoom::px;

/// A definition was saved locally for `path`.
pub struct Saved {
    pub path: PathBuf,
    pub def: FormatDef,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Date,
    Amount,
    Debit,
    Credit,
    Description,
    Balance,
    Account,
    Currency,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::Date => "Date",
            Role::Amount => "Amount",
            Role::Debit => "Money out",
            Role::Credit => "Money in",
            Role::Description => "Description",
            Role::Balance => "Balance",
            Role::Account => "Account",
            Role::Currency => "Currency",
        }
    }

    fn required(self) -> bool {
        matches!(self, Role::Date | Role::Amount | Role::Description)
    }
}

pub struct Mapper {
    path: PathBuf,
    file_name: String,
    sheet: Option<String>,
    draft: Result<Draft, String>,
    def: FormatDef,
    name_input: Entity<InputState>,
    currency_input: Entity<InputState>,
    error: Option<String>,
    /// money out + money in, rather than one signed amount column
    two: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Saved> for Mapper {}

/// Rows of the live preview.
const PREVIEW: usize = 12;
/// Rows of the raw table.
const RAW: usize = 4;

impl Mapper {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Bank name, e.g. Barclays").default_value(guess_name(&stem)));
        let currency_input = cx.new(|cx| InputState::new(window, cx).placeholder("EUR"));
        let subs = vec![
            cx.subscribe(&name_input, |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.error = None;
                    cx.notify();
                }
            }),
            cx.subscribe(&currency_input, |this: &mut Self, s, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    let v = s.read(cx).value().trim().to_uppercase();
                    this.def.currency = Some(v);
                    this.error = None;
                    cx.notify();
                }
            }),
        ];
        let mut this = Mapper {
            path,
            file_name,
            sheet: None,
            draft: Err(String::new()),
            def: FormatDef::default(),
            name_input,
            currency_input,
            error: None,
            two: false,
            _subs: subs,
        };
        this.load();
        this
    }

    /// (Re)read the file and guess: on open, and when the sheet changes.
    fn load(&mut self) {
        let opts = ReadOpts { sheet: self.sheet.clone(), ..ReadOpts::default() };
        self.draft = format::guess(&self.path, &opts).map_err(|e| format!("{e:#}"));
        if let Ok(d) = &self.draft {
            let currency = self.def.currency.clone().filter(|c| !c.is_empty());
            self.def = d.def.clone();
            self.two = self.def.columns.amount.is_none() && (self.def.columns.debit.is_some() || self.def.columns.credit.is_some());
            if d.def.columns.currency.is_none() {
                self.def.currency = currency.or(Some(String::new()));
            }
            self.sync_detect();
        }
    }

    /// The header signature is every mapped column (so a file of this bank
    /// is recognized by the columns this definition reads).
    fn sync_detect(&mut self) {
        let c = &self.def.columns;
        let mut cols: Vec<String> = Vec::new();
        for n in [Some(&c.date), c.amount.as_ref(), c.debit.as_ref(), c.credit.as_ref(), c.balance.as_ref()]
            .into_iter()
            .flatten()
            .chain(c.description.0.iter())
        {
            if !n.is_empty() && !cols.contains(n) {
                cols.push(n.clone());
            }
        }
        self.def.detect.columns = cols;
    }

    fn get(&self, role: Role) -> Vec<String> {
        let c = &self.def.columns;
        match role {
            Role::Date => (!c.date.is_empty()).then(|| c.date.clone()).into_iter().collect(),
            Role::Amount => c.amount.clone().into_iter().collect(),
            Role::Debit => c.debit.clone().into_iter().collect(),
            Role::Credit => c.credit.clone().into_iter().collect(),
            Role::Description => c.description.0.clone(),
            Role::Balance => c.balance.clone().into_iter().collect(),
            Role::Account => c.account.clone().into_iter().collect(),
            Role::Currency => c.currency.clone().into_iter().collect(),
        }
    }

    /// Assign `col` to `role` (None clears it). The description takes
    /// several columns: picking one toggles it.
    fn set(&mut self, role: Role, col: Option<String>, cx: &mut Context<Self>) {
        let c = &mut self.def.columns;
        match role {
            Role::Date => c.date = col.unwrap_or_default(),
            Role::Amount => c.amount = col,
            Role::Debit => c.debit = col,
            Role::Credit => c.credit = col,
            Role::Balance => c.balance = col,
            Role::Account => c.account = col,
            Role::Currency => c.currency = col,
            Role::Description => match col {
                None => c.description = Names::default(),
                Some(n) => {
                    if let Some(i) = c.description.0.iter().position(|x| *x == n) {
                        c.description.0.remove(i);
                    } else {
                        c.description.0.push(n);
                    }
                }
            },
        }
        if role == Role::Date {
            self.redetect_dates();
        }
        self.sync_detect();
        self.error = None;
        cx.notify();
    }

    fn redetect_dates(&mut self) {
        let Ok(d) = &mut self.draft else { return };
        let ix = d.columns.iter().position(|c| *c == self.def.columns.date);
        let vals: Vec<Val> = ix.map(|i| d.rows[d.header + 1..].iter().filter_map(|r| r.get(i).cloned()).collect()).unwrap_or_default();
        d.date_formats = format::detect_date_formats(&vals);
        if let Some(f) = d.date_formats.first() {
            self.def.parse.date_format = Names(vec![f.to_string()]);
        }
    }

    fn set_two_columns(&mut self, two: bool, cx: &mut Context<Self>) {
        self.two = two;
        let c = &mut self.def.columns;
        if two && c.amount.is_some() {
            c.debit = c.amount.take();
        } else if !two && c.amount.is_none() {
            c.amount = c.debit.take().or(c.credit.take());
            c.debit = None;
            c.credit = None;
        }
        self.sync_detect();
        cx.notify();
    }

    fn preview(&self) -> Result<Vec<RowOut>, String> {
        let Ok(d) = &self.draft else { return Err(String::new()) };
        if self.def.detect.columns.is_empty() {
            return Err("Pick the date, amount and description columns.".into());
        }
        // the header row the draft found, whatever the signature picks
        let mut def = self.def.clone();
        def.parse.skip_rows = Some(d.header);
        // no currency typed yet: the rows still preview
        if def.columns.currency.is_none() && def.currency.as_deref().is_none_or(|c| c.trim().is_empty()) {
            def.currency = Some("?".into());
        }
        def.rows(&d.rows).map_err(|e| format!("{e:#}"))
    }

    /// Validate, save locally, tell the Upload page.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_input.read(cx).value().trim().to_string();
        let mut def = self.def.clone();
        def.label = name.clone();
        def.id = format::slug(&name);
        if def.columns.currency.is_some() {
            def.currency = None;
        }
        if name.is_empty() {
            self.error = Some("Give the format a name (the bank's).".into());
            cx.notify();
            return;
        }
        if let Some(f) = finance_engine::sources::format::get(&def.id).filter(|f| f.builtin) {
            self.error = Some(format!("\"{}\" is a built-in format's id: pick another name.", f.def.id));
            cx.notify();
            return;
        }
        if let Err(e) = def.validate() {
            self.error = Some(format!("{e:#}"));
            cx.notify();
            return;
        }
        match self.preview() {
            Ok(rows) if rows.iter().any(|r| matches!(r.row, Ok(Some(_)))) => {}
            Ok(_) => {
                self.error = Some("No row reads yet: check the date and amount columns.".into());
                cx.notify();
                return;
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        }
        if let Err(e) = format::save(&crate::formats::local_dir(), &def) {
            self.error = Some(format!("couldn't save it: {e:#}"));
            cx.notify();
            return;
        }
        cx.emit(Saved { path: self.path.clone(), def });
        window.close_dialog(cx);
    }
}

/// "barclays-statement-2026-03" -> "Barclays"
fn guess_name(stem: &str) -> String {
    let word: String = stem.chars().take_while(|c| c.is_alphabetic()).collect();
    if word.len() < 3 || ["export", "statement", "transactions", "account", "download", "activity"].contains(&word.to_lowercase().as_str()) {
        return String::new();
    }
    let mut c = word.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase()).unwrap_or_default()
}

// ---------- rendering ----------

fn field(label: &str, required: bool, control: impl IntoElement, cx: &App) -> Div {
    let t = cx.theme();
    v_flex()
        .gap(px(4.))
        .min_w(px(150.))
        .child(
            h_flex()
                .gap(px(3.))
                .text_size(px(ts::CAPTION))
                .text_color(t.muted_foreground)
                .child(label.to_string())
                .when(required, |d| d.child(div().text_color(t.primary).child("*"))),
        )
        .child(control)
}

/// A dropdown of the file's columns for one role.
fn column_picker(role: Role, current: &[String], columns: &[String], weak: WeakEntity<Mapper>) -> impl IntoElement {
    let label = match current {
        [] => "—".to_string(),
        [one] => one.clone(),
        many => many.join(" + "),
    };
    let (cols, cur) = (columns.to_vec(), current.to_vec());
    Button::new(SharedString::from(format!("map-{role:?}")))
        .small()
        .outline()
        .label(label)
        .dropdown_menu(move |menu, _, _| {
            let mut menu = menu.min_w(px(200.)).max_h(px(320.));
            if role != Role::Description || !cur.is_empty() {
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(if role == Role::Description { "Clear" } else { "— none" }).on_click(move |_, _, cx| {
                    let _ = w.update(cx, |m, cx| m.set(role, None, cx));
                }));
            }
            for c in cols.iter().filter(|c| !c.trim().is_empty()) {
                let (w, name) = (weak.clone(), c.clone());
                menu = menu.item(PopupMenuItem::new(c.clone()).checked(cur.contains(c)).on_click(move |_, _, cx| {
                    let _ = w.update(cx, |m, cx| m.set(role, Some(name.clone()), cx));
                }));
            }
            menu
        })
}

fn toggle(id: &'static str, label: &'static str, on: bool, f: impl Fn(&mut Mapper, &mut Context<Mapper>) + 'static, cx: &mut Context<Mapper>) -> Button {
    Button::new(id)
        .small()
        .label(label)
        .map(|b| if on { b.primary() } else { b.outline() })
        .on_click(cx.listener(move |this, _, _, cx| {
            f(this, cx);
            this.error = None;
            cx.notify();
        }))
}

fn raw_table(d: &Draft, def: &FormatDef, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let role_of = |name: &str| -> Option<&'static str> {
        let c = &def.columns;
        let is = |o: &Option<String>| o.as_deref() == Some(name);
        if c.date == name {
            Some("date")
        } else if is(&c.amount) {
            Some("amount")
        } else if is(&c.debit) {
            Some("out")
        } else if is(&c.credit) {
            Some("in")
        } else if c.description.0.iter().any(|x| x == name) {
            Some("description")
        } else if is(&c.balance) {
            Some("balance")
        } else if is(&c.account) {
            Some("account")
        } else if is(&c.currency) {
            Some("currency")
        } else {
            None
        }
    };
    let n = d.columns.len();
    let col_w = 128.;
    let cell = |text: String, head: bool, mapped: bool| {
        div()
            .w(px(col_w))
            .flex_shrink_0()
            .px(px(6.))
            .py(px(4.))
            .text_size(px(ts::CAPTION))
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .when(head, |d| d.font_semibold())
            .when(mapped, |d| d.bg(t.primary.opacity(0.08)))
            .child(text)
    };
    let header = h_flex().children(d.columns.iter().map(|c| {
        let role = role_of(c);
        v_flex()
            .w(px(col_w))
            .flex_shrink_0()
            .child(cell(c.clone(), true, role.is_some()))
            .child(
                div()
                    .h(px(16.))
                    .px(px(6.))
                    .text_size(px(10.))
                    .bg(if role.is_some() { t.primary.opacity(0.08) } else { gpui_kit::transparent_black() })
                    .text_color(t.primary)
                    .child(role.unwrap_or("")),
            )
    }));
    let rows = d.rows[d.header + 1..].iter().take(RAW).map(|r| {
        h_flex().border_t_1().border_color(ui::hairline(cx)).children((0..n).map(|i| {
            let v = r.get(i).map(Val::text).unwrap_or_default();
            cell(v, false, role_of(&d.columns[i]).is_some())
        }))
    });
    div()
        .id("mapper-raw")
        .w_full()
        .overflow_x_scroll()
        .rounded(px(6.))
        .border_1()
        .border_color(t.border)
        .child(v_flex().child(header).children(rows))
}

fn preview_list(rows: &Result<Vec<RowOut>, String>, cx: &App) -> Div {
    let t = cx.theme();
    let list = match rows {
        Err(e) => return div().text_size(px(ts::CAPTION)).text_color(t.warning).child(e.clone()),
        Ok(r) => r,
    };
    let read = list.iter().filter(|r| matches!(r.row, Ok(Some(_)))).count();
    let bad = list.iter().filter(|r| r.row.is_err()).count();
    let skipped = list.iter().filter(|r| matches!(r.row, Ok(None))).count();
    let mut bits = vec![format!("{} read", fmt::count(read as f64))];
    if bad > 0 {
        bits.push(format!("{} unreadable", fmt::count(bad as f64)));
    }
    if skipped > 0 {
        bits.push(format!("{} skipped (blank or no date)", fmt::count(skipped as f64)));
    }
    let shown = list.iter().filter(|r| !matches!(r.row, Ok(None))).take(PREVIEW);
    let line = |text: String| div().w(px(44.)).flex_shrink_0().text_size(px(11.)).text_color(t.muted_foreground).child(text);
    v_flex()
        .gap_0()
        .child(ui::caption(format!("{} of the first rows", bits.join(" · ")), cx).figures().pb(px(4.)))
        .children(shown.map(|r| match &r.row {
            Ok(Some(b)) => h_flex()
                .gap(px(sp::S))
                .py(px(3.))
                .border_t_1()
                .border_color(ui::hairline(cx))
                .child(line(format!("#{}", r.line)))
                .child(div().w(px(92.)).flex_shrink_0().text_size(px(ts::CAPTION)).figures().child(b.txn_date.clone()))
                .child(
                    div()
                        .w(px(110.))
                        .flex_shrink_0()
                        .text_right()
                        .text_size(px(ts::CAPTION))
                        .figures()
                        .text_color(b.amount.map(fmt::signed_color).unwrap_or(t.muted_foreground))
                        .child(b.amount.map(|a| if b.currency == "?" { fmt::dec2(a) } else { format!("{} {}", fmt::dec2(a), b.currency) }).unwrap_or_else(|| "—".into())),
                )
                .child(div().flex_1().min_w_0().text_size(px(ts::CAPTION)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(b.description.clone())),
            Ok(None) => div(),
            Err(e) => h_flex()
                .gap(px(sp::S))
                .py(px(3.))
                .border_t_1()
                .border_color(ui::hairline(cx))
                .bg(fmt::money_out().opacity(0.07))
                .child(line(format!("#{}", r.line)))
                .child(div().flex_1().min_w_0().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).line_clamp(1).child(e.clone())),
        }))
}

impl Render for Mapper {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let weak = cx.entity().downgrade();
        let d = match &self.draft {
            Ok(d) => d.clone(),
            Err(e) => {
                return v_flex()
                    .gap_2()
                    .child(ui::empty_with(IconName::TriangleAlert, "Can't read this file as a table", e, cx))
                    .into_any_element();
            }
        };
        let two = self.two;
        let roles: Vec<Role> = if two {
            vec![Role::Date, Role::Debit, Role::Credit, Role::Description, Role::Balance, Role::Account, Role::Currency]
        } else {
            vec![Role::Date, Role::Amount, Role::Description, Role::Balance, Role::Account, Role::Currency]
        };
        let pickers = h_flex().gap(px(sp::M)).flex_wrap().items_start().children(roles.into_iter().map(|r| {
            let required = r.required() || (two && matches!(r, Role::Debit | Role::Credit) && self.def.columns.debit.is_none() && self.def.columns.credit.is_none());
            field(r.label(), required, column_picker(r, &self.get(r), &d.columns, weak.clone()), cx)
        }));

        // the date pattern: the detected ones first
        let cur_fmt = self.def.parse.date_format.0.first().cloned().unwrap_or_default();
        let fits = d.date_formats.clone();
        let date_label = if cur_fmt.is_empty() {
            "ISO (2026-03-31)".to_string()
        } else {
            format!("{}{}", format::date_format_example(&cur_fmt), if fits.contains(&cur_fmt.as_str()) { "  · detected" } else { "" })
        };
        let w = weak.clone();
        let date_menu = Button::new("map-date-format").small().outline().label(date_label).dropdown_menu(move |menu, _, _| {
            let mut menu = menu.min_w(px(240.)).max_h(px(360.));
            let ordered: Vec<&str> = fits.iter().copied().chain(format::DATE_FORMATS.iter().copied().filter(|f| !fits.contains(f))).collect();
            for (i, f) in ordered.into_iter().enumerate() {
                if i == fits.len() && i > 0 {
                    menu = menu.separator();
                }
                let w2 = w.clone();
                let label = format!("{}  ({f}){}", format::date_format_example(f), if fits.contains(&f) { " · detected" } else { "" });
                menu = menu.item(PopupMenuItem::new(label).checked(cur_fmt == f).on_click(move |_, _, cx| {
                    let _ = w2.update(cx, |m, cx| {
                        m.def.parse.date_format = Names(vec![f.to_string()]);
                        cx.notify();
                    });
                }));
            }
            menu
        });

        let dec_comma = self.def.parse.decimal == ",";
        let inverted = self.def.parse.sign == Sign::Inverted;
        let kind = self.def.kind;
        let options = h_flex()
            .gap(px(sp::L))
            .flex_wrap()
            .items_start()
            .child(field("Date format", true, date_menu, cx))
            .child(field(
                "Numbers",
                false,
                h_flex()
                    .gap_1()
                    .child(toggle("dec-point", "1,234.56", !dec_comma, |m, _| m.def.parse.decimal = ".".into(), cx))
                    .child(toggle("dec-comma", "1.234,56", dec_comma, |m, _| m.def.parse.decimal = ",".into(), cx)),
                cx,
            ))
            .child(field(
                "Amounts",
                false,
                h_flex()
                    .gap_1()
                    .child(toggle("amt-one", "One column", !two, |m, cx| m.set_two_columns(false, cx), cx))
                    .child(toggle("amt-two", "Out + in", two, |m, cx| m.set_two_columns(true, cx), cx)),
                cx,
            ))
            .child(field(
                "Sign",
                false,
                h_flex()
                    .gap_1()
                    .child(toggle("sign-signed", "− is money out", !inverted, |m, _| m.def.parse.sign = Sign::Signed, cx))
                    .child(toggle("sign-inverted", "+ is money out", inverted, |m, _| m.def.parse.sign = Sign::Inverted, cx)),
                cx,
            ))
            .child(field(
                "Kind",
                false,
                h_flex()
                    .gap_1()
                    .child(toggle("kind-bank", "Bank", kind == FormatKind::Bank, |m, _| m.def.kind = FormatKind::Bank, cx))
                    .child(toggle("kind-card", "Card", kind == FormatKind::Card, |m, _| m.def.kind = FormatKind::Card, cx))
                    .child(toggle("kind-wallet", "Wallet", kind == FormatKind::Wallet, |m, _| m.def.kind = FormatKind::Wallet, cx)),
                cx,
            ));

        let sheets = (d.sheets.len() > 1).then(|| {
            let (w, sheets, cur) = (weak.clone(), d.sheets.clone(), self.sheet.clone().or_else(|| d.sheets.first().cloned()));
            field(
                "Sheet",
                false,
                Button::new("map-sheet").small().outline().label(cur.clone().unwrap_or_default()).dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.min_w(px(180.));
                    for s in &sheets {
                        let (w2, name) = (w.clone(), s.clone());
                        menu = menu.item(PopupMenuItem::new(s.clone()).checked(cur.as_ref() == Some(s)).on_click(move |_, _, cx| {
                            let _ = w2.update(cx, |m, cx| {
                                m.sheet = Some(name.clone());
                                m.load();
                                cx.notify();
                            });
                        }));
                    }
                    menu
                }),
                cx,
            )
        });

        let rows = self.preview();
        let name = self.name_input.read(cx).value().trim().to_string();
        let id = format::slug(&name);
        let needs_currency = self.def.columns.currency.is_none();
        let naming = h_flex()
            .gap(px(sp::M))
            .flex_wrap()
            .items_start()
            .child(field("Name", true, div().w(px(220.)).child(Input::new(&self.name_input).small()), cx))
            .when(needs_currency, |d| d.child(field("Currency", true, div().w(px(90.)).child(Input::new(&self.currency_input).small()), cx)))
            .child(
                v_flex().pt(px(20.)).child(ui::caption(
                    if id.is_empty() { "saved as a format file in this machine's formats folder, then sent to the server".to_string() } else { format!("saved as {id}.toml here, then sent to the server") },
                    cx,
                )),
            );

        let body = v_flex()
            .gap(px(sp::M))
            .child(ui::caption(
                format!("{} · {} columns · header on row {}. Assign the columns once; every export of this bank reads the same way afterwards.", self.file_name, d.columns.len(), d.header + 1),
                cx,
            ))
            .child(naming)
            .children(sheets)
            .child(raw_table(&d, &self.def, cx))
            .child(pickers)
            .child(options)
            .child(div().text_size(px(ts::TITLE)).font_semibold().pt(px(4.)).child("Preview"))
            .child(preview_list(&rows, cx));
        let error = self.error.clone();

        v_flex()
            .gap(px(sp::M))
            .child(div().id("mapper-scroll").max_h(px(620.)).overflow_y_scroll().pr(px(6.)).child(body))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .children(error.map(|e| div().flex_1().min_w_0().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).child(e)))
                    .child(Button::new("map-cancel").small().ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        Button::new("map-save")
                            .small()
                            .primary()
                            .icon(IconName::Check)
                            .label("Save as format")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
            .text_color(t.foreground)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::guess_name;

    #[test]
    fn names_from_files() {
        assert_eq!(guess_name("barclays-2026-03"), "Barclays");
        assert_eq!(guess_name("statement_2026"), "");
        assert_eq!(guess_name("ab"), "");
    }
}
