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

    /// A store this crate reads or writes could not be reached.
    ///
    /// `reason` is the implementor's own rendering, **without** its source
    /// chain: the driver error and the filesystem path it may carry stay in the
    /// trace, because what this type carries is rendered towards the interface
    /// (CLAUDE.md §4, §8).
    #[error("the journal is unavailable: {reason}")]
    Unavailable {
        /// Short, path-free summary of the underlying failure.
        reason: String,
    },
}
