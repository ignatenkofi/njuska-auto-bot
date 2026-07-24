//! NjuskaAutoBot as a library.
//!
//! The bot is fundamentally a binary, but integration tests in `tests/`
//! can only link against a *library* target — `tests/*.rs` are separate
//! crates that `use njuska_auto_bot::…`. This facade exists for them (#22);
//! `main.rs` stays the only produced executable and simply pulls these
//! modules back in.

pub mod bot;
pub mod commands;
pub mod config;
pub mod dyncatalog;
pub mod i18n;
pub mod models;
pub mod scraper;
pub mod signals;
pub mod storage;
pub mod telegram;
pub mod version;
