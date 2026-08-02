//! The two ports this crate defines and neither of which it implements.
//!
//! Guide §8.1 and CLAUDE.md §3: a port is declared by the layer that
//! **consumes** it and implemented by the layer below. Milestone 006 makes
//! that real for the send path — ADR 0010 records the reasoning and the shape
//! of the dependency graph it produces.
//!
//! | Port | Implemented by | What it abstracts |
//! |------|----------------|-------------------|
//! | [`MessageRepository`] | `persistence` | the durable message journal |
//! | [`SmscSession`] | `smpp-session` | one live, bound SMPP session |
//! | [`RecipientSource`] | the IPC layer, over `contacts` | the recipients of one campaign |
//!
//! Both are consequences of the same rule and neither is optional. Without the
//! first, the write-ahead of CLAUDE.md §4 could only be tested against a real
//! SQLite file; without the second, an orchestration test would need a socket.
//! With them, `messaging` depends on `smpp-core` and on nothing else of ours,
//! and every failure path — a full disk, a throttling SMSC, a response that
//! never comes — is one line of a double.
//!
//! # Shape of the methods
//!
//! Each returns `impl Future<Output = …> + Send` rather than being an
//! `async fn`. Same thing to write against, one difference that matters: the
//! `Send` bound is part of the trait, so an implementation that is not `Send`
//! fails to compile at its definition instead of at the `tokio::spawn` three
//! layers up.

use core::future::Future;

use futures_core::stream::BoxStream;
use smpp_core::codec::{Command, Pdu};
use smpp_core::types::{ClientMessageId, Msisdn, SessionId};
use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking};

use crate::message::{Message, MessageStateUpdate};

/// Why the message journal refused a call.
///
/// # Why not the implementor's own error
///
/// `persistence` reports a `PersistenceError` carrying a `sqlx::Error` and,
/// on one variant, a filesystem path. Threading that type through this port
/// would put SQLx in the signature of a crate that must not know a database
/// exists, and would drag a path into an error `messaging` renders towards the
/// IPC boundary.
///
/// So the port names the three outcomes a caller can *act* on, and the
/// implementor maps onto them. The cost is stated rather than hidden: the
/// source chain is lost at the boundary, so the implementor is expected to log
/// the full failure — with its chain — before returning
/// [`Self::Unavailable`]. `persistence` does.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MessageStoreError {
    /// A message already carries this `client_message_id`.
    ///
    /// The guard that makes a replayed send idempotent (spec §10.5): the
    /// second insert of the same identifier fails instead of duplicating the
    /// message.
    #[error("a message with this client_message_id already exists")]
    Conflict,

    /// No message carries this `client_message_id`.
    #[error("no message carries this client_message_id")]
    NotFound,

    /// The journal could not be read or written.
    ///
    /// `reason` is the implementor's own rendering, without its source chain.
    #[error("the message journal is unavailable: {reason}")]
    Unavailable {
        /// Short, path-free summary of the underlying failure.
        reason: String,
    },
}

/// Reads and writes the durable message journal (spec §14.2, CLAUDE.md §4).
///
/// Only the half `messaging` consumes. The read-and-paginate half — paging,
/// counting, streaming a filtered set — has no consumer here: its caller is
/// the log screen and the exporter of milestone 013. It stays in
/// `persistence::ports::MessageJournal` until that crate exists to own it,
/// which is the same argument ADR 0007 made and this port has now outgrown.
pub trait MessageRepository: Send + Sync {
    /// Writes one message, **before** it is sent.
    ///
    /// # Errors
    ///
    /// [`MessageStoreError::Conflict`] if the `client_message_id` already
    /// exists, [`MessageStoreError::Unavailable`] if the write fails.
    fn insert_message(
        &self,
        message: &Message,
    ) -> impl Future<Output = Result<(), MessageStoreError>> + Send;

    /// Writes a batch of messages in **one** transaction.
    ///
    /// # Errors
    ///
    /// [`MessageStoreError::Conflict`] if any identifier already exists — and
    /// then **no** message of the batch is written.
    fn insert_messages(
        &self,
        messages: &[Message],
    ) -> impl Future<Output = Result<u64, MessageStoreError>> + Send;

    /// Reads one message by its client-side identifier.
    ///
    /// # Errors
    ///
    /// [`MessageStoreError::Unavailable`] if the read fails or a stored value
    /// no longer fits its type.
    fn find_message(
        &self,
        client_message_id: ClientMessageId,
    ) -> impl Future<Output = Result<Option<Message>, MessageStoreError>> + Send;

