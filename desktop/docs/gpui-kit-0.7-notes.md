# gpui-kit 0.7 cheat sheet (verified)

*Verified against gpui-kit 0.7.0 by compiling a probe app (2026-10-02); the
app is on 0.7.3 now, and the differences that matter are listed below. The
probe was a scratch crate that no longer exists; its full source is at the
end of this file. Read this before touching the UI.*

All snippets below are taken from or checked by that probe (full `main.rs`
at the end). It **compiled with `cargo build`, zero warnings** on rustc
1.97.1, edition 2024. It was compiled, not run: no display was available, so
runtime behaviour is from reading the sources only.
Build: cold `cargo build` (all deps) took **1m14s** (8-core); incremental rebuilds of main.rs take **~3.3s**.

**Since 0.7.0** (0.7.3, what `app/` and `web/` use):
- Charts draw in the first time they are painted, by default. Every chart
  in the app calls `.appear(false)` (data that is ready draws at once,
  DESIGN.md); do the same on a new one.
- Dialogs have an entrance (`.entrance(DialogEntrance::..)`, default a
  slide down); closing is immediate.

```toml
[dependencies]
gpui-kit = "0.7.3"          # default features: component + assets
# features: decimal (rust_decimal PlotValue), inspector, tree-sitter*, test-support (dev-deps)
```

## 0. Imports

```rust
use gpui_kit::*;                                // ALL of GPUI: div, px, size, App, Window, Context, Entity, Render, ...
use gpui_kit::prelude::FluentBuilder as _;      // .when()/.when_some() -- NOT covered by the glob!
use gpui_kit::component::{                      // = gpui-component 0.7
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, Theme, ThemeMode, WindowExt as _,
    h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    chart::{BarChart, LineChart},               // also AreaChart, PieChart, CandlestickChart, RadarChart, SankeyChart
    command::{Command, CommandGroup, CommandItem, CommandState},
    input::{Input, InputEvent, InputState},
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState},
};
actions!(probe, [ToggleTheme, OpenPalette, Quit, Reload]);   // gpui_kit::actions! (unit structs, namespace "probe")
```
Never depend on or import `gpui` directly. Use `gpui_kit::` paths; the hidden `gpui_kit::gpui` re-export is only kept for compatibility.

## 1. App bootstrap, window title and size, Root

```rust
fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)   // REQUIRED for icons (else blank). AllAssets = whole Lucide set
        .run(|cx| {
            gpui_kit::init(cx);                  // = gpui_component::init (theme, root plugin, input, table keys...)
            cx.bind_keys([KeyBinding::new("ctrl-k", OpenPalette, None)]);
            Theme::change(ThemeMode::Dark, None, cx);
            let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Finance".into()), ..Default::default() }),
                window_min_size: Some(size(px(800.), px(500.))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let view = cx.new(|cx| MyApp::new(window, cx));
                window.focus(&view.focus_handle(cx), cx);   // needed for context-scoped keybindings
                view
            }).expect("open window");       // -> Result<(AnyWindowHandle, Entity<MyApp>)>
            cx.activate(true);
        });
}
```
- **Root**: `gpui_kit::open_window` wraps your view in `gpui_kit::base::Root` (= `component::Root`) for you. gpui-component registers its window state (dialogs, sheets, notifications, tooltips) as a Root *plugin*, and the layers render automatically. Do **not** call `Root::new` yourself, and do not render a dialog or notification layer. `init` must run **before** `open_window`, or it panics with "component window state is missing".
- Other window fields: `is_resizable`, `is_minimizable`, `app_id: Option<String>` (Linux WM class), `window_background`, `focus`, `show`, `kind`.
- Change the title at runtime with `window.set_window_title("...")`.
- Quit with `cx.quit()`.

## 2. Views, entities, state

```rust
struct MyApp { focus: FocusHandle, search: Entity<InputState>, _subs: Vec<Subscription>, /* ... */ }
impl Render for MyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement { div() }
}
impl Focusable for MyApp { fn focus_handle(&self, _: &App) -> FocusHandle { self.focus.clone() } }
```
- Create: `cx.new(|cx| T { .. })`. Many component states need the window: `cx.new(|cx| InputState::new(window, cx))`.
- Read: `entity.read(cx)`. Mutate: `entity.update(cx, |s, cx| { ...; cx.notify(); })`.
- `cx.notify()` re-renders the view. Keep any `Subscription` in a field, because dropping it unsubscribes.
- `cx.entity()` gives an `Entity<Self>`; `.downgrade()` gives a `WeakEntity`, and `weak.update(cx, |this, cx| ..)` returns `anyhow::Result`.
- `cx.listener(|this, ev: &E, window, cx| ..)` turns a method into an `Fn(&E, &mut Window, &mut App)` for `on_click` or `on_action`.
- **Borrow gotcha**: `self.palette.read(cx).focus(window, cx)` fails with E0502 because `cx` is borrowed twice. Use `self.palette.update(cx, |s, cx| s.focus(window, cx))` instead.

