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
//! On top of them sits the machinery that actually runs one:
//!
//! * [`campaign::feeder`] reads the recipients in **streaming** and fills a
//!   **bounded** queue, so a message centre that slows down slows the reading
//!   down rather than filling memory (L-010-02, CA-010-01);
//! * [`campaign::resume`] holds the invariant of this milestone — *at most one
//!   accepted message per recipient* — through a write-ahead key derived from
//!   the campaign and the recipient, and a state check before every emission
//!   (L-010-04, CA-010-04, CA-010-05);
//! * [`campaign::control`] carries start, pause, resume and cancel to every task
//!   of one campaign;
//! * [`campaign::schedule`] answers when a campaign may send — a deferred start
//!   and a daily window, midnight crossings and time zones included (CA-010-10);
//! * [`campaign::runner`] ties them together and counts what happened;
//! * [`campaign::progress`] is what an observer reads of a campaign that has
//!   **not** finished — the counters a progress bar needs, published on every
//!   item and sampled at whatever cadence the reader chooses (L-010-07).
//!
//! * [`submit_multi`] batches the recipients that share a body into one PDU and
//!   falls back onto individual `submit_sm` when the message centre refuses the
//!   operation, without losing a recipient (L-010-06, CA-010-08). Its header
//!   states the consequence the milestone does **not** settle: one identifier
//!   for N messages means a batched message's delivery receipt cannot be
//!   correlated.
//!
//! The campaign runner does not batch yet — see [`submit_multi`]'s header.

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
pub mod submit_multi;
pub mod template;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use campaign::control::{CampaignControl, ControlHandle, Resumption, RunState};
pub use campaign::feeder::{Fed, FeedItem, FeedRejection, FeedSummary, Feeder};
pub use campaign::progress::{
    AcceptanceRate, CampaignProgress, CampaignReading, RATE_WINDOW_SECONDS,
};
pub use campaign::resume::{message_key, Admission, EmissionGuard, SkipReason, UnansweredPolicy};
pub use campaign::runner::{
    CampaignOutcome, CampaignPlan, CampaignRunner, CampaignSummary, CampaignTally, StartMode,
};
pub use campaign::schedule::{DailyWindow, Schedule, ScheduleDecision, ScheduleError};
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
pub use submit_multi::{
    build_submit_multi, match_refusals, read_multi_response, Batch, BatchRecipient, BatchReport,
    BatchSender, FallbackReason, MultiResponse, MultiSupport, MultiSupportState, RecipientOutcome,
    RecipientReport, Refusal, Via, MAX_DESTINATIONS,
};
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