    /// Reads one message by the identifier the SMSC assigned, on one session.
    ///
    /// The lookup a delivery receipt needs (spec §7.8, milestone 008).
    ///
    /// # Why the session is part of the key
    ///
    /// `smsc_message_id` is unique **per message centre**, and nothing more.
    /// CLAUDE.md §1 has this application talk to "one or several SMSCs", and two
    /// providers handing out sequential identifiers both mint `1234` within the
    /// week. Without the session in the predicate, a receipt arriving on the
    /// second session finds the *first* session's message — the oldest row
    /// wins — and fails a message that has nothing to do with it. Silently, and
    /// deterministically.
    ///
    /// `None` means "any session", and is not a convenience: a message sent
    /// before its profile was deleted has a `NULL` `session_id`
    /// (`ON DELETE SET NULL`, spec §17.6), and a caller that cannot name a
    /// session must still be able to find it.
    ///
    /// # Errors
    ///
    /// [`MessageStoreError::Unavailable`] if the read fails.
    fn find_message_by_smsc_id(
        &self,
        smsc_message_id: &str,
        session_id: Option<SessionId>,
    ) -> impl Future<Output = Result<Option<Message>, MessageStoreError>> + Send;

    /// Applies one state transition, reporting whether it was **written**.
    ///
    /// `false` means the message exists and the machine of spec §14.3 refused
    /// the move — a delivery receipt for a message that already failed, most
    /// often. That is not an error and not a fault of the caller, but it is
    /// something the caller must be able to see: announcing to the interface a
    /// transition the journal declined is how a screen ends up showing `FAILED`
    /// for a row the journal holds at `DELIVERED`.
    ///
    /// # Errors
    ///
    /// [`MessageStoreError::NotFound`] if the message does not exist.
    fn update_state(
        &self,
        update: &MessageStateUpdate,
    ) -> impl Future<Output = Result<bool, MessageStoreError>> + Send;

    /// Applies a batch of state transitions in **one** transaction.
    ///
    /// All-or-nothing, and that is observable: if any message of the batch is
    /// missing, the whole batch is rolled back and **none** of the transitions
    /// applies.
    ///
    /// Returns how many transitions were **written**, which is not the length
    /// of the batch: the machine of spec §14.3 refuses some of them, and a
    /// caller told otherwise reports progress that did not happen. See
    /// [`Self::update_state`].
    ///
    /// # Errors
    ///
    /// [`MessageStoreError::NotFound`] if any message does not exist.
    fn update_states(
        &self,
        updates: &[MessageStateUpdate],
    ) -> impl Future<Output = Result<u64, MessageStoreError>> + Send;
}

/// One recipient of a campaign, as the feeder reads them (spec §10.2).
///
/// Deliberately **not** `contacts::model::Contact`: this crate does not depend
/// on `contacts` and must not start (ADR 0010 for the shape of the graph). A
/// campaign needs two things of a recipient — where to send, and what to
/// substitute into the template — and everything else a contact carries (its
/// identifier, its country, its line type, its lists) is the importer's
/// business.
///
/// The recipient sources spec §10.2 lists are three — an imported list,
/// generated numbers, a hand-typed paste — and only the first is a contact.
/// A port shaped like a contact would have made the other two adapters that
/// fabricate the fields they do not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    /// Where the message goes.
    pub destination: Msisdn,

    /// The recipient's attributes, as the JSON object of spec §11.1.
    ///
    /// Raw text rather than a decoded map, because that is exactly what the
    /// column holds and [`crate::template::Variables::from_attributes`] is what
    /// decides what a usable value is. Decoding it in the adapter would put
    /// that decision in a layer CLAUDE.md §3 keeps free of business logic.
    ///
    /// `None` is a recipient with no attributes at all, and is not a failure:
    /// every variable is missing and the campaign's
    /// [`crate::template::MissingVariablePolicy`] decides what that means.
    pub attributes: Option<String>,
}

/// Why the recipients could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RecipientSourceError {
    /// The source could not be read.
    ///
    /// `reason` is the implementor's own rendering, without its source chain
    /// and without a filesystem path (CA-001-06).
    #[error("the recipients could not be read: {reason}")]
    Unavailable {
        /// Short, path-free summary of the underlying failure.
        reason: String,
    },
}

/// Where the recipients of one campaign come from.
///
/// # Why this is a stream and not a page, a batch or a `Vec`
///
/// CA-010-01: a campaign of 500 000 recipients runs with a **stable** memory
/// footprint. A method returning the recipients would have to hold all of them,
/// and a paged one would put the paging state — and the "did the underlying set
/// change between page 3 and page 4" question — in the feeder. A stream lets the
/// implementor keep one row in flight and nothing else, which is what
/// `contacts::ports::ContactRepository::stream_contacts` already does over
/// SQLite.
///
/// # Ordering
///
/// An implementor **must** traverse the recipients in a stable order: a resumed
/// campaign re-reads the source from the beginning, and a source that reordered
/// itself between two runs would not lose the invariant — the write-ahead key is
/// derived from the recipient, not from its position — but it would make the
/// progress figures meaningless. `stream_contacts` orders by `rowid`.
pub trait RecipientSource: Send + Sync {
    /// Traverses every recipient of the campaign, one at a time.
    fn stream_recipients(&self) -> BoxStream<'_, Result<Recipient, RecipientSourceError>>;
}

