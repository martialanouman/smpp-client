//! Reading a `deliver_sm` (deliverable L-008-01).
//!
//! Two questions, in this order:
//!
//! 1. **Is this a delivery receipt or a mobile-originated message?** Spec §7.8
//!    answers it with `esm_class`, and nothing else: a receipt whose body looks
//!    like a receipt but whose `esm_class` says "normal message" is an incoming
//!    SMS whose text happens to start with `id:`, and treating it as a receipt
//!    would move somebody's message to `DELIVERED` because a subscriber typed
//!    the right seven letters. [`classify`] reads the flag and stops there.
//!
//! 2. **Which message is it about, and what does it say?** The identifier comes
//!    from the `receipted_message_id` TLV when the message centre sent one, and
//!    from the body otherwise. The body is where the tolerance lives, and
//!    [`parse_receipt_body`] is where it is spent.
//!
//! # The body is not a format
//!
//! Spec §7.8 quotes a shape:
//!
//! ```text
//! id:… sub:… dlvrd:… submit date:… done date:… stat:… err:… text:…
//! ```
//!
//! and it is a **convention**, not a grammar. Real message centres differ on
//! every axis this parser is tolerant about, and each one below is a fixture in
//! the tests at the bottom of this file:
//!
//! * key case — `Id:`, `STAT:`, `Stat:`;
//! * separator — `submit date:`, `submitdate:`, `submit_date:`;
//! * spacing — several spaces, a tab, a newline between fields;
//! * ordering — `stat:` before `id:`;
//! * absence — no `sub:`, no `dlvrd:`, no `err:`, no `text:` at all;
//! * junk — vendor fields nobody documents, sitting between two known ones.
//!
//! **Tolerant on the way in, strict on the way out.** Everything this module
//! *derives* is typed: the status is one of the seven codes of spec §7.8 or
//! [`DeliveryStatus::Other`], the two dates are [`Timestamp`]s or nothing, the
//! counts are numbers or nothing. A field that will not parse is dropped, never
//! guessed at, and never turns the whole receipt into a failure — a body this
//! module cannot read still yields whatever it *could* read, and the raw text
//! travels with it so an operator can see what arrived.
//!
//! # This module never fails
//!
//! [`parse_receipt_body`] returns a value, not a `Result`. There is no input
//! for which a delivery receipt should be *rejected*: the message centre is not
//! going to send it again, and an error here would mean throwing away the one
//! record that a message was delivered. What a body with no readable identifier
//! produces is a receipt with `id: None`, which
//! [`crate::correlation`] turns into a journalled orphan (CA-008-03).

use smpp_core::codec::Pdu;
use smpp_core::pdus::DeliverSm;
use smpp_core::time::Timestamp;
use smpp_core::tlvs::TlvValue;
// `MessageState` on the wire is the `message_state` TLV of spec §7.8, and has
// nothing to do with [`crate::message::MessageState`], which is this
// application's lifecycle. Two types, one name: the alias is what stops a
// reader — or a `use` three months from now — from confusing them.
use smpp_core::values::{MessageState as WireMessageState, MessageType};

use crate::message::MessageState;

/// What a `deliver_sm` turned out to be.
///
/// The distinction is `esm_class` and only `esm_class` — see the module note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// A delivery receipt, or an intermediate notification.
    Receipt(DeliveryReceipt),
    /// An ordinary incoming message.
    ///
    /// Acknowledged and logged, with no further processing: step-008 §2 puts
    /// business handling of mobile-originated traffic out of scope.
    MobileOriginated,
}

/// The seven `stat` codes of spec §7.8, plus whatever else arrives.
///
/// Stored as they were received (see [`Self::as_str`]) so the journal shows the
/// message centre's own word, and mapped onto the lifecycle by
/// [`Self::message_state`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeliveryStatus {
    /// `DELIVRD` — the handset received it.
    Delivered,
    /// `EXPIRED` — the validity period ran out first.
    Expired,
    /// `DELETED` — cancelled or removed at the message centre.
    Deleted,
    /// `UNDELIV` — permanently undeliverable.
    Undeliverable,
    /// `ACCEPTD` — accepted on the subscriber's behalf; no further delivery.
    Accepted,
    /// `REJECTD` — refused by a delivery interface.
    Rejected,
    /// `UNKNOWN` — the message centre cannot say.
    Unknown,
    /// Anything else, kept verbatim.
    Other(String),
}

