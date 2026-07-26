//! Persistence of the preferences in a configuration file.
//!
//! A JSON file, not SQLite. The preferences are read **before** anything else
//! starts — they drive the log level, which the database layer itself needs to
//! report. Making them depend on a database that milestone 002 has yet to
//! introduce would invert that order.
//!
//! The store owns exactly one directory and one file name. It never composes a
//! path from anything the WebView sends, which is what makes CA-001-02
//! ("in the standard OS directory and nowhere else") a property of the type
//! rather than a discipline.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use super::error::ConfigError;
use super::model::AppConfig;

/// Reads and writes the preferences of a single application directory.
#[derive(Debug, Clone)]
pub(crate) struct ConfigStore {
    directory: PathBuf,
}

impl ConfigStore {
    /// Name of the file inside the application configuration directory.
    pub(crate) const FILE_NAME: &'static str = "config.json";

    /// Binds a store to an application configuration directory.
    ///
    /// The directory does not need to exist yet: [`ConfigStore::save`] creates
    /// it.
    pub(crate) fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Full path of the configuration file.
    pub(crate) fn file_path(&self) -> PathBuf {
        self.directory.join(Self::FILE_NAME)
    }

    /// Loads the preferences, falling back to the defaults on first run.
    ///
    /// A missing file is **not** an error: it is what a fresh installation
    /// looks like. A file that exists but cannot be parsed is one, because
    /// silently overwriting it would destroy a user's settings on a transient
    /// read problem.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::Unreadable`] on an I/O failure other than "not found";
    /// - [`ConfigError::Malformed`] if the content is not the expected JSON.
    pub(crate) fn load(&self) -> Result<AppConfig, ConfigError> {
        let raw = match fs::read_to_string(self.file_path()) {
            Ok(raw) => raw,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(AppConfig::default()),
            Err(error) => return Err(ConfigError::Unreadable(error)),
        };

        serde_json::from_str(&raw).map_err(ConfigError::Malformed)
    }

    /// Writes the preferences, creating the directory if needed.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Unwritable`] if the directory cannot be created or the
    /// file cannot be written.
    pub(crate) fn save(&self, config: &AppConfig) -> Result<(), ConfigError> {
        fs::create_dir_all(&self.directory).map_err(ConfigError::Unwritable)?;

        // `to_string_pretty` cannot fail on `AppConfig` — no map with
        // non-string keys, no custom `Serialize`. Mapping it anyway avoids an
        // `expect`, which the workspace lints forbid outside tests.
        let serialised = serde_json::to_string_pretty(config)
            .map_err(|error| ConfigError::Unwritable(std::io::Error::other(error)))?;

        fs::write(self.file_path(), serialised).map_err(ConfigError::Unwritable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{Language, LogLevel, RetentionDays, Theme};

    fn temp_store() -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().expect("a temporary directory must be creatable");
        let store = ConfigStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn puts_the_configuration_file_inside_the_given_directory_and_nowhere_else() {
        let (dir, store) = temp_store();

        assert_eq!(store.file_path(), dir.path().join(ConfigStore::FILE_NAME));
        assert!(store.file_path().starts_with(dir.path()));
    }

    #[test]
    fn loads_the_defaults_when_no_file_exists_yet() {
        let (_dir, store) = temp_store();

        let config = store.load().expect("a missing file must not be an error");

        assert_eq!(config, AppConfig::default());
    }

    #[test]
    fn saves_then_loads_the_same_configuration() {
        let (_dir, store) = temp_store();
        let config = AppConfig {
            language: Language::En,
            theme: Theme::Dark,
            log_level: LogLevel::Trace,
            retention_days: RetentionDays::parse(7).expect("7 is within bounds"),
        };

        store.save(&config).expect("saving must succeed");

        assert_eq!(store.load().expect("loading must succeed"), config);
    }

    #[test]
    fn writes_nothing_outside_its_own_directory() {
        let (dir, store) = temp_store();

        store
            .save(&AppConfig::default())
            .expect("saving must succeed");

        let written: Vec<_> = fs::read_dir(dir.path())
            .expect("the directory must be readable")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();

        assert_eq!(
            written,
            vec![std::ffi::OsString::from(ConfigStore::FILE_NAME)]
        );
    }

    #[test]
    fn creates_the_directory_when_it_does_not_exist() {
        let dir = tempfile::tempdir().expect("a temporary directory must be creatable");
        let nested = dir.path().join("nested").join("deeper");
        let store = ConfigStore::new(&nested);

        store
            .save(&AppConfig::default())
            .expect("saving must create the directory");

        assert!(nested.join(ConfigStore::FILE_NAME).is_file());
    }

    #[test]
    fn reports_a_malformed_file_instead_of_panicking() {
        let (_dir, store) = temp_store();
        fs::write(store.file_path(), "{ not json").expect("writing must succeed");

        let error = store.load().expect_err("a malformed file must be reported");

        assert!(matches!(error, ConfigError::Malformed(_)));
    }
}