/// Why a submission did not produce a response.
///
/// Distinct from a response that came back **carrying** a failure: an
/// `ESME_RTHROTTLED` is a perfectly delivered `submit_sm_resp` and reaches the
/// caller as an `Ok`. This type covers only the cases where no `command_status`
/// exists to report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SubmitError {
    /// The session is not bound right now.
    #[error("the session is not bound ({state})")]
    NotBound {
        /// The state code the session reports, e.g. `RECONNECT`.
        state: String,
    },

    /// The bind type in force does not allow submitting.
    ///
    /// Refused before the PDU leaves rather than by an `ESME_RINVBNDSTS` from
    /// the message centre.
    #[error("this session may not submit messages")]
    OperationNotAllowed,

    /// No response arrived before the profile's `response_timeout`.
    ///
    /// The message is **not** known to have failed: the SMSC may have accepted
    /// it and lost the response. Spec §10.7 is why a retry gets a new
    /// `smsc_message_id` and why [`crate::message::SmscMessageIdUpdate`] is a
    /// tri-state.
    #[error("no response within the session timeout")]
    ResponseTimeout,

    /// The session ended while the request was in flight, or is gone.
    #[error("the session closed while the request was in flight")]
    Closed,

    /// The transport or the codec failed.
    #[error("the submission failed: {reason}")]
    Transport {
        /// Short, path-free summary of the underlying failure.
        reason: String,
    },
}

impl SubmitError {
    /// Whether the implementation refused **before** the PDU reached the wire.
    ///
    /// Part of the port's contract, not a hint: an implementor returning
    /// [`Self::NotBound`] or [`Self::OperationNotAllowed`] guarantees nothing
    /// was written to the socket, and every other variant leaves it open.
    ///
    /// # What it is for
    ///
    /// The journal records `sent_at` and an attempt number, and both are
    /// claims about the wire. A message refused because the session is a
    /// receiver bind was never emitted, so stamping it with an emission
    /// instant would put a departure time on a message that never left — the
    /// log of milestone 013 would show one, and the retry budget of spec
    /// §10.7 would have spent an attempt on nothing.
    ///
    /// [`Self::ResponseTimeout`] is deliberately **not** in this set: the PDU
    /// did leave, only the answer never came. That is exactly the case spec
    /// §10.7 retries.
    #[must_use]
    pub const fn prevented_emission(&self) -> bool {
        matches!(self, Self::NotBound { .. } | Self::OperationNotAllowed)
    }
}

/// One live SMPP session, seen from the send orchestrator.
///
/// Narrow on purpose: everything a session also does — connecting, binding,
/// keep-alive, reconnection, the state machine of spec §7.9 — belongs to
/// `smpp-session` and none of it is the orchestrator's business. What the
/// orchestrator needs is *send this PDU, give me its response*, plus the two
/// settings that decide how the body is written.
///
/// # The two encoding settings, and why they are on the session
///
/// ADR 0008 and ADR 0009: how GSM 7-bit septets sit in `short_message`, and
/// what those octets mean, are properties of the **message centre**, not of
/// the message. They are columns of the session profile. A message sent on a
/// `Latin1` session must be encoded in Latin-1 or the operator's `é` arrives
/// as something else — and since the two readings agree on every ASCII
/// character, getting it wrong is invisible until a real accent goes out.
pub trait SmscSession: Send + Sync {
    /// Which session this is, for the message journal and the log span.
    fn session_id(&self) -> SessionId;

    /// How GSM 7-bit septets sit in `short_message` (ADR 0008).
    fn gsm7_packing(&self) -> Gsm7BitPacking;

    /// What those octets mean (ADR 0009).
    fn gsm7_charset(&self) -> Gsm7BitCharset;

    /// Sends one request and waits for the response that matches it.
    ///
    /// The `sequence_number` is allocated by the implementor, which is the
    /// only place that can guarantee a value still in flight is not reused.
    /// The returned [`Command`] is the response to *this* request and to no
    /// other.
    ///
    /// # Errors
    ///
    /// One of [`SubmitError`]. A response carrying a non-`ESME_ROK`
    /// `command_status` is **not** an error here: it is an answer, and the
    /// caller reads its status.
    fn submit(&self, pdu: Pdu) -> impl Future<Output = Result<Command, SubmitError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{MessageStoreError, SubmitError};

    /// CLAUDE.md §8 and CA-001-06: nothing crossing towards the interface may
    /// carry a filesystem path. Both port errors are rendered into an
    /// `ErrorDto`, so the rule is checked where the strings are built.
    #[test]
    fn a_store_failure_renders_without_leaking_its_source() {
        let error = MessageStoreError::Unavailable {
            reason: String::from("database query failed"),
        };

        assert_eq!(
            error.to_string(),
            "the message journal is unavailable: database query failed"
        );
    }

    #[test]
    fn a_timeout_is_distinguishable_from_a_closed_session() {
        assert_ne!(SubmitError::ResponseTimeout, SubmitError::Closed);
        assert_eq!(
            SubmitError::ResponseTimeout.to_string(),
            "no response within the session timeout"
        );
    }
}
