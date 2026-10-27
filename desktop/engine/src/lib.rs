//! The finance server's engine: the store, file parsers and ingest,
//! categorization, the valuation views and the exchange / broker / price
//! fetchers. UI-free and HTTP-free: the server (`anser-server`) and the
//! desktop app build on it.
//!
//! Layers: config; db (the one store: schema + every write); sources +
//! ingest + categorize (files in); preview (a dry-run ingest on a copy, for
//! the desktop app's Upload page); fetch (network in: ledgers, prices, FX);
//! views + export (everything computed, network only via views::MarketFeed).

pub mod config;
pub mod tenant;
pub mod catalog;
pub mod profile;
pub mod secrets;
pub mod db;
pub mod categorize;
pub mod categories;
pub mod invest_setup;
pub mod rules;
pub mod ingest;
pub mod preview;
pub mod sources;
pub mod views;
pub mod export;
pub mod archive;
pub mod demo;
pub mod fetch;
pub mod mcp;
