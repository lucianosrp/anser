//! The window: sidebar, header (sync state, search), the current page, and
//! the Ctrl+K palette that searches everything.
//!
//! Local-first: the cached replica loads on a background thread at startup
//! (a few ms), so the window paints immediately and works offline. Sync runs
//! in the background on launch when the cache is older than SYNC_AFTER, every
//! SYNC_EVERY while open, and on Ctrl+R.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
// std's Instant panics in the browser; web_time is std's on native
use web_time::Instant;

use finance_core::search::Searcher;
use finance_core::{cache, sync, Bundle, Ledger};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    command::{Command, CommandGroup, CommandItem, CommandState},
    h_flex,
    input::{Input, InputEvent, InputState},
    table::{DataTable, TableEvent, TableState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::pages::{self, Page, settings::ConnTest, transactions::TxnTable};
use crate::palette::{self, Action, Choice, Entry};
use crate::theme::ThemeConfig;
use crate::ui::{self, Figures as _};
use crate::zoom::px;

// filters, the summary strip, row menus, deep dives
#[path = "app_filters.rs"]
mod filters;
// the server account: Settings' account card, the onboarding wizard
#[path = "app_categories.rs"]
mod cats_glue;
#[path = "app_account.rs"]
mod account;
// the avatar menu: accounts on this machine, signing out
#[path = "accounts_menu.rs"]
pub(crate) mod accounts_menu;
// "Just try it": the demo account (its header pill, building and leaving it)
#[path = "app_demo.rs"]
pub(crate) mod demo;

actions!(finance, [OpenPalette, SyncNow, RefreshLive, ToggleMode, FocusSearch, Go1, Go2, Go3, Go4, Go5, Go6, Go7, Go8, Go9, Go0, ZoomIn, ZoomOut, ZoomReset, TogglePrivacy]);
// drill-down: month zoom on Spending, the row detail sheet on Transactions
actions!(finance, [PrevMonth, NextMonth, OpenDetail, PlotQuery]);
// pages with letter keys (Ctrl+Y, Ctrl+U) or reached from a tile
actions!(finance, [GoReview, GoUpload, GoBudgets, GoDoctor, GoHomeSetup, GoSupported]);
// the jump list (Ctrl+O / Ctrl+I, history.rs) and the sidebar rail (Ctrl+B)
actions!(finance, [JumpBack, JumpForward, ToggleSidebar]);
// the avatar menu: another account (desktop), signing out
actions!(finance, [AddAccount, SignOut]);

/// The browser's page query (`?page=settings&account_menu=1`), set at boot.
static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[cfg(target_family = "wasm")]
pub fn set_query(q: String) {
    let _ = QUERY.set(q);
}

/// A dev/screenshot aid: the environment variable on the desktop; in the
/// browser the query parameter named like it without the prefix,
/// lowercase (`FINANCE_DESKTOP_PAGE` -> `?page=`).
pub fn dev_aid(var: &str) -> Option<String> {
    if let Ok(v) = std::env::var(var) {
        return Some(v);
    }
    let key = var.trim_start_matches("FINANCE_DESKTOP_").to_ascii_lowercase();
    QUERY.get()?.trim_start_matches('?').split('&').find_map(|kv| kv.strip_prefix(key.as_str())?.strip_prefix('=').map(String::from))
}

const SYNC_AFTER: Duration = Duration::from_secs(15 * 60);
const SYNC_EVERY: Duration = Duration::from_secs(30 * 60);
const PALETTE_HITS: usize = 14;
/// content column cap, so a full-screen window stays readable
const MAX_CONTENT: f32 = 1320.;
/// How long the result of a live refresh stays in the sync line.
const LIVE_NOTE_FOR: Duration = Duration::from_secs(20);

struct Store {
    ledger: Arc<Ledger>,
    bundle: Arc<Bundle>,
    /// discovered once per load (a few ms over the whole ledger)
    trips: Arc<Vec<finance_core::travel::Trip>>,
    /// recurring charges and income (finance_core::recurring), once per load
    recurring: Arc<Vec<finance_core::recurring::Series>>,
    /// what `trip:` names, for the search and the filter bars
    trip_index: Arc<finance_core::filter::Trips>,
}

/// A place in the jump list: the page and its cheap view state.
#[derive(Clone, PartialEq, Debug)]
struct Spot {
    page: Page,
    inv_tab: pages::investments::Tab,
    broker_tab: pages::broker::Tab,
    broker_symbol: Option<SharedString>,
    crypto_tab: pages::crypto::Tab,
    spend_month: Option<String>,
    spend_tab: pages::spending::Tab,
    trip_open: Option<usize>,
    travel_year: Option<i32>,
    /// the Transactions search (empty on other pages)
    query: String,
    /// an open merchant or category deep dive
    dive: Option<crate::dive::Dive>,
}

impl Spot {
    /// Only the search differs: typing refines one jump.
    fn same_place(a: &Spot, b: &Spot) -> bool {
        a.query != b.query && Spot { query: String::new(), ..a.clone() } == Spot { query: String::new(), ..b.clone() }
    }
}

enum SyncState {
    Idle,
    Running,
    Failed(String),
}

pub struct FinanceApp {
    page: Page,
    focus: FocusHandle,
    store: Option<Store>,
    load_error: Option<String>,
    sync: SyncState,
    /// a live refresh is in flight (the server fetching): `sync` is Running too
    refreshing: bool,
    /// the last live refresh's result and its failures, shown for LIVE_NOTE_FOR
    live_note: Option<(Instant, String, String)>,
    /// when the empty first-load screen first drew (the braille field's delay)
    first_load: std::cell::Cell<Option<Instant>>,
    /// the window's height as of the last render (the empty state fills it)
    viewport_h: std::cell::Cell<Pixels>,
    searcher: Searcher,
    search: Entity<InputState>,
    search_stats: (usize, Duration),
    table: Entity<TableState<TxnTable>>,
    palette: Entity<CommandState>,
    palette_results: palette::Results,
    /// a highlighted theme is applied but not saved (restored on leaving it)
    palette_previewing: bool,
    open_palette_on_load: bool,
    theme: ThemeConfig,
    theme_stamp: Option<crate::theme::ThemeStamp>,
    /// Investments hero chart: HK$ (false) or time-weighted % growth (true)
    inv_pct: bool,
    /// Investments: the open tab
    inv_tab: pages::investments::Tab,
    /// Overview projection: how the invested pool grows
    growth: pages::projection::Growth,
    /// Broker page: account overview or per-holding monitoring
    broker_tab: pages::broker::Tab,
    /// Broker Monitoring: which holding the price chart shows
    broker_symbol: Option<SharedString>,
    /// Crypto page: holdings or the BTC DCA analysis, and the DCA's scope
    crypto_tab: pages::crypto::Tab,
    dca_scope: finance_core::dca::Scope,
    /// Travel: the expanded trip (index into the oldest-first list)
    trip_open: Option<usize>,
    /// Spending month zoom ("YYYY-MM"); None = the last completed month
    spend_month: Option<String>,
    spend_tab: pages::spending::Tab,
    /// dev aid: open the detail of the first table row once data is in
    detail_on_load: bool,
    accent_input: Entity<InputState>,
    server_input: Entity<InputState>,
    conn_test: ConnTest,
    settings_note: Option<String>,
    /// the Upload page: its own view, fed by drops anywhere in the window
    upload: Entity<pages::upload::Uploader>,
    /// the Downloads watcher's banner (opt-in, Settings)
    downloads: Entity<pages::downloads::Downloads>,
    /// Settings › General: the app's own update (checks, one-click apply)
    updates: Entity<pages::updates::Updates>,
    /// Settings › Home: the home setup's form
    home_setup: Entity<pages::home_setup::HomeSetupForm>,
    /// Spending › Home (mortgage): the "extra a month" what-if field
    home_whatif: Entity<InputState>,
    /// Budgets and Year in review: their own views (edit fields, a cached review)
    budgets: Entity<pages::budgets::BudgetsView>,
    review: Entity<pages::review::ReviewView>,
    /// Subscriptions: how the active list is ordered
    subs_sort: pages::subscriptions::Sort,
    /// the Cash flow page: its own view (calendar config, entry editor)
    cashflow: Entity<pages::cashflow::CashFlow>,
    /// the Doctor page: its own view (checks land as they finish)
    doctor: Entity<pages::doctor::Doctor>,
    /// what the current search's matches add up to (the summary strip)
    search_summary: finance_core::filter::Summary,
    /// Spending's filter, Subscriptions' and Travel's
    filters: filters::PageFilters,
    /// Broker › Deep dive: its own view (section, holding, targets editor)
    deep: Entity<pages::deep::DeepDive>,
    /// the listed pages' scroll and measured section heights (section_list)
    page_list: ListState,
    /// (page, its tab, data generation, sections): a change resets page_list
    page_list_key: Option<(Page, String, u64, usize)>,
    /// bumps on every data load
    data_gen: u64,
    /// where the owner has been (Ctrl+O / Ctrl+I)
    jumps: crate::history::JumpList<Spot>,
    /// Categories, Rules, Categorize Other (pages::categories)
    cats: Entity<pages::categories::CategoriesView>,
    /// the deep dive open in a dialog, if any (a jump-list spot)
    dive_open: Option<crate::dive::Dive>,
    acct: account::Glue,
    _subs: Vec<Subscription>,
    /// Settings' open tab
    pub(crate) settings_tab: pages::settings::Tab,
}

/// What [`FinanceApp::page_body`] made.
enum Body {
    One(AnyElement),
    Sections(ui::Sections),
}

fn search_placeholder(acct: &str) -> String {
    format!("Search everything — coffee, cat:dining, -cat:rent, merchant:sushiro, {acct}, >500, 2026-01..2026-06, out")
}

impl FinanceApp {
    pub fn new(theme: ThemeConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::timing::mark("view new");
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(search_placeholder("acct:savings"))
        });
        let table = cx.new(|cx| TableState::new(TxnTable::new(Arc::new(Ledger::default())), window, cx));
        let palette = cx.new(|cx| CommandState::new(window, cx));
        // the OS switched light/dark: "system" follows it live
        let mut subs = vec![cx.observe_window_appearance(window, |this, _, cx| {
            if this.theme.mode == "system" {
                crate::theme::apply(&this.theme, cx);
                cx.notify();
            }
        }), cx.subscribe_in(&search, window, |this, state, ev: &InputEvent, window, cx| {
            match ev {
                InputEvent::Change => {
                    let q = state.read(cx).value().to_string();
                    this.run_search(&q, cx);
                }
                // Ctrl+Enter: plot every match
                InputEvent::PressEnter { secondary: true, .. } => {
                    let q = state.read(cx).value().to_string();
                    this.open_plot(q, window, cx);
                }
                _ => {}
            }
        })];
        // Ctrl+Enter inside the palette: its query input sits in a dialog, outside
        // this view's dispatch path -- the dialog content handles it (open_palette)
        cx.bind_keys([KeyBinding::new("ctrl-enter", PlotQuery, Some("Command > Input"))]);
        crate::codeslots::bind_keys(cx);
        // drill-down and settings wiring
        cx.bind_keys([
            KeyBinding::new("left", PrevMonth, Some("FinanceApp")),
            KeyBinding::new("right", NextMonth, Some("FinanceApp")),
            KeyBinding::new("[", PrevMonth, Some("FinanceApp")),
            KeyBinding::new("]", NextMonth, Some("FinanceApp")),
            KeyBinding::new("enter", OpenDetail, Some("DataTable")),
            // not while typing: Ctrl+I / Ctrl+O belong to the text there
            KeyBinding::new("ctrl-o", JumpBack, Some("FinanceApp && !Input")),
            KeyBinding::new("ctrl-i", JumpForward, Some("FinanceApp && !Input")),
            KeyBinding::new("ctrl-b", ToggleSidebar, Some("FinanceApp")),
        ]);
        let custom = if theme.accent.starts_with('#') { theme.accent.clone() } else { String::new() };
        let accent_input = cx.new(|cx| InputState::new(window, cx).placeholder("#7aa2f7").default_value(custom));
        let server_input = cx.new(|cx| {
            let url = cache::saved_server_url().unwrap_or_else(cache::server_url);
            InputState::new(window, cx).placeholder(cache::DEFAULT_SERVER).default_value(url)
        });
        subs.push(cx.subscribe_in(&table, window, |this, _, ev: &TableEvent, window, cx| {
            if let TableEvent::DoubleClickedRow(row) = ev {
                this.open_detail_row(*row, window, cx);
            }
        }));
        subs.push(cx.subscribe_in(&accent_input, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.apply_custom_accent(window, cx);
            }
        }));
        subs.push(cx.subscribe_in(&server_input, window, |this, _, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.save_server(cx);
            }
        }));
        let upload = cx.new(|cx| pages::upload::Uploader::new(window, cx));
        let downloads = cx.new(|cx| pages::downloads::Downloads::new(window, cx));
        let updates = cx.new(|cx| pages::updates::Updates::new(window, cx));
        // the Downloads banner: import now, or review on the Upload page
        subs.push(cx.subscribe_in(&downloads, window, |this, _, ev: &pages::downloads::DownloadsEvent, window, cx| {
            this.go(Page::Upload, window, cx);
            match ev {
                pages::downloads::DownloadsEvent::ImportAll(p) => { let p = p.clone(); this.upload.update(cx, |u, cx| u.add(p, cx)) }
                pages::downloads::DownloadsEvent::Review(p) => { let p = p.clone(); this.upload.update(cx, |u, cx| u.add_held(p, cx)) }
            }
        }));
        let home_setup = cx.new(|cx| pages::home_setup::HomeSetupForm::new(window, cx));
        // the rule the landlord's text became: patch the ledger now
        subs.push(cx.subscribe(&home_setup, |this, _, ev: &pages::home_setup::HomeSetupEvent, cx| match ev {
            pages::home_setup::HomeSetupEvent::Written(v) => this.apply_write(v.clone(), cx),
        }));
        let home_whatif = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. 500"));
        subs.push(cx.subscribe_in(&home_whatif, window, |_, _, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        }));
        let budgets = cx.new(pages::budgets::BudgetsView::new);
        let review = {
            let view = cx.entity().downgrade();
            let on_pick: pages::spending::OnPick = Rc::new(move |ix, window, cx| {
                let _ = view.update(cx, |this, cx| this.open_detail(ix, window, cx));
            });
            let on_search = Self::search_callback(cx);
            cx.new(|cx| pages::review::ReviewView::new(on_pick, on_search, cx))
        };
        // an upload saved files on the server: pull them (after any sync already under way)
        subs.push(cx.subscribe(&upload, |_, _, _: &pages::upload::Uploaded, cx| {
            cx.spawn(async move |this, cx| loop {
                let busy = this.update(cx, |this, cx| {
                    let busy = matches!(this.sync, SyncState::Running);
                    if !busy {
                        this.sync_now(&SyncNow, cx);
                    }
                    busy
                });
                match busy {
                    Ok(true) => cx.background_executor().timer(Duration::from_millis(500)).await,
                    _ => break,
                }
            })
            .detach();
        }));
        let cashflow = cx.new(|cx| pages::cashflow::CashFlow::new(window, cx));
        let deep = cx.new(|cx| pages::deep::DeepDive::new(window, cx));
        subs.push(cx.subscribe_in(&cashflow, window, |this, _, ev: &pages::cashflow::Pick, window, cx| {
            this.open_detail(ev.0, window, cx);
        }));
        let doctor = cx.new(|cx| pages::doctor::Doctor::new(window, cx));
        subs.push(cx.subscribe_in(&doctor, window, |this, _, ev: &pages::doctor::DoctorEvent, window, cx| match ev {
            pages::doctor::DoctorEvent::SyncNow => this.sync_now(&SyncNow, cx),
            pages::doctor::DoctorEvent::OpenUpload => this.go(Page::Upload, window, cx),
        }));
        let cats = cx.new(|cx| pages::categories::CategoriesView::new(window, cx));
        subs.push(Self::wire_categories(&cats, window, cx));
        // its rows are the app's list sections now, so its changes redraw the app
        subs.push(cx.observe(&cats, |_, _, cx| cx.notify()));
        let (acct, acct_subs) = account::Glue::new(window, cx);
        subs.extend(acct_subs);
        let mut this = FinanceApp {
            acct,
            page: Page::Overview,
            focus: cx.focus_handle(),
            store: None,
            load_error: None,
            sync: SyncState::Idle,
            refreshing: false,
            live_note: None,
            first_load: std::cell::Cell::new(None),
            viewport_h: std::cell::Cell::new(gpui_kit::px(600.)),
            searcher: Searcher::default(),
            search,
            search_stats: (0, Duration::ZERO),
            table,
            palette,
            palette_results: crate::palette::filter("", Page::ALL.into_iter().filter(|p| p.visible()), &theme),
            palette_previewing: false,
            open_palette_on_load: std::env::var_os("FINANCE_DESKTOP_PALETTE").is_some(),
            theme_stamp: theme.theme_stamp(),
            inv_pct: std::env::var_os("FINANCE_DESKTOP_INV_PCT").is_some(), // dev/screenshot aid
            inv_tab: pages::investments::Tab::from_env(), // FINANCE_DESKTOP_INV_TAB, dev/screenshot aid
            growth: pages::projection::Growth::default(),
            broker_tab: match dev_aid("FINANCE_DESKTOP_BROKER_TAB").as_deref() { // dev/screenshot aid, ?broker_tab=
                Some("monitoring") => pages::broker::Tab::Monitoring,
                Some("deep") => pages::broker::Tab::DeepDive,
                _ => pages::broker::Tab::Overview,
            },
            broker_symbol: std::env::var("FINANCE_DESKTOP_SYMBOL").ok().map(SharedString::from), // dev/screenshot aid
            theme,
            spend_month: std::env::var("FINANCE_DESKTOP_MONTH").ok(),
            spend_tab: pages::spending::Tab::from_env(), // FINANCE_DESKTOP_SPEND_TAB, dev/screenshot aid
            // dev/screenshot aid: FINANCE_DESKTOP_TRIP=N expands the N-th newest trip
            trip_open: None,
            // dev/screenshot aid: FINANCE_DESKTOP_CRYPTO_TAB=dca, FINANCE_DESKTOP_DCA_SCOPE=all
            crypto_tab: if std::env::var("FINANCE_DESKTOP_CRYPTO_TAB").is_ok_and(|v| v == "dca") { pages::crypto::Tab::Dca } else { Default::default() },
            dca_scope: if std::env::var("FINANCE_DESKTOP_DCA_SCOPE").is_ok_and(|v| v == "all") { finance_core::dca::Scope::All } else { Default::default() },
            detail_on_load: dev_aid("FINANCE_DESKTOP_DETAIL").is_some(), // ?detail=1
            accent_input,
            server_input,
            conn_test: ConnTest::Idle,
            settings_note: None,
            upload,
            downloads,
            updates,
            home_setup,
            home_whatif,
            budgets,
            review,
            subs_sort: Default::default(),
            cashflow,
            doctor,
            search_summary: Default::default(),
            filters: filters::PageFilters::from_env(),
            deep,
            page_list: ListState::new(0, ListAlignment::Top, px(600.)),
            page_list_key: None,
            data_gen: 0,
            jumps: Default::default(),
            dive_open: None,
            cats,
            _subs: subs,
            settings_tab: pages::settings::Tab::from_env(),
        };
        // dev/screenshot aids: start on a page, optionally with a search
        if let Some(p) = dev_aid("FINANCE_DESKTOP_PAGE") {
            if let Some(page) = Page::ALL.into_iter().find(|x| x.label().eq_ignore_ascii_case(&p) || format!("{x:?}").eq_ignore_ascii_case(&p)) {
                this.page = page;
            }
        }
        if let Ok(q) = std::env::var("FINANCE_DESKTOP_QUERY") {
            this.search.update(cx, |s, cx| s.set_value(q, window, cx));
        }
        this.load_cache(true, cx);
        this.start_background_loops(cx);
        this
    }

    // ---------- data ----------

    /// Load the replica off the UI thread; optionally sync afterwards when stale.
    /// Read the local replica before the first frame: it takes ~15 ms, less
    /// than the empty frame (and its flash) a background load would cost.
    pub(crate) fn load_cache(&mut self, sync_if_stale: bool, cx: &mut Context<Self>) {
        // an encrypted copy opens from the keyring by itself; a passphrase
        // one waits for the unlock screen (pages::vault)
        if !finance_core::replica::auto_unlock() {
            cx.notify();
            return;
        }
        // the demo's made-up months follow the calendar
        #[cfg(not(target_family = "wasm"))]
        demo::refresh_if_stale();
        let loaded = Ledger::load(&cache::db_path()).and_then(|ledger| {
            crate::timing::mark("ledger loaded");
            Ok((ledger, Bundle::load(&cache::bundle_path())?))
        });
        crate::timing::mark("bundle loaded");
        match loaded {
            Ok((ledger, bundle)) => {
                self.set_store(ledger, bundle, cx);
                crate::timing::mark("store set");
                if let (Some(n), Some(st)) = (std::env::var("FINANCE_DESKTOP_TRIP").ok().and_then(|v| v.parse::<usize>().ok()), &self.store) {
                    self.trip_open = st.trips.len().checked_sub(n + 1);
                }
            }
            Err(e) => self.load_error = Some(format!("{e:#}")),
        }
        // the rest needs the window, which exists only once new() returns
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.store.is_some() {
                    if let Ok(q) = std::env::var("FINANCE_DESKTOP_PLOT") {
                        // dev/screenshot aid: open the Ctrl+Enter plot for a query
                        if let Some(handle) = cx.windows().first().copied() {
                            let view = cx.entity().downgrade();
                            cx.defer(move |cx| {
                                let _ = handle.update(cx, |_, window, cx| {
                                    let _ = view.update(cx, |this, cx| this.open_plot(q, window, cx));
                                });
                            });
                        }
                    }
                    // dev/screenshot aids: a deep dive, a picker, saved filters
                    if let Some(handle) = cx.windows().first().copied() {
                        let view = cx.entity().downgrade();
                        cx.defer(move |cx| {
                            let _ = handle.update(cx, |_, window, cx| {
                                let _ = view.update(cx, |this, cx| this.filter_aids_on_load(window, cx));
                            });
                        });
                    }
                    if std::mem::take(&mut this.open_palette_on_load) {
                        if let Some(handle) = cx.windows().first().copied() {
                            let _ = handle.update(cx, |_, window, cx| {
                                window.dispatch_action(Box::new(OpenPalette), cx)
                            });
                        }
                    }
                }
                // after the first load, so it never delays it
                this.updates.update(cx, |u, cx| u.start(cx));
                let stale = cache::age_secs().is_none_or(|a| a > SYNC_AFTER.as_secs());
                if sync_if_stale && stale {
                    this.sync_auto(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_store(&mut self, ledger: Ledger, bundle: Bundle, cx: &mut Context<Self>) {
        let ledger = Arc::new(ledger);
        self.load_error = None;
        let trips = Arc::new(finance_core::travel::discover(&ledger));
        let recurring = Arc::new(finance_core::recurring::detect(&ledger));
        let bundle = Arc::new(bundle);
        self.budgets.update(cx, |b, cx| b.set_ledger(ledger.clone(), cx));
        self.home_setup.update(cx, |h, cx| h.set_ledger(ledger.clone(), cx));
        self.review.update(cx, |r, cx| r.set_data(ledger.clone(), bundle.clone(), trips.clone(), cx));
        let (l, b, r) = (ledger.clone(), bundle.clone(), recurring.clone());
        self.cashflow.update(cx, |c, cx| c.set_data(l, b, r, cx));
        let (l, b) = (ledger.clone(), bundle.clone());
        self.deep.update(cx, |d, cx| d.set_data(l, b, cx));
        let l = ledger.clone();
        self.cats.update(cx, |c, cx| c.set_data(l, cx));
        let trip_index = Arc::new(Self::trip_index(&trips));
        self.searcher.set_trips((*trip_index).clone());
        self.after_store(&bundle, cx);
        // pages the server has nothing for leave the sidebar, palette and tab bar
        let caps = finance_core::capabilities::Capabilities::of(&bundle);
        pages::set_capabilities(&caps);
        // the search box's account example is one of theirs
        if let Some(handle) = cx.windows().first().copied() {
            let (search, example) = (self.search.clone(), search_placeholder(&caps.search_example()));
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| search.update(cx, |s, cx| s.set_placeholder(example, window, cx)));
            });
        }
        // the Upload page's hint and paste card follow the user's sources
        self.acct.account.update(cx, |a, cx| a.set_capabilities(caps.clone(), cx));
        self.upload.update(cx, |u, cx| u.set_capabilities(caps, cx));
        self.data_gen += 1;
        crate::memo::clear();
        self.store = Some(Store { ledger: ledger.clone(), bundle, trips, recurring, trip_index });
        let menu = Self::row_menu(cx);
        self.table.update(cx, |t, cx| {
            *t.delegate_mut() = TxnTable::new(ledger);
            t.delegate_mut().menu = Some(menu);
            t.refresh(cx);
            cx.notify();
        });
        let q = self.search.read(cx).value().to_string();
        self.run_search(&q, cx);
    }

    /// Sync because the owner asked (Ctrl+R, a button, a sign-in): after a
    /// 401 this is what lets the data be asked for once more.
    pub(crate) fn sync_now(&mut self, _: &SyncNow, cx: &mut Context<Self>) {
        finance_core::signin::user_action();
        self.run_sync(cx);
    }

    /// Ctrl+Shift+K: the server fetches this user's live sources now
    /// (exchanges, broker, tickers), then the replica comes down as Ctrl+R
    /// does. An old server without the endpoint just gets the plain sync.
    pub(crate) fn refresh_live(&mut self, _: &RefreshLive, cx: &mut Context<Self>) {
        if demo::active() || matches!(self.sync, SyncState::Running) || finance_core::replica::locked() {
            return;
        }
        finance_core::signin::user_action();
        self.sync = SyncState::Running;
        self.refreshing = true;
        self.live_note = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = finance_core::client::refresh_live().await;
            let _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                this.sync = SyncState::Idle;
                this.live_note = Some(match res {
                    Ok(r) => (Instant::now(), r.summary(), r.problems()),
                    Err(e) => (Instant::now(), "Live refresh failed".into(), e),
                });
                this.run_sync(cx);
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(LIVE_NOTE_FOR).await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            });
        })
        .detach();
    }

    /// Sync on its own (stale at start, every SYNC_EVERY): never while the
    /// server turns this device away; the sign-in screen is showing then.
    fn sync_auto(&mut self, cx: &mut Context<Self>) {
        if !finance_core::signin::needed() {
            self.run_sync(cx);
        }
    }

    fn run_sync(&mut self, cx: &mut Context<Self>) {
        // the demo has no server: nothing to pull (the header says so)
        if demo::active() || matches!(self.sync, SyncState::Running) || finance_core::replica::locked() {
            return;
        }
        self.sync = SyncState::Running;
        cx.notify();
        let task = cx.background_spawn(async move {
            sync::pull()?;
            let ledger = Ledger::load(&cache::db_path())?;
            let bundle = Bundle::load(&cache::bundle_path())?;
            anyhow::Ok((ledger, bundle))
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok((ledger, bundle)) => {
                        this.sync = SyncState::Idle;
                        this.set_store(ledger, bundle, cx);
                    }
                    Err(e) => {
                        this.sync = SyncState::Failed(format!("{e:#}"));
                        // turned away with nothing local to show: sign in
                        if finance_core::signin::needed() && this.store.is_none() {
                            this.open_sign_in_later(cx);
                        }
                    }
                }
                this.doctor.update(cx, |d, cx| d.synced(cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// Periodic sync, and live re-theming when Omarchy or matugen change the desktop theme.
    fn start_background_loops(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut last_sync = Instant::now();
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let alive = this.update(cx, |this, cx| {
                    let stamp = this.theme.theme_stamp();
                    if stamp != this.theme_stamp {
                        this.theme_stamp = stamp;
                        crate::theme::apply(&this.theme, cx);
                    }
                    if last_sync.elapsed() >= SYNC_EVERY {
                        last_sync = Instant::now();
                        this.sync_auto(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn run_search(&mut self, q: &str, cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return };
        let started = Instant::now();
        let rows = if q.trim().is_empty() {
            (0..store.ledger.entries.len()).collect()
        } else {
            self.searcher.search(&store.ledger, q, usize::MAX)
        };
        self.search_stats = (rows.len(), started.elapsed());
        self.summarize(&rows);
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set_rows(rows);
            cx.notify();
        });
        cx.notify();
    }

    // ---------- navigation & actions ----------

    fn go(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        // Settings asks the server again on every visit: a user or device added
        // elsewhere (another browser, the CLI) shows up in Admin, Users
        if page == Page::Settings && self.page != page {
            self.acct.account.update(cx, |a, cx| a.refresh_cx(cx));
        }
        // a new key's secret doesn't outlive the Settings page
        if self.page == Page::Settings && page != Page::Settings {
            self.acct.assistants.update(cx, |a, cx| a.forget(cx));
        }
        self.page = page;
        if page == Page::Transactions && !crate::layout::compact() {
            self.search.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    fn toggle_mode(&mut self, _: &ToggleMode, _: &mut Window, cx: &mut Context<Self>) {
        self.theme.mode = if self.theme.dark() { "light".into() } else { "dark".into() };
        self.theme.save();
        crate::theme::apply(&self.theme, cx);
        cx.notify();
    }

    /// Zoom by `step` (0 = back to 100%), saved with the theme.
    pub(crate) fn zoom_by(&mut self, step: f32, window: &mut Window, cx: &mut Context<Self>) {
        let z = if step == 0. { 1. } else { crate::zoom::get() + step };
        let z = crate::zoom::set(z);
        crate::zoom::apply(window, cx);
        self.theme.zoom = z;
        self.theme.save();
        cx.notify();
    }

    /// Where the app is now, for the jump list.
    fn spot(&self, cx: &App) -> Spot {
        Spot {
            page: self.page,
            inv_tab: self.inv_tab,
            broker_tab: self.broker_tab,
            broker_symbol: self.broker_symbol.clone(),
            crypto_tab: self.crypto_tab,
            spend_month: self.spend_month.clone(),
            spend_tab: self.spend_tab,
            trip_open: self.trip_open,
            travel_year: self.filters.travel_year,
            query: if self.page == Page::Transactions { self.search.read(cx).value().to_string() } else { String::new() },
            dive: self.dive_open.clone(),
        }
    }

    /// Ctrl+O (`back`) / Ctrl+I: move through the jump list and put the app
    /// where that spot was. Recording it again on the next frame is a no-op.
    fn jump(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(to) = (if back { self.jumps.back() } else { self.jumps.forward() }) else { return };
        // the page directly, not go(): no search focus, so the next Ctrl+O works
        if self.page == Page::Settings && to.page != Page::Settings {
            self.acct.assistants.update(cx, |a, cx| a.forget(cx));
        }
        self.page = to.page;
        self.inv_tab = to.inv_tab;
        self.broker_tab = to.broker_tab;
        self.broker_symbol = to.broker_symbol;
        self.crypto_tab = to.crypto_tab;
        self.spend_month = to.spend_month;
        self.spend_tab = to.spend_tab;
        self.trip_open = to.trip_open;
        self.filters.travel_year = to.travel_year;
        if to.page == Page::Transactions && self.search.read(cx).value() != to.query.as_str() {
            self.search.update(cx, |s, cx| s.set_value(to.query.clone(), window, cx));
            self.run_search(&to.query, cx);
        }
        if to.dive != self.dive_open {
            window.close_dialog(cx);
            self.dive_open = None;
            if let Some(d) = to.dive {
                self.dive(d, window, cx);
            }
        }
        cx.notify();
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.update_theme(|c| c.sidebar_collapsed = !c.sidebar_collapsed, cx);
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.go(Page::Transactions, window, cx);
    }

    /// Jump to the transactions table with `query` searched.
    fn show_in_table(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_value(query.clone(), window, cx));
        self.run_search(&query, cx); // set_value emits no Change event
        self.go(Page::Transactions, window, cx);
    }

    /// Every row matching `query`, plotted cumulatively in a dialog.
    fn open_plot(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return };
        if query.trim().is_empty() {
            return;
        }
        let ledger = store.ledger.clone();
        let rows = self.searcher.search(&ledger, &query, usize::MAX);
        if rows.is_empty() {
            return;
        }
        crate::plot::open(query, ledger, rows, window, cx);
    }

    fn palette_query(&mut self, q: &str, cx: &mut Context<Self>) {
        let mut results = palette::filter(q, Page::ALL.into_iter().filter(|p| p.visible()), &self.theme);
        if let (Some(store), false) = (&self.store, q.trim().is_empty()) {
            results.hits = self.searcher.search(&store.ledger, q, PALETTE_HITS).into_iter().map(Entry::Hit).collect();
        }
        self.palette_results = results;
        // a new query moves the highlight to the first row, and gpui-component's
        // on_select for that fires against the previous rows: settle it here
        match self.palette_results.groups().iter().find_map(|g| g.first()).cloned() {
            Some(Entry::Theme(t)) => self.palette_preview(t.choice, cx),
            _ => self.palette_restore(cx),
        }
        cx.notify();
    }

    /// Show a theme without saving it, until [`Self::palette_restore`].
    fn palette_preview(&mut self, choice: palette::Choice, cx: &mut Context<Self>) {
        let mut cfg = self.theme.clone();
        choice.apply_to(&mut cfg);
        crate::theme::apply(&cfg, cx);
        self.palette_previewing = true;
        cx.notify();
    }

    /// Back to the saved theme after a preview.
    fn palette_restore(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.palette_previewing) {
            crate::theme::apply(&self.theme, cx);
            cx.notify();
        }
    }

    fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        // dev/screenshot aid: the first open may start from $FINANCE_DESKTOP_PALETTE
        static SEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let initial = if SEEDED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            String::new()
        } else {
            std::env::var("FINANCE_DESKTOP_PALETTE").unwrap_or_default()
        };
        self.palette_restore(cx);
        self.palette_query(&initial, cx);
        let state = self.palette.clone();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let Some(app) = view.upgrade() else { return dialog };
            let app = app.read(cx);
            let ledger = app.store.as_ref().map(|s| s.ledger.clone());
            let results = app.palette_results.clone();
            let [pages, acts, themes, hits] = results.groups();

            let go = CommandGroup::new().label("Go to").items(pages.iter().filter_map(|e| match e {
                Entry::Page(p) => Some(CommandItem::new().label(p.label()).icon(p.icon())),
                _ => None,
            }));
            let actions = CommandGroup::new().label("Actions").items(acts.iter().filter_map(|e| match e {
                Entry::Action(a) => {
                    let (_, label, _) = palette::ACTIONS.iter().find(|(x, ..)| x == a)?;
                    Some(CommandItem::new().label(*label).icon(if *a == Action::SyncNow { IconName::RefreshCw } else { IconName::Sun }))
                }
                _ => None,
            }));
            let themes = CommandGroup::new().label("Themes").items(themes.iter().filter_map(|e| match e {
                Entry::Theme(t) => Some(CommandItem::new().label(t.label.clone()).icon(match (t.current, t.choice) {
                    (true, _) => IconName::Check,
                    (_, Choice::Mode(true)) => IconName::Moon,
                    (_, Choice::Mode(false)) => IconName::Sun,
                    _ => IconName::Settings,
                })),
                _ => None,
            }));
            let txns = CommandGroup::new().label("Transactions").items(hits.iter().filter_map(|e| {
                let Entry::Hit(i) = e else { return None };
                let e = ledger.as_ref()?.entries.get(*i)?.clone();
                Some(CommandItem::new().label(e.description.clone()).child(move |_, cx| {
                    let t = cx.theme();
                    let hkd = e.hkd.unwrap_or(e.amount);
                    h_flex()
                        .w_full()
                        .gap_3()
                        .child(div().w(px(84.)).text_xs().text_color(t.muted_foreground).child(e.date.clone()))
                        .when_some(crate::logo::tile(crate::logo::for_entry(&e), 20., cx), |d, l| d.child(l))
                        .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.description.clone()))
                        .child(crate::category::label(&e.category, cx).text_xs())
                        .child(div().w(px(110.)).text_sm().text_right().text_color(fmt::signed_color(hkd)).child(fmt::signed_hkd(hkd)))
                }))
            }));

            let (v_query, v_select, v_confirm, v_cancel, v_plot) = (view.clone(), view.clone(), view.clone(), view.clone(), view.clone());
            // the handler lives on the dialog's own element: it gets the window
            // in-dispatch (an app-level handler can't borrow it then)
            dialog.p_0().w(px(720.)).close_button(false).child(div().on_action(move |_: &PlotQuery, window, cx| {
                let _ = v_plot.update(cx, |this, cx| {
                    let q = this.palette.read(cx).query(cx).to_string();
                    window.close_dialog(cx);
                    this.open_plot(q, window, cx);
                });
            }).child(
                Command::new(&state)
                    .bordered(false)
                    .filterable(false)
                    .placeholder("Search transactions, pages, actions, themes…")
                    .max_h(px(520.))
                    .group(go)
                    .group(actions)
                    .group(themes)
                    .group(txns)
                    .on_query(move |q, window, cx| {
                        let _ = v_query.update(cx, |this, cx| this.palette_query(q, cx));
                        window.refresh();
                    })
                    .on_select(move |ix, _, cx| {
                        let _ = v_select.update(cx, |this, cx| match this.palette_results.get(ix.section, ix.row).cloned() {
                            Some(Entry::Theme(t)) => this.palette_preview(t.choice, cx),
                            _ => this.palette_restore(cx),
                        });
                    })
                    .on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        let _ = v_confirm.update(cx, |this, cx| {
                            this.palette_restore(cx);
                            let Some(entry) = this.palette_results.get(ix.section, ix.row).cloned() else { return };
                            match entry {
                                Entry::Page(p) => this.go(p, window, cx),
                                Entry::Action(Action::SyncNow) => this.sync_now(&SyncNow, cx),
                                Entry::Action(Action::RefreshLive) => this.refresh_live(&RefreshLive, cx),
                                Entry::Action(Action::ToggleMode) => this.toggle_mode(&ToggleMode, window, cx),
                                Entry::Theme(t) => this.update_theme(|c| t.choice.apply_to(c), cx),
                                Entry::Hit(i) => {
                                    let desc = this.store.as_ref().and_then(|s| s.ledger.entries.get(i).map(|e| e.description.clone()));
                                    if let Some(d) = desc {
                                        this.show_in_table(d, window, cx);
                                    }
                                }
                            }
                        });
                    })
                    .on_cancel(move |window, cx| {
                        let _ = v_cancel.update(cx, |this, cx| this.palette_restore(cx));
                        window.close_dialog(cx);
                    })
                    .when(ui::show_shortcuts(), |d| d.footer(|_, _, cx| {
                        h_flex().px_3().py_2().gap_2().items_center().border_t_1().border_color(ui::hairline(cx))
                            .child(ui::kbd("ctrl-enter", cx))
                            .child(ui::caption("plot every match", cx))
                    })),
            ))
        });
        self.palette.update(cx, |s, cx| {
            s.set_query(initial, window, cx);
            s.focus(window, cx);
        });
    }

    // ---------- rendering ----------

    /// (dot color, short label, full detail, in progress) for the sync indicator.
    fn sync_status(&self, cx: &App) -> (Hsla, String, String, bool) {
        let t = cx.theme();
        if demo::active() {
            return (t.warning, finance_core::demo::LABEL.into(), finance_core::demo::NOTE.into(), false);
        }
        match &self.sync {
            SyncState::Running if self.refreshing => (t.primary, "Refreshing live data…".into(), format!("{} is fetching the exchanges, broker and prices", cache::server_url()), true),
            SyncState::Running => (t.primary, "Syncing…".into(), format!("Pulling from {}", cache::server_url()), true),
            SyncState::Failed(e) => (fmt::money_out(), "Sync failed".into(), e.clone(), false),
            SyncState::Idle if self.live_note.as_ref().is_some_and(|(at, ..)| at.elapsed() < LIVE_NOTE_FOR) => {
                let (_, text, problems) = self.live_note.clone().unwrap_or((Instant::now(), String::new(), String::new()));
                let c = if problems.is_empty() { fmt::money_in() } else { t.warning };
                let tip = if ui::show_shortcuts() { "Sync now (Ctrl+R), refresh live (Ctrl+Shift+K)" } else { "Sync now" };
                (c, text, if problems.is_empty() { tip.into() } else { problems }, false)
            }
            SyncState::Idle => match self.store.as_ref().and_then(|s| s.bundle.generated_at()) {
                Some(ms) => {
                    let age = (chrono::Utc::now().timestamp_millis() - ms).max(0) / 1000;
                    let c = if age <= 2 * SYNC_EVERY.as_secs() as i64 { fmt::money_in() } else { t.warning };
                    (c, format!("Synced {}", fmt::ago(ms)), ui::keyed("Sync now", "Ctrl+R").into(), false)
                }
                None => (t.muted_foreground, "Never synced".into(), ui::keyed("Sync now", "Ctrl+R").into(), false),
            },
        }
    }

    /// The left sidebar: full (200 px: labels, key hints, the sync line), or
    /// collapsed to an icon rail (Ctrl+B, saved in theme.toml): icons with
    /// tooltips, the sync state as a dot.
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let rail = self.theme.sidebar_collapsed;
        let pad = if rail { 8. } else { 10. };
        let item = |page: Page, cx: &mut Context<Self>| {
            let active = self.page == page;
            let ix = Page::ALL.iter().position(|p| *p == page).unwrap_or(0);
            let (hover_bg, hover_fg) = (t.sidebar_accent.opacity(0.55), t.sidebar_foreground);
            // no key hints without a keyboard (ui::show_shortcuts)
            let key = if ui::show_shortcuts() { page.key() } else { String::new() };
            h_flex()
                .id(("nav", ix))
                .relative()
                .h(px(if rail { 36. } else { 32. }))
                .px(px(10.))
                .gap(px(10.))
                .when(rail, |d| d.justify_center().px_0())
                .rounded(t.radius)
                .cursor_pointer()
                .text_size(px(13.5))
                .text_color(if active { t.sidebar_foreground } else { t.muted_foreground })
                .when(active, |d| d.bg(t.sidebar_accent).font_weight(FontWeight::MEDIUM))
                .when(!active, |d| d.hover(move |s| s.bg(hover_bg).text_color(hover_fg)))
                .when(active, |d| {
                    d.child(div().absolute().left(px(-pad)).top(px(if rail { 10. } else { 8. })).w(px(3.)).h(px(16.)).rounded_full().bg(t.primary))
                })
                .child(page.icon().small().text_color(if active { t.primary } else { t.muted_foreground }))
                .when(!rail, |d| d
                    .child(div().flex_1().whitespace_nowrap().child(page.label()))
                    .child(div().text_size(px(10.5)).text_color(t.muted_foreground.opacity(0.6)).child(key.clone())))
                .when(rail, |d| {
                    let tip = if key.is_empty() { page.label().to_string() } else { format!("{}  ·  Ctrl+{key}", page.label()) };
                    d.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.go(page, window, cx)))
        };
        let group = |label: &'static str| {
            if rail {
                // the groups as hairlines between icons
                return div().mx(px(10.)).my(px(8.)).h(px(1.)).bg(t.sidebar_border);
            }
            div()
                .px(px(10.))
                .pt(px(14.))
                .pb(px(4.))
                .text_size(px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.muted_foreground.opacity(0.8))
                .child(label.to_uppercase())
        };
        let entries = self.store.as_ref().map(|s| s.ledger.entries.len());
        let (dot_c, label, detail, running) = self.sync_status(cx);
        // the owner's avatar in the brand tile's place (the "F" until a profile is known)
        let logo = crate::avatar::owner("brand-avatar", 26., 7., cx);
        // the signed-in user's name, over the server's short host
        let (title, who) = self.brand_lines(cx);
        // full width with the nav rows' padding: the menu button centres its
        // content, so a narrower row would drift with the name's length; this
        // way the avatar lines up with the group labels and nav icons below
        let brand = h_flex()
            .when(!rail, |d| d.w_full())
            .gap(px(10.))
            .px(px(if rail { 0. } else { 10. }))
            .when(rail, |d| d.justify_center())
            .pb(px(6.))
            .child(logo)
            .when(!rail, |d| d.child(
                v_flex()
                    .min_w_0()
                    .child(div().text_size(px(14.)).font_semibold().line_height(px(17.)).truncate().child(title))
                    .child(div().text_size(px(11.)).text_color(t.muted_foreground).line_height(px(14.)).truncate().child(who)),
            ));
        // the avatar menu: switch accounts, add one, sign out
        let brand = self.account_menu(brand.into_any_element(), rail, cx);
        let sync_dot = if running { ui::pulse_dot("side-sync", dot_c, 7.).into_any_element() } else { ui::dot(dot_c, 7.).into_any_element() };
        let hover_bg = t.sidebar_accent.opacity(0.55);
        let chevron = h_flex()
            .id("sidebar-toggle")
            .h(px(28.))
            .when(rail, |d| d.justify_center())
            .when(!rail, |d| d.justify_end().px(px(4.)))
            .child(
                h_flex()
                    .id("sidebar-chevron")
                    .size(px(28.))
                    .justify_center()
                    .rounded(t.radius)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .child(Icon::new(if rail { IconName::ChevronRight } else { IconName::ChevronLeft }).small().text_color(t.muted_foreground))
                    .tooltip(move |w, cx| Tooltip::new(ui::keyed(if rail { "Expand the sidebar" } else { "Collapse the sidebar" }, "Ctrl+B")).build(w, cx))
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_sidebar(&ToggleSidebar, w, cx))),
            );
        let footer = v_flex()
            .gap(px(8.))
            .px(px(if rail { 0. } else { 10. }))
            .pt(px(12.))
            .border_t_1()
            .border_color(ui::hairline(cx))
            .text_size(px(11.5))
            .text_color(t.muted_foreground)
            .when(rail, |d| d.items_center().child(
                h_flex()
                    .id("side-sync-rail")
                    .size(px(28.))
                    .justify_center()
                    .rounded(t.radius)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .child(sync_dot)
                    .tooltip(move |w, cx| Tooltip::new(format!("{label} · {detail}")).build(w, cx))
                    .on_click(cx.listener(|this, _, _, cx| this.sync_now(&SyncNow, cx))),
            ))
            .when(!rail, |d| d
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(if running { ui::pulse_dot("side-sync", dot_c, 7.).into_any_element() } else { ui::dot(dot_c, 7.).into_any_element() })
                        .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(self.sync_status(cx).1)),
                )
                .child(
                    div().figures().whitespace_nowrap().overflow_hidden().text_ellipsis().child(match entries {
                        Some(n) => format!("{} entries on this machine", fmt::count(n as f64)),
                        None if self.load_error.is_some() => "no local copy yet".into(),
                        None => "loading the local copy…".into(),
                    }),
                )
                .when(ui::show_shortcuts(), |d| d.child(
                    h_flex().gap(px(10.)).flex_wrap()
                        .child(h_flex().gap_1().child(ui::kbd("ctrl-k", cx)).child("search"))
                        .child(h_flex().gap_1().child(ui::kbd("ctrl-r", cx)).child("sync"))
                        .child(h_flex().gap_1().child(ui::kbd("ctrl-shift-k", cx)).child("live")),
                )))
            .child(chevron);
        v_flex()
            .w(px(if rail { 56. } else { 200. }))
            .flex_shrink_0()
            .h_full()
            .px(px(pad))
            .pt(px(16.))
            .pb(px(14.))
            .bg(t.sidebar)
            .border_r_1()
            .border_color(t.sidebar_border)
            .child(brand)
            .child(
                v_flex()
                    .id("nav")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .gap(px(2.))
                    .child(group("Money"))
                    .children(Page::Overview.visible().then(|| item(Page::Overview, cx)))
                    .children(Page::Spending.visible().then(|| item(Page::Spending, cx)))
                    .children(Page::Budgets.visible().then(|| item(Page::Budgets, cx)))
                    .children(Page::Travel.visible().then(|| item(Page::Travel, cx)))
                    .children(Page::Transactions.visible().then(|| item(Page::Transactions, cx)))
                    .children(Page::Review.visible().then(|| item(Page::Review, cx)))
                    .children(Page::Subscriptions.visible().then(|| item(Page::Subscriptions, cx)))
                    .children(Page::CashFlow.visible().then(|| item(Page::CashFlow, cx)))
                    .child(group("Wealth"))
                    .children(Page::Investments.visible().then(|| item(Page::Investments, cx)))
                    .children(Page::Banks.visible().then(|| item(Page::Banks, cx)))
                    .children(Page::Crypto.visible().then(|| item(Page::Crypto, cx)))
                    .children(Page::Broker.visible().then(|| item(Page::Broker, cx)))
                    .child(group("App"))
                    .children(Page::Upload.visible().then(|| item(Page::Upload, cx)))
                    .children(Page::Categories.visible().then(|| item(Page::Categories, cx)))
                    .children(Page::Settings.visible().then(|| item(Page::Settings, cx)))
                    .children(Page::Doctor.visible().then(|| item(Page::Doctor, cx))),
            )
            .child(footer)
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        // compact (phone): title, then icon-only search and sync, touch-sized
        let compact = crate::layout::compact();
        let ctl_h = if compact { 40. } else { 32. };
        let (dot_c, label, detail, running) = self.sync_status(cx);
        let subtitle = match self.page {
            Page::Overview => "Where everything stands",
            Page::Spending => "What came in, where it went",
            Page::Budgets => "This month against a budget per category",
            Page::Review => "A calendar year, told from the ledger",
            Page::Travel => "Trips found in your spending, and what they cost",
            Page::Transactions => "Every entry, every account",
            Page::Investments => fmt::local_static("Measured against the S&P 500"),
            Page::Banks => "Balances across banks and currencies",
            Page::Crypto => "Exchanges and cold storage",
            Page::Broker => "Interactive Brokers positions and trades",
            Page::Upload => "Send new exports to the server",
            Page::Settings => "Theme, server and the local copy",
            Page::Subscriptions => "Every recurring charge, what it costs a year, what to look at",
            Page::CashFlow => "What's expected in and out, day by day",
            Page::Doctor => "Is everything healthy?",
            Page::Categories => "What each category means, and the rules that fill them",
        };
        let hover_bg = t.accent;
        let search = h_flex()
            .id("search-affordance")
            .flex_shrink(1.)
            .min_w(px(if compact { ctl_h } else { 110. }))
            .w(px(if compact { ctl_h } else { 200. }))
            .h(px(ctl_h))
            .px(px(10.))
            .gap_2()
            .rounded(t.radius)
            .border_1()
            .border_color(ui::hairline(cx))
            .bg(t.secondary)
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(t.muted_foreground)
            .hover(move |s| s.bg(hover_bg))
            .child(Icon::new(IconName::Search).small())
            .when(!compact, |d| d
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child("Search…"))
                .child(ui::kbd("ctrl-k", cx)))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenPalette), cx));
        // fixed width: idle / syncing / failed never shift the header
        let sync = h_flex()
            .id("sync-indicator")
            .flex_shrink_0()
            .w(px(if compact { ctl_h } else { 178. }))
            .h(px(ctl_h))
            .px(px(if compact { 0. } else { 10. }))
            .when(compact, |d| d.justify_center())
            .gap_2()
            .rounded(t.radius)
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(if matches!(self.sync, SyncState::Failed(_)) { fmt::money_out() } else { t.muted_foreground })
            .hover(move |s| s.bg(hover_bg))
            .child(if running { ui::pulse_dot("hdr-sync", dot_c, 7.).into_any_element() } else { ui::dot(dot_c, 7.).into_any_element() })
            .when(!compact, |d| d
                .child(div().flex_1().whitespace_nowrap().overflow_hidden().text_ellipsis().child(label))
                .child(Icon::new(IconName::RefreshCw).xsmall().text_color(t.muted_foreground.opacity(0.7))))
            .tooltip(move |w, cx| Tooltip::new(detail.clone()).build(w, cx))
            .on_click(cx.listener(|this, _, _, cx| this.sync_now(&SyncNow, cx)));
        h_flex()
            .gap_4()
            .justify_between()
            .flex_wrap()
            .gap_y_2()
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(if compact { 120. } else { 160. }))
                    .gap(px(2.))
                    .child(ui::page_title(self.page.label(), cx))
                    .when(!compact, |d| d.child(ui::caption(subtitle, cx))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(search)
                    // the demo: no sync to show, its pill instead
                    .map(|d| if demo::active() { d.child(self.demo_pill(compact, cx)) } else { d.child(sync) })
                    .child(Button::new("privacy").when(!compact, |b| b.small()).ghost()
                        .icon(if crate::fmt::private() { IconName::EyeOff } else { IconName::Eye })
                        .tooltip(ui::keyed(if crate::fmt::private() { "Show amounts" } else { "Hide amounts" }, "Ctrl+H"))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(TogglePrivacy), cx)))
                    .child(Button::new("mode").when(!compact, |b| b.small()).ghost()
                        .icon(if t.is_dark() { IconName::Sun } else { IconName::Moon })
                        .tooltip(ui::keyed("Dark / light", "Ctrl+T"))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleMode), cx)))
                    // phone: no sidebar, so the avatar sits top right; it opens Settings
                    .when(compact, |d| d.child(
                        crate::avatar::owner("hdr-avatar", 30., 8., cx)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.settings_tab = pages::settings::Tab::Account;
                                this.go(Page::Settings, w, cx)
                            })))),
            )
    }

    /// The current page's content: one element, or sections the render
    /// draws as a virtualized list (only what's on screen is laid out).
    fn page_body(&mut self, cx: &mut Context<Self>) -> Body {
        // a locked encrypted copy: the unlock screen, before anything else
        if finance_core::replica::locked() {
            return Body::One(self.acct.vault.clone().into_any_element());
        }
        if self.store.is_some() {
            self.first_load.set(None); // a later empty state gets the delay again
        }
        match (&self.store, self.page) {
            (Some(_), p) if !p.visible() => Body::One(self.render_not_set_up(p, cx)),
            (_, Page::Transactions) => Body::One(self.render_transactions(cx).into_any_element()),
            (_, Page::Upload) => Body::One(self.upload.clone().into_any_element()),
            (_, Page::Doctor) if demo::active() => Body::One(self.render_demo_only("The Doctor", cx)),
            (_, Page::Doctor) => Body::One(self.doctor.clone().into_any_element()),
            (Some(_), Page::Budgets) => Body::One(self.budgets.clone().into_any_element()),
            (Some(_), Page::Review) => Body::One(self.review.clone().into_any_element()),
            (Some(_), Page::Categories) => Body::Sections(self.cats.update(cx, |v, cx| v.sections(cx))),
            (None, _) => Body::One(self.render_no_data(cx)),
            (Some(s), Page::Overview) => {
                let view = cx.entity().downgrade();
                let on_growth: pages::projection::OnGrowth = std::rc::Rc::new(move |g, _, cx| {
                    let _ = view.update(cx, |this, cx| { this.growth = g; cx.notify(); });
                });
                let next30 = self.cashflow.read(cx).outlook();
                Body::Sections(pages::overview::render(&s.bundle, &s.ledger, self.growth, on_growth, next30, cx))
            }
            (Some(s), Page::Spending) => {
                let filtering = pages::spending::Filtering {
                    query: &self.filters.spend,
                    data: crate::filterbar::Data { ledger: s.ledger.clone(), trips: s.trip_index.clone() },
                    on_query: self.on_spend_filter(cx),
                    on_categorize: self.categorize_button("spend-categorize", cx).is_some().then(|| self.on_categorize(cx)),
                };
                let view = cx.entity().downgrade();
                let on_tab: pages::spending::OnTab = Rc::new(move |tab, _, cx| {
                    let _ = view.update(cx, |this, cx| { this.spend_tab = tab; cx.notify(); });
                });
                Body::Sections(pages::spending::render(&s.bundle, &s.ledger, self.spend_month.as_deref(), self.spend_tab, on_tab, filtering, self.on_pick(cx),
                    self.on_search(cx), self.on_dive(cx), &self.home_whatif, cx)
                    )
            }
            (Some(s), Page::Travel) => {
                let view = cx.entity().downgrade();
                let on_toggle: pages::travel::OnToggle = Rc::new(move |i, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.trip_open = if this.trip_open == Some(i) { None } else { Some(i) };
                        cx.notify();
                    });
                });
                let view = cx.entity().downgrade();
                let on_pick: pages::spending::OnPick = Rc::new(move |ix, window, cx| {
                    let _ = view.update(cx, |this, cx| this.open_detail(ix, window, cx));
                });
                Body::Sections(pages::travel::render(&s.trips, &s.ledger, self.trip_open, self.filters.travel_year, self.on_travel_year(cx),
                    on_toggle, on_pick, self.on_search(cx), cx))
            }
            (Some(s), Page::Investments) => {
                let view = cx.entity().downgrade();
                let on_toggle: pages::investments::OnToggle = std::rc::Rc::new(move |_, cx| {
                    let _ = view.update(cx, |this, cx| { this.inv_pct = !this.inv_pct; cx.notify(); });
                });
                let view = cx.entity().downgrade();
                let on_deep: pages::investments::OnDeep = std::rc::Rc::new(move |section, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.deep.update(cx, |d, cx| d.open(section, cx));
                        this.page = Page::Broker;
                        this.broker_tab = pages::broker::Tab::DeepDive;
                        cx.notify();
                    });
                });
                let view = cx.entity().downgrade();
                let on_go: crate::layout::OnGo = Rc::new(move |page, window, cx| {
                    let _ = view.update(cx, |this, cx| this.go(page, window, cx));
                });
                let view = cx.entity().downgrade();
                let on_tab: pages::investments::OnTab = Rc::new(move |tab, _, cx| {
                    let _ = view.update(cx, |this, cx| { this.inv_tab = tab; cx.notify(); });
                });
                Body::Sections(pages::investments::render(&s.bundle, &s.ledger, self.inv_tab, self.inv_pct, on_tab, on_toggle, on_deep, on_go, cx))
            }
            (Some(s), Page::Banks) => Body::Sections(pages::banks::render(&s.bundle, cx)),
            (Some(s), Page::Crypto) => {
                let view = cx.entity().downgrade();
                let on_event: pages::crypto::OnEvent = Rc::new(move |ev, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        match ev {
                            pages::crypto::Event::Tab(tab) => this.crypto_tab = tab,
                            pages::crypto::Event::Scope(sc) => this.dca_scope = sc,
                        }
                        cx.notify();
                    });
                });
                Body::Sections(pages::crypto::render(&s.bundle, &s.ledger, self.crypto_tab, self.dca_scope, on_event, cx))
            }
            (Some(s), Page::Broker) => {
                let view = cx.entity().downgrade();
                let on_event: pages::broker::OnEvent = std::rc::Rc::new(move |ev, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        match ev {
                            pages::broker::Event::Tab(tab) => this.broker_tab = tab,
                            pages::broker::Event::Symbol(sym) => this.broker_symbol = Some(sym),
                        }
                        cx.notify();
                    });
                });
                let deep = (self.broker_tab == pages::broker::Tab::DeepDive).then(|| self.deep.clone().into_any_element());
                Body::Sections(pages::broker::render(&s.bundle, &s.ledger, self.broker_tab, self.broker_symbol.as_deref(), on_event, deep, cx))
            }
            (_, Page::Settings) => {
                let tab = self.settings_tab;
                self.acct.account.update(cx, |a, _| a.tab = tab);
                let account = self.acct.account.clone().into_any_element();
                Body::One(pages::settings::render(self.settings_model(cx), account, cx).into_any_element())
            }
            (Some(s), Page::Subscriptions) => {
                let view = cx.entity().downgrade();
                let on_sort: pages::subscriptions::OnSort = Rc::new(move |sort, _, cx| {
                    let _ = view.update(cx, |this, cx| { this.subs_sort = sort; cx.notify(); });
                });
                let view = cx.entity().downgrade();
                let on_pick: pages::spending::OnPick = Rc::new(move |ix, window, cx| {
                    let _ = view.update(cx, |this, cx| this.open_detail(ix, window, cx));
                });
                Body::Sections(pages::subscriptions::render(&s.ledger, &s.recurring, self.subs_sort, on_sort, &self.filters.subs,
                    self.on_subs_view(cx), on_pick, cx))
            }
            (Some(_), Page::CashFlow) => Body::One(self.cashflow.clone().into_any_element()),
        }
    }

    /// A listed page: its sections in a virtualized list, so a scroll frame
    /// lays out only the ones on screen (plus 600 px of overdraw). The list
    /// asks for a section by index, possibly more than once a frame; the
    /// sections built in this render are handed out first, then the page is
    /// built again for any asked twice.
    fn section_list(&mut self, sections: ui::Sections, gutter: f32, cx: &mut Context<Self>) -> AnyElement {
        let (gap, items) = sections.into_parts();
        let n = items.len();
        // a tab switch starts at the top too
        let tab = match self.page {
            Page::Investments => format!("{:?}", self.inv_tab),
            Page::Broker => format!("{:?}", self.broker_tab),
            Page::Crypto => format!("{:?}", self.crypto_tab),
            Page::Categories => format!("{:?}", self.cats.read(cx).tab()),
            _ => String::new(),
        };
        let key = (self.page, tab, self.data_gen, n);
        if self.page_list_key.as_ref() != Some(&key) {
            self.page_list.reset(n);
            self.page_list_key = Some(key);
        }
        let built: Rc<std::cell::RefCell<Vec<Option<AnyElement>>>> = Rc::new(std::cell::RefCell::new(items.into_iter().map(Some).collect()));
        let view = cx.entity().downgrade();
        list(self.page_list.clone(), move |ix, _window, cx| {
            let mut taken = built.borrow_mut().get_mut(ix).and_then(Option::take);
            if taken.is_none() {
                let fresh = view.update(cx, |this, cx| match this.page_body(cx) {
                    Body::Sections(s) => Some(s.into_parts().1),
                    Body::One(_) => None,
                }).ok().flatten();
                if let Some(fresh) = fresh {
                    let mut b = built.borrow_mut();
                    *b = fresh.into_iter().map(Some).collect();
                    taken = b.get_mut(ix).and_then(Option::take);
                }
            }
            let last = ix + 1 == n;
            div().w_full().px(px(gutter))
                .child(div().w_full().max_w(px(MAX_CONTENT)).mx_auto().pb(px(if last { 28. } else { gap }))
                    .children(taken))
                .into_any_element()
        })
        .size_full()
        .into_any_element()
    }

    /// A page the server has nothing for, reached by its key: what to add.
    fn render_not_set_up(&self, page: Page, cx: &mut Context<Self>) -> AnyElement {
        let (needs, upload) = page.needs();
        let (label, to) = if upload { ("Open Upload", Page::Upload) } else { ("Open Doctor", Page::Doctor) };
        ui::empty_with(IconName::Inbox, &format!("{} isn't set up", page.label()), needs, cx)
            .child(div().mt_2().child(Button::new("not-set-up").small().primary().label(label)
                .on_click(cx.listener(move |this, _, window, cx| this.go(to, window, cx)))))
            .into_any_element()
    }

    /// First load / no data: a skeleton of the page, or a clear way out.
    fn render_no_data(&self, cx: &mut Context<Self>) -> AnyElement {
        let failed = matches!(self.sync, SyncState::Failed(_));
        let running = matches!(self.sync, SyncState::Running);
        if (self.load_error.is_none() && !failed) || running {
            let note = running.then(|| {
                h_flex().gap_2().px(px(16.)).py(px(8.)).rounded(px(999.)).text_size(px(12.))
                    .bg(cx.theme().background.opacity(0.92)).border_1().border_color(cx.theme().border)
                    .text_color(cx.theme().foreground)
                    .child(ui::braille_spinner("first-sync", cx.theme().primary, 13.))
                    .child("Fetching your data from the server…")
            });
            // blank for the first moments: a fast load never flashes it
            let started = self.first_load.get().unwrap_or_else(|| {
                let now = Instant::now();
                self.first_load.set(Some(now));
                now
            });
            // the dot watermark fills the page under the header, the note centred over it
            return div().relative().w_full().h(self.viewport_h.get() - gpui_kit::px(120.))
                .child(div().absolute().size_full().child(ui::dot_field(started, cx)))
                .child(div().absolute().size_full().flex().items_center().justify_center().children(note))
                .into_any_element();
        }
        if finance_core::signin::needed() {
            return ui::empty_with(IconName::User, "Sign in to see your data", "The server wants to know this device first.", cx)
                .child(div().mt_2().child(Button::new("no-data-signin").small().primary().label("Sign in")
                    .on_click(cx.listener(|this, _, window, cx| this.open_sign_in(window, cx)))))
                .into_any_element();
        }
        let (icon, title, detail) = match &self.sync {
            SyncState::Failed(e) => (IconName::TriangleAlert, "Couldn't reach the server".to_string(),
                format!("No local copy yet, and the sync failed: {}", e.chars().take(160).collect::<String>())),
            _ => (IconName::Inbox, "No local data yet".to_string(),
                format!("Sync once from {} and everything works offline afterwards.", cache::server_url())),
        };
        ui::empty_with(icon, &title, &detail, cx)
            .child(div().mt_2().child(Button::new("first-sync").small().primary().icon(IconName::RefreshCw).label("Sync now")
                .on_click(cx.listener(|this, _, _, cx| this.sync_now(&SyncNow, cx)))))
            .when(ui::show_shortcuts(), |d| d.child(h_flex().gap_1().text_size(px(11.)).child("or press").child(ui::kbd("ctrl-r", cx))))
            .child(h_flex().mt_2().gap_2()
                .child(Button::new("first-setup").small().outline().label("Set up a server")
                    .on_click(cx.listener(|this, _, window, cx| this.open_onboarding(true, window, cx))))
                // look around first: made-up data, nothing sent anywhere
                .child(Button::new("first-demo").small().ghost().label("Just try it")
                    .tooltip("Explore with made-up data. Nothing is sent anywhere.")
                    .on_click(|_, _, cx| demo::start(cx, |e, _| eprintln!("anser: {e}")))))
            .into_any_element()
    }

    fn render_transactions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (n, dt) = self.search_stats;
        // phone: a list of two-line rows instead of the six-column table
        let compact = self.store.as_ref().filter(|_| crate::layout::compact()).map(|s| {
            let view = cx.entity().downgrade();
            let on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)> = Rc::new(move |ix, window, cx| {
                let _ = view.update(cx, |this, cx| this.open_detail(ix, window, cx));
            });
            pages::transactions::compact_list(s.ledger.clone(), self.table.read(cx).delegate().rows.clone(), on_pick)
        });
        let filters = self.render_filters(cx);
        let categorize = self.categorize_button("txn-categorize", cx);
        let t = cx.theme();
        v_flex()
            .size_full()
            .gap_3()
            .child(Input::new(&self.search).cleanable(true).prefix(IconName::Search))
            .children(filters)
            .child(h_flex().justify_between().items_center().gap_2().child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap()
                .text_xs().text_color(t.muted_foreground).figures()
                .child(if compact.is_some() {
                    format!("{} results · tap a row for details", fmt::count(n as f64))
                } else {
                    format!("{} results in {:.1} ms · Enter or double-click a row for details · right-click for its merchant, category, or to change it", fmt::count(n as f64), dt.as_secs_f64() * 1000.)
                }))
                .children(categorize))
            .child(div().flex_1().min_h_0().map(|d| match compact {
                Some(list) => d.child(list),
                None => d.child(DataTable::new(&self.table).stripe(true).small()),
            }))
    }
}

