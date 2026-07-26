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
///
/// # Both a return and an event, and who owns which
///
/// The failure is signalled twice on purpose: returned to the caller, which
/// knows the form to mark invalid, and emitted on `error:notify`, which is the
/// global surface. `error:notify` is also the witness event this milestone
/// requires, and `config_set` is its only producer until milestone 005 starts
/// reporting dropped sessions.
///
/// The frontend must therefore NOT notify again on a returned `backend`
/// failure — it did, and the same toast appeared twice, to be dismissed
/// separately. The rule lives in `persistPreference`: the event owns the
/// notification, the return owns the form.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The restart round-trip, without a window.
    ///
    /// `config_get` returns an `AppConfig`; the frontend edits it and sends a
    /// `ConfigSetInput` back. Nothing checked that the two agree, and they are
    /// separate structs by design — one has `deny_unknown_fields`, the other
    /// takes raw values so the backend can reject them. Renaming a field on
    /// one side alone would pass every other test in the suite and break the
    /// application at runtime.
    #[test]
    fn what_config_get_returns_can_be_fed_back_to_config_set() {
        let stored = AppConfig::default();

        let json = serde_json::to_value(stored).expect("AppConfig serialises");
        let back: ConfigSetInput =
            serde_json::from_value(json.clone()).expect("an AppConfig must be a valid input");

        let reparsed =
            AppConfig::parse(back.clone()).expect("the defaults must survive validation");
        assert_eq!(reparsed, stored, "the round trip changed the configuration");

        // Field names are part of the contract: the generated TypeScript keys
        // off them on both sides.
        let input_json = serde_json::to_value(&back).expect("input serialises");
        let (mut left, mut right) = (
            json.as_object().expect("object").keys().collect::<Vec<_>>(),
            input_json
                .as_object()
                .expect("object")
                .keys()
                .collect::<Vec<_>>(),
        );
        left.sort_unstable();
        right.sort_unstable();
        assert_eq!(
            left, right,
            "AppConfig and ConfigSetInput disagree on field names"
        );
    }
}
