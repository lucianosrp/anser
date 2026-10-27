//! finance-core: everything the desktop app knows that isn't pixels.
//!
//! * [`cache`]  -- where the local replica lives, and the server URL
//! * [`profiles`] -- the accounts this machine signs in with (server, user), each with its own replica
//! * [`demo`]   -- "Just try it": the made-up account, answered on this machine, never sent anywhere
//! * [`sync`]   -- pull `/api/export.json` + `/api/db.sqlite` from the server
//! * [`ledger`] -- every bank row, crypto event and broker trade, in memory
//! * [`drill`]  -- spending rows, biggest in a month, same-merchant context
//! * [`dca`]    -- how a dollar-cost averaging plan is doing (BTC, any broker holding)
//! * [`invest`] -- the investing deep dive: broker plans in HKD, value split, dividends
//! * [`fees`]   -- broker commissions, IBKR Fixed vs Tiered re-pricing
//! * [`exposure`] -- by currency, venue, type; region and sector look-through
//! * [`risk`]   -- volatility, drawdowns, correlations from daily bars
//! * [`rebalance`] -- drift from target weights, splitting a contribution
//! * [`base`]   -- the profile's base currency, home country and account countries
//! * [`budget`] -- monthly budgets per category: spend, suggestions, pace
//! * [`review`] -- a calendar year in review
//! * [`recurring`] -- subscriptions, bills, salary: detection, flags, the cash-flow plan
//! * [`bundle`] -- the computed views (net worth, spending, costs, ...)
//! * [`capabilities`] -- what the server has: sources and features (pages show or hide)
//! * [`categories`] -- what each category means, the rules, and the calls that change them
//! * [`search`] -- fuzzy "search everything" over the ledger
//! * [`sources`] -- `GET /api/sources`: each data source's state, no secrets
//! * [`filter`] -- the shared filter model and its query syntax (chips <-> search box)
//! * [`dive`]   -- merchant and category deep dives
//! * [`travel`] -- trips discovered from where the money was spent, their routes and transport
//! * [`geo`]    -- bundled country outlines and city gazetteer for the travel map
//! * [`tailnet`] -- `*.ts.net` names via MagicDNS when a VPN owns the system DNS
//! * [`doctor`] -- health checks (server, tailnet, replica, GPU, install) and their fixes
//! * [`tools`]  -- what the machine has: VPNs, package manager, Hyprland, OS (which Doctor checks apply)
//! * [`qr`]     -- QR codes for the phone-access link
//! * [`upload`] -- send transaction files to the server (POST /upload)
//! * [`version`] -- the one build version string (git describe, stamped by build.rs)
//! * [`selfupdate`] -- the desktop app updating itself from a GitHub release (offered, one click)
//! * [`webapp`] -- build the web app (wasm) and install it on the server
//! * [`backups`] -- the backup key (age identity) and .finance archive downloads/uploads
//! * [`auth`]   -- this machine's device token per server: pairing, keyring or 0600 file
//! * [`client`] -- async calls to the server from a page (native thread or browser fetch)
//! * [`locale`] -- a new user's base currency and country, guessed from the device's locale
//! * [`onboarding`] -- the first-run wizard's state machine (UI-free)
//! * [`replica`] -- the optional encrypted local replica: every replica read and write
//! * [`signin`] -- turned away (401): the loop guard, codes, refusals
//! * [`avatar`] -- the profile picture's spec: kind, initials, colour
//!
//! Without the default `native` feature (the wasm web build) there is no
//! SQLite, HTTP client or filesystem: `cache`/`sync` are inert stand-ins and
//! the ledger and bundle come from [`preload`], filled by the page.
//!
//! The server (anser-server, on your server) stays the source of truth; this crate
//! reads the replica and writes to the server only through its API (uploads,
//! overrides, settings). It has no UI dependency, so the server and the
//! wasm web build reuse it.

#[cfg(feature = "native")]
pub mod auth;
#[cfg(feature = "native")]
pub mod backups;
pub mod avatar;
pub mod broker;
pub mod base;
pub mod budget;
pub mod bundle;
#[cfg_attr(not(feature = "native"), path = "cache_web.rs")]
pub mod cache;
pub mod capabilities;
pub mod categories;
pub mod client;
pub mod dca;
pub mod demo;
pub mod supported;
#[cfg(feature = "native")]
pub mod doctor;
#[cfg(feature = "native")]
pub mod local_server;
pub mod dive;
pub mod drill;
pub mod exposure;
pub mod fees;
pub mod filter;
pub mod geo;
pub mod home;
pub mod mcp_setup;
pub mod mortgage;
pub mod invest;
pub mod jkb;
pub mod ledger;
pub mod locale;
pub mod onboarding;
pub mod paste;
pub mod qr;
#[cfg(not(feature = "native"))]
pub mod preload;
pub mod portfolio;
pub mod rebalance;
pub mod review;
pub mod recurring;
#[cfg(feature = "native")]
pub mod profiles;
#[cfg_attr(not(feature = "native"), path = "replica_web.rs")]
pub mod replica;
pub mod risk;
#[cfg(feature = "native")]
pub mod selfupdate;
pub mod search;
pub mod signin;
pub mod sources;
#[cfg_attr(not(feature = "native"), path = "sync_web.rs")]
pub mod sync;
#[cfg(feature = "native")]
pub mod tailnet;
pub mod travel;
pub mod user_settings;
#[cfg(feature = "native")]
pub mod tools;
#[cfg(feature = "native")]
pub mod upload;
#[cfg(feature = "native")]
pub mod webapp;

pub mod version;

pub use bundle::Bundle;
pub use version::VERSION;
pub use ledger::{Bar, CoinEvent, Entry, Kind, Ledger};