## 3. Theme

```rust
let t = cx.theme();              // &Theme (needs ActiveTheme in scope); Theme derefs to ThemeColor (all Hsla)
t.is_dark(); t.mode;             // ThemeMode::{Light, Dark}
Theme::change(ThemeMode::Dark, Some(window), cx);   // or None; refreshes every window
t.radius, t.radius_lg (Pixels); t.font_family, t.font_size, t.mono_font_family, t.mono_font_size
t.tokens.<field>                 // ThemeToken (color + optional gradient) per field, e.g. tokens.table
```
ThemeColor fields (all `Hsla`):
`background foreground border ring selection caret overlay muted muted_foreground accent accent_foreground primary primary_foreground primary_hover primary_active secondary secondary_foreground secondary_hover secondary_active success success_foreground success_hover success_active danger danger_foreground danger_hover danger_active warning warning_foreground warning_hover warning_active info info_foreground info_hover info_active link link_hover link_active input popover popover_foreground group_box group_box_foreground list list_active list_active_border list_even list_head list_hover table table_active table_active_border table_even table_head table_head_foreground table_foot table_foot_foreground table_hover table_row_border sidebar sidebar_foreground sidebar_border sidebar_accent sidebar_accent_foreground sidebar_primary sidebar_primary_foreground tab tab_active tab_active_foreground tab_bar tab_bar_segmented tab_foreground title_bar title_bar_border status_bar status_bar_border scrollbar scrollbar_thumb scrollbar_thumb_hover skeleton slider_bar slider_thumb switch switch_thumb progress_bar accordion description_list_label description_list_label_foreground drag_border drop_target window_border chart_1 chart_2 chart_3 chart_4 chart_5 chart_bullish chart_bearish chart_grid red red_light green green_light blue blue_light yellow yellow_light magenta magenta_light cyan cyan_light` and `button_*` variants.
There is no `card` field: use secondary or muted, or background plus border.
Color ops: `t.muted_foreground.opacity(0.6)`, and `hsla(h,s,l,a)`/`rgb(0xRRGGBB)` from gpui.

## 4. Styling (gpui `Styled` + gpui-base `StyledExt`)

Everything in this section is compile-checked in `style_samples()`.
```rust
h_flex() / v_flex()                     // component helpers: flex row (items_center) / flex col
div().size_full().w(px(240.)).h(px(260.)).min_w_0().min_h_0().max_h(px(200.)).flex_1().flex_shrink_0()
   .p_4().px_3().py_2().gap_2().gap_4()                         // 4px scale: _1=4px, _2=8px, _4=16px
   .items_center().justify_between().justify_center().flex_wrap()
   .grid().grid_cols(3).col_span(2)                            // CSS grid
   .bg(t.secondary).border_1().border_color(t.border).rounded(t.radius_lg).rounded_md().shadow_sm()
   .text_xs()/.text_sm()/.text_xl()/.text_2xl().text_size(px(13.)).text_right()
   .font_weight(FontWeight::MEDIUM)                            // gpui
   .font_semibold()/.font_bold()                               // StyledExt -- needs `StyledExt as _`
   .font_family(t.mono_font_family.clone())
   .text_color(t.muted_foreground).whitespace_nowrap().overflow_hidden().text_ellipsis()
   .cursor_pointer().hover(|s| s.bg(t.accent))
   .when(cond, |d| d.col_span(2))                             // FluentBuilder (prelude import!)
div().id("x").overflow_y_scroll()                              // scrolling/click need .id() (Stateful<Div>)
```
To make the right column fill the remaining height, use `v_flex().size_full()` containing `div().flex_1().min_h_0().child(table)`. Without `min_h_0`, a flex child refuses to shrink.

## 5. Actions and keybindings

```rust
actions!(probe, [ToggleTheme, OpenPalette]);
cx.bind_keys([
    KeyBinding::new("ctrl-k", OpenPalette, None),        // None = any context
    KeyBinding::new("ctrl-t", ToggleTheme, Some("App")), // only while focus is inside .key_context("App")
]);                                                      // mac: "cmd-k"; "secondary-k" = cmd on mac / ctrl elsewhere
cx.on_action(|_: &Quit, cx| cx.quit());                  // global (App-level) handler

// in render():
h_flex().id("app").key_context("App").track_focus(&self.focus)
    .on_action(cx.listener(Self::toggle_theme))          // fn toggle_theme(&mut self, _: &ToggleTheme, w: &mut Window, cx: &mut Context<Self>)
// dispatch programmatically:
window.dispatch_action(Box::new(ToggleTheme), cx);
```
Element `on_action` handlers only fire when the focused element is the handler's element or one of its descendants. Focus the root view at startup (see section 1), or nothing fires until the user clicks into something focusable. Dialogs live in a Root layer, a **sibling** of your view, so actions dispatched from inside a dialog never reach your view's `on_action` handlers. Inside dialogs, use callbacks or global `cx.on_action` handlers.

