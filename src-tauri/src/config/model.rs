//! Application preferences and their parsing.
//!
//! *Parse, don't validate* (CLAUDE.md §4): the raw strings arriving from the
//! WebView are turned into closed enums and a bounded newtype **once**, at the
//! boundary. Past this module no code has to wonder whether a language is
//! supported — the type says so.
//!
//! Why does [`ConfigSetInput`] carry `String`s rather than those enums? Because
//! a typed field would be rejected by `serde` *before* reaching the command,
//! and Tauri would return its own deserialisation error — an opaque string,
//! not the stable `{ code, message, details }` DTO that CA-001-05 requires.
//! Widening the input by one step is what lets the error stay ours.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::error::ConfigError;

/// Interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Language {
    /// French — the default (guide §10.2).
    Fr,
    /// English.
    En,
}

impl Language {
    /// The accepted values, in the order the interface lists them.
    pub(crate) const ALLOWED: &'static [&'static str] = &["fr", "en"];

    /// Parses a raw language tag.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidLanguage`] if the tag is not one of
    /// [`Language::ALLOWED`].
    pub(crate) fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw {
            "fr" => Ok(Self::Fr),
            "en" => Ok(Self::En),
            _ => Err(ConfigError::InvalidLanguage {
                allowed: Self::ALLOWED,
            }),
        }
    }
}

/// Interface colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Theme {
    /// Always light.
    Light,
    /// Always dark.
    Dark,
    /// Follow the operating system preference.
    System,
}

impl Theme {
    /// The accepted values, in the order the interface lists them.
    pub(crate) const ALLOWED: &'static [&'static str] = &["light", "dark", "system"];

    /// Parses a raw theme name.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidTheme`] if the name is not one of
    /// [`Theme::ALLOWED`].
    pub(crate) fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw {
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            "system" => Ok(Self::System),
            _ => Err(ConfigError::InvalidTheme {
                allowed: Self::ALLOWED,
            }),
        }
    }
}

/// Verbosity of the `tracing` subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LogLevel {
    /// Errors only.
    Error,
    /// Errors and warnings.
    Warn,
    /// Default level: significant lifecycle events.
    Info,
    /// Diagnosis level, including PDU summaries.
    Debug,
    /// Everything, including hexadecimal PDU dumps.
    Trace,
}

impl LogLevel {
    /// The accepted values, from least to most verbose.
    pub(crate) const ALLOWED: &'static [&'static str] =
        &["error", "warn", "info", "debug", "trace"];

    /// Parses a raw level name.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidLogLevel`] if the name is not one of
    /// [`LogLevel::ALLOWED`].
    pub(crate) fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(ConfigError::invalid_log_level()),
        }
    }

    /// The `EnvFilter` directive matching this level.
    pub(crate) const fn as_directive(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// How long, in days, the rolling log files are kept.
///
/// A newtype rather than a bare `u16`: the bound is an invariant of the value,
/// not a check every caller has to remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(transparent)]
pub(crate) struct RetentionDays(u16);

impl RetentionDays {
    /// Lower bound, inclusive. Zero would mean "delete straight away", which
    /// is a way of disabling logging by mistake.
    pub(crate) const MIN: u16 = 1;

    /// Upper bound, inclusive — one year.
    pub(crate) const MAX: u16 = 365;

    /// Parses a retention expressed in days.
    ///
    /// The parameter is a `u32` because that is the widest value the IPC
    /// boundary can hand over without a lossy cast; the range check narrows it.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidRetention`] outside `MIN..=MAX`.
    pub(crate) fn parse(days: u32) -> Result<Self, ConfigError> {
        let days = u16::try_from(days).map_err(|_| ConfigError::invalid_retention())?;

        if (Self::MIN..=Self::MAX).contains(&days) {
            Ok(Self(days))
        } else {
            Err(ConfigError::invalid_retention())
        }
    }

    /// The retention, in days.
    pub(crate) const fn get(self) -> u16 {
        self.0
    }
}

impl Default for RetentionDays {
    fn default() -> Self {
        Self(30)
    }
}

/// The complete set of application preferences.
///
/// This type is both the persisted shape and the IPC output DTO: a single
/// declaration, so the file and the WebView cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AppConfig {
    /// Interface language.
    pub(crate) language: Language,
    /// Colour scheme.
    pub(crate) theme: Theme,
    /// Verbosity of the logs.
    pub(crate) log_level: LogLevel,
    /// Retention of the rolling log files, in days.
    pub(crate) retention_days: RetentionDays,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            language: Language::Fr,
            theme: Theme::System,
            log_level: LogLevel::Info,
            retention_days: RetentionDays::default(),
        }
    }
}

