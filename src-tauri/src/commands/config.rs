//! `config_get` and `config_set` — the application preferences.

use tauri::{AppHandle, State};

use crate::config::{AppConfig, ConfigSetInput};
use crate::error::ErrorDto;
use crate::events::ErrorNotify;
use crate::state::AppState;
use crate::telemetry::LogLevelHandle;

/// Returns the preferences currently in force.
///
/// # Errors
///
/// Infallible today. The `Result` is part of the signature anyway: every
/// command shares one shape (guide §9.1), and turning a command fallible later
/// must not be a breaking change to the TypeScript contract.
#[tauri::command]
#[specta::specta]
pub(crate) async fn config_get(state: State<'_, AppState>) -> Result<AppConfig, ErrorDto> {
    Ok(state.settings().await)
}

/// Validates, persists and adopts new preferences.
///
/// The input is treated as **untrusted** (CLAUDE.md §3): the WebView constrains
/// its own controls, but nothing stops a hand-crafted call, so validation
/// happens here and only here.
///
/// A failure is reported twice, on purpose: returned to the caller, which
/// knows which form to mark invalid, and emitted as `error:notify`, which
/// feeds the application-wide notification surface.
///
/// # Errors
///
/// - `CONFIG_INVALID_*` if a field fails validation;
/// - `CONFIG_UNWRITABLE` if the preferences file cannot be written.
#[tauri::command]
#[specta::specta]
pub(crate) async fn config_set(
    app: AppHandle,
    state: State<'_, AppState>,
    level_handle: State<'_, LogLevelHandle>,
    input: ConfigSetInput,
) -> Result<AppConfig, ErrorDto> {
    let settings = match AppConfig::parse(input) {
        Ok(settings) => settings,
        Err(error) => return Err(report(&app, &state, error).await),
    };

    let applied = match state.replace_settings(settings).await {
        Ok(applied) => applied,
        Err(error) => return Err(report(&app, &state, error).await),
    };

    level_handle.apply(applied.log_level);

    tracing::info!(
        language = ?applied.language,
        theme = ?applied.theme,
        log_level = ?applied.log_level,
        retention_days = applied.retention_days.get(),
        "preferences updated"
    );

    Ok(applied)
}

/// Logs an error **with** its source chain, then projects it onto the IPC
/// contract and announces it.
///
/// This is the single funnel where the two audiences part ways: `tracing` gets
/// everything, including the paths; the WebView gets the sanitised DTO
/// (CA-001-06).
async fn report(
    app: &AppHandle,
    state: &State<'_, AppState>,
    error: crate::config::ConfigError,
) -> ErrorDto {
    tracing::warn!(error = ?error, "config_set rejected");

    let dto = ErrorDto::from(&error);
    state
        .events()
        .emit_error(app, &ErrorNotify::from(&dto))
        .await;

    dto
}
