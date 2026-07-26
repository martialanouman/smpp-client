//! Typed configuration errors.
//!
//! Every `#[error(...)]` string below is a **fixed, path-free sentence**. That
//! is not a stylistic choice: these strings become the `message` of the
//! [`crate::error::ErrorDto`] handed to the WebView, and CA-001-06 forbids an
//! absolute path, a secret or an internal trace from crossing that boundary.
//!
//! The variable part of the diagnosis — which file, which OS error — is
//! attached as a `#[source]` and goes to `tracing`, never to the frontend.

use super::model::{LogLevel, RetentionDays};

/// Anything that can go wrong while reading, writing or validating the
/// application preferences.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// The submitted language is not one of the supported ones.
    #[error("unsupported language")]
    InvalidLanguage {
        /// The accepted values, so the frontend can report them without
        /// duplicating the list.
        allowed: &'static [&'static str],
    },

    /// The submitted theme is not one of the supported ones.
    #[error("unsupported theme")]
    InvalidTheme {
        /// The accepted values.
        allowed: &'static [&'static str],
    },

    /// The submitted log level is not one of the supported ones.
    #[error("unsupported log level")]
    InvalidLogLevel {
        /// The accepted values.
        allowed: &'static [&'static str],
    },

    /// The submitted retention falls outside the accepted range.
    #[error("log retention outside the accepted range")]
    InvalidRetention {
        /// Lower bound, in days, inclusive.
        min: u16,
        /// Upper bound, in days, inclusive.
        max: u16,
    },

    /// The configuration file exists but could not be read.
    #[error("the configuration file could not be read")]
    Unreadable(#[source] std::io::Error),

    /// The configuration file could not be written.
    #[error("the configuration file could not be written")]
    Unwritable(#[source] std::io::Error),

    /// The configuration file is not valid JSON, or does not match the
    /// expected shape.
    #[error("the configuration file is malformed")]
    Malformed(#[source] serde_json::Error),
}

impl ConfigError {
    /// The invalid log level error, pre-filled with the accepted values.
    pub(crate) const fn invalid_log_level() -> Self {
        Self::InvalidLogLevel {
            allowed: LogLevel::ALLOWED,
        }
    }

    /// The out-of-range retention error, pre-filled with the bounds.
    pub(crate) const fn invalid_retention() -> Self {
        Self::InvalidRetention {
            min: RetentionDays::MIN,
            max: RetentionDays::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{Language, Theme};

    #[test]
    fn no_message_leaks_a_filesystem_path() {
        let source = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "/Users/someone/Library/Application Support/com.shinobismpp.desktop/config.json",
        );

        let errors = [
            ConfigError::InvalidLanguage {
                allowed: Language::ALLOWED,
            },
            ConfigError::InvalidTheme {
                allowed: Theme::ALLOWED,
            },
            ConfigError::invalid_log_level(),
            ConfigError::invalid_retention(),
            ConfigError::Unreadable(source),
            ConfigError::Unwritable(std::io::Error::from(std::io::ErrorKind::NotFound)),
        ];

        for error in errors {
            let rendered = error.to_string();
            assert!(!rendered.contains('/'), "path leaked: {rendered}");
            assert!(!rendered.contains('\\'), "path leaked: {rendered}");
        }
    }
}