## 6. Sidebar

```rust
Sidebar::new("sidebar")                       // Sidebar<E: SidebarItem>; E inferred from .child
    .collapsible(false)                        // or SidebarCollapsible::{Icon,Offcanvas,None}; .collapsed(bool)
    .header(SidebarHeader::new().child("Finance"))
    .footer(div().child("v0.1"))
    .child(SidebarGroup::new("Views").child(
        SidebarMenu::new()
            .child(SidebarMenuItem::new("Overview")
                .icon(IconName::LayoutDashboard)
                .active(self.page == Page::Overview)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_page(Page::Overview, cx))))
            // .suffix(|w, cx| ...), .children([...sub-items]), .default_open(bool), .disable(bool)
    ))
```
`SidebarToggleButton::new().collapsed(b).on_click(..)` drives collapse. Keep the collapsed `bool` in your own state.

## 7. Virtualized table (DataTable + TableDelegate)

```rust
impl TableDelegate for TxnTable {               // trait: Sized + 'static
    fn columns_count(&self, cx: &App) -> usize;
    fn rows_count(&self, cx: &App) -> usize;
    fn column(&self, col_ix: usize, cx: &App) -> Column;
    fn render_td(&mut self, row_ix: usize, col_ix: usize, window: &mut Window,
                 cx: &mut Context<TableState<Self>>) -> impl IntoElement;
    // optional:
    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, window: &mut Window, cx: &mut Context<TableState<Self>>) {}
    fn render_th(&mut self, col_ix, window, cx) -> impl IntoElement
    fn render_tr(&mut self, row_ix, window, cx) -> Stateful<Div>       // default div().id(("row", row_ix))
    fn context_menu(&mut self, row_ix, menu: PopupMenu, window, cx) -> PopupMenu
    fn render_empty(..) / loading(&self, cx) -> bool / render_loading(..)
    fn has_more(&self, cx) -> bool / load_more_threshold(&self) -> usize / load_more(&mut self, window, cx)
    fn visible_rows_changed(&mut self, range: Range<usize>, window, cx)
    fn cell_text(&self, row_ix, col_ix, cx: &App) -> String           // for dump()/CSV export
    fn move_column(..), group_headers(..), render_group_th(..), render_last_empty_col(..)
}
Column::new("amount", "Amount").width(px(120.)).text_right().sortable()  // .ascending()/.descending() initial,
     // .fixed_left(), .resizable(bool), .movable(bool), .selectable(bool), .min_width/.max_width, .p_0(), .text_center()
let table = cx.new(|cx| TableState::new(TxnTable::new(rows), window, cx)   // NOTE: takes window
    /* .sortable(true) .row_selectable(true) .cell_selectable(false) .col_movable(true) .col_resizable(true) .loop_selection(true) */);
DataTable::new(&table).stripe(true).bordered(true).small()   // .scrollbar_visible(v, h); Sizable: .xsmall/.small/.large
```
- Rows are virtualized: `render_td` runs only for visible rows. 5000 rows is fine.
- **Sorting**: click the header of a `.sortable()` column. It cycles Default → Descending → Ascending → Default and calls `perform_sort(col_ix, ColumnSort)`. You sort your own `Vec`. With `ColumnSort::Default`, restore the original order yourself.
- **Mutating data**: `table.update(cx, |t, cx| { t.delegate_mut().filter(q); cx.notify(); })`. Call `t.refresh(cx)` only when the **columns** change. It rebuilds the column groups from `column()`, which also resets the header sort indicators.
- Gotcha: filtering re-derives rows from `all`, so any active sort order is lost. Re-apply the sort after filtering if you need to keep it.
- **Right-aligned numbers**: use `Column::text_right()` (header) plus a cell `div().w_full().text_right()`.
- Events: `cx.subscribe(&table, |this, _, ev: &TableEvent, cx| ..)`. Variants: `SelectRow(usize)`, `DoubleClickedRow(usize)`, `SelectColumn`, `SelectCell(r,c)`, `DoubleClickedCell`, `RightClickedRow(Option<usize>)`, `RightClickedCell`, `ColumnWidthsChanged(Vec<Pixels>)`, `MoveColumn(from,to)`, `ClearSelection`.
- Other `TableState` API: `selected_row()`, `set_selected_row(ix, cx)`, `scroll_to_row(ix, cx)`, `selection()`, `clear_selection(cx)`, `dump(cx) -> (headers, rows)` (uses `cell_text`), `visible_range()`.
- Key bindings are built in under context "DataTable": arrows, home/end, pageup/pagedown, tab, escape.
- For small static tables, `component::table::{Table, TableHeader, TableBody, TableRow, TableHead, TableCell, TableCaption}` is a stateless builder without virtualization.

