//! PAC Keeper: bookkeeping and reports for a BC school Parent Advisory Committee.
//! <https://packeeper.ca>
//!
//! The library holds everything; `main.rs` is the command line and the
//! `desktop` crate wraps the same server in an app window.

pub mod backup;
pub mod crypt;
pub mod db;
pub mod demo;
pub mod fiscal;
pub mod hosted;
pub mod import;
pub mod ledger;
pub mod lunch;
pub mod meetings;
pub mod models;
pub mod money;
pub mod reports;
pub mod server;
pub mod web;
