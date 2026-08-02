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

mod campaigns;
mod commands;
mod config;
mod contacts;
mod error;
mod events;
mod ipc;
mod logs;
mod messages;
mod paths;
mod sessions;
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
        // The native file picker, and nothing more of the filesystem: the
        // WebView gets a path the operator chose, and the backend is what
        // opens it (CLAUDE.md §8).
        .plugin(tauri_plugin_dialog::init())
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

            // The database is opened and migrated **before** the state is
            // handed to Tauri: a command that found an unmigrated file would
            // fail on a missing table, which reads as a bug rather than as a
            // failed start. `block_on` at the entry point is the one place
            // `clippy.toml` allows it, and `setup` is not async.
            let database_file = paths.database_file();
            let database = tauri::async_runtime::block_on(async {
                let database =
                    persistence::Database::open(persistence::DatabaseConfig::new(&database_file))
                        .await?;
                database.migrate().await?;

                Ok::<_, persistence::PersistenceError>(database)
            })
            .context("failed to open the application database")?;

            app.manage(guard);
            app.manage(level_handle);
            app.manage(state::AppState::new(store, settings, database));

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
        .build(tauri::generate_context!())
        .context("failed to start the Tauri application")?
        .run(|app, event| {
            // CA-005-08 — closing the application unbinds every live session:
            // `unbind`, a bounded wait for `unbind_resp`, then the socket.
            // Without this the process exits on an open bind, and the message
            // centre keeps the session until its own timeout reaps it — which
            // makes the next start fail with `ESME_RALYBND`.
            //
            // `ExitRequested` rather than `Exit`: by the time `Exit` fires the
            // runtime is already winding down and an `unbind` would not get
            // out.
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                let state = app.state::<state::AppState>();

                tauri::async_runtime::block_on(async {
                    // Campaigns first: a runner still feeding the send window
                    // would keep submitting into a session that is about to
                    // unbind, and each of those messages would be journalled
                    // `SENT` with no answer — the uncertain family of ADR 0014,
                    // manufactured by our own shutdown order.
                    state.campaigns().shutdown().await;
                    state.sessions().shutdown().await;
                    // And whatever the PDU recorder still holds: the last PDUs
                    // before a shutdown are the ones somebody turned it on for,
                    // and they are the ones a buffer would swallow.
                    state.logs().flush().await;
                });
            }
        });

    Ok(())
}
