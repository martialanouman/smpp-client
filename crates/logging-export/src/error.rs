//! Error type for this crate.

/// Errors produced by this crate.
///
/// Per guide §6.1, every crate exposes **one** exhaustive `thiserror` type.
/// No public API returns a `Box<dyn Error>`: callers must be able to
/// discriminate between cases.
///
/// `#[non_exhaustive]` lets later milestones add variants without breaking
/// `match` expressions in calling crates.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoggingExportError {
    /// Milestone 000 placeholder: this crate exposes no logic yet. Replaced
    /// by real variants as soon as its layer is implemented.
    #[error("not implemented yet")]
    NotImplemented,
}