impl DeliveryStatus {
    /// The seven codes of spec §7.8, in the order that section lists them.
    pub const ALL: &'static [Self] = &[
        Self::Delivered,
        Self::Expired,
        Self::Deleted,
        Self::Undeliverable,
        Self::Accepted,
        Self::Rejected,
        Self::Unknown,
    ];

    /// Reads a `stat` value.
    ///
    /// Case-insensitive, and surrounding whitespace is ignored: a message
    /// centre writing `stat: delivrd` means the same thing as one writing
    /// `stat:DELIVRD`. Anything that is not one of the seven is kept as
    /// [`Self::Other`], **uppercased**, so the journal groups two spellings of
    /// one vendor code together.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let normalised = raw.trim().to_ascii_uppercase();

        match normalised.as_str() {
            "DELIVRD" => Self::Delivered,
            "EXPIRED" => Self::Expired,
            "DELETED" => Self::Deleted,
            "UNDELIV" => Self::Undeliverable,
            "ACCEPTD" => Self::Accepted,
            "REJECTD" => Self::Rejected,
            "UNKNOWN" => Self::Unknown,
            _ => Self::Other(normalised),
        }
    }

    /// The code, as it is written in the journal.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Delivered => "DELIVRD",
            Self::Expired => "EXPIRED",
            Self::Deleted => "DELETED",
            Self::Undeliverable => "UNDELIV",
            Self::Accepted => "ACCEPTD",
            Self::Rejected => "REJECTD",
            Self::Unknown => "UNKNOWN",
            Self::Other(raw) => raw,
        }
    }

    /// Where this receipt puts the message in the lifecycle of spec §14.3.
    ///
    /// # Why `ACCEPTD` and `UNKNOWN` map to a state the message is already in
    ///
    /// The lifecycle has six states and the receipt vocabulary has seven codes,
    /// so the mapping is not a bijection and two codes have to land somewhere
    /// that is not a new terminal state.
    ///
    /// `ACCEPTD` and `UNKNOWN` are exactly the two that say **nothing about
    /// delivery**: the first means a human intervened at the message centre,
    /// the second that the centre cannot report. Mapping either onto
    /// `DELIVERED` would count a message the recipient never saw; mapping
    /// either onto `FAILED` would bury a message that may still arrive. So both
    /// map to [`MessageState::Accepted`], which is where an answered
    /// `submit_sm` already sits — the transition is a self-transition, the state
    /// does not move, and `dlr_stat`, `dlr_err` and `dlr_at` are still recorded.
    ///
    /// The consequence is stated rather than hidden: a message whose only
    /// receipt is `UNKNOWN` stays non-terminal, and milestone 010 will resume it
    /// as unfinished. That is the truth of what the centre reported.
    ///
    /// A code outside the seven ([`Self::Other`]) is treated the same way: an
    /// unrecognised word is not evidence of anything.
    #[must_use]
    pub const fn message_state(&self) -> MessageState {
        match self {
            Self::Delivered => MessageState::Delivered,
            Self::Expired => MessageState::Expired,
            Self::Deleted | Self::Undeliverable | Self::Rejected => MessageState::Failed,
            Self::Accepted | Self::Unknown | Self::Other(_) => MessageState::Accepted,
        }
    }

    /// Whether this receipt closes the message for good.
    #[must_use]
    pub const fn is_final(&self) -> bool {
        self.message_state().is_terminal()
    }
}

impl core::fmt::Display for DeliveryStatus {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Everything a delivery receipt carried.
///
/// Every field but [`Self::raw`] is optional, because every field but the raw
/// text is genuinely absent from some real message centre's receipts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryReceipt {
    /// The identifier the message centre assigned, from the
    /// `receipted_message_id` TLV or from the body's `id:` field.
    pub smsc_message_id: Option<String>,
    /// `sub:` — how many parts were submitted.
    pub submitted: Option<u32>,
    /// `dlvrd:` — how many were delivered.
    pub delivered: Option<u32>,
    /// `submit date:`, when it could be read.
    pub submit_date: Option<Timestamp>,
    /// `done date:`, when it could be read.
    pub done_date: Option<Timestamp>,
    /// `stat:`, or the `message_state` TLV when the body carried no `stat:`.
    pub status: Option<DeliveryStatus>,
    /// `err:`, kept verbatim — its meaning is vendor-specific.
    pub error_code: Option<String>,
    /// `text:` — the first characters of the original message.
    ///
    /// Message **content**, which is why the journal masks it (CLAUDE.md §8).
    pub text: Option<String>,
    /// The whole body, as it arrived.
    ///
    /// Kept so an operator can see what a message centre actually sent when a
    /// receipt did not correlate. It is content too, and masked in the same
    /// place [`Self::text`] is.
    pub raw: String,
    /// Whether this is an intermediate notification rather than a final
    /// receipt.
    ///
    /// Both arrive as a `deliver_sm` and both are parsed identically; only the
    /// `esm_class` bit tells them apart. An intermediate notification is
    /// journalled and does **not** close the message.
    pub intermediate: bool,
}

