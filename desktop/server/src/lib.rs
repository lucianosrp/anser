//! finance-server as a library: the HTTP server, the background sync, the
//! push contract, the web app's hosting and the MCP endpoint. The CLI
//! ([`cli`], one subcommand per file in src/cli/) is a thin layer over
//! these; both `anser-server` and the desktop app's `anser server` run it.

pub mod accounts;
pub mod api;
pub mod auth;
pub mod backups;
pub mod categories;
pub mod cli;
pub mod demo;
pub mod avatar;
pub mod feed;
pub mod formats;
pub mod gate;
pub mod homes;
pub mod investments;
pub mod live;
pub mod log;
pub mod mcp;
pub mod push;
pub mod server;
pub mod sources;
pub mod suggest;
pub mod syncer;
pub mod upload;
pub mod webapp;
pub mod wipe;
