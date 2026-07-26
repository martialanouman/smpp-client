//! The message aggregate and its state machine (deliverable L-006-02).
//!
//! # Why this lives here and not in `persistence`
//!
//! It was written at milestone 002 next to the SQLx code that stores it, and
//! ADR 0007 said so explicitly: the consuming crate was an empty shell, so
//! paying the cost of the inversion — an upward `persistence` → `messaging`
//! edge — would have bought nothing. Milestone 006 is the deadline that ADR
//! set itself, and this is the move. ADR 0010 records it.
//!
//! What follows from the move is the point: `messaging` owns the lifecycle of
//! a message, so it owns the type that carries it. [`MessageState`] and its
//! legal transitions are stated **once**, here, above the storage rather than
//! inside it. `persistence` re-exports every type of this module, so
//! `persistence::Message` still resolves and no call site outside the two
//! crates changed.

use smpp_core::time::Timestamp;
use smpp_core::types::{CampaignId, ClientMessageId, Msisdn, SessionId};
use smpp_core::values::{CommandStatus, DataCoding, Npi, Ton};

/// Where a message stands in the lifecycle of spec §14.3.
///
/// `QUEUED` → `SENT` → `ACCEPTED` → `DELIVERED` | `FAILED` | `EXPIRED`, with a
/// rejected `submit_sm_resp` jumping straight from `SENT` to `FAILED`.
///
/// `persistence` stores whichever state it is handed and does not police the
/// transitions; [`Self::can_move_to`] is where the machine is stated. What the
/// storage does guarantee is that no value outside this set reaches the
/// column — the enum on the way in, a `CHECK` constraint on the file itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum MessageState {
    /// Persisted, not yet handed to a session. The write-ahead state.
    Queued,
    /// `submit_sm` has left.
    Sent,
    /// `submit_sm_resp` came back clean, with an SMSC message identifier.
    Accepted,
    /// A delivery receipt reported success.
    Delivered,
    /// Rejected by the SMSC, or a delivery receipt reported failure.
    Failed,
    /// The SMSC gave up before the validity period ran out.
    Expired,
}

impl MessageState {
    /// Every variant, in lifecycle order.
    pub const ALL: &'static [Self] = &[
        Self::Queued,
        Self::Sent,
        Self::Accepted,
        Self::Delivered,
        Self::Failed,
        Self::Expired,
    ];

    /// The text form stored in SQLite (spec §14.2) and shown by the interface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "QUEUED",
            Self::Sent => "SENT",
            Self::Accepted => "ACCEPTED",
            Self::Delivered => "DELIVERED",
            Self::Failed => "FAILED",
            Self::Expired => "EXPIRED",
        }
    }

    /// Parses the text form, or `None` when the text names no known state.
    ///
    /// Returns an [`Option`] rather than an error on purpose: the two callers
    /// want different errors out of the same failure — `persistence` reports
    /// which column of which table could not be read, the IPC boundary reports
    /// a rejected input — and an error type chosen here would be wrong for one
    /// of them.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|state| state.as_str() == raw)
    }

    /// Reports whether no further transition is expected.
    ///
    /// A resumed campaign (spec §10.5) restarts from the messages that are
    /// *not* terminal; anything else would re-send what already went out.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Failed | Self::Expired)
    }

    /// Reports whether `next` is a legal successor of this state.
    ///
    /// The machine of spec §14.3, stated once. Three properties it holds, and
    /// each one is a bug it prevents:
    ///
    /// * a **terminal** state has no successor — a late delivery receipt for a
    ///   message already `FAILED` must not resurrect it;
    /// * a state may always move to **itself** — the same transition replayed
    ///   after a crash has to be a no-op, which is the idempotence CLAUDE.md §4
    ///   requires;
    /// * nothing moves **backwards** — an `ACCEPTED` message never returns to
    ///   `SENT`, so a response arriving out of order cannot undo progress.
    #[must_use]
    pub const fn can_move_to(self, next: Self) -> bool {
        match (self, next) {
            // A committed transition replayed after a crash lands here.
            (Self::Queued, Self::Queued)
            | (Self::Sent, Self::Sent)
            | (Self::Accepted, Self::Accepted)
            | (Self::Delivered, Self::Delivered)
            | (Self::Failed, Self::Failed)
            | (Self::Expired, Self::Expired) => true,
            // Nothing was sent, so only a local refusal can end it.
            (Self::Queued, Self::Sent | Self::Failed) => true,
            // A `submit_sm_resp` either assigns an identifier or rejects.
            (Self::Sent, Self::Accepted | Self::Failed | Self::Expired) => true,
            // From here on it is the delivery receipt that speaks.
            (Self::Accepted, Self::Delivered | Self::Failed | Self::Expired) => true,
            _ => false,
        }
    }
}