impl DeliveryReceipt {
    /// Where this receipt puts the message, or `None` when it says nothing.
    ///
    /// An intermediate notification never moves the state: spec §7.8 has the
    /// message centre send it *while it is still trying*, so acting on it would
    /// close a message that is still in flight.
    #[must_use]
    pub fn message_state(&self) -> Option<MessageState> {
        if self.intermediate {
            return None;
        }

        self.status.as_ref().map(DeliveryStatus::message_state)
    }

    /// The `stat` code as text, for the `dlr_stat` column that stores it.
    #[must_use]
    pub fn dlr_stat_text(&self) -> Option<String> {
        self.status
            .as_ref()
            .map(|status| status.as_str().to_owned())
    }
}

/// Reads a `deliver_sm` (spec §7.8).
///
/// Returns [`Incoming::MobileOriginated`] for anything whose `esm_class` does
/// not carry the receipt bit, whatever its body looks like.
#[must_use]
pub fn classify(pdu: &DeliverSm) -> Incoming {
    let intermediate = match pdu.esm_class.message_type {
        MessageType::ShortMessageContainsMCDeliveryReceipt => false,
        MessageType::ShortMessageContainsIntermediateDeliveryNotification => true,
        _ => return Incoming::MobileOriginated,
    };

    let mut receipt = parse_receipt_body(&body_text(pdu));
    receipt.intermediate = intermediate;

    // The TLV wins over the body. Spec §7.8 makes it the machine-readable
    // form; the body is a rendering meant for humans, and the two disagree in
    // the field — a message centre that pads the body's `id:` to ten digits
    // still sends the true identifier in the TLV.
    if let Some(identifier) = receipted_message_id(pdu) {
        receipt.smsc_message_id = Some(identifier);
    }

    // The `message_state` TLV fills in only when the body said nothing. It is
    // coarser than `stat:` — it cannot distinguish `DELETED` from `UNDELIV` —
    // so it is the fallback, not the authority.
    if receipt.status.is_none() {
        receipt.status = message_state_tlv(pdu).map(status_of);
    }

    Incoming::Receipt(receipt)
}

/// Reads the `receipted_message_id` TLV, when the PDU carries one.
#[must_use]
pub fn receipted_message_id(pdu: &DeliverSm) -> Option<String> {
    pdu.tlvs().iter().find_map(|tlv| match tlv.value() {
        Some(TlvValue::ReceiptedMessageId(identifier)) => {
            let identifier = identifier.as_str().trim();

            (!identifier.is_empty()).then(|| identifier.to_owned())
        }
        _ => None,
    })
}

/// Reads the `message_state` TLV, when the PDU carries one.
fn message_state_tlv(pdu: &DeliverSm) -> Option<WireMessageState> {
    pdu.tlvs().iter().find_map(|tlv| match tlv.value() {
        Some(TlvValue::MessageState(state)) => Some(*state),
        _ => None,
    })
}

/// The `message_state` TLV, as the equivalent `stat:` code.
fn status_of(state: WireMessageState) -> DeliveryStatus {
    match state {
        WireMessageState::Delivered => DeliveryStatus::Delivered,
        WireMessageState::Expired => DeliveryStatus::Expired,
        WireMessageState::Deleted => DeliveryStatus::Deleted,
        WireMessageState::Undeliverable => DeliveryStatus::Undeliverable,
        WireMessageState::Accepted => DeliveryStatus::Accepted,
        WireMessageState::Rejected => DeliveryStatus::Rejected,
        // `SCHEDULED`, `ENROUTE` and `SKIPPED` have no `stat:` spelling, and
        // neither has a value outside the enumeration. They say the message is
        // still moving, which is what `UNKNOWN` maps to: no state change.
        _ => DeliveryStatus::Unknown,
    }
}

/// The body of a `deliver_sm`, as text.
///
/// `message_payload` first — a receipt long enough to overflow `short_message`
/// puts its body there (spec §7.5), and reading `short_message` then would read
/// an empty field and lose the whole receipt.
///
/// The octets are decoded as ISO-8859-1, which cannot fail: every byte is a
/// code point. A receipt body is ASCII in practice, and the one thing that must
/// not happen is a non-UTF-8 octet costing the identifier that sits next to it.
fn body_text(pdu: &DeliverSm) -> String {
    let payload = pdu.tlvs().iter().find_map(|tlv| match tlv.value() {
        Some(TlvValue::MessagePayload(payload)) => Some(payload.value.as_ref()),
        _ => None,
    });

    let octets = payload.unwrap_or_else(|| pdu.short_message().as_ref());

    octets.iter().map(|byte| char::from(*byte)).collect()
}

/// Every key this parser recognises, with the spellings seen in the field.
///
/// Longest spelling first **within a key** and longest key first overall: the
/// scanner takes the first match, so `submit date` has to be tried before a
/// hypothetical `submit`, and `submitdate` before `submit`.
const KEYS: &[(Field, &[&str])] = &[
    (
        Field::SubmitDate,
        &["submit date", "submit_date", "submitdate"],
    ),
    (Field::DoneDate, &["done date", "done_date", "donedate"]),
    (Field::Dlvrd, &["dlvrd", "delivered"]),
    (Field::Text, &["text"]),
    (Field::Stat, &["stat"]),
    (Field::Sub, &["sub"]),
    (Field::Err, &["err"]),
    (Field::Id, &["id"]),
];

