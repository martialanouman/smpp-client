//! Business activity log and CSV, XLSX and JSON exports.
//!
//! Distinct from technical logging (`tracing`): this crate produces the
//! *functional* log the user consults — sends, delivery receipts, session
//! transitions — and its exports.
//!
//! Confidentiality constraint (CLAUDE.md §8): message content is masked or
//! truncated by default in any log meant to be shared, and hexadecimal PDU
//! dumps stay behind an explicit debug mode.
//!
//! Implemented at milestone 014.

mod error;

pub use error::LoggingExportError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!logging_export::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::LoggingExportError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            LoggingExportError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
