//! Global application state, handed to Tauri through `manage`.
//!
//! Holds no business logic: a preferences store, the preferences currently in
//! force, and the event emitter. Milestone 002 onwards will add the handles of
//! the services (`SessionRegistry`, repositories) — never their implementation.

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::{AppConfig, ConfigError, ConfigStore};
use crate::events::EventEmitter;

/// What the IPC commands are allowed to reach.
#[derive(Debug)]
pub(crate) struct AppState {
    /// Reader and writer of `config.json`.
    store: Arc<ConfigStore>,
    /// The preferences currently in force.
    ///
    /// `tokio::sync::RwLock`, not `std::sync::RwLock`: the commands are async
    /// and CLAUDE.md §4 bans a std guard held across an `.await`.
    settings: RwLock<AppConfig>,
    /// The single place events are emitted from.
    events: EventEmitter,
}

impl AppState {
    /// Builds the state from a store and the preferences read at startup.
    pub(crate) fn new(store: ConfigStore, settings: AppConfig) -> Self {
        Self {
            store: Arc::new(store),
            settings: RwLock::new(settings),
            events: EventEmitter::default(),
        }
    }

    /// The preferences currently in force.
    pub(crate) async fn settings(&self) -> AppConfig {
        *self.settings.read().await
    }

    /// Persists new preferences, then adopts them.
    ///
    /// The order matters and is the same write-ahead discipline as messages
    /// (CLAUDE.md §4): what could not be written must not be believed. A
    /// failed write leaves the previous preferences in force, and the next
    /// `config_get` reports the truth rather than a value that would vanish at
    /// the next start.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Unwritable`] if the file cannot be written.
    pub(crate) async fn replace_settings(
        &self,
        settings: AppConfig,
    ) -> Result<AppConfig, ConfigError> {
        // The lock is taken *before* the write so two concurrent `config_set`
        // calls cannot interleave "write A, write B, adopt B, adopt A".
        let mut current = self.settings.write().await;

        // The write goes to `spawn_blocking`: `create_dir_all` and `write` are
        // blocking syscalls, and CLAUDE.md §4 forbids blocking the runtime.
        // Two hundred bytes on a local disk return instantly — but a config
        // directory on a network home, which is ordinary in a company, parks
        // the worker serving the command, and every command scheduled on that
        // worker waits behind it.
        //
        // Holding the `tokio::sync::RwLock` guard across the await is safe and
        // intended: it is what keeps two concurrent writes from interleaving.
        // A `std::sync::RwLock` here would be a bug, which is why `clippy.toml`
        // bans that type outright.
        let store = Arc::clone(&self.store);
        let to_write = settings;

        tauri::async_runtime::spawn_blocking(move || store.save(&to_write))
            .await
            .map_err(|error| ConfigError::Unwritable(std::io::Error::other(error)))??;

        *current = settings;

        Ok(settings)
    }

    /// The event emitter.
    pub(crate) const fn events(&self) -> &EventEmitter {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    // See `crate::events::tests`: `#[tokio::test]` expands to `block_on`,
    // which the deny list targets in production code, not in a test harness.
    #![allow(clippy::disallowed_methods)]

    use super::*;
    use crate::config::{Language, LogLevel, RetentionDays, Theme};

    fn state_in(directory: &std::path::Path) -> AppState {
        AppState::new(ConfigStore::new(directory), AppConfig::default())
    }

    #[tokio::test]
    async fn starts_from_the_preferences_it_was_given() {
        let dir = tempfile::tempdir().expect("a temporary directory must be creatable");

        assert_eq!(state_in(dir.path()).settings().await, AppConfig::default());
    }

    #[tokio::test]
    async fn persists_before_adopting_new_preferences() {
        let dir = tempfile::tempdir().expect("a temporary directory must be creatable");
        let state = state_in(dir.path());
        let wanted = AppConfig {
            language: Language::En,
            theme: Theme::Dark,
            log_level: LogLevel::Debug,
            retention_days: RetentionDays::parse(15).expect("15 is within bounds"),
        };

        let returned = state
            .replace_settings(wanted)
            .await
            .expect("the write must succeed");

        assert_eq!(returned, wanted);
        assert_eq!(state.settings().await, wanted);
        // A brand-new store reading the same directory sees the same thing:
        // this is what survives a restart (CA-001-02).
        assert_eq!(
            ConfigStore::new(dir.path())
                .load()
                .expect("reading back must succeed"),
            wanted
        );
    }

    #[tokio::test]
    async fn keeps_the_previous_preferences_when_the_write_fails() {
        let dir = tempfile::tempdir().expect("a temporary directory must be creatable");
        // A *file* where the store expects a directory: `create_dir_all` then
        // fails, which is the cheapest portable way to provoke a write error.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "not a directory").expect("writing must succeed");

        let state = state_in(&blocked);
        let wanted = AppConfig {
            language: Language::En,
            ..AppConfig::default()
        };

        let error = state
            .replace_settings(wanted)
            .await
            .expect_err("the write must fail");

        assert!(matches!(error, ConfigError::Unwritable(_)));
        assert_eq!(state.settings().await, AppConfig::default());
    }
}