/// Which field a `key:` introduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Id,
    Sub,
    Dlvrd,
    SubmitDate,
    DoneDate,
    Stat,
    Err,
    Text,
}

/// Reads a delivery receipt body.
///
/// Never fails: see the module note. A body it understands nothing of comes
/// back with every field `None` and [`DeliveryReceipt::raw`] holding the input.
#[must_use]
pub fn parse_receipt_body(body: &str) -> DeliveryReceipt {
    let mut receipt = DeliveryReceipt {
        raw: body.to_owned(),
        ..DeliveryReceipt::default()
    };

    for (field, value) in fields(body) {
        match field {
            // `id:` may appear twice — a vendor prefix followed by the real
            // one. The FIRST wins: it is the one the convention puts at the
            // head of the body, and a later one is the exception.
            Field::Id => set_once(&mut receipt.smsc_message_id, non_empty(value)),
            Field::Sub => set_once(&mut receipt.submitted, count(value)),
            Field::Dlvrd => set_once(&mut receipt.delivered, count(value)),
            Field::SubmitDate => set_once(&mut receipt.submit_date, receipt_date(value)),
            Field::DoneDate => set_once(&mut receipt.done_date, receipt_date(value)),
            Field::Stat => set_once(
                &mut receipt.status,
                non_empty(value).map(|raw| DeliveryStatus::parse(&raw)),
            ),
            Field::Err => set_once(&mut receipt.error_code, non_empty(value)),
            Field::Text => set_once(&mut receipt.text, Some(value.to_owned())),
        }
    }

    receipt
}

/// Writes `value` into `slot` unless the slot already holds something.
fn set_once<T>(slot: &mut Option<T>, value: Option<T>) {
    if slot.is_none() {
        if let Some(value) = value {
            *slot = Some(value);
        }
    }
}

/// The trimmed value, or `None` when there is nothing left.
fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();

    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// A `sub:`/`dlvrd:` count.
///
/// Leading zeroes are the convention (`sub:001`), so this is a plain decimal
/// parse of the trimmed value. Anything else — a letter, an empty field — is
/// dropped rather than defaulted to zero: "no answer" and "zero delivered" are
/// different facts, and one of them is a lie.
fn count(value: &str) -> Option<u32> {
    value.trim().parse().ok()
}

/// A `submit date:`/`done date:` value.
///
/// Spec §7.8 writes `YYMMDDhhmm`; several message centres append seconds, and a
/// few send an already-formatted instant. All three are read, and anything else
/// — a malformed date, an impossible month — yields `None` rather than a
/// failure (CA-008-03).
///
/// The two-digit year is read as `20YY`. There is no other reading available:
/// the field carries no century, and a receipt for a message sent in 1999 is
/// not a case this application has.
fn receipt_date(value: &str) -> Option<Timestamp> {
    let raw = value.trim();

    if raw.is_empty() {
        return None;
    }

    // Some message centres send RFC 3339 outright. Cheap to try, and it is the
    // only form the fallback below would misread as a `YYMMDDhhmm`.
    if let Ok(instant) = Timestamp::parse(raw) {
        return Some(instant);
    }

    if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let (year, month, day, hour, minute) = (
        raw.get(0..2)?,
        raw.get(2..4)?,
        raw.get(4..6)?,
        raw.get(6..8)?,
        raw.get(8..10)?,
    );
    let second = match raw.len() {
        10 => "00",
        12 => raw.get(10..12)?,
        // Neither `YYMMDDhhmm` nor `YYMMDDhhmmss`. Nothing to guess from.
        _ => return None,
    };

    // Built as RFC 3339 and handed to the one parser of the application, which
    // is what rejects month 13 and 31 February without a second calendar here.
    Timestamp::parse(&format!("20{year}-{month}-{day}T{hour}:{minute}:{second}Z")).ok()
}

