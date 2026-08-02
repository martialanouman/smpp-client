//! Encoding, segmentation and send orchestration.
//!
//! The business entry point for sending: picks the DCS, segments long
//! messages (UDH or `sar_*` TLVs), persists before sending, then hands PDUs
//! to a live session through [`ports::SmscSession`]. Also orchestrates bulk
//! campaigns — resume after interruption, progress tracking, volume caps.
//!
//! Declares the *ports* the layers below implement ([`ports`]): the traits
//! belong to this layer, their SQLx and socket implementations to the lower
//! ones. That is what makes the orchestrator testable with neither a database
//! nor a socket, and it is why this crate depends on `smpp-core` and on
//! nothing else of ours (ADR 0010).
//!
//! Implemented at milestones 004, 006 and 010.
//!
//! # Milestone 004 — encoding and segmentation
//!
//! Text in, segments out, no I/O at all. Two modules:
//!
//! * [`encoding`] settles the encoding (spec §7.5: GSM 7-bit when the text
//!   allows, UCS2 otherwise, or whatever the user forced), writes the octets,
//!   and answers the live counter the editor needs;
//! * [`segmentation`] cuts a long message into parts and gives each one the
//!   concatenation information its mode calls for.
//!
//! Both take the same
//! [`SegmentationOptions`](segmentation::SegmentationOptions), which carry
//! what the message centre expects rather than what the message contains.
//!
//! ```
//! use messaging::{
//!     encoding::Encoding,
//!     segmentation::{segment, ConcatenationReference, SegmentationOptions},
//! };
//!
//! let text = "a".repeat(161);
//! let split = segment(
//!     &text,
//!     &SegmentationOptions::default(),
//!     ConcatenationReference::new(42),
//! )?;
//!
//! assert_eq!(split.encoding(), Encoding::Gsm7Bit);
//! assert_eq!(split.segments().len(), 2);
//! // Septets, not characters: `€` would count for two.
//! assert_eq!(split.segments()[0].content_units(), 153);
//! assert_eq!(split.segments()[1].content_units(), 8);
//! # Ok::<(), messaging::encoding::EncodingError>(())
//! ```
//!
//! # Milestone 010 — campaigns: the decisions, before the machinery
//!
//! Three modules, none of which touches a database, a session or a clock, so
//! all three are decided and tested without a runtime:
//!
//! * [`template`] resolves `{{prenom}}` per recipient and guarantees that no
//!   text holding an unresolved placeholder ever leaves it (CA-010-06);
//! * [`retry`] answers "send this message again?" from the `command_status`
//!   classification of milestone 003, and says how long to wait — as a pure
//!   function of the attempt number (CA-010-07);
//! * [`campaign`] holds the lifecycle of spec §10.3 and refuses the transitions
//!   it does not allow.
//!
//! What feeds a campaign from the database, what resumes it after a crash and
//! what puts a `submit_multi` on the wire (L-010-02, L-010-04, L-010-06) is
//! built on top of these and is not here yet.

pub mod addressing;
pub mod campaign;
pub mod correlation;
pub mod dlr;
pub mod encoding;
mod error;
pub mod message;
pub mod ports;
pub mod retry;
pub mod segmentation;
pub mod sender;
pub mod submit;
pub mod template;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use campaign::{CampaignStatus, InvalidCampaignTransition};
pub use correlation::{Correlated, Correlator, OrphanReason, OrphanReceipt, OrphanReceiptStore};
pub use dlr::{DeliveryReceipt, DeliveryStatus, Incoming};
pub use error::MessagingError;
pub use message::{Message, MessageState, MessageStateUpdate, SmscMessageIdUpdate};
pub use ports::{
    MessageRepository, MessageStoreError, Recipient, RecipientSource, RecipientSourceError,
    SmscSession, SubmitError,
};
pub use retry::{
    GiveUpReason, RetryBackoff, RetryDecision, RetryPolicy, RetryPolicyError, SendFailure,
};
pub use sender::{SegmentOutcome, SendObserver, SendReport, SendRequest, Sender};
pub use template::{MissingVariablePolicy, RenderError, Template, TemplateError, Variables};

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!messaging::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::MessagingError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            MessagingError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
