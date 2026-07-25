//! `tracing` initialisation: rolling file, plus console in development.
//!
//! `tracing` exclusively, never `println!` (CLAUDE.md §4) — the workspace
//! lints make that a compile error, this module makes it usable: without a
//! subscriber installed, every `tracing` macro is a no-op and the rule would
//! be respected while producing no logs at all.
//!
//! Two layers:
//!
//! - a **rolling file** in the OS log directory, rotated daily and capped at
//!   the retention the user chose. `tracing-appender` drops the oldest files
//!   itself, so no cleanup task is needed;
//! - a **console** layer, compiled in debug builds only. A shipped application
//!   has no terminal to write to.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{fmt, reload, EnvFilter, Registry};

use crate::config::{LogLevel, RetentionDays};

/// Base name of the rolling log files, suffixed with the rotation date.
const FILE_PREFIX: &str = "shinobismpp";

/// Extension of the rolling log files.
const FILE_SUFFIX: &str = "log";

/// Environment variable overriding the computed filter, as `.env.example`
/// documents it.
const FILTER_VAR: &str = "RUST_LOG";

/// Keeps the background writer of the file appender alive.
///
/// `tracing-appender` writes from a dedicated thread; dropping this guard
/// flushes it. Losing it — by ignoring the return value of [`init`] — silently
/// truncates the last lines written before exit, which is precisely when the
/// interesting ones happen.
#[derive(Debug)]
#[must_use = "dropping the guard stops the log writer and loses the pending lines"]
pub(crate) struct TelemetryGuard {
    /// Held only for its `Drop`. The underscore prefix is what tells `rustc`
    /// that never reading it is the point, rather than dead code.
    _worker: WorkerGuard,
}

/// Failure to install the `tracing` subscriber.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TelemetryError {
    /// The log directory could not be created or the appender could not open
    /// its file.
    #[error("the log file could not be opened")]
    Appender(#[source] tracing_appender::rolling::InitError),

    /// A subscriber is already installed for this process.
    #[error("a tracing subscriber is already installed")]
    AlreadyInstalled(#[source] tracing_subscriber::util::TryInitError),
}

/// Lets `config_set` change the verbosity of a running application.
///
/// Without this, the log level preference would only take effect at the next
/// start — a setting that appears to do nothing is worse than no setting.
///
/// The handle is `None` when `RUST_LOG` pins the filter: an explicit
/// environment override must not be silently undone by a click in the
/// interface.
#[derive(Debug, Clone)]
pub(crate) struct LogLevelHandle {
    /// `None` when the filter is pinned by `RUST_LOG`.
    handle: Option<FilterHandle>,
}

/// The reload handle over the `EnvFilter` layer of the registry.
type FilterHandle = reload::Handle<EnvFilter, Registry>;

impl LogLevelHandle {
    /// Applies a new level to the running subscriber.
    ///
    /// A no-op when the filter is pinned, and never fatal: failing to change a
    /// verbosity is not a reason to fail the command that asked for it.
    pub(crate) fn apply(&self, level: LogLevel) {
        let Some(handle) = self.handle.as_ref() else {
            tracing::debug!("log level pinned by RUST_LOG, preference not applied");
            return;
        };

        match EnvFilter::try_new(level.as_directive()) {
            Ok(filter) => {
                if let Err(error) = handle.reload(filter) {
                    tracing::warn!(error = %error, "failed to apply the new log level");
                } else {
                    tracing::info!(level = ?level, "log level applied");
                }
            }
            Err(error) => tracing::warn!(error = %error, "invalid log level directive"),
        }
    }
}

/// Installs the subscriber and returns the guard that keeps it writing.
///
/// # Errors
///
/// [`TelemetryError`] if the log file cannot be opened, or if a subscriber has
/// already been installed for the process.
pub(crate) fn init(
    log_dir: &Path,
    level: LogLevel,
    retention: RetentionDays,
) -> Result<(TelemetryGuard, LogLevelHandle), TelemetryError> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .filename_suffix(FILE_SUFFIX)
        .max_log_files(usize::from(retention.get()))
        .build(log_dir)
        .map_err(TelemetryError::Appender)?;

    let (writer, guard) = tracing_appender::non_blocking(appender);

    let pinned = std::env::var(FILTER_VAR).ok();
    let directive = filter_directive(level, pinned.as_deref());

    // `EnvFilter::new` panics on a malformed directive, and `RUST_LOG` is
    // user-supplied. Falling back to the stored level keeps a typo in a shell
    // from taking the whole application down.
    let filter = EnvFilter::try_new(&directive)
        .or_else(|_| EnvFilter::try_new(level.as_directive()))
        .unwrap_or_default();

    let (filter, reload_handle) = reload::Layer::new(filter);

    tracing_subscriber::registry()
        .with(filter)
        // No ANSI escapes in the file: they would turn every log a user sends
        // us into an unreadable soup of control characters.
        .with(fmt::layer().with_ansi(false).with_writer(writer))
        // `Option<Layer>` is itself a `Layer`, so the console stays a plain
        // value rather than a `#[cfg]` that changes the type of the whole
        // subscriber.
        .with(cfg!(debug_assertions).then(|| fmt::layer().with_writer(std::io::stderr)))
        .try_init()
        .map_err(TelemetryError::AlreadyInstalled)?;

    let handle = LogLevelHandle {
        handle: if is_pinned(pinned.as_deref()) {
            None
        } else {
            Some(reload_handle)
        },
    };

    Ok((TelemetryGuard { _worker: guard }, handle))
}