## 8. Text input and live filtering

```rust
let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter..."));
   // builder: .default_value("x") .masked(true) .clean_on_escape()
let sub = cx.subscribe_in(&search, window, |this, state, ev: &InputEvent, _window, cx| {
    if let InputEvent::Change = ev {            // fires on every keystroke
        let q = state.read(cx).value();         // SharedString
        this.table.update(cx, |t, cx| { t.delegate_mut().filter(&q); cx.notify(); });
    }
});
// InputEvent::{Change, PressEnter{secondary, shift}, Focus, Blur}
Input::new(&self.search).cleanable(true).prefix(IconName::Search)   // .suffix(..) .disabled(b) .small() .w(px(240.))
search.update(cx, |s, cx| s.set_value("abc", window, cx));            // programmatic; emits NO Change event
search.update(cx, |s, cx| s.focus(window, cx));
```
Also available: `NumberInput`, `Textarea` (`TextareaState`), `OtpInput`.

## 9. Command palette (ctrl-k)

```rust
// state, created once (needs window):
palette: cx.new(|cx| CommandState::new(window, cx)),

fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
    let state = self.palette.clone();
    let view = cx.entity().downgrade();
    window.open_dialog(cx, move |dialog, _, _| {         // builder re-runs every render: Fn + 'static
        let view = view.clone();
        dialog.p_0().close_button(false).child(
            Command::new(&state).bordered(false).placeholder("Type a command...")
                .group(CommandGroup::new().label("Go to")
                    .item(CommandItem::new().label("Overview").icon(IconName::LayoutDashboard))
                    .item(CommandItem::new().label("Transactions").icon(IconName::Inbox)))
                .group(CommandGroup::new().label("Actions")
                    .item(CommandItem::new().label("Toggle theme").keywords(["dark", "light"])))
                .on_confirm(move |ix: IndexPath, window, cx| {    // ix.section = group idx, ix.row = item idx (unfiltered coords)
                    let _ = view.update(cx, |this, cx| { /* match (ix.section, ix.row) */ cx.notify(); });
                    window.close_dialog(cx);
                })
                .on_cancel(|window, cx| window.close_dialog(cx)),   // Esc with empty query
        )
    });
    self.palette.update(cx, |s, cx| { s.set_query("", window, cx); s.focus(window, cx); });
}
```
- Also available: `.item()`/`.items()` without groups (section 0), `.separator()`, `.on_select(IndexPath)` (highlight change), `.on_query(|q, w, cx|)`, `.filterable(false)` for async search, `.max_h(..)`, `.empty(|state, w, cx| ..)`, `.header`/`.footer`, `CommandItem::action(Box::new(SomeAction))`, which dispatches the action and shows its key binding as a hint, and `.checked(bool)`.
- `CommandState` persists the query between opens, so reset it with `set_query("")`.
- Built-in keys inside the palette: up/down/enter/escape.
- Other dialog APIs: `window.open_dialog(cx, |d, w, cx| d.title("..").child(..).w(px(560.)).overlay_closable(true))`, `window.close_dialog(cx)`, `window.has_active_dialog(cx)`, `window.open_alert_dialog(..)`, `window.open_sheet(cx, |sheet, w, cx| ..)`.
- Toasts: `window.push_notification(Notification::new().message("Synced"), cx)` (`component::notification::Notification`).

## 10. Charts

```rust
struct Monthly { month: &'static str, value: f64 }   // value types: f32 | f64 (| Decimal with feature "decimal")
// The PlotElement is size_full(): the PARENT MUST HAVE A DEFINITE HEIGHT, else zero height.
v_flex().h(px(260.)).child(div().flex_1().child(
    BarChart::new(data)                              // BarChart<T, B: Eq+Hash+Into<SharedString>, V: PlotValue>
        .band(|d: &Monthly| d.month)
        .value(|d: &Monthly| d.value)
        .value_axis(true)
        .value_tick_format(|v| format!("{:.0}k", v / 1000.))
        // .label(|d| ..) value labels on bars, .corner_radii(..), .grid(bool), .tick_margin(n), .name("Spend"),
        // .tooltip_value(|d, v| ..), .fill(|d, bar, chart, align| color), .id("x") when siblings share a call site
))
LineChart::new(data2)                                // LineChart<T, X: PartialEq+Into<SharedString>, Y: PlotValue>
    .x(|d: &Monthly| d.month).y(|d: &Monthly| d.value)
    .stroke(t.chart_2).natural()                     // .linear() .step_after()
    .dot().y_axis(true).y_tick_format(|v| format!("{:.2}M", v / 1e6))
    // .y_domain(min,max) .reference_line(v) .grid(bool) .x_axis(bool) .tick_margin(n) .point_count(n)
```
- Default colors come from `chart_1`..`chart_5`.
- Charts are interactive by default (crosshair and tooltip). Turn this off with `.interactive(false)`.
- If one call site (for example a loop) renders several chart siblings, give each a distinct `.id(..)`.

