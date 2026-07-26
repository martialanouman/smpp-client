//! The aggregates the repositories read and write.
//!
//! One struct per table of spec §14.2, with the columns typed rather than
//! stringly: a `state` is a [`MessageState`], a `dest_ton` is a
//! [`smpp_core::values::Ton`], a `created_at` is a [`Timestamp`]. Nothing here
//! knows about SQL — the mapping to and from rows lives in
//! [`crate::repositories`], so a caller can build a record in a test without a
//! database in sight.

mod enums;
mod ids;

pub use enums::{BindType, CampaignStatus, MessageState, PduDirection};
pub use ids::{CampaignId, ContactId, ListId};

use smpp_core::types::{ClientMessageId, Msisdn, SessionId};
use smpp_core::values::{CommandStatus, DataCoding, Npi, SmppVersion, Ton};

use crate::Timestamp;

/// A connection profile (spec §14.2, `session_profiles`).
///
/// # `password_enc`
///
/// An opaque blob. This crate stores and returns the bytes and never looks
/// inside: the AES-256-GCM envelope and the OS keyring holding its key are
/// milestone 015's (spec §17.2), and step-002 puts the encryption explicitly
/// out of scope. Until then nothing writes a real password here.
///
/// The field is what makes [`SessionProfile`] deliberately **not** `Debug`-
/// derivable in a way that prints it: see the manual implementation below,
/// which renders the blob as its length. A `tracing` call that formats a
/// profile would otherwise put credential material in a log file, which
/// CLAUDE.md §8 forbids outright.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionProfile {
    /// Primary key.
    pub session_id: SessionId,
    /// Name shown in the interface.
    pub name: String,
    /// SMSC hostname or address.
    pub host: String,
    /// SMSC port.
    pub port: u16,
    /// Which bind operation opens the session.
    pub bind_type: BindType,
    /// Protocol version requested at bind time.
    pub interface_version: SmppVersion,
    /// ESME identity presented to the SMSC.
    pub system_id: String,
    /// Encrypted password, opaque to this crate.
    pub password_enc: Vec<u8>,
    /// `system_type` of the bind PDU; empty when unused.
    pub system_type: String,
    /// TLS settings, as an opaque JSON document.
    pub tls_config: Option<String>,
    /// Number of unacknowledged PDUs allowed in flight (spec §9.2).
    pub window_size: u32,
    /// Target throughput in messages per second (spec §9.5).
    pub throughput_tps: u32,
    /// `enquire_link` period, in seconds.
    pub enquire_link_s: u32,
    /// How long a response may take before the request is abandoned.
    pub response_timeout_s: u32,
    /// Reconnection policy, as an opaque JSON document.
    pub reconnect_config: Option<String>,
    /// Number of parallel binds for this logical session (spec §8.5).
    pub bind_count: u32,
    /// When the profile was created.
    pub created_at: Timestamp,
    /// When the profile was last written.
    pub updated_at: Timestamp,
}

impl core::fmt::Debug for SessionProfile {
    /// Renders every field except the credential, which appears as its length.
    ///
    /// CLAUDE.md §8: no secret in a log, "even at `trace`". The derived
    /// implementation would print `password_enc` as a byte array, and one
    /// `tracing::debug!(?profile)` three milestones from now would be enough.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionProfile")
            .field("session_id", &self.session_id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("bind_type", &self.bind_type)
            .field("interface_version", &self.interface_version)
            .field("system_id", &self.system_id)
            .field(
                "password_enc",
                &format_args!("<{} bytes>", self.password_enc.len()),
            )
            .field("system_type", &self.system_type)
            .field("tls_config", &self.tls_config)
            .field("window_size", &self.window_size)
            .field("throughput_tps", &self.throughput_tps)
            .field("enquire_link_s", &self.enquire_link_s)
            .field("response_timeout_s", &self.response_timeout_s)
            .field("reconnect_config", &self.reconnect_config)
            .field("bind_count", &self.bind_count)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// A contact (spec §14.2, `contacts`; spec §11.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    /// Primary key.
    pub contact_id: ContactId,
    /// Subscriber number, normalised by `smpp_core::types::Msisdn`.
    pub msisdn: Msisdn,
    /// ISO 3166-1 alpha-2 country, when it could be derived.
    pub country: Option<String>,
    /// Whether the number passed validation at import time.
    pub valid: bool,
    /// Line type reported by the numbering plan (`mobile`, `fixed_line`…).
    pub line_type: Option<String>,
    /// Template variables, as an opaque JSON document.
    pub attributes: Option<String>,
    /// Where the contact came from (`import_xlsx`, `generated`…).
    pub source: Option<String>,
    /// When the contact was created.
    pub created_at: Timestamp,
}

/// A named group of contacts (spec §14.2, `contact_lists`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactList {
    /// Primary key.
    pub list_id: ListId,
    /// Name shown in the interface.
    pub name: String,
    /// When the list was created.
    pub created_at: Timestamp,
}

