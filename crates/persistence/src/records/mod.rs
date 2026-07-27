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

pub use enums::{BindType, CampaignStatus, PduDirection};
pub use ids::{ContactId, ListId};
pub use smpp_core::types::CampaignId;

/// The message aggregate, its lifecycle and its transitions.
///
/// Defined in `messaging` since milestone 006 (ADR 0010): the crate that owns
/// the lifecycle owns the type that carries it, and this crate implements its
/// port. Re-exported because the whole message half of this crate's public
/// surface speaks in these types, so `persistence::Message` still resolves.
pub use messaging::message::{Message, MessageState, MessageStateUpdate, SmscMessageIdUpdate};

use smpp_core::time::Timestamp;
use smpp_core::types::{Msisdn, SessionId};
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

/// Which messages to return.
///
/// Every field is a conjunction: `None` means "do not restrict on this
/// column". An all-`None` filter selects the whole table, which is exactly
/// what [`crate::ports::MessageJournal::stream_messages`] is for.
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

    use super::{MessageFilter, SessionProfile};
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
