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
//! # What is here, and when
//!
//! | Module | Milestone | Contents |
//! |--------|-----------|----------|
//! | [`journal`] | 008 | the paginated business log the interface reads |
//! | [`pdu_log`] | 008 | the PDU recorder, off unless explicitly enabled |
//!
//! The exports themselves — CSV, XLSX, JSON — the aggregate statistics and the
//! retention policy are milestone 014's, and step-008 §2 puts all three out of
//! scope.

pub mod journal;
pub mod pdu_log;

mod error;

pub use error::LoggingExportError;
pub use journal::{ContentVisibility, Journal, JournalPage, OrphanPage, MAX_PAGE};
pub use pdu_log::{PduRecorder, PduSink};

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

    /// CLAUDE.md §8 and CA-001-06: nothing crossing towards the interface may
    /// carry a filesystem path, so the rule is checked where the string is
    /// built rather than where it is rendered.
    #[test]
    fn a_store_failure_renders_without_leaking_its_source() {
        let error = LoggingExportError::Unavailable {
            reason: String::from("database query failed"),
        };

        assert_eq!(
            error.to_string(),
            "the journal is unavailable: database query failed"
        );
    }
}