/// A bulk-send campaign (spec §14.2, `campaigns`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Campaign {
    /// Primary key.
    pub campaign_id: CampaignId,
    /// Name shown in the interface.
    pub name: String,
    /// Where the campaign stands in the lifecycle of spec §10.3.
    pub status: CampaignStatus,
    /// Message template, with its variables.
    pub template: String,
    /// Sessions, routing and SMPP options, as an opaque JSON document.
    pub send_config: String,
    /// Recipients enrolled.
    pub total_count: u32,
    /// Messages handed to a session.
    pub sent_count: u32,
    /// Messages a delivery receipt confirmed.
    pub delivered_count: u32,
    /// Messages that failed for good.
    pub failed_count: u32,
    /// When the campaign was created.
    pub created_at: Timestamp,
    /// When sending began.
    pub started_at: Option<Timestamp>,
    /// When the campaign reached a terminal status.
    pub completed_at: Option<Timestamp>,
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

/// Which messages to return.
///
/// Every field is a conjunction: `None` means "do not restrict on this
/// column". An all-`None` filter selects the whole table, which is exactly
/// what [`crate::ports::MessageRepository::stream_messages`] is for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageFilter {
    /// Restrict to one campaign.
    pub campaign_id: Option<CampaignId>,
    /// Restrict to one session.
    pub session_id: Option<SessionId>,
    /// Restrict to one state.
    pub state: Option<MessageState>,
}

impl MessageFilter {
    /// Selects every message.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Restricts to one campaign.
    #[must_use]
    pub fn for_campaign(mut self, campaign_id: CampaignId) -> Self {
        self.campaign_id = Some(campaign_id);
        self
    }

    /// Restricts to one session.
    #[must_use]
    pub fn for_session(mut self, session_id: SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Restricts to one state.
    #[must_use]
    pub fn in_state(mut self, state: MessageState) -> Self {
        self.state = Some(state);
        self
    }
}

/// What a transition does to `messages.smsc_message_id`.
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

/// One logged PDU (spec §14.2, `pdu_log`).
///
/// # Confidentiality
///
/// `raw_hex` and `decoded` hold the wire form of a PDU: message content, and
/// on a bind PDU the password. Spec §17.7 and CLAUDE.md §8 confine both to an
/// explicitly enabled debug mode. The schema cannot enforce that and neither
/// can this crate — the decision belongs to whoever calls
/// [`crate::ports::PduLogRepository::insert_entry`], which is why the
/// repository is deliberately dull and writes exactly what it is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PduLogEntry {
    /// Session the PDU belonged to.
    pub session_id: Option<SessionId>,
    /// Which way it travelled.
    pub direction: PduDirection,
    /// `command_id` of the PDU header.
    pub command_id: Option<u32>,
    /// `command_status` of the PDU header.
    pub command_status: Option<u32>,
    /// `sequence_number` of the PDU header.
    pub sequence_number: Option<u32>,
    /// Hexadecimal dump, when debug mode allowed one.
    pub raw_hex: Option<String>,
    /// Decoded rendering, when debug mode allowed one.
    pub decoded: Option<String>,
    /// When the PDU crossed the socket.
    pub ts: Timestamp,
}

#[cfg(test)]
mod tests {
    use smpp_core::types::{ClientMessageId, SessionId};

    use super::{
        MessageFilter, MessageState, MessageStateUpdate, SessionProfile, SmscMessageIdUpdate,
    };
    use crate::records::BindType;
    use crate::Timestamp;
    use smpp_core::values::SmppVersion;

    fn a_profile() -> SessionProfile {
        SessionProfile {
            session_id: SessionId::new(),
            name: String::from("staging"),
            host: String::from("smsc.example.test"),
            port: 2775,
            bind_type: BindType::Transceiver,
            interface_version: SmppVersion::V3_4,
            system_id: String::from("esme"),
            password_enc: vec![1, 2, 3, 4, 5, 6, 7, 8],
            system_type: String::new(),
            tls_config: None,
            window_size: 50,
            throughput_tps: 100,
            enquire_link_s: 30,
            response_timeout_s: 10,
            reconnect_config: None,
            bind_count: 1,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn debugging_a_profile_never_shows_the_credential() {
        let rendered = format!("{:?}", a_profile());

        assert!(rendered.contains("<8 bytes>"), "{rendered}");
        assert!(!rendered.contains(", 5, 6, 7"), "{rendered}");
    }

    #[test]
    fn an_empty_filter_restricts_nothing() {
        let filter = MessageFilter::all();

        assert!(filter.campaign_id.is_none());
        assert!(filter.session_id.is_none());
        assert!(filter.state.is_none());
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
