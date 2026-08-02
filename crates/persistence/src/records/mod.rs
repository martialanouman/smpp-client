//! The aggregates the repositories read and write.
//!
//! One struct per table of spec §14.2, with the columns typed rather than
//! stringly: a `state` is a [`MessageState`], a `dest_ton` is a
//! [`smpp_core::values::Ton`], a `created_at` is a [`Timestamp`]. Nothing here
//! knows about SQL — the mapping to and from rows lives in
//! [`crate::repositories`], so a caller can build a record in a test without a
//! database in sight.

mod enums;

pub use enums::{BindType, PduDirection};
pub use smpp_core::types::CampaignId;

/// The campaign lifecycle of spec §10.3.
///
/// Defined in `messaging` since milestone 010 (ADR 0013): the crate that owns
/// the lifecycle owns the type that carries it, and the state machine could not
/// live anywhere else — `messaging` sits above this crate and cannot depend on
/// it. Re-exported because [`Campaign`] speaks in it, so
/// `persistence::CampaignStatus` still resolves.
pub use messaging::campaign::CampaignStatus;

/// The contact aggregate, its identifiers and its lists.
///
/// Defined in `contacts` since milestone 009 (ADR 0012, CA-009-13): the crate
/// that owns the import owns the types the import produces, and this crate
/// implements its port. Re-exported because the whole contact half of this
/// crate's public surface speaks in them, so `persistence::Contact` still
/// resolves.
pub use contacts::model::{Contact, ContactId, ContactList, LineType, ListId, ProfileId};

/// A saved column-mapping profile (CA-009-09, `import_profiles`).
///
/// Defined in `contacts` for the same reason as the aggregate above: the shape
/// of a mapping is the importer's business, and this crate stores it as the
/// opaque JSON document spec §14.2 prescribes.
pub use contacts::import::ImportProfile;

/// The message aggregate, its lifecycle and its transitions.
///
/// Defined in `messaging` since milestone 006 (ADR 0010): the crate that owns
/// the lifecycle owns the type that carries it, and this crate implements its
/// port. Re-exported because the whole message half of this crate's public
/// surface speaks in these types, so `persistence::Message` still resolves.
pub use messaging::correlation::IdMatching;
pub use messaging::message::{Message, MessageState, MessageStateUpdate, SmscMessageIdUpdate};

use smpp_core::time::Timestamp;
use smpp_core::types::SessionId;
use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking, SmppVersion};

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
    ///
    /// Zero means unlimited. It is also the **ceiling** of the adaptive band
    /// of spec §9.4 — the `max_tps` that section names is this value, not a
    /// column of its own.
    pub throughput_tps: u32,
    /// Floor of the adaptive throughput band (spec §9.4, §9.5).
    ///
    /// The congestion adaptation of milestone 012 may not push the effective
    /// rate below this. Carried and validated from milestone 007, which
    /// applies a constant factor of 1.0 and therefore never reaches it.
    pub min_tps: u32,
    /// `enquire_link` period, in seconds.
    pub enquire_link_s: u32,
    /// How long a response may take before the request is abandoned.
    pub response_timeout_s: u32,
    /// Reconnection policy, as an opaque JSON document.
    pub reconnect_config: Option<String>,
    /// How GSM 7-bit septets sit in `short_message` (ADR 0008).
    pub gsm7_packing: Gsm7BitPacking,
    /// What those octets mean — GSM 03.38 positions, or ISO-8859-1 code
    /// points the message centre transcodes (ADR 0009).
    ///
    /// A column rather than a guess: nothing on the wire distinguishes the
    /// two, and a pure-ASCII message is identical under both.
    pub gsm7_charset: Gsm7BitCharset,
    /// Number of parallel binds for this logical session (spec §8.5).
    pub bind_count: u32,
    /// How hard to look for a message when a delivery receipt quotes its
    /// identifier differently (step-008 §6).
    ///
    /// A property of the **message centre**, like the two GSM 7-bit settings
    /// beside it: whether identifiers come back in another base is something
    /// the provider decides, not something a message carries.
    pub dlr_id_matching: IdMatching,
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
            .field("min_tps", &self.min_tps)
            .field("enquire_link_s", &self.enquire_link_s)
            .field("response_timeout_s", &self.response_timeout_s)
            .field("reconnect_config", &self.reconnect_config)
            .field("gsm7_packing", &self.gsm7_packing)
            .field("gsm7_charset", &self.gsm7_charset)
            .field("bind_count", &self.bind_count)
            .field("dlr_id_matching", &self.dlr_id_matching)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
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