/// Whether `RUST_LOG` carries a usable directive.
///
/// An empty or blank variable means "unset": `.env.example` documents it that
/// way, and treating `RUST_LOG=` as a pin would freeze the level of anyone who
/// merely sourced the template.
fn is_pinned(env_override: Option<&str>) -> bool {
    env_override
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

/// Computes the `EnvFilter` directive.
///
/// `RUST_LOG` wins over the stored preference: a developer chasing a bug must
/// be able to raise the verbosity without going through the interface, and a
/// support session must be able to do so without editing a JSON file.
fn filter_directive(level: LogLevel, env_override: Option<&str>) -> String {
    if is_pinned(env_override) {
        env_override.unwrap_or_default().trim().to_owned()
    } else {
        level.as_directive().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_directive_from_the_stored_level() {
        assert_eq!(filter_directive(LogLevel::Info, None), "info");
        assert_eq!(filter_directive(LogLevel::Trace, None), "trace");
        assert_eq!(filter_directive(LogLevel::Error, None), "error");
    }

    #[test]
    fn lets_the_environment_override_the_stored_level() {
        assert_eq!(
            filter_directive(LogLevel::Info, Some("shinobismpp=debug,tower=warn")),
            "shinobismpp=debug,tower=warn"
        );
    }

    #[test]
    fn ignores_an_empty_or_blank_environment_override() {
        assert_eq!(filter_directive(LogLevel::Warn, Some("")), "warn");
        assert_eq!(filter_directive(LogLevel::Warn, Some("  ")), "warn");
    }

    #[test]
    fn every_level_produces_a_directive_env_filter_accepts() {
        for raw in LogLevel::ALLOWED {
            let level = LogLevel::parse(raw).expect("ALLOWED only holds parseable values");
            let directive = filter_directive(level, None);

            assert!(
                EnvFilter::try_new(&directive).is_ok(),
                "EnvFilter rejected {directive}"
            );
        }
    }

    #[test]
    fn writes_the_rolling_file_inside_the_given_directory() {
        let dir = tempfile::tempdir().expect("a temporary directory must be creatable");

        let appender = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(FILE_PREFIX)
            .filename_suffix(FILE_SUFFIX)
            .max_log_files(3)
            .build(dir.path())
            .expect("the appender must open its file");
        drop(appender);

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the directory must be readable")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();

        assert_eq!(entries.len(), 1, "unexpected content: {entries:?}");
        let name = entries.first().expect("exactly one entry was asserted");
        assert!(name.starts_with(FILE_PREFIX), "unexpected name: {name}");
        assert!(name.ends_with(FILE_SUFFIX), "unexpected name: {name}");
    }
}