impl Focusable for FinanceApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FinanceApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // the palette closed some other way (overlay click, another dialog): drop its preview
        if self.palette_previewing && !window.has_active_dialog(cx) {
            self.palette_restore(cx);
        }
        crate::timing::mark(if self.store.is_some() { "render (data)" } else { "render (empty)" });
        // the responsive breakpoint, decided once per frame for every page (layout.rs)
        let compact = crate::layout::update(window);
        let gutter = crate::layout::gutter();
        // the wizard owns the window: no sidebar, header, tab bar or banner
        // (the unlock screen of a locked local copy still comes first)
        if !finance_core::replica::locked() {
            if let Some(o) = self.onboarding_el() {
                return div().size_full().bg(cx.theme().background).child(o).into_any_element();
            }
        }
        if self.detail_on_load && self.store.is_some() {
            self.detail_on_load = false;
            cx.defer_in(window, |this, window, cx| this.open_detail_row(0, window, cx));
        }
        // the dive's dialog was closed (Esc, outside click): not there anymore
        if self.dive_open.is_some() && !window.has_active_dialog(cx) {
            self.dive_open = None;
        }
        let here = self.spot(cx);
        self.jumps.record(here, Spot::same_place);
        self.viewport_h.set(window.viewport_size().height);
        let built = Instant::now();
        let (body, listed) = match self.page_body(cx) {
            Body::One(e) => (e, false),
            Body::Sections(sections) => (self.section_list(sections, gutter, cx), true),
        };
        crate::timing::render_sample(self.page.label(), built.elapsed());
        if crate::timing::render_mode() == crate::timing::Render::Spin && self.store.is_some() {
            // build the page 60 more times: a scroll's worth of frames, without
            // depending on the compositor handing out frames
            for _ in 0..60 {
                let t = Instant::now();
                drop(self.page_body(cx));
                crate::timing::render_sample(self.page.label(), t.elapsed());
            }
        }
        // a listed page scrolls itself (and pads its own sections)
        let scrolls = self.page != Page::Transactions && !listed;
        let body = ui::page_frame(body, !scrolls);
        h_flex()
            .id("app")
            .key_context("FinanceApp")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(|this, a: &SyncNow, _, cx| this.sync_now(a, cx)))
            .on_action(cx.listener(|this, a: &RefreshLive, _, cx| this.refresh_live(a, cx)))
            .on_action(cx.listener(Self::toggle_mode))
            .on_action(cx.listener(|this, _: &ZoomIn, w, cx| this.zoom_by(crate::zoom::STEP, w, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, w, cx| this.zoom_by(-crate::zoom::STEP, w, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, w, cx| this.zoom_by(0., w, cx)))
            .on_action(cx.listener(|this, _: &TogglePrivacy, _, cx| this.update_theme(|c| c.privacy = !c.privacy, cx)))
            .on_action(cx.listener(|this, _: &JumpBack, w, cx| this.jump(true, w, cx)))
            .on_action(cx.listener(|this, _: &JumpForward, w, cx| this.jump(false, w, cx)))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(|this, _: &AddAccount, w, cx| this.add_account(w, cx)))
            .on_action(cx.listener(|this, _: &SignOut, w, cx| this.sign_out(w, cx)))
            // Settings, Account, signed out: Ctrl+V pastes the pairing code with no
            // field focused (a focused field pastes by itself)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if this.page == Page::Settings && !this.acct.open
                    && (window.focused(cx).is_none() || this.focus.is_focused(window))
                    && this.acct.account.update(cx, |a, cx| a.paste_key(ev, window, cx))
                {
                    cx.stop_propagation();
                }
            }))
            // the mouse's back and forward buttons walk the jump list too
            .on_mouse_down(MouseButton::Navigate(NavigationDirection::Back), cx.listener(|this, _, w, cx| this.jump(true, w, cx)))
            .on_mouse_down(MouseButton::Navigate(NavigationDirection::Forward), cx.listener(|this, _, w, cx| this.jump(false, w, cx)))
            // Ctrl+scroll zooms: caught in the capture phase, before any
            // scroll area under the pointer takes the wheel
            .child({
                let view = cx.entity().downgrade();
                canvas(|_, _, _| (), move |_, _, window, _| {
                    let view = view.clone();
                    window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture || !ev.modifiers.control {
                            return;
                        }
                        let dy = match ev.delta {
                            ScrollDelta::Lines(d) => d.y,
                            ScrollDelta::Pixels(d) => f32::from(d.y),
                        };
                        if dy.abs() > f32::EPSILON {
                            cx.stop_propagation();
                            let step = if dy > 0. { crate::zoom::STEP } else { -crate::zoom::STEP };
                            let _ = view.update(cx, |this, cx| this.zoom_by(step, window, cx));
                        }
                    });
                }).absolute().size_0()
            })
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(|this, _: &Go1, w, cx| this.go(Page::NUMBERED[0], w, cx)))
            .on_action(cx.listener(|this, _: &Go2, w, cx| this.go(Page::NUMBERED[1], w, cx)))
            .on_action(cx.listener(|this, _: &Go3, w, cx| this.go(Page::NUMBERED[2], w, cx)))
            .on_action(cx.listener(|this, _: &Go4, w, cx| this.go(Page::NUMBERED[3], w, cx)))
            .on_action(cx.listener(|this, _: &Go5, w, cx| this.go(Page::NUMBERED[4], w, cx)))
            .on_action(cx.listener(|this, _: &Go6, w, cx| this.go(Page::NUMBERED[5], w, cx)))
            .on_action(cx.listener(|this, _: &Go7, w, cx| this.go(Page::NUMBERED[6], w, cx)))
            .on_action(cx.listener(|this, _: &Go8, w, cx| this.go(Page::NUMBERED[7], w, cx)))
            .on_action(cx.listener(|this, _: &Go9, w, cx| this.go(Page::NUMBERED[8], w, cx)))
            .on_action(cx.listener(|this, _: &Go0, w, cx| this.go(Page::Settings, w, cx)))
            .on_action(cx.listener(|this, _: &GoReview, w, cx| this.go(Page::Review, w, cx)))
            .on_action(cx.listener(|this, _: &GoUpload, w, cx| this.go(Page::Upload, w, cx)))
            .on_action(cx.listener(|this, _: &GoBudgets, w, cx| this.go(Page::Budgets, w, cx)))
            .on_action(cx.listener(|this, _: &GoDoctor, w, cx| this.go(Page::Doctor, w, cx)))
            .on_action(cx.listener(|this, _: &GoHomeSetup, w, cx| {
                this.settings_tab = pages::settings::Tab::Home;
                this.go(Page::Settings, w, cx)
            }))
            .on_action(cx.listener(|this, _: &GoSupported, w, cx| {
                this.settings_tab = pages::settings::Tab::Supported;
                this.go(Page::Settings, w, cx)
            }))
            // files dropped anywhere: show the Upload page and start on them
            .on_drop(cx.listener(|this, paths: &ExternalPaths, w, cx| {
                this.go(Page::Upload, w, cx);
                let paths = paths.paths().to_vec();
                this.upload.update(cx, |u, cx| u.add(paths, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenDetail, w, cx| {
                let row = this.table.read(cx).selected_row().unwrap_or(0);
                this.open_detail_row(row, w, cx);
            }))
            .when(self.page == Page::Spending, |d| {
                d.on_action(cx.listener(|this, _: &PrevMonth, _, cx| this.shift_month(-1, cx)))
                    .on_action(cx.listener(|this, _: &NextMonth, _, cx| this.shift_month(1, cx)))
            })
            // Year in review: ← → change the year
            .when(self.page == Page::Review, |d| {
                d.on_action(cx.listener(|this, _: &PrevMonth, _, cx| this.review.update(cx, |r, cx| r.shift_year(-1, cx))))
                    .on_action(cx.listener(|this, _: &NextMonth, _, cx| this.review.update(cx, |r, cx| r.shift_year(1, cx))))
            })
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(!compact, |d| d.child(self.render_sidebar(cx)))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .pt(px(if compact { 12. } else { 18. }) + crate::layout::safe_top())
                    .gap(px(if compact { 12. } else { 18. }))
                    .child(div().px(px(gutter)).child(div().w_full().max_w(px(MAX_CONTENT)).mx_auto().child(self.render_header(cx))))
                    .child(
                        div()
                            .id("page")
                            .flex_1()
                            .min_h_0()
                            .when(!listed, |d| d.px(px(gutter)))
                            // a sideways swipe over a wide table scrolls the table, not the page
                            .when(scrolls, |d| d.overflow_y_scroll().restrict_scroll_to_axis().pb(px(28.)))
                            .when(!scrolls && !listed, |d| d.pb_4())
                            .child(div().w_full().max_w(px(MAX_CONTENT)).mx_auto().when(!scrolls, |d| d.h_full()).child(body)),
                    )
                    // phone: the pages move from the sidebar to a bottom tab bar
                    .when(compact, |d| {
                        let view = cx.entity().downgrade();
                        let on_go: crate::layout::OnGo = Rc::new(move |page, window, cx| {
                            let _ = view.update(cx, |this, cx| this.go(page, window, cx));
                        });
                        d.child(crate::layout::tab_bar(self.page, on_go, cx))
                    }),
            )
            // last, so it paints over the page
            .child(self.downloads.clone())
            .into_any_element()
    }
}

