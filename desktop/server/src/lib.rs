//! finance-server as a library: the HTTP server, the background sync, the
//! push contract, the web app's hosting and the MCP endpoint. The anser-server binary's
//! subcommands (src/cmd/) are thin wrappers over these.

pub mod accounts;
pub mod api;
pub mod auth;
pub mod backups;
pub mod categories;
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