impl AppConfig {
    /// Parses an untrusted input coming from the WebView.
    ///
    /// Fields are validated in declaration order and the **first** failure is
    /// returned: the interface constrains each control anyway, so an invalid
    /// input means a hand-crafted call, and enumerating every problem for such
    /// a caller buys nothing.
    ///
    /// # Errors
    ///
    /// The [`ConfigError`] of the first field that fails validation.
    pub(crate) fn parse(input: ConfigSetInput) -> Result<Self, ConfigError> {
        Ok(Self {
            language: Language::parse(&input.language)?,
            theme: Theme::parse(&input.theme)?,
            log_level: LogLevel::parse(&input.log_level)?,
            retention_days: RetentionDays::parse(input.retention_days)?,
        })
    }
}

/// Untrusted input of the `config_set` command.
///
/// Deliberately made of `String`s — see the module documentation.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigSetInput {
    /// Raw language tag.
    pub(crate) language: String,
    /// Raw theme name.
    pub(crate) theme: String,
    /// Raw log level name.
    pub(crate) log_level: String,
    /// Raw retention, in days.
    pub(crate) retention_days: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_supported_language() {
        // `.ok()` rather than comparing the `Result` directly: `ConfigError`
        // wraps `std::io::Error`, which is not `PartialEq`.
        assert_eq!(Language::parse("fr").ok(), Some(Language::Fr));
        assert_eq!(Language::parse("en").ok(), Some(Language::En));
    }

    #[test]
    fn rejects_an_unknown_language_with_the_allowed_values() {
        let error = Language::parse("kl").expect_err("an unknown language must be rejected");

        assert!(
            matches!(error, ConfigError::InvalidLanguage { allowed } if allowed == Language::ALLOWED)
        );
    }

    #[test]
    fn rejects_an_unknown_theme() {
        let error = Theme::parse("neon").expect_err("an unknown theme must be rejected");

        assert!(matches!(error, ConfigError::InvalidTheme { .. }));
    }

    #[test]
    fn rejects_an_unknown_log_level() {
        let error = LogLevel::parse("verbose").expect_err("an unknown level must be rejected");

        assert!(matches!(error, ConfigError::InvalidLogLevel { .. }));
    }

    #[test]
    fn accepts_retention_at_both_bounds() {
        assert!(RetentionDays::parse(u32::from(RetentionDays::MIN)).is_ok());
        assert!(RetentionDays::parse(u32::from(RetentionDays::MAX)).is_ok());
    }

    #[test]
    fn rejects_retention_outside_the_bounds() {
        assert!(RetentionDays::parse(0).is_err());
        assert!(RetentionDays::parse(u32::from(RetentionDays::MAX) + 1).is_err());
        assert!(RetentionDays::parse(u32::MAX).is_err());
    }

    #[test]
    fn parses_a_complete_valid_input() {
        let input = ConfigSetInput {
            language: "en".to_owned(),
            theme: "dark".to_owned(),
            log_level: "debug".to_owned(),
            retention_days: 90,
        };

        let config = AppConfig::parse(input).expect("a fully valid input must parse");

        assert_eq!(config.language, Language::En);
        assert_eq!(config.theme, Theme::Dark);
        assert_eq!(config.log_level, LogLevel::Debug);
        assert_eq!(config.retention_days.get(), 90);
    }

    #[test]
    fn reports_the_first_invalid_field_of_an_input() {
        let input = ConfigSetInput {
            language: "kl".to_owned(),
            theme: "neon".to_owned(),
            log_level: "verbose".to_owned(),
            retention_days: 0,
        };

        let error = AppConfig::parse(input).expect_err("an invalid input must be rejected");

        assert!(matches!(error, ConfigError::InvalidLanguage { .. }));
    }

    #[test]
    fn defaults_are_french_system_theme_and_info_level() {
        let config = AppConfig::default();

        assert_eq!(config.language, Language::Fr);
        assert_eq!(config.theme, Theme::System);
        assert_eq!(config.log_level, LogLevel::Info);
    }

    #[test]
    fn round_trips_through_json() {
        let config = AppConfig::default();
        let json = serde_json::to_string(&config).expect("serialisation must succeed");
        let parsed: AppConfig = serde_json::from_str(&json).expect("deserialisation must succeed");

        assert_eq!(config, parsed);
    }

    #[test]
    fn serialises_field_names_in_camel_case_for_the_ipc_boundary() {
        let json = serde_json::to_string(&AppConfig::default()).expect("serialisation");

        assert!(json.contains("\"logLevel\""), "unexpected payload: {json}");
        assert!(
            json.contains("\"retentionDays\""),
            "unexpected payload: {json}"
        );
    }
}