// ---------- drill-down: month zoom, row detail ----------

impl FinanceApp {
    fn shift_month(&mut self, step: i32, cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return };
        let months = pages::spending::months(&store.bundle);
        let from = self.spend_month.clone().or_else(|| pages::spending::default_month(&store.bundle));
        if let Some(m) = from.and_then(|f| pages::spending::step_month(&months, &f, step)) {
            self.spend_month = Some(m);
            cx.notify();
        }
    }

    /// Callback for pages: close any sheet, show the transactions table with a search.
    fn on_search(&self, cx: &mut Context<Self>) -> Rc<dyn Fn(String, &mut Window, &mut App)> {
        Self::search_callback(cx)
    }

    fn search_callback(cx: &mut Context<Self>) -> Rc<dyn Fn(String, &mut Window, &mut App)> {
        let view = cx.entity().downgrade();
        Rc::new(move |q, window, cx| {
            window.close_sheet(cx);
            let _ = view.update(cx, |this, cx| this.show_in_table(q, window, cx));
        })
    }

    /// Detail for table row `row` (display order of the current search).
    fn open_detail_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.table.read(cx).delegate().entry_at(row) {
            self.open_detail(ix, window, cx);
        }
    }

    /// Side sheet with every field of ledger entry `ix` and its merchant's history.
    fn open_detail(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return };
        let ledger = store.ledger.clone();
        if ix >= ledger.entries.len() {
            return;
        }
        let detail = Rc::new(pages::transactions::Detail::new(&ledger, ix));
        let on_search = self.on_search(cx);
        let on_dive = self.on_dive(cx);
        let on_change = self.on_change_category(cx);
        let on_mark = self.on_mark_as(cx);
        window.open_sheet(cx, move |sheet, _, cx| {
            pages::transactions::detail_sheet(sheet, &ledger, &detail, on_search.clone(), on_dive.clone(), on_change.clone(), on_mark.clone(), cx)
        });
    }
}