## 11. Background work (SQLite, HTTP) and updating entities

```rust
fn reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
    self.status = "loading...".into(); cx.notify();
    let task = cx.background_spawn(async move {      // AppContext trait method; runs on the thread pool. Future must be Send.
        blocking_sqlite_or_http()                      // blocking is OK here (it's a pool thread)
    });
    cx.spawn(async move |this, cx| {                   // foreground (UI thread) task; this: WeakEntity<Self>, cx: &mut AsyncApp
        let rows = task.await;
        let _ = this.update(cx, |this, cx| {           // Result: Err if the view was dropped
            this.table.update(cx, |t, cx| { *t.delegate_mut() = TxnTable::new(rows); t.refresh(cx); cx.notify(); });
            cx.notify();
        });
    }).detach();                                       // or store the Task<()> in a field (dropping it cancels)
}
```
- `cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| ..)` gives a task that also has window access.
- Timers: `cx.background_executor().timer(Duration::from_secs(60)).await` inside a spawned task, for periodic sync.
- Executors: `cx.background_executor().spawn(fut)` is equivalent to `background_spawn`. `cx.foreground_executor()`.
- `rusqlite::Connection` is not `Sync`. Open it inside the background closure, or keep it in a dedicated thread behind a channel. Don't capture `Entity`/`WeakEntity` in `background_spawn`, because they are not `Send`.

## 12. Buttons, icons, misc

```rust
Button::new("reload").small().ghost().icon(IconName::RefreshCw).label("Reload")
    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| ..))     // or |_, window, cx| ..
// variants (ButtonVariants): .primary() .secondary() .danger() .warning() .success() .info() .ghost(); .outline(); .loading(b) .tooltip("..") .compact()
```
- Component `IconName` values that the default `Assets` bundle actually contains include `LayoutDashboard`, `ChartPie`, `Inbox`, `Search`, `Sun`, `Moon`, `RefreshCw`, `Settings`, `Calendar`, `ArrowUp`, `ArrowDown`, `TriangleAlert`, `CircleCheck`, `Bell`, `Building2`, `Globe`, `Star`, `Plus`, `Minus`, `Copy`, `Eye`, `EyeOff`, `FileText`, `Folder`, `Menu`, `Palette`, `Network`, `Cpu`, `User`, `SortAscending`, `SortDescending`.
- For any other Lucide icon, use `gpui_kit::assets::IconName::X` together with `.with_assets(gpui_kit::assets::AllAssets)`, or `icon_assets!(AppAssets, [X, Y])`.
- Other modules exist but are untested here: `tag`, `badge`, `kbd::Kbd`, `label`, `tab`, `select`, `switch`, `progress`, `skeleton`, `description_list`, `status_bar`, `title_bar::TitleBar` (custom title bar), `resizable::h_resizable`, `notification`, `tooltip`, `date_picker`.

## 13. Gotchas hit (and the fixes)