/// Splits a receipt body into `(field, value)` pairs, in order of appearance.
///
/// The scanner of the module note. At each position it tries every known key;
/// on a match the value runs to the next whitespace, except for `text:` whose
/// value is **the rest of the body** — the original message may contain
/// anything, colons and spaces included, and cutting it at the first space
/// would truncate every receipt that carries one.
///
/// A token matching no key is skipped whole, which is what makes an
/// undocumented vendor field harmless rather than a parse failure.
///
/// # The cursor advances by characters, never by bytes
///
/// `at` is always a character boundary and every branch moves it forward by at
/// least one **character**. Both halves of that sentence were bugs.
///
/// The scanner first tested `body.as_bytes()[at].is_ascii_whitespace()` and
/// skipped the rest of the token with `find(char::is_whitespace)`. The two
/// disagree about U+0085 and U+00A0 — whitespace to `char`, not ASCII — and
/// every octet arriving from a message centre becomes a `char` here (see
/// [`body_text`]). So on a body containing one of those two bytes the leading
/// test said "not whitespace", the token skip found whitespace at offset **0**,
/// and `at` was assigned its own value: an infinite loop inside a task that
/// holds the delivery queue, on an input the peer chooses. It was found by the
/// property test in `tests/properties.rs`, which is why that test generates
/// arbitrary octets rather than plausible receipts.
fn fields(body: &str) -> Vec<(Field, &str)> {
    let mut found = Vec::new();
    let mut at = 0;

    while let Some(first) = body.get(at..).and_then(|rest| rest.chars().next()) {
        if first.is_whitespace() {
            at += first.len_utf8();
            continue;
        }

        match key_at(body, at) {
            Some((field, after_colon)) => {
                if field == Field::Text {
                    // Everything left, leading spaces stripped: `text: hello`
                    // and `text:hello` carry the same message.
                    found.push((field, body[after_colon..].trim_start()));

                    return found;
                }

                // `after_colon` is past the key and its colon, so strictly
                // greater than `at`: this branch always advances.
                let end = body[after_colon..]
                    .find(char::is_whitespace)
                    .map_or(body.len(), |offset| after_colon + offset);

                found.push((field, &body[after_colon..end]));
                at = end;
            }
            None => {
                // Skip the whole token. Advancing by one character instead
                // would let `myid:7` match the `id:` key two characters in,
                // and attach a vendor field's value to the identifier.
                //
                // The search starts *after* the first character, which is what
                // guarantees progress whatever that character is.
                let from = at + first.len_utf8();

                at = body[from..]
                    .find(char::is_whitespace)
                    .map_or(body.len(), |offset| from + offset);
            }
        }
    }

    found
}

/// Matches a `key:` at `at`, returning the field and the offset after the `:`.
///
/// Case-insensitive on the key, and tolerant of spaces around the colon —
/// `stat :` and `stat : ` both occur.
fn key_at(body: &str, at: usize) -> Option<(Field, usize)> {
    let rest = body.get(at..)?;

    for (field, spellings) in KEYS {
        for spelling in *spellings {
            let Some(after_key) = rest.get(..spelling.len()) else {
                continue;
            };

            if !after_key.eq_ignore_ascii_case(spelling) {
                continue;
            }

            let tail = rest.get(spelling.len()..)?;
            let spaces = tail.len() - tail.trim_start_matches(' ').len();

            if tail.get(spaces..spaces + 1) == Some(":") {
                return Some((*field, at + spelling.len() + spaces + 1));
            }
        }
    }

    None
}