/// Which messages to return.
///
/// Every field is a conjunction: `None` means "do not restrict on this
/// column". An all-`None` filter selects the whole table, which is exactly
/// what [`crate::ports::MessageJournal::stream_messages`] is for.
///
/// # Two families of field, and the difference is the query plan
///
/// [`Self::campaign_id`] and [`Self::state`] sit on indexes and are matched at
/// the Rust level, into the four literal queries an index can serve — see the
/// header of `repositories::messages` for the measurement that forced it.
///
/// Everything below them arrived with the log screen of milestone 008 and has
/// **no index**: a date range, a destination prefix, an error code, a
/// full-text search. They are written as `(? IS NULL OR …)`, the form that
/// prevents an index from being used — which costs nothing here, since there is
/// none to prevent, and which is what keeps the number of literal queries at
/// four instead of at sixty-four.
///
/// The cost is stated rather than assumed: `search` is a `LIKE '%…%'` over
/// `text`, `dest_addr` and `smsc_message_id`, so it is a scan of whatever the
/// indexed predicates left. On the 200 000 rows CA-008-07 measures, that is
/// tens of milliseconds — the criterion allows a second — and a full-text index
/// (FTS5) is the answer if the table grows an order of magnitude, not now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageFilter {
    /// Restrict to one campaign.
    pub campaign_id: Option<CampaignId>,
    /// Restrict to one session.
    pub session_id: Option<SessionId>,
    /// Restrict to one state.
    pub state: Option<MessageState>,
    /// Restrict to messages created at or after this instant.
    pub created_from: Option<Timestamp>,
    /// Restrict to messages created at or before this instant.
    ///
    /// Compared on the **stored text**, which is what makes the bound work at
    /// all: the storage form is RFC 3339 with a `Z` offset, and it sorts
    /// lexicographically in the same order as chronologically
    /// ([`Timestamp`]).
    pub created_to: Option<Timestamp>,
    /// Restrict to recipients starting with this prefix.
    ///
    /// A prefix and not a whole number: an operator filtering a log looks for
    /// a country or an operator range far more often than for one subscriber,
    /// and a whole number is a prefix of itself.
    pub dest_prefix: Option<String>,
    /// Restrict to messages whose delivery receipt carried this `err` code.
    pub dlr_err: Option<String>,
    /// Restrict to messages whose recipient, body or SMSC identifier contains
    /// this text.
    pub search: Option<String>,
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

    /// Restricts to a range of creation instants, either end open.
    #[must_use]
    pub fn created_between(mut self, from: Option<Timestamp>, to: Option<Timestamp>) -> Self {
        self.created_from = from;
        self.created_to = to;
        self
    }

    /// Restricts to recipients starting with `prefix`.
    ///
    /// # The leading `+` is stripped, and that is not cosmetic
    ///
    /// `Msisdn` stores a number as **digits only** — `2250102030405`, no `+`.
    /// An operator filtering a log types `+225`, because that is how E.164
    /// numbers are written everywhere else in the interface, and a literal
    /// `LIKE '+225%'` against that column matches nothing at all. Not an error,
    /// not an empty state anyone would question: a screen that silently says
    /// there are no messages.
    ///
    /// So the constructor normalises, once, where the two forms meet. Nothing
    /// else is stripped: a prefix containing a space or a dash is a prefix the
    /// operator will not find, and inventing a second normalisation here would
    /// put it out of step with `Msisdn`'s.
    #[must_use]
    pub fn with_dest_prefix(mut self, prefix: impl Into<String>) -> Self {
        let prefix: String = prefix.into();

        self.dest_prefix = Some(prefix.strip_prefix('+').unwrap_or(&prefix).to_owned());
        self
    }

    /// Restricts to one delivery-receipt error code.
    #[must_use]
    pub fn with_dlr_err(mut self, code: impl Into<String>) -> Self {
        self.dlr_err = Some(code.into());
        self
    }

    /// Restricts to rows containing `needle` in their recipient, body or SMSC
    /// identifier.
    ///
    /// # A leading `+` on a number is dropped, and only then
    ///
    /// Same mismatch as [`Self::with_dest_prefix`]: `Msisdn` stores digits
    /// only, so a needle pasted from anywhere else in the interface —
    /// `+2250102030405` — matches no recipient at all. Silently, since an empty
    /// result is a legitimate answer.
    ///
    /// The stripping is **conditional**, unlike the prefix's: this needle is
    /// also matched against the message body, where a `+` is an ordinary
    /// character somebody may be looking for. So it is removed only when the
    /// needle is `+` followed by digits and nothing else — a phone number, and
    /// nothing a body search would want spelled that way.
    #[must_use]
    pub fn matching(mut self, needle: impl Into<String>) -> Self {
        let needle: String = needle.into();

        self.search = Some(match needle.strip_prefix('+') {
            Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                digits.to_owned()
            }
            _ => needle,
        });
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
    use smpp_core::types::SessionId;

    use super::{IdMatching, MessageFilter, SessionProfile};
    use crate::records::BindType;
    use crate::Timestamp;
    use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking, SmppVersion};

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
            min_tps: 1,
            enquire_link_s: 30,
            response_timeout_s: 10,
            reconnect_config: None,
            gsm7_packing: Gsm7BitPacking::Unpacked,
            gsm7_charset: Gsm7BitCharset::Gsm0338,
            bind_count: 1,
            dlr_id_matching: IdMatching::default(),
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
}
