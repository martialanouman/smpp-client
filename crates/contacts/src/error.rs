//! Error type for this crate.

use crate::import::MappingError;
use crate::ports::ContactStoreError;

/// Errors produced by this crate.
///
/// Per guide §6.1, every crate exposes **one** exhaustive `thiserror` type.
/// No public API returns a `Box<dyn Error>`: callers must be able to
/// discriminate between cases.
///
/// # No path, ever
///
/// A variant that carried the file being imported would put an absolute path
/// into an error rendered towards the IPC boundary, which CA-001-06 forbids.
/// The I/O variants therefore carry the operating system's own message and the
/// line it happened on, and the *file* is something the interface already knows
/// — it is the one the operator just chose in the dialog.
///
/// `#[non_exhaustive]` lets later milestones add variants without breaking
/// `match` expressions in calling crates.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ContactsError {
    /// The file could not be read.
    #[error("the file could not be read: {reason}")]
    Read {
        /// The operating system's message, without a path.
        reason: String,
    },

    /// A row could not be parsed.
    #[error("line {line} could not be parsed: {reason}")]
    Malformed {
        /// The line the parser stopped on.
        line: u64,
        /// What the parser objected to, without any cell value.
        reason: String,
    },

    /// The workbook holds no sheet by that name.
    #[error("the workbook has no sheet named {sheet:?}")]
    UnknownSheet {
        /// The name that was asked for.
        sheet: String,
    },

    /// The workbook holds no sheet at all.
    #[error("the workbook holds no sheet")]
    EmptyWorkbook,

    /// The mapping does not fit the file.
    #[error(transparent)]
    Mapping(#[from] MappingError),

    /// The contact store refused a write.
    #[error(transparent)]
    Store(#[from] ContactStoreError),

    /// The operator cancelled the import (CA-009-10).
    ///
    /// Not a failure: the batches committed before the cancellation are the
    /// import's result, and the report says how many there were.
    #[error("the import was cancelled")]
    Cancelled,

    /// An argument the caller built is not usable.
    #[error("{reason}")]
    Invalid {
        /// What is wrong with it.
        reason: String,
    },
}

impl ContactsError {
    /// Wraps an I/O failure, keeping the message and dropping any path.
    pub(crate) fn read(error: std::io::Error) -> Self {
        Self::Read {
            reason: error.kind().to_string(),
        }
    }

    /// Wraps a spreadsheet failure.
    ///
    /// `calamine`'s errors quote the internal zip entry, never a filesystem
    /// path, but they are rendered through `Display` here rather than kept as
    /// a source so the boundary rule holds whatever a future version adds.
    pub(crate) fn spreadsheet<E: core::fmt::Display>(error: E) -> Self {
        Self::Read {
            reason: error.to_string(),
        }
    }

    /// Wraps a parse failure at a known line.
    pub(crate) fn read_at<E: core::fmt::Display>(line: u64, error: &E) -> Self {
        Self::Malformed {
            line,
            reason: error.to_string(),
        }
    }

    /// Reports a caller mistake.
    pub(crate) fn invalid(reason: &str) -> Self {
        Self::Invalid {
            reason: reason.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ContactsError;

    /// CA-001-06: an error crossing the IPC boundary carries no filesystem
    /// path. `std::io::Error::to_string` on an error opening a file does not
    /// include one either, but `kind()` cannot, which is why it is used.
    #[test]
    fn a_read_failure_carries_no_path() {
        let error = ContactsError::read(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "/Users/someone/clients.csv: no such file",
        ));

        let rendered = error.to_string();

        assert!(!rendered.contains('/'), "{rendered}");
        assert!(rendered.contains("not found"), "{rendered}");
    }

    #[test]
    fn a_parse_failure_names_the_line_it_stopped_on() {
        let error = ContactsError::read_at(4_500, &"unequal lengths");

        assert!(error.to_string().contains("4500"));
    }
}