impl core::fmt::Display for MessageState {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A message, written **before** it is sent (spec §14.2, CLAUDE.md §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Primary key, minted by this application before any PDU leaves. It is
    /// what makes a replay after a crash idempotent (spec §10.5).
    pub client_message_id: ClientMessageId,
    /// Campaign this message belongs to, if any.
    pub campaign_id: Option<CampaignId>,
    /// Session it was, or will be, sent on.
    pub session_id: Option<SessionId>,
    /// Identifier the SMSC assigned in `submit_sm_resp`. Absent until then,
    /// and possibly for ever.
    pub smsc_message_id: Option<String>,
    /// Sender address. A `String` rather than an `Msisdn`: a source address is
    /// frequently alphanumeric (a sender ID), which is not a subscriber
    /// number.
    pub source_addr: Option<String>,
    /// Type of number of the sender address.
    pub source_ton: Option<Ton>,
    /// Numbering plan indicator of the sender address.
    pub source_npi: Option<Npi>,
    /// Recipient number.
    pub dest_addr: Option<Msisdn>,
    /// Type of number of the recipient address.
    pub dest_ton: Option<Ton>,
    /// Numbering plan indicator of the recipient address.
    pub dest_npi: Option<Npi>,
    /// Data coding scheme of the payload (spec §7.5).
    pub data_coding: Option<DataCoding>,
    /// Number of segments the payload was split into.
    pub segments: u32,
    /// Message body.
    pub text: Option<String>,
    /// Where the message stands in the lifecycle of spec §14.3.
    pub state: MessageState,
    /// `command_status` of the response, when one came back.
    pub command_status: Option<CommandStatus>,
    /// `stat` field of the delivery receipt.
    pub dlr_stat: Option<String>,
    /// `err` field of the delivery receipt.
    pub dlr_err: Option<String>,
    /// How many times sending was attempted (spec §10.7).
    pub attempts: u32,
    /// When the row was written — before sending, by definition.
    pub created_at: Timestamp,
    /// When `submit_sm` left.
    pub sent_at: Option<Timestamp>,
    /// When `submit_sm_resp` came back.
    pub resp_at: Option<Timestamp>,
    /// When the delivery receipt arrived.
    pub dlr_at: Option<Timestamp>,
}

/// What a transition does to the identifier the SMSC assigned.
///
/// # Why this column, and only this one, needs a tri-state
///
/// Every other optional field of [`MessageStateUpdate`] merges: `None` means
/// "leave what is there", because two transitions bring disjoint facts and
/// neither should erase the other's.
///
/// `smsc_message_id` is different: its value can legitimately **change**. Spec
/// §10.7 has a `submit_sm` time out and be retried; the SMSC assigns a new
/// identifier to the retry. If the late response to the first attempt lands
/// first, a merging update would pin the stale identifier for ever — the
/// second attempt's response could not overwrite it, its delivery receipt
/// would never correlate (spec §7.8), and the message would sit in `ACCEPTED`
/// while `delivered_count` drifted.
///
/// So the caller says which it means. There is deliberately **no** `Clear`
/// variant: nothing in the protocol un-assigns an identifier, and a variant
/// nobody can justify is a variant somebody will eventually misuse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SmscMessageIdUpdate {
    /// Leave whatever the column holds.
    #[default]
    Keep,
    /// Write this identifier, over an existing one if there is one.
    Set(String),
}