| Symptom | Fix |
|---|---|
| `no method font_semibold/font_bold on Div` | `use gpui_kit::component::StyledExt as _;` |
| `no method when on Div` | `use gpui_kit::prelude::FluentBuilder as _;` (the glob import doesn't bring it in) |
| E0502 `entity.read(cx).method(window, cx)` | `entity.update(cx, \|s, cx\| s.method(window, cx))` |
| Icons render blank (by source reading) | `.with_assets(gpui_kit::assets::Assets)` on `application()` |
| Chart invisible (by source reading) | give the parent a fixed `h(px(..))`, or a `flex_1` child inside a sized column |
| Context keybinding never fires | `.key_context("App").track_focus(&focus)` and focus the view at startup |
| Dialog content's actions don't reach the view | dialogs are a Root layer; use callbacks with `WeakEntity::update` or global `cx.on_action` |
| "component window state is missing" | call `gpui_kit::init(cx)` before `open_window` |
| `TableState::new(delegate, cx)` from older docs | 0.7 signature is `TableState::new(delegate, window, cx)` |
| `InputState::set_value` doesn't filter | it suppresses `InputEvent::Change`, so refilter manually |

## Components status

The probe compiles everything below. None of it has been run.

| Component | Status |
|---|---|
| Root (automatic via `open_window`) | compiles |
| Theme init, dark/light switch | compiles |
| Sidebar with 3 items that switch the view | compiles |
| Stat cards (divs with theme colors) | compiles |
| Virtualized DataTable, 5000 rows, 4 columns, right-aligned sortable amount and date | compiles |
| Input that filters on each keystroke | compiles |
| Command palette in a Dialog on ctrl-k, with `on_confirm` callback | compiles |
| BarChart and LineChart, 12 points | compiles |
| Actions bound to ctrl-k, ctrl-t (contexted), ctrl-r, ctrl-q (global) | compiles |
| Background `background_spawn` plus `cx.spawn` entity update | compiles |
| Grid, scroll, hover and text styling | compiles |

Nothing turned out unusable.

## Full probe `src/main.rs`

```rust
//! gpui-kit 0.7.0 probe: sidebar, stat cards, virtualized table + filter input,
//! command palette (ctrl-k), bar + line chart, theme toggle, background task.
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, Theme, ThemeMode, WindowExt as _,
    button::{Button, ButtonVariants as _},
    chart::{BarChart, LineChart},
    command::{Command, CommandGroup, CommandItem, CommandState},
    h_flex,
    input::{Input, InputEvent, InputState},
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState},
    v_flex,
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _; // .when() / .when_some() -- NOT in the glob

actions!(probe, [ToggleTheme, OpenPalette, Quit, Reload]);

// ---------- data ----------

#[derive(Clone)]
struct Txn {
    date: SharedString,
    payee: SharedString,
    category: SharedString,
    amount: f64,
}

fn gen_rows(n: usize) -> Vec<Txn> {
    let payees = ["Wellcome", "MTR", "HSBC", "Netflix", "Apple", "Uber", "ParknShop"];
    let cats = ["Groceries", "Transport", "Fees", "Subscriptions", "Shopping"];
    (0..n)
        .map(|i| Txn {
            date: format!("2026-{:02}-{:02}", i % 12 + 1, i % 28 + 1).into(),
            payee: payees[i % payees.len()].into(),
            category: cats[i % cats.len()].into(),
            amount: ((i * 7919) % 100_000) as f64 / 100.0 - 300.0,
        })
        .collect()
}

// ---------- table delegate ----------

struct TxnTable {
    all: Vec<Txn>,
    rows: Vec<Txn>, // filtered + sorted view
    columns: Vec<Column>,
}

impl TxnTable {
    fn new(all: Vec<Txn>) -> Self {
        Self {
            rows: all.clone(),
            all,
            columns: vec![
                Column::new("date", "Date").width(px(110.)).sortable(),
                Column::new("payee", "Payee").width(px(220.)),
                Column::new("category", "Category").width(px(160.)),
                Column::new("amount", "Amount").width(px(120.)).text_right().sortable(),
            ],
        }
    }

    fn filter(&mut self, q: &str) {
        let q = q.to_lowercase();
        self.rows = self
            .all
            .iter()
            .filter(|t| q.is_empty() || t.payee.to_lowercase().contains(&q) || t.category.to_lowercase().contains(&q))
            .cloned()
            .collect();
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
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let t = &self.rows[row_ix];
        match col_ix {
            0 => div().child(t.date.clone()),
            1 => div().child(t.payee.clone()),
            2 => div().text_color(cx.theme().muted_foreground).child(t.category.clone()),
            _ => div()
                .w_full()
                .text_right()
                .text_color(if t.amount < 0. { cx.theme().danger } else { cx.theme().success })
                .child(format!("{:.2}", t.amount)),
        }
    }
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        match (col_ix, sort) {
            (0, ColumnSort::Ascending) => self.rows.sort_by(|a, b| a.date.cmp(&b.date)),
            (0, ColumnSort::Descending) => self.rows.sort_by(|a, b| b.date.cmp(&a.date)),
            (3, ColumnSort::Ascending) => self.rows.sort_by(|a, b| a.amount.total_cmp(&b.amount)),
            (3, ColumnSort::Descending) => self.rows.sort_by(|a, b| b.amount.total_cmp(&a.amount)),
            _ => {} // ColumnSort::Default: leave as-is (or re-filter to restore order)
        }
    }
    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        let t = &self.rows[row_ix];
        match col_ix {
            0 => t.date.to_string(),
            1 => t.payee.to_string(),
            2 => t.category.to_string(),
            _ => format!("{:.2}", t.amount),
        }
    }
}

// ---------- main view ----------

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Transactions,
    Charts,
}

struct Monthly {
    month: &'static str,
    value: f64,
}

struct App_ {
    page: Page,
    focus: FocusHandle,
    search: Entity<InputState>,
    table: Entity<TableState<TxnTable>>,
    palette: Entity<CommandState>,
    status: SharedString,
    _subs: Vec<Subscription>,
}

impl App_ {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter payee / category..."));
        let table = cx.new(|cx| TableState::new(TxnTable::new(gen_rows(5000)), window, cx));
        let palette = cx.new(|cx| CommandState::new(window, cx));

        let subs = vec![
            // Input -> filter table on each keystroke.
            cx.subscribe_in(&search, window, |this, state, ev: &InputEvent, _window, cx| {
                if let InputEvent::Change = ev {
                    let q = state.read(cx).value();
                    this.table.update(cx, |t, cx| {
                        t.delegate_mut().filter(&q);
                        cx.notify();
                    });
                }
            }),
            // Table events.
            cx.subscribe(&table, |this, _, ev: &TableEvent, cx| {
                if let TableEvent::SelectRow(ix) = ev {
                    this.status = format!("selected row {ix}").into();
                    cx.notify();
                }
            }),
        ];

        Self {
            page: Page::Overview,
            focus: cx.focus_handle(),
            search,
            table,
            palette,
            status: "ready".into(),
            _subs: subs,
        }
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        let mode = if cx.theme().is_dark() { ThemeMode::Light } else { ThemeMode::Dark };
        Theme::change(mode, Some(window), cx);
        cx.notify();
    }

    fn reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        self.status = "loading...".into();
        cx.notify();
        // Heavy work on the background pool, then hop back to update the entity.
        let task = cx.background_spawn(async move {
            std::thread::sleep(Duration::from_millis(300)); // e.g. SQLite query / HTTP sync
            gen_rows(8000)
        });
        cx.spawn(async move |this, cx| {
            let rows = task.await;
            let n = rows.len();
            // WeakEntity::update returns anyhow::Result (Err if the view was dropped).
            let _ = this.update(cx, |this, cx| {
                this.table.update(cx, |t, cx| {
                    *t.delegate_mut() = TxnTable::new(rows);
                    t.refresh(cx);
                    cx.notify();
                });
                this.status = format!("loaded {n} rows").into();
                cx.notify();
            });
        })
        .detach();
    }

    fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.palette.clone();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog.p_0().close_button(false).child(
                Command::new(&state)
                    .bordered(false)
                    .placeholder("Type a command...")
                    .group(
                        CommandGroup::new()
                            .label("Go to")
                            .item(CommandItem::new().label("Overview").icon(IconName::LayoutDashboard))
                            .item(CommandItem::new().label("Transactions").icon(IconName::Inbox))
                            .item(CommandItem::new().label("Charts").icon(IconName::ChartPie)),
                    )
                    .group(
                        CommandGroup::new().label("Actions").item(
                            CommandItem::new()
                                .label("Toggle theme")
                                .icon(IconName::Sun)
                                .keywords(["dark", "light"]),
                        ),
                    )
                    // IndexPath { section, row } in input-model coordinates
                    // (group index, item index), unaffected by filtering.
                    .on_confirm(move |ix, window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            match (ix.section, ix.row) {
                                (0, 0) => this.page = Page::Overview,
                                (0, 1) => this.page = Page::Transactions,
                                (0, 2) => this.page = Page::Charts,
                                (1, 0) => {
                                    let mode = if cx.theme().is_dark() { ThemeMode::Light } else { ThemeMode::Dark };
                                    Theme::change(mode, None, cx);
                                }
                                _ => {}
                            }
                            cx.notify();
                        });
                        window.close_dialog(cx);
                    })
                    .on_cancel(|window, cx| window.close_dialog(cx)),
            )
        });
        self.palette.update(cx, |s, cx| {
            s.set_query("", window, cx); // palette state persists between opens
            s.focus(window, cx);
        });
    }

    fn set_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |label: &'static str, icon: IconName, page: Page, cx: &mut Context<Self>| {
            SidebarMenuItem::new(label)
                .icon(icon)
                .active(self.page == page)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_page(page, cx)))
        };
        Sidebar::new("sidebar")
            .collapsible(false)
            .header(SidebarHeader::new().child("Finance"))
            .child(
                SidebarGroup::new("Views").child(
                    SidebarMenu::new()
                        .child(item("Overview", IconName::LayoutDashboard, Page::Overview, cx))
                        .child(item("Transactions", IconName::Inbox, Page::Transactions, cx))
                        .child(item("Charts", IconName::ChartPie, Page::Charts, cx)),
                ),
            )
    }

    fn stat_card(label: &str, value: &str, delta: f64, cx: &App) -> impl IntoElement {
        let t = cx.theme();
        v_flex()
            .flex_1()
            .gap_1()
            .p_4()
            .rounded(t.radius_lg)
            .border_1()
            .border_color(t.border)
            .bg(t.secondary)
            .child(div().text_xs().text_color(t.muted_foreground).child(label.to_string()))
            .child(div().text_2xl().font_semibold().child(value.to_string()))
            .child(
                div()
                    .text_sm()
                    .text_color(if delta >= 0. { t.success } else { t.danger })
                    .child(format!("{delta:+.1}% MoM")),
            )
    }

    fn render_overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .child(Self::stat_card("Net worth", "HKD 1,234,567", 2.3, cx))
                    .child(Self::stat_card("Spending (Sep)", "HKD 23,450", -4.1, cx))
                    .child(Self::stat_card("Runway", "38 mo", 0.0, cx)),
            )
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.status.clone()))
            .child(Self::style_samples(cx))
    }

    /// Compile-checked tour of the Styled API (gpui + StyledExt).
    fn style_samples(cx: &App) -> impl IntoElement {
        let t = cx.theme();
        div()
            .id("samples") // .id() makes a Stateful<Div>: needed for overflow_y_scroll / on_click / hover tooltips
            .grid()
            .grid_cols(3)
            .gap_2()
            .max_h(px(200.))
            .overflow_y_scroll()
            .children((0..9).map(|i| {
                div()
                    .when(i == 0, |d| d.col_span(2)) // FluentBuilder::when (prelude)
                    .px_3()
                    .py_2()
                    .min_w_0()
                    .rounded_md()
                    .shadow_sm()
                    .cursor_pointer()
                    .bg(t.muted)
                    .hover(|s| s.bg(t.accent))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .font_family(t.mono_font_family.clone())
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(format!("cell {i} with a long label that truncates"))
            }))
    }

    fn render_transactions(&self, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .gap_3()
            .child(Input::new(&self.search).cleanable(true).prefix(IconName::Search))
            .child(div().flex_1().min_h_0().child(DataTable::new(&self.table).stripe(true).small()))
    }

    fn render_charts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let data: Vec<Monthly> = months
            .iter()
            .enumerate()
            .map(|(i, m)| Monthly { month: m, value: 18_000. + (i as f64 * 1371.) % 9000. })
            .collect();
        let data2: Vec<Monthly> = months
            .iter()
            .enumerate()
            .map(|(i, m)| Monthly { month: m, value: 1_000_000. + i as f64 * 21_000. })
            .collect();
        let t = cx.theme();
        v_flex()
            .gap_4()
            .child(
                v_flex().h(px(260.)).p_3().border_1().border_color(t.border).rounded(t.radius)
                    .child(div().text_sm().child("Monthly spending"))
                    .child(
                        div().flex_1().child(
                            BarChart::new(data)
                                .band(|d: &Monthly| d.month)
                                .value(|d: &Monthly| d.value)
                                .value_axis(true)
                                .value_tick_format(|v| format!("{:.0}k", v / 1000.)),
                        ),
                    ),
            )
            .child(
                v_flex().h(px(260.)).p_3().border_1().border_color(t.border).rounded(t.radius)
                    .child(div().text_sm().child("Net worth"))
                    .child(
                        div().flex_1().child(
                            LineChart::new(data2)
                                .x(|d: &Monthly| d.month)
                                .y(|d: &Monthly| d.value)
                                .stroke(t.chart_2)
                                .natural()
                                .dot()
                                .y_axis(true)
                                .y_tick_format(|v| format!("{:.2}M", v / 1e6)),
                        ),
                    ),
            )
    }
}

impl Focusable for App_ {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for App_ {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.page {
            Page::Overview => self.render_overview(cx).into_any_element(),
            Page::Transactions => self.render_transactions(cx).into_any_element(),
            Page::Charts => self.render_charts(cx).into_any_element(),
        };
        h_flex()
            .id("app")
            .key_context("App")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(Self::reload))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .p_6()
                    .gap_4()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(div().text_xl().font_bold().child("Probe"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("reload")
                                            .small()
                                            .ghost()
                                            .icon(IconName::RefreshCw)
                                            .on_click(cx.listener(|this, _, w, cx| this.reload(&Reload, w, cx))),
                                    )
                                    .child(
                                        Button::new("theme")
                                            .small()
                                            .outline()
                                            .icon(if cx.theme().is_dark() { IconName::Sun } else { IconName::Moon })
                                            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleTheme), cx)),
                                    ),
                            ),
                    )
                    .child(div().flex_1().min_h_0().child(content)),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets) // icons render blank without this
        .run(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-k", OpenPalette, None),
                KeyBinding::new("ctrl-t", ToggleTheme, Some("App")),
                KeyBinding::new("ctrl-r", Reload, Some("App")),
                KeyBinding::new("ctrl-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit()); // global action handler
            Theme::change(ThemeMode::Dark, None, cx);

            let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Finance probe".into()), ..Default::default() }),
                window_min_size: Some(size(px(800.), px(500.))),
                ..Default::default()
            };
            let (_handle, view) =
                gpui_kit::open_window(options, cx, |window, cx| {
                    let view = cx.new(|cx| App_::new(window, cx));
                    // Context-scoped bindings (Some("App")) fire only while focus is inside.
                    window.focus(&view.focus_handle(cx), cx);
                    view
                })
                .expect("open window");
            let _ = view; // keep / store if needed
            cx.activate(true);
        });
}
```
