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
//! # Startup order
//!
//! 1. resolve the per-OS directories ([`paths`]);
//! 2. read the preferences ([`config`]) — they carry the log level, so nothing
//!    that could be worth logging may happen before this point;
//! 3. install `tracing` ([`telemetry`]);
//! 4. hand the state to Tauri and run.
//!
//! Step 2 must not be fatal: a corrupt preferences file falls back to the
//! defaults and is reported, because refusing to start over a colour scheme
//! would be a worse failure than the one being handled.

use std::path::Path;

use anyhow::Context as _;
use tauri::Manager as _;

mod commands;
mod config;
mod error;
mod events;
mod ipc;
mod paths;
mod state;
mod telemetry;

/// Writes the TypeScript IPC bindings to `path`.
///
/// The one public entry point besides [`run`], called by the `gen_ipc` binary.
/// It lives in the library rather than in the binary so the generated bindings
/// come from the very same [`ipc::builder`] the application mounts.
///
/// # Errors
///
/// Returns an error if the target directory cannot be created or the file
/// cannot be written.
pub fn export_ipc_bindings(path: &Path) -> anyhow::Result<()> {
    ipc::export(path).context("failed to export the TypeScript IPC bindings")
}

/// Builds and runs the Tauri application.
///
/// # Errors
///
/// Returns an error if the context generated at build time is invalid, if the
/// per-OS directories cannot be resolved, or if the WebView cannot be
/// initialised — typically a missing system dependency (WebView2 on Windows,
/// `webkit2gtk` on Linux).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> anyhow::Result<()> {
    let ipc = ipc::builder();

    tauri::Builder::default()
        .invoke_handler(ipc.invoke_handler())
        .setup(move |app| {
            // Wires the typed event channels. Without this, `error:notify`
            // is emitted into the void.
            ipc.mount_events(app);

            let paths = paths::AppPaths::resolve(app.handle())
                .context("failed to resolve the application directories")?;

            let store = config::ConfigStore::new(&paths.config_dir);

            // A read failure is degraded, not fatal — see the module header.
            // It cannot be logged yet: the subscriber is installed from the
            // level this very value carries.
            let (settings, load_failure) = match store.load() {
                Ok(settings) => (settings, None),
                Err(error) => (config::AppConfig::default(), Some(error)),
            };

            let (guard, level_handle) =
                telemetry::init(&paths.log_dir, settings.log_level, settings.retention_days)
                    .context("failed to install the tracing subscriber")?;

            app.manage(guard);
            app.manage(level_handle);
            app.manage(state::AppState::new(store, settings));

            if let Some(error) = load_failure {
                tracing::warn!(
                    error = ?error,
                    "unreadable preferences, falling back to the defaults"
                );
            }

            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                language = ?settings.language,
                log_level = ?settings.log_level,
                "ShinobiSMPP started"
            );

            Ok(())
        })
        .run(tauri::generate_context!())
        .context("failed to start the Tauri application")
}
