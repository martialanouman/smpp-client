//! ShinobiSMPP IPC layer — the only crate that knows about Tauri.
//!
//! This crate is a **boundary**, not a business layer (guide §8.3). Its role
//! is limited to four moves: deserialise and validate an IPC input, call a
//! business service, serialise the output, emit an event. Anything beyond
//! those four moves belongs in a crate under `crates/`.
//!
//! It is also the only place in the repository where `anyhow` is allowed:
//! business crates expose typed `thiserror` errors, which the entry point
//! aggregates (CLAUDE.md §4).
//!
//! At milestone 000 the application merely starts and shows the placeholder
//! page. The IPC contract lands at milestone 001.

use anyhow::Context as _;

mod commands;
mod events;

/// Builds and runs the Tauri application.
///
/// # Errors
///
/// Returns an error if the context generated at build time is invalid, or if
/// the WebView cannot be initialised — typically a missing system dependency
/// (WebView2 on Windows, `webkit2gtk` on Linux).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> anyhow::Result<()> {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![])
        .run(tauri::generate_context!())
        .context("failed to start the Tauri application")
}