/// The body of a `deliver_sm`, when the PDU is one.
///
/// A convenience for the delivery loop, which holds a whole [`Command`] rather
/// than a body.
///
/// [`Command`]: smpp_core::codec::Command
#[must_use]
pub fn as_deliver_sm(pdu: Option<&Pdu>) -> Option<&DeliverSm> {
    match pdu {
        Some(Pdu::DeliverSm(body)) => Some(body),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify, parse_receipt_body, DeliveryReceipt, DeliveryStatus, Incoming, MessageState,
    };

    use smpp_core::octets::{COctetString, OctetString};
    use smpp_core::pdus::DeliverSm;
    use smpp_core::tlvs::MessageDeliveryRequestTlvValue as DeliveryTlv;
    use smpp_core::values::{Ansi41Specific, EsmClass, GsmFeatures, MessageType, MessagingMode};

    /// The canonical body of spec §7.8.
    const CANONICAL: &str = "id:0123456789 sub:001 dlvrd:001 submit date:2607261200 \
                             done date:2607261205 stat:DELIVRD err:000 text:Hello world";

    fn a_deliver_sm(message_type: MessageType, body: &str) -> DeliverSm {
        DeliverSm::builder()
            .esm_class(EsmClass::new(
                MessagingMode::Default,
                message_type,
                Ansi41Specific::ShortMessageContainsDeliveryAcknowledgement,
                GsmFeatures::NotSelected,
            ))
            .short_message(OctetString::from_slice(body.as_bytes()).expect("under 255 octets"))
            .build()
    }

    fn a_receipt_pdu(body: &str) -> DeliverSm {
        a_deliver_sm(MessageType::ShortMessageContainsMCDeliveryReceipt, body)
    }

    fn receipt_of(pdu: &DeliverSm) -> DeliveryReceipt {
        match classify(pdu) {
            Incoming::Receipt(receipt) => receipt,
            Incoming::MobileOriginated => panic!("expected a receipt"),
        }
    }

    // --- Detection ----------------------------------------------------------

    /// CA-008-02, and the reason detection is not "does the body look like a
    /// receipt": a subscriber can type `id:1 stat:DELIVRD`.
    #[test]
    fn a_normal_message_is_mobile_originated_however_its_body_reads() {
        let pdu = a_deliver_sm(MessageType::Default, CANONICAL);

        assert_eq!(classify(&pdu), Incoming::MobileOriginated);
    }

    #[test]
    fn the_receipt_bit_of_esm_class_makes_it_a_receipt() {
        let receipt = receipt_of(&a_receipt_pdu(CANONICAL));

        assert_eq!(receipt.smsc_message_id.as_deref(), Some("0123456789"));
        assert!(!receipt.intermediate);
    }

    /// An intermediate notification arrives while the centre is still trying.
    /// Parsed and journalled, but it must not close the message.
    #[test]
    fn an_intermediate_notification_reports_no_state_change() {
        let pdu = a_deliver_sm(
            MessageType::ShortMessageContainsIntermediateDeliveryNotification,
            "id:7 stat:DELIVRD err:000",
        );

        let receipt = receipt_of(&pdu);

        assert!(receipt.intermediate);
        assert_eq!(receipt.status, Some(DeliveryStatus::Delivered));
        assert_eq!(receipt.message_state(), None);
    }

    // --- The TLVs -----------------------------------------------------------

    /// CA-008-02 — the TLV is authoritative when both are present.
    #[test]
    fn the_receipted_message_id_tlv_wins_over_the_body() {
        let mut pdu = a_receipt_pdu("id:PADDED-0007 stat:DELIVRD");
        pdu.push_tlv(DeliveryTlv::ReceiptedMessageId(
            COctetString::from_slice(b"7\0").expect("fits"),
        ));

        assert_eq!(receipt_of(&pdu).smsc_message_id.as_deref(), Some("7"));
    }

    /// CA-008-02 — a body-only receipt correlates just as well.
    #[test]
    fn the_body_supplies_the_identifier_when_no_tlv_does() {
        assert_eq!(
            receipt_of(&a_receipt_pdu(CANONICAL))
                .smsc_message_id
                .as_deref(),
            Some("0123456789")
        );
    }

    /// A message centre that sends the state as a TLV and nothing in the body.
    #[test]
    fn the_message_state_tlv_fills_in_for_a_missing_stat_field() {
        let mut pdu = a_receipt_pdu("id:7");
        pdu.push_tlv(DeliveryTlv::MessageState(
            smpp_core::values::MessageState::Delivered,
        ));

        let receipt = receipt_of(&pdu);

        assert_eq!(receipt.status, Some(DeliveryStatus::Delivered));
        assert_eq!(receipt.message_state(), Some(MessageState::Delivered));
    }

    /// And it does **not** override a body that said something else: `stat:`
    /// distinguishes `DELETED` from `UNDELIV`, which the TLV cannot.
    #[test]
    fn a_body_status_is_not_overwritten_by_the_message_state_tlv() {
        let mut pdu = a_receipt_pdu("id:7 stat:UNDELIV");
        pdu.push_tlv(DeliveryTlv::MessageState(
            smpp_core::values::MessageState::Deleted,
        ));

        assert_eq!(receipt_of(&pdu).status, Some(DeliveryStatus::Undeliverable));
    }

    /// A receipt too long for `short_message` travels in `message_payload`.
    /// Reading `short_message` first would find it empty and lose everything.
    #[test]
    fn a_receipt_carried_in_message_payload_is_read() {
        let mut pdu = a_receipt_pdu("");
        pdu.push_tlv(DeliveryTlv::MessagePayload(
            smpp_core::values::MessagePayload::new(smpp_core::octets::AnyOctetString::from_slice(
                b"id:PAYLOAD-1 stat:DELIVRD err:000",
            )),
        ));

        assert_eq!(
            receipt_of(&pdu).smsc_message_id.as_deref(),
            Some("PAYLOAD-1")
        );
    }

    // --- The seven statuses (CA-008-05) -------------------------------------

    #[test]
    fn ca_008_05_the_seven_statuses_map_onto_internal_states() {
        let expected = [
            (
                "DELIVRD",
                DeliveryStatus::Delivered,
                MessageState::Delivered,
            ),
            ("EXPIRED", DeliveryStatus::Expired, MessageState::Expired),
            ("DELETED", DeliveryStatus::Deleted, MessageState::Failed),
            (
                "UNDELIV",
                DeliveryStatus::Undeliverable,
                MessageState::Failed,
            ),
            ("ACCEPTD", DeliveryStatus::Accepted, MessageState::Accepted),
            ("REJECTD", DeliveryStatus::Rejected, MessageState::Failed),
            ("UNKNOWN", DeliveryStatus::Unknown, MessageState::Accepted),
        ];

        assert_eq!(
            expected.len(),
            DeliveryStatus::ALL.len(),
            "the table must cover every documented status"
        );

        for (code, status, state) in expected {
            let receipt = parse_receipt_body(&format!("id:7 stat:{code}"));

            assert_eq!(receipt.status.as_ref(), Some(&status), "parsing {code}");
            assert_eq!(receipt.message_state(), Some(state), "mapping {code}");
        }
    }

    /// The two that must never be mistaken for progress.
    #[test]
    fn accepted_and_unknown_are_not_terminal() {
        assert!(!DeliveryStatus::Accepted.is_final());
        assert!(!DeliveryStatus::Unknown.is_final());
        assert!(DeliveryStatus::Delivered.is_final());
        assert!(DeliveryStatus::Undeliverable.is_final());
    }

    /// An unrecognised code is kept, uppercased, and claims nothing.
    #[test]
    fn an_unknown_status_code_is_preserved_and_claims_nothing() {
        let receipt = parse_receipt_body("id:7 stat:BuFfErEd");

        assert_eq!(
            receipt.status,
            Some(DeliveryStatus::Other(String::from("BUFFERED")))
        );
        assert_eq!(receipt.message_state(), Some(MessageState::Accepted));
    }

    // --- Body tolerance (CA-008-03) -----------------------------------------

    #[test]
    fn the_canonical_body_yields_every_field() {
        let receipt = parse_receipt_body(CANONICAL);

        assert_eq!(receipt.smsc_message_id.as_deref(), Some("0123456789"));
        assert_eq!(receipt.submitted, Some(1));
        assert_eq!(receipt.delivered, Some(1));
        assert_eq!(
            receipt.submit_date.map(|date| date.to_storage()),
            Some(String::from("2026-07-26T12:00:00Z"))
        );
        assert_eq!(
            receipt.done_date.map(|date| date.to_storage()),
            Some(String::from("2026-07-26T12:05:00Z"))
        );
        assert_eq!(receipt.status, Some(DeliveryStatus::Delivered));
        assert_eq!(receipt.error_code.as_deref(), Some("000"));
        assert_eq!(receipt.text.as_deref(), Some("Hello world"));
    }

    /// Real bodies, one per message centre family. Each was a decision the
    /// parser had to make; each is a non-regression fixture (step-008 §5).
    #[test]
    fn real_world_bodies_are_all_read() {
        // Kannel: uppercase keys, no `sub`/`dlvrd`, seconds on the dates.
        let kannel = parse_receipt_body(
            "id:1a2b3c SUB:000 DLVRD:000 SUBMIT DATE:260726120000 \
             DONE DATE:260726120533 STAT:DELIVRD ERR:0 TEXT:",
        );
        assert_eq!(kannel.smsc_message_id.as_deref(), Some("1a2b3c"));
        assert_eq!(kannel.status, Some(DeliveryStatus::Delivered));
        assert_eq!(kannel.error_code.as_deref(), Some("0"));
        assert_eq!(
            kannel.done_date.map(|date| date.to_storage()),
            Some(String::from("2026-07-26T12:05:33Z"))
        );

        // A centre that runs the words together and pads with tabs.
        let compact = parse_receipt_body(
            "id:99\tsubmitdate:2607261200\tdonedate:2607261201\tstat:UNDELIV\terr:058",
        );
        assert_eq!(compact.smsc_message_id.as_deref(), Some("99"));
        assert_eq!(compact.status, Some(DeliveryStatus::Undeliverable));
        assert_eq!(compact.error_code.as_deref(), Some("058"));

        // A centre that puts `stat:` first and the identifier last.
        let reordered = parse_receipt_body("stat:EXPIRED err:001 id:ABC-123");
        assert_eq!(reordered.smsc_message_id.as_deref(), Some("ABC-123"));
        assert_eq!(reordered.status, Some(DeliveryStatus::Expired));

        // A centre that inserts undocumented vendor fields between the known
        // ones. `mccmnc:` must not be mistaken for anything.
        let vendor = parse_receipt_body(
            "id:77 mccmnc:61202 dlvrd:1 vendorRef:xyz stat:DELIVRD err:000 price:0.02",
        );
        assert_eq!(vendor.smsc_message_id.as_deref(), Some("77"));
        assert_eq!(vendor.delivered, Some(1));
        assert_eq!(vendor.status, Some(DeliveryStatus::Delivered));

        // Several spaces, a newline, an alphanumeric error code.
        let spaced = parse_receipt_body("id:5   stat:REJECTD\n  err:ESME_RSUBMITFAIL");
        assert_eq!(spaced.smsc_message_id.as_deref(), Some("5"));
        assert_eq!(spaced.status, Some(DeliveryStatus::Rejected));
        assert_eq!(spaced.error_code.as_deref(), Some("ESME_RSUBMITFAIL"));
    }

    /// The bug this catches: cutting `text:` at the first space or the first
    /// colon. Both truncate every receipt whose message contains one.
    #[test]
    fn the_text_field_swallows_spaces_colons_and_further_keys() {
        let receipt = parse_receipt_body("id:7 stat:DELIVRD text:call me at 10:30 id:not-an-id");

        assert_eq!(receipt.smsc_message_id.as_deref(), Some("7"));
        assert_eq!(
            receipt.text.as_deref(),
            Some("call me at 10:30 id:not-an-id")
        );
    }

    /// A token that merely *ends* with a known key must not match it. Scanning
    /// byte by byte instead of token by token made `myid:7` set the identifier.
    #[test]
    fn a_vendor_key_ending_in_a_known_one_is_not_matched() {
        let receipt = parse_receipt_body("myid:7 xstat:DELIVRD id:9");

        assert_eq!(receipt.smsc_message_id.as_deref(), Some("9"));
        assert_eq!(receipt.status, None);
    }

    /// CA-008-03 — an unreadable body is a receipt with nothing in it, never a
    /// panic and never a failure that would lose the record.
    #[test]
    fn an_unreadable_body_yields_an_empty_receipt_rather_than_a_failure() {
        for body in ["", "   ", ":::", "hello there", "id:", "\0\u{1}\u{2}"] {
            let receipt = parse_receipt_body(body);

            assert_eq!(receipt.status, None, "{body:?}");
            assert_eq!(receipt.raw, body, "{body:?} must keep what arrived");
        }

        assert_eq!(parse_receipt_body("id:").smsc_message_id, None);
    }

    /// A malformed date is dropped; the fields around it survive. Reporting an
    /// error instead would lose the identifier sitting next to it.
    #[test]
    fn a_malformed_date_costs_only_itself() {
        for date in ["", "not-a-date", "9999999999999", "2613261200", "26072612"] {
            let receipt = parse_receipt_body(&format!("id:7 submit date:{date} stat:DELIVRD"));

            assert_eq!(receipt.submit_date, None, "{date:?} must not parse");
            assert_eq!(receipt.smsc_message_id.as_deref(), Some("7"), "{date:?}");
            assert_eq!(receipt.status, Some(DeliveryStatus::Delivered), "{date:?}");
        }
    }

    /// `2613261200` is month 13. A hand-rolled calendar would accept it; the
    /// application's one parser does not.
    #[test]
    fn an_impossible_calendar_date_is_refused() {
        assert_eq!(
            parse_receipt_body("id:7 done date:2602301200").done_date,
            None,
            "30 February is not a date"
        );
    }

    /// A count that is not a number is dropped rather than defaulted to zero:
    /// "the centre said nothing" and "the centre said none" are different.
    #[test]
    fn a_non_numeric_count_is_absent_rather_than_zero() {
        let receipt = parse_receipt_body("id:7 sub:many dlvrd:001");

        assert_eq!(receipt.submitted, None);
        assert_eq!(receipt.delivered, Some(1));
    }

    /// **Non-regression, and the reason the property test exists.**
    ///
    /// The scanner tested `is_ascii_whitespace` on the leading byte and skipped
    /// tokens with `char::is_whitespace`. U+0085 and U+00A0 are whitespace to
    /// the second and not to the first, so a body containing either left the
    /// cursor where it was — an infinite loop, in the task that drains the
    /// delivery queue, on a body the message centre chooses.
    ///
    /// Both octets arrive from an ordinary Latin-1 body, so this is not a
    /// contrived input.
    #[test]
    fn a_non_ascii_whitespace_separator_terminates() {
        for separator in ['\u{0085}', '\u{00a0}', '\u{2028}'] {
            let receipt =
                parse_receipt_body(&format!("id:42{separator}stat:DELIVRD{separator}err:000"));

            assert_eq!(
                receipt.smsc_message_id.as_deref(),
                Some("42"),
                "separator {separator:?}"
            );
            assert_eq!(
                receipt.status,
                Some(DeliveryStatus::Delivered),
                "separator {separator:?}"
            );
        }
    }

    /// The same class of bug on the skip path: a body of nothing but non-ASCII
    /// whitespace must terminate and yield nothing.
    #[test]
    fn a_body_of_non_ascii_whitespace_terminates_with_nothing() {
        let receipt = parse_receipt_body("\u{00a0}\u{0085}\u{00a0}");

        assert_eq!(receipt.smsc_message_id, None);
        assert_eq!(receipt.status, None);
    }

    /// The first `id:` wins, not the last: a vendor prefix does not displace
    /// the identifier the convention puts at the head.
    #[test]
    fn the_first_identifier_wins_when_a_body_carries_two() {
        assert_eq!(
            parse_receipt_body("id:FIRST stat:DELIVRD id:SECOND")
                .smsc_message_id
                .as_deref(),
            Some("FIRST")
        );
    }
}