/// One state transition to apply to one message.
///
/// # Merge, not overwrite
///
/// Spec §14.3 has a transition arrive with only part of the picture: a
/// `submit_sm_resp` brings a response instant, a delivery receipt brings
/// `dlr_stat` and a receipt instant, and neither should erase what the other
/// wrote. So a `None` here means **leave the column as it is**, not "set it to
/// NULL".
///
/// # Idempotence
///
/// CLAUDE.md §4 requires a transition to be replayable: a batch may be
/// committed and then reapplied after a crash, and the second application must
/// leave the row exactly as the first did. Merging gives that for free on the
/// optional fields. The two fields where it does not come for free are called
/// out where they are declared — [`Self::attempt`], a **number** rather than
/// an increment, and [`Self::smsc_message_id`], an explicit
/// [`SmscMessageIdUpdate`].
///
/// Build one with [`Self::new`] and the `with_*` methods rather than a struct
/// literal, so a field added by a later milestone does not break every call
/// site.
// NO `derive(Default)`: it would require one on `ClientMessageId`, which
// `smpp-core` refuses for the reason spelled out there — a defaulted
// identifier in a struct literal is a fabricated one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageStateUpdate {
    /// Which message to update.
    pub client_message_id: ClientMessageId,
    /// The state to move to.
    pub state: MessageState,
    /// What to do with the identifier the SMSC assigned.
    pub smsc_message_id: SmscMessageIdUpdate,
    /// Response status, when this transition carries one.
    pub command_status: Option<CommandStatus>,
    /// `stat` field of the delivery receipt.
    pub dlr_stat: Option<String>,
    /// `err` field of the delivery receipt.
    pub dlr_err: Option<String>,
    /// When `submit_sm` left.
    pub sent_at: Option<Timestamp>,
    /// When the response came back.
    pub resp_at: Option<Timestamp>,
    /// When the delivery receipt arrived.
    pub dlr_at: Option<Timestamp>,
    /// Which sending attempt this transition belongs to, counting from 1.
    ///
    /// A **number**, not a flag, and stored as `MAX(attempts, ?)` rather than
    /// `attempts + 1`. An increment is not replayable: reapplying a committed
    /// batch after a crash would count every message of it one attempt too
    /// high, and the retry budget of spec §10.7 would be silently cut. Two
    /// applications of "this was attempt 2" leave 2.
    pub attempt: Option<u32>,
}

impl MessageStateUpdate {
    /// A transition to `state`, carrying nothing else.
    #[must_use]
    pub const fn new(client_message_id: ClientMessageId, state: MessageState) -> Self {
        Self {
            client_message_id,
            state,
            smsc_message_id: SmscMessageIdUpdate::Keep,
            command_status: None,
            dlr_stat: None,
            dlr_err: None,
            sent_at: None,
            resp_at: None,
            dlr_at: None,
            attempt: None,
        }
    }

    /// Writes the identifier the SMSC assigned, replacing any earlier one.
    ///
    /// Replacing is the point: see [`SmscMessageIdUpdate`].
    #[must_use]
    pub fn with_smsc_message_id(mut self, smsc_message_id: impl Into<String>) -> Self {
        self.smsc_message_id = SmscMessageIdUpdate::Set(smsc_message_id.into());
        self
    }

    /// Records the response status.
    #[must_use]
    pub const fn with_command_status(mut self, command_status: CommandStatus) -> Self {
        self.command_status = Some(command_status);
        self
    }

    /// Records the delivery receipt fields.
    #[must_use]
    pub fn with_delivery_receipt(mut self, stat: impl Into<String>, err: Option<String>) -> Self {
        self.dlr_stat = Some(stat.into());
        self.dlr_err = err;
        self
    }

    /// Records when `submit_sm` left, and which attempt it was.
    ///
    /// The attempt number is not optional here on purpose: a send that does
    /// not say which attempt it is cannot be counted idempotently, and a
    /// caller that has to pass `1` explicitly is a caller that has thought
    /// about the retry case.
    #[must_use]
    pub const fn sent_at(mut self, instant: Timestamp, attempt: u32) -> Self {
        self.sent_at = Some(instant);
        self.attempt = Some(attempt);
        self
    }

    /// Records when the response came back.
    #[must_use]
    pub const fn responded_at(mut self, instant: Timestamp) -> Self {
        self.resp_at = Some(instant);
        self
    }

