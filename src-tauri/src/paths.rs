//! Resolution of the per-OS application directories.
//!
//! Everything the application writes goes under a directory Tauri resolves
//! from the bundle identifier — `~/Library/Application Support/…` on macOS,
//! `%APPDATA%\…` on Windows, `~/.config` and `~/.local/state` on Linux. No
//! path is ever composed from a value the WebView sent (CA-001-02).
//!
//! The single exception is `SHINOBI_LOG_DIR`, already documented in
//! `.env.example`: it redirects the **log** files, never the preferences, and
//! exists so a test profile can collect traces without touching the developer's
//! real directory.

use std::path::PathBuf;

use tauri::{Manager, Runtime};

/// Environment variable redirecting the rolling log files.
const LOG_DIR_VAR: &str = "SHINOBI_LOG_DIR";

/// Environment variable redirecting the data directory.
///
/// Documented in `.env.example` alongside `SHINOBI_LOG_DIR`, and for the same
/// reason: a test profile must be able to open its own database without
/// touching the developer's real one.
const DATA_DIR_VAR: &str = "SHINOBI_DATA_DIR";

/// File name of the SQLite database (spec §14.4).
const DATABASE_FILE: &str = "shinobismpp.db";

/// The directories the application is allowed to write to.
#[derive(Debug, Clone)]
pub(crate) struct AppPaths {
    /// Holds `config.json`.
    pub(crate) config_dir: PathBuf,
    /// Holds the rolling log files.
    pub(crate) log_dir: PathBuf,
    /// Holds the SQLite database (spec §14.4).
    pub(crate) data_dir: PathBuf,
}

/// Failure to resolve an application directory.
///
/// The shared `Dir` suffix is the subject of this enum, not an accident:
/// every variant *is* a directory that could not be resolved, and the prefix
/// says which. Dropping it would leave `Config`, `Log` and `Data`, which say
/// nothing on their own.
#[derive(Debug, thiserror::Error)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum PathError {
    /// Tauri could not resolve the configuration directory.
    #[error("the application configuration directory could not be resolved")]
    ConfigDir(#[source] tauri::Error),

    /// Tauri could not resolve the log directory.
    #[error("the application log directory could not be resolved")]
    LogDir(#[source] tauri::Error),

    /// Tauri could not resolve the data directory.
    #[error("the application data directory could not be resolved")]
    DataDir(#[source] tauri::Error),
}

impl AppPaths {
    /// Resolves the directories from a Tauri manager.
    ///
    /// # Errors
    ///
    /// [`PathError`] if the platform exposes neither directory — in practice a
    /// misconfigured environment with no home directory.
    pub(crate) fn resolve<R: Runtime, M: Manager<R>>(manager: &M) -> Result<Self, PathError> {
        let resolver = manager.path();

        let config_dir = resolver.app_config_dir().map_err(PathError::ConfigDir)?;
        let log_dir = resolver.app_log_dir().map_err(PathError::LogDir)?;
        let data_dir = resolver.app_data_dir().map_err(PathError::DataDir)?;

        Ok(Self {
            config_dir,
            log_dir: resolve_override(std::env::var(LOG_DIR_VAR).ok().as_deref(), log_dir),
            data_dir: resolve_override(std::env::var(DATA_DIR_VAR).ok().as_deref(), data_dir),
        })
    }

    /// Where the SQLite database lives.
    ///
    /// Composed here and nowhere else: no path in this application is ever
    /// built from a value the WebView sent (CA-001-02).
    pub(crate) fn database_file(&self) -> PathBuf {
        self.data_dir.join(DATABASE_FILE)
    }
}

/// Applies an environment override to a directory, ignoring an empty value.
///
/// `.env.example` documents an empty variable as "use the platform default";
/// treating `""` as a path would resolve to the current working directory,
/// which is the developer's shell — anywhere at all.
fn resolve_override(override_value: Option<&str>, fallback: PathBuf) -> PathBuf {
    match override_value.map(str::trim) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_platform_directory_when_no_override_is_set() {
        let fallback = PathBuf::from("/platform/logs");

        assert_eq!(resolve_override(None, fallback.clone()), fallback);
    }

    #[test]
    fn ignores_an_empty_or_blank_override() {
        let fallback = PathBuf::from("/platform/logs");

        assert_eq!(resolve_override(Some(""), fallback.clone()), fallback);
        assert_eq!(resolve_override(Some("   "), fallback.clone()), fallback);
    }

    #[test]
    fn honours_a_non_empty_override() {
        let fallback = PathBuf::from("/platform/logs");

        assert_eq!(
            resolve_override(Some("/tmp/profile/logs"), fallback),
            PathBuf::from("/tmp/profile/logs")
        );
    }
}