// ---------- settings page hooks ----------

impl FinanceApp {
    fn settings_model(&self, cx: &App) -> pages::settings::Model<'_> {
        pages::settings::Model {
            tab: self.settings_tab,
            admin: self.acct.account.read(cx).admin(),
            theme: &self.theme,
            accent_input: &self.accent_input,
            server_input: &self.server_input,
            conn: &self.conn_test,
            server_in_use: cache::server_url(),
            server_env: std::env::var("FINANCE_SERVER").ok(),
            server_file: cache::server_file(),
            server_version: self.store.as_ref()
                .and_then(|s| finance_core::capabilities::Capabilities::of(&s.bundle).server_version().map(String::from)),
            note: self.settings_note.as_deref(),
            syncing: matches!(self.sync, SyncState::Running),
            sync_error: match &self.sync {
                SyncState::Failed(e) => Some(e.as_str()),
                _ => None,
            },
            synced_ms: self.store.as_ref().and_then(|s| s.bundle.generated_at()),
            cache_dir: cache::dir(),
            cache_bytes: cache::size_bytes(),
            downloads: &self.downloads,
            updates: pages::updates::settings_card(&self.updates, cx),
            vault: self.acct.vault.clone().into_any_element(),
            backups: self.acct.backups.clone().into_any_element(),
            assistants: self.acct.assistants.clone().into_any_element(),
            home: self.home_setup.clone().into_any_element(),
        }
    }

    /// Settings' tab bar: the AI assistants tab drops a new key's secret when
    /// left and asks the server again when opened.
    pub(crate) fn open_settings_tab(&mut self, tab: pages::settings::Tab, cx: &mut Context<Self>) {
        use pages::settings::Tab;
        if self.settings_tab == Tab::Assistants {
            self.acct.assistants.update(cx, |a, cx| a.forget(cx));
        }
        self.settings_tab = tab;
        if tab == Tab::Assistants {
            self.acct.assistants.update(cx, |a, cx| a.refresh(cx));
        }
        cx.notify();
    }

    /// Change the theme config, apply it live and save it.
    pub(crate) fn update_theme(&mut self, f: impl FnOnce(&mut ThemeConfig), cx: &mut Context<Self>) {
        f(&mut self.theme);
        self.theme.save();
        self.theme_stamp = self.theme.theme_stamp();
        crate::theme::apply(&self.theme, cx);
        cx.notify();
    }

    pub(crate) fn set_accent_hex(&mut self, hex: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.accent_input.update(cx, |s, cx| s.set_value(hex.to_string(), window, cx));
        let hex = hex.to_string();
        self.update_theme(move |c| c.accent = hex, cx);
    }

    /// Use the hex in the custom field (the first swatch when it's empty or invalid).
    pub(crate) fn apply_custom_accent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.accent_input.read(cx).value().trim().to_string();
        let typed = if typed.starts_with('#') { typed } else { format!("#{typed}") };
        let hex = if crate::theme::hex(&typed).is_some() { typed } else { pages::settings::SWATCHES[0].to_string() };
        self.set_accent_hex(&hex, window, cx);
    }

    pub(crate) fn save_server(&mut self, cx: &mut Context<Self>) {
        let url = self.server_input.read(cx).value().trim().to_string();
        self.settings_note = Some(if url.is_empty() {
            "Enter a URL first.".into()
        } else {
            match cache::save_server_url(&url) {
                Ok(()) => format!("Saved. The next sync uses {url}."),
                Err(e) => format!("Could not save: {e:#}"),
            }
        });
        cx.notify();
    }

    /// GET {url}/healthz for the URL in the field, off the UI thread.
    pub(crate) fn test_connection(&mut self, cx: &mut Context<Self>) {
        if matches!(self.conn_test, ConnTest::Running) {
            return;
        }
        let url = self.server_input.read(cx).value().trim().to_string();
        self.conn_test = ConnTest::Running;
        cx.notify();
        let task = cx.background_spawn(async move { sync::healthz(&url).map(|d| (url, d)) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |this, cx| {
                this.conn_test = match res {
                    Ok((url, d)) => ConnTest::Ok(format!("{url} answered in {} ms", d.as_millis())),
                    Err(e) => ConnTest::Failed(format!("{e:#}")),
                };
                cx.notify();
            });
        })
        .detach();
    }
}