    /// Records when the delivery receipt arrived.
    #[must_use]
    pub const fn receipt_at(mut self, instant: Timestamp) -> Self {
        self.dlr_at = Some(instant);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{MessageState, MessageStateUpdate, SmscMessageIdUpdate};
    use smpp_core::time::Timestamp;
    use smpp_core::types::ClientMessageId;

    #[test]
    fn every_state_parses_back_from_its_stored_form() {
        for state in MessageState::ALL {
            assert_eq!(MessageState::parse(state.as_str()), Some(*state));
        }
    }

    #[test]
    fn the_stored_text_matches_the_specification() {
        assert_eq!(MessageState::Queued.as_str(), "QUEUED");
        assert_eq!(MessageState::Accepted.as_str(), "ACCEPTED");
    }

    #[test]
    fn an_unknown_state_is_not_parsed() {
        assert_eq!(MessageState::parse("PENDING"), None);
        assert_eq!(MessageState::parse("queued"), None);
    }

    #[test]
    fn only_the_three_end_states_are_terminal() {
        assert!(MessageState::Delivered.is_terminal());
        assert!(MessageState::Failed.is_terminal());
        assert!(MessageState::Expired.is_terminal());

        assert!(!MessageState::Queued.is_terminal());
        assert!(!MessageState::Sent.is_terminal());
        assert!(!MessageState::Accepted.is_terminal());
    }

    #[test]
    fn the_nominal_path_of_the_specification_is_legal() {
        assert!(MessageState::Queued.can_move_to(MessageState::Sent));
        assert!(MessageState::Sent.can_move_to(MessageState::Accepted));
        assert!(MessageState::Accepted.can_move_to(MessageState::Delivered));
    }

    /// A rejected `submit_sm_resp` skips `ACCEPTED` (CA-006-05).
    #[test]
    fn a_rejected_submission_moves_straight_from_sent_to_failed() {
        assert!(MessageState::Sent.can_move_to(MessageState::Failed));
        assert!(!MessageState::Sent.can_move_to(MessageState::Delivered));
    }

    /// Replaying a committed transition must be a no-op, not a rejection.
    #[test]
    fn every_state_may_move_to_itself() {
        for state in MessageState::ALL {
            assert!(state.can_move_to(*state), "{state} rejects its own replay");
        }
    }

    /// A late delivery receipt must not resurrect a message that already
    /// failed for good.
    #[test]
    fn a_terminal_state_has_no_successor_but_itself() {
        for terminal in [
            MessageState::Delivered,
            MessageState::Failed,
            MessageState::Expired,
        ] {
            for next in MessageState::ALL {
                assert_eq!(
                    terminal.can_move_to(*next),
                    terminal == *next,
                    "{terminal} -> {next}"
                );
            }
        }
    }

    #[test]
    fn nothing_moves_backwards_along_the_lifecycle() {
        assert!(!MessageState::Sent.can_move_to(MessageState::Queued));
        assert!(!MessageState::Accepted.can_move_to(MessageState::Sent));
        assert!(!MessageState::Accepted.can_move_to(MessageState::Queued));
    }

    #[test]
    fn a_bare_transition_carries_nothing_but_the_state() {
        let update = MessageStateUpdate::new(ClientMessageId::new(), MessageState::Queued);

        assert_eq!(update.state, MessageState::Queued);
        assert_eq!(update.smsc_message_id, SmscMessageIdUpdate::Keep);
        assert!(update.attempt.is_none());
    }

    #[test]
    fn recording_a_send_records_which_attempt_it_was() {
        let update = MessageStateUpdate::new(ClientMessageId::new(), MessageState::Sent)
            .sent_at(Timestamp::now(), 2);

        assert_eq!(update.attempt, Some(2));
    }

    #[test]
    fn recording_an_smsc_identifier_asks_for_a_replacement() {
        let update = MessageStateUpdate::new(ClientMessageId::new(), MessageState::Accepted)
            .with_smsc_message_id("SMSC-1");

        assert_eq!(
            update.smsc_message_id,
            SmscMessageIdUpdate::Set(String::from("SMSC-1"))
        );
    }
}
