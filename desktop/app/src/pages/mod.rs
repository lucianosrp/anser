pub mod account;
pub mod assistants;
// Settings, Backups: the encrypted archives (the browser: Export only)
pub mod backups;
pub mod avatar_edit;
pub mod banks;
pub mod broker;
pub mod budgets;
pub mod cashflow;
// Categories, Rules, Categorize Other (Phase 3: categories as data)
pub mod categories;
pub mod crypto;
pub mod dca;
// the browser can't probe DNS, the GPU or local files: a stand-in page
#[cfg_attr(target_family = "wasm", path = "doctor_web.rs")]
pub mod doctor;
pub mod deep;
// the Downloads watcher; the browser has no downloads folder: a stub
#[cfg_attr(target_family = "wasm", path = "downloads_web.rs")]
pub mod downloads;
// Settings, Updates: the desktop app updating itself; nothing to update in a browser
#[cfg_attr(target_family = "wasm", path = "updates_web.rs")]
pub mod updates;
pub mod home_mortgage;
pub mod home_setup;
pub mod invest_setup;
pub mod investments;
// the Upload page's column mapper (native: it reads local files)
#[cfg(not(target_family = "wasm"))]
pub mod mapper;
pub mod onboarding;
pub mod overview;
pub mod owed;
pub mod projection;
pub mod review;
pub mod settings;
pub mod sources;
pub mod supported;
pub mod spending;
pub mod spending_home;
pub mod subscriptions;
pub mod transactions;
pub mod travel;
// the encrypted local copy: its switch and the unlock screen (none in the browser)
#[cfg_attr(target_family = "wasm", path = "vault_web.rs")]
pub mod vault;
// the web build has no local files to parse: a stand-in page
#[cfg_attr(target_family = "wasm", path = "upload_web.rs")]
pub mod upload;

use std::sync::atomic::{AtomicU32, Ordering};

use finance_core::capabilities::Capabilities;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{Icon, IconName};

/// The pages the server has nothing for, one bit per Page (index in
/// Page::ALL); set from the bundle's capabilities on every load. 0: all on
/// (no data yet, or an older server).
static HIDDEN: AtomicU32 = AtomicU32::new(0);

/// Hide the pages `caps` has nothing for (sidebar, palette, tab bar, More).
pub fn set_capabilities(caps: &Capabilities) {
    let bits = Page::ALL.iter().enumerate().filter(|(_, p)| !p.visible_with(caps)).fold(0u32, |a, (i, _)| a | 1 << i);
    HIDDEN.store(bits, Ordering::Relaxed);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Overview,
    Spending,
    Budgets,
    Travel,
    Transactions,
    Review,
    Investments,
    Banks,
    Crypto,
    Broker,
    Upload,
    Settings,
    Subscriptions,
    CashFlow,
    Doctor,
    Categories,
}

impl Page {
    /// Sidebar and palette order.
    pub const ALL: [Page; 16] = [
        Page::Overview, Page::Spending, Page::Budgets, Page::Travel, Page::Transactions, Page::Review,
        Page::Subscriptions, Page::CashFlow,
        Page::Investments, Page::Banks, Page::Crypto, Page::Broker, Page::Upload, Page::Categories, Page::Settings, Page::Doctor,
    ];

    /// Ctrl+1 … Ctrl+9. Year in review (Ctrl+Y), Upload (Ctrl+U; dropping
    /// files opens it anyway) and Settings (Ctrl+,) have letter keys.
    pub const NUMBERED: [Page; 9] = [
        Page::Overview, Page::Spending, Page::Budgets, Page::Travel, Page::Transactions,
        Page::Investments, Page::Banks, Page::Crypto, Page::Broker,
    ];

    /// The key hint the sidebar shows (with Ctrl).
    pub fn key(self) -> String {
        match self {
            Page::Review => "Y".into(),
            Page::Upload => "U".into(),
            Page::Settings => ",".into(),
            p => Page::NUMBERED.iter().position(|x| *x == p).map(|i| (i + 1).to_string()).unwrap_or_default(),
        }
    }

    /// Does the server have what this page shows? Pages that work on any
    /// data (or none) always do.
    pub fn visible_with(self, caps: &Capabilities) -> bool {
        match self {
            Page::Broker => caps.has("broker"),
            Page::Crypto => caps.has("crypto"),
            Page::Investments => caps.investments(),
            Page::Travel => caps.has("travel"),
            Page::Banks => caps.has("banks"),
            Page::CashFlow => caps.has("cashflow"),
            _ => true,
        }
    }

    /// [`Page::visible_with`] for the capabilities of the last load.
    pub fn visible(self) -> bool {
        let i = Page::ALL.iter().position(|p| *p == self).unwrap_or(0);
        HIDDEN.load(Ordering::Relaxed) & (1 << i) == 0
    }

    /// For a hidden page reached by its key: what it needs, and whether the
    /// fix is a file upload (else keys in the server's .env, see the Doctor).
    pub fn needs(self) -> (&'static str, bool) {
        match self {
            Page::Broker => ("Connect a broker: Interactive Brokers' Flex Web Service token and query ID in the server's .env (IBKR_FLEX_TOKEN, IBKR_FLEX_QUERY_ID). The Doctor shows the steps.", false),
            Page::Crypto => ("Add crypto: exchange API keys (Binance, OKX; read-only) in the server's .env, or upload a wallet export (Trezor, Revolut).", true),
            Page::Investments => ("Add an investment source: a broker (IBKR), a fund (HSBC FlexInvest via its SMS confirmations), or crypto.", true),
            Page::Travel => ("Upload bank or card exports: trips are found from where the money was spent.", true),
            Page::Banks => ("Upload a bank export (N26, HSBC, Revolut CSV, Mox statement PDF).", true),
            Page::CashFlow => ("Upload a bank export with balances: the cash-flow calendar plans from them.", true),
            _ => ("", true),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Spending => "Spending",
            Page::Budgets => "Budgets",
            Page::Travel => "Travel",
            Page::Transactions => "Transactions",
            Page::Review => "Year in review",
            Page::Investments => "Investments",
            Page::Banks => "Banks",
            Page::Crypto => "Crypto",
            Page::Broker => "Broker",
            Page::Upload => "Upload",
            Page::Settings => "Settings",
            Page::Subscriptions => "Subscriptions",
            Page::CashFlow => "Cash flow",
            Page::Doctor => "Doctor",
            Page::Categories => "Categories",
        }
    }

    pub fn icon(self) -> Icon {
        let kit = |n: IconName| Icon::new(n);
        match self {
            Page::Budgets => Icon::new(Lucide::Target),
            Page::Review => Icon::new(Lucide::CalendarRange),
            Page::Overview => kit(IconName::LayoutDashboard),
            Page::Spending => kit(IconName::ChartPie),
            Page::Travel => kit(IconName::Map),
            Page::Transactions => kit(IconName::Inbox),
            Page::Investments => kit(IconName::Building2),
            Page::Banks => kit(IconName::Folder),
            Page::Crypto => kit(IconName::Globe),
            Page::Broker => kit(IconName::Network),
            Page::Upload => kit(IconName::ArrowUp),
            Page::Settings => kit(IconName::Settings),
            Page::Subscriptions => kit(IconName::RotateCw),
            Page::CashFlow => kit(IconName::Calendar),
            Page::Doctor => Icon::new(Lucide::Stethoscope),
            Page::Categories => Icon::new(Lucide::Tags),
        }
    }
}
