//! Error type for this crate.

use crate::addressing::AddressError;
use crate::encoding::EncodingError;
use crate::ports::MessageStoreError;
use crate::retry::RetryPolicyError;
use crate::submit::SubmitBuildError;
use crate::template::{RenderError, TemplateError};

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

    /// An address did not pass validation.
    ///
    /// Reached **before** anything is persisted or sent (CA-006-07), so a
    /// message that fails this way leaves no row behind.
    #[error("an address was rejected")]
    Address(#[from] AddressError),

    /// A field of spec §7.3 does not fit the PDU.
    #[error("the submit_sm could not be built")]
    Submit(#[from] SubmitBuildError),

    /// The message journal refused a read or a write.
    ///
    /// On the write-ahead insert this means **nothing was sent**: the
    /// orchestrator does not submit a message it could not persist.
    #[error("the message journal refused the operation")]
    Store(#[from] MessageStoreError),

    /// The campaign template could not be read (spec §10.2).
    ///
    /// Raised once, when the campaign is validated, and never per recipient:
    /// a template is parsed before the first message is built.
    #[error("the message template could not be parsed")]
    Template(#[from] TemplateError),

    /// The message of **one** recipient could not be built.
    ///
    /// Separate from [`Self::Template`] because the two are acted on
    /// differently: a template failure stops the campaign, a render failure
    /// rejects one line and the campaign carries on (CA-010-06).
    #[error("the message could not be rendered for this recipient")]
    Render(#[from] RenderError),

    /// The replay settings of a campaign are not usable (spec §10.7).
    #[error("the retry policy was refused")]
    RetryPolicy(#[from] RetryPolicyError),
}
