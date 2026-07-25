//! Application preferences: model, parsing, persistence.
//!
//! This module is deliberately **free of any Tauri type**. `src-tauri` is an
//! IPC boundary, not a business layer (CLAUDE.md §3): the commands in
//! [`crate::commands::config`] only validate, call and serialise — everything
//! they would otherwise have grown lives here, where it is testable without a
//! WebView.
//!
//! Milestone 001 stores the preferences in a JSON file inside the OS
//! configuration directory. Milestone 002 introduces SQLite, but for
//! *business* data: bootstrap preferences must stay readable before any
//! database is open.

mod error;
mod model;
mod store;

pub(crate) use error::ConfigError;
pub(crate) use model::{AppConfig, ConfigSetInput, LogLevel, RetentionDays};
pub(crate) use store::ConfigStore;

#[cfg(test)]
pub(crate) use model::{Language, Theme};
