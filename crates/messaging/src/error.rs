//! Error type for this crate.

use crate::encoding::EncodingError;

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
pub enum MessagingError {
    /// Milestone 000 placeholder: this crate exposes no logic yet. Replaced
    /// by real variants as soon as its layer is implemented.
    #[error("not implemented yet")]
    NotImplemented,

    /// The text could not be encoded, or could not be split into segments.
    ///
    /// [`EncodingError`] is a type of its own rather than a handful of
    /// variants here because the callers that need to *discriminate* — the
    /// live preview, the campaign validator — only ever see encoding
    /// failures. Guide §6.1 still holds: this enum remains the single error
    /// type of the crate, and carries that one as a source.
    #[error("encoding or segmentation failed")]
    Encoding(#[from] EncodingError),
}
