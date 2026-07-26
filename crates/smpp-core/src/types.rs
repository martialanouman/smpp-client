//! Domain newtypes.
//!
//! Guide §5.6 and CLAUDE.md §4: *parse, don't validate*. Each type here is
//! built **once** through a validating constructor; downstream code
//! manipulates the safe type and never re-checks it. Every inner field is
//! private, so an invalid value is not merely unlikely — it is unrepresentable.

use uuid::Uuid;

use crate::error::{FieldRejection, SmppError};

/// A subscriber number, normalised to bare digits.
///
/// # What is validated here, and what is not
///
/// `smpp-core` depends on no other internal crate and, deliberately, on no
/// numbering-plan database: the validation below is the **protocol-level** one
/// — an SMPP address field carries at most 20 digits plus its NUL terminator
/// (spec §7.3, `destination_addr` is a 21-octet C-Octet String). Country rules,
/// line type and portability are the business of milestone 006 and the
/// `contacts` crate, which layers `phonenumber` on top of this type.
///
/// The stored form has no `+`: that is a presentation detail, restored by
/// [`Msisdn::to_e164`].
///
/// # Construction
///
/// The inner field is private and there is no other constructor, so the
/// validation cannot be bypassed:
///
/// ```compile_fail
/// // The tuple field is private: this does not compile.
/// let _ = smpp_core::types::Msisdn(String::from("2250102030405"));
/// ```
///
/// ```
/// use smpp_core::types::Msisdn;
///
/// let msisdn = Msisdn::parse("+225 01 02 03 04 05")?;
/// assert_eq!(msisdn.as_str(), "2250102030405");
/// # Ok::<(), smpp_core::SmppError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Msisdn(String);

impl Msisdn {
    /// Shortest accepted number, in digits.
    ///
    /// Short codes exist and are legitimate destinations; three digits is the
    /// floor below which no numbering plan routes anything.
    pub const MIN_DIGITS: usize = 3;

    /// Longest accepted number, in digits.
    ///
    /// An SMPP address is a `COctetString<1, 21>`: twenty digits plus the NUL
    /// terminator. E.164 itself caps at fifteen, but SMSCs do route longer
    /// national forms, and truncating here would corrupt them silently.
    pub const MAX_DIGITS: usize = 20;

    /// Characters accepted as visual separators and removed on parsing.
    const SEPARATORS: [char; 6] = [' ', '\t', '\n', '-', '(', ')'];

    /// Parses and normalises a number.
    ///
    /// Accepts one optional leading `+`, the separators above, and digits.
    ///
    /// # Errors
    ///
    /// [`SmppError::InvalidField`] with the reason: empty, illegal character,
    /// too short or too long. The rejected input is **never** included in the
    /// message — an MSISDN is personal data (CLAUDE.md §8).
    pub fn parse(raw: &str) -> Result<Self, SmppError> {
        let trimmed = raw.trim_matches(|character: char| {
            character.is_whitespace() || Self::SEPARATORS.contains(&character)
        });
        let digits_part = trimmed.strip_prefix('+').unwrap_or(trimmed);

        let mut digits = String::with_capacity(digits_part.len());

        for character in digits_part.chars() {
            if Self::SEPARATORS.contains(&character) || character == '.' {
                continue;
            }

            if !character.is_ascii_digit() {
                return Err(SmppError::invalid_field(
                    "msisdn",
                    FieldRejection::IllegalCharacter,
                ));
            }

            digits.push(character);
        }

        if digits.is_empty() {
            return Err(SmppError::invalid_field("msisdn", FieldRejection::Empty));
        }

        if digits.len() < Self::MIN_DIGITS {
            return Err(SmppError::invalid_field("msisdn", FieldRejection::TooShort));
        }

        if digits.len() > Self::MAX_DIGITS {
            return Err(SmppError::invalid_field("msisdn", FieldRejection::TooLong));
        }

        Ok(Self(digits))
    }

    /// The normalised number, digits only, no `+`.
    ///
    /// This is the form that goes into the `destination_addr` field of a PDU.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The number in its E.164 presentation form, `+` included.
    #[must_use]
    pub fn to_e164(&self) -> String {
        let mut rendered = String::with_capacity(self.0.len() + 1);
        rendered.push('+');
        rendered.push_str(&self.0);
        rendered
    }
}

impl core::fmt::Display for Msisdn {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A PDU sequence number.
///
/// Spec §7.1 bounds it to `1..=0x7FFFFFFF`: zero is not a valid correlation
/// key, and the high bit is reserved. Milestone 005 correlates a response with
/// its request through this value, so a zero slipping in would silently break
/// the pairing rather than fail loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SequenceNumber(u32);

impl SequenceNumber {
    /// First usable value.
    pub const FIRST: Self = Self(1);

    /// Last usable value, `0x7FFFFFFF`.
    pub const LAST: Self = Self(0x7FFF_FFFF);

    /// Builds a sequence number from a raw value.
    ///
    /// # Errors
    ///
    /// [`SmppError::InvalidField`] with [`FieldRejection::OutOfRange`] outside
    /// `1..=0x7FFFFFFF`.
    pub const fn new(value: u32) -> Result<Self, SmppError> {
        if value == 0 || value > Self::LAST.0 {
            return Err(SmppError::invalid_field(
                "sequence_number",
                FieldRejection::OutOfRange,
            ));
        }

        Ok(Self(value))
    }

    /// The raw value, as carried by the PDU header.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The next number in the sequence, wrapping to [`SequenceNumber::FIRST`]
    /// after [`SequenceNumber::LAST`].
    ///
    /// Wrapping is what the protocol prescribes: the space is finite, and a
    /// long-lived session does exhaust it. By the time it wraps, the responses
    /// to the first requests are long gone, so reuse is harmless.
    #[must_use]
    pub const fn next(self) -> Self {
        if self.0 >= Self::LAST.0 {
            Self::FIRST
        } else {
            Self(self.0 + 1)
        }
    }
}

impl From<SequenceNumber> for u32 {
    fn from(value: SequenceNumber) -> Self {
        value.0
    }
}

impl core::fmt::Display for SequenceNumber {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Generates a UUID-backed identifier newtype.
///
/// The four identifiers of the domain share the same shape; writing them out
/// four times would be four opportunities for them to drift apart.
macro_rules! uuid_newtype {
    ($(#[$meta:meta])* $name:ident, $field:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            /// Draws a fresh random identifier (UUID v4).
            #[must_use]
            // clippy::new_without_default asks for a `Default` next to any
            // argument-less `new`. Here the lint is wrong: see the comment
            // below on why `Default` is precisely what must NOT exist for an
            // identifier that has to be minted explicitly.
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing UUID, typically one read back from storage.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Parses the text form persisted in SQLite (spec §14.2).
            ///
            /// # Errors
            ///
            /// [`SmppError::InvalidField`] with
            /// [`FieldRejection::MalformedUuid`] if the text is not a UUID.
            pub fn parse(raw: &str) -> Result<Self, SmppError> {
                Uuid::parse_str(raw).map(Self).map_err(|_| {
                    SmppError::invalid_field($field, FieldRejection::MalformedUuid)
                })
            }
        }

        // NO `impl Default`, deliberately.
        //
        // `default()` would mint a fresh UUID, so a `..Default::default()` in a
        // struct literal would silently fabricate a brand-new identifier. For a
        // `client_message_id` — the write-ahead key whose whole purpose is to
        // make a replay idempotent (CLAUDE.md §4) — that turns a resumed
        // campaign into a duplicate send. `new()` stays explicit.

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

uuid_newtype!(
    /// Identifies a session profile, and every connection opened from it.
    ///
    /// Spec §8.2: a session is identified by a `session_id` (UUID) and is the
    /// key of the `session_profiles` table (§14.2). It is also the field of the
    /// `tracing` span that carries every log line of that session.
    SessionId,
    "session_id"
);

uuid_newtype!(
    /// Identifies a message **before** it is sent.
    ///
    /// This is the write-ahead key of CLAUDE.md §4: a message is persisted
    /// under this identifier before any `submit_sm` leaves, which is what makes
    /// its state transitions idempotent and a crash mid-send recoverable. The
    /// SMSC's own `message_id` only arrives with the response, and may never
    /// arrive at all.
    ClientMessageId,
    "client_message_id"
);

uuid_newtype!(
    /// Identifies a bulk-send campaign (spec §14.2, `campaigns.campaign_id`).
    ///
    /// # Why a campaign identifier is in the protocol crate
    ///
    /// It is not a protocol object, and `smpp-core` says elsewhere that it
    /// stays free of anything the wire format does not know about. The
    /// exception is deliberate and bounded: the `Message` aggregate carries a
    /// `campaign_id`, and milestone 006 moved that aggregate into `messaging`
    /// so the crate could own its `MessageRepository` port (ADR 0010). Since
    /// `persistence` implements that port, both crates need the *same*
    /// identifier type, and the only crate below both is this one.
    ///
    /// `ContactId` and `ListId` did **not** follow: nothing above them needs
    /// them yet, and they stay in `persistence` until `contacts` claims them
    /// at milestone 009.
    CampaignId,
    "campaign_id"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_international_number_parses() {
        let msisdn = Msisdn::parse("+225 01 02 03 04 05").expect("valid number");

        assert_eq!(msisdn.as_str(), "2250102030405");
        assert_eq!(msisdn.to_e164(), "+2250102030405");
    }

    #[test]
    fn separators_are_normalised_away() {
        for raw in [
            "+225-01-02-03-04-05",
            "+225 (01) 02.03.04.05",
            "\t+2250102030405\n",
            "2250102030405",
        ] {
            assert_eq!(
                Msisdn::parse(raw).expect("valid number").as_str(),
                "2250102030405",
                "{raw:?} was not normalised"
            );
        }
    }

    #[test]
    fn an_invalid_number_is_rejected() {
        for raw in [
            "",
            "+",
            "12",                                // too short
            "0123456789012345678901",            // 22 digits, too long
            "+225ABC0102030",                    // letters
            "225 01 02 03 04 05 06 07 08 09 10", // too long once normalised
            "++2250102030405",
            "+225+0102030405",
        ] {
            assert!(
                Msisdn::parse(raw).is_err(),
                "{raw:?} should have been rejected"
            );
        }
    }

    /// Guide §17.5 and CLAUDE.md §8: an MSISDN is personal data. The rejection
    /// message must say what was wrong, never echo the input.
    #[test]
    fn a_rejection_never_echoes_the_input() {
        let error = Msisdn::parse("+225ABC0102030").expect_err("letters");

        assert!(!error.to_string().contains("0102030"));
    }

    #[test]
    fn the_longest_and_shortest_accepted_numbers_are_at_the_documented_bounds() {
        assert!(Msisdn::parse(&"1".repeat(Msisdn::MIN_DIGITS)).is_ok());
        assert!(Msisdn::parse(&"1".repeat(Msisdn::MIN_DIGITS - 1)).is_err());
        assert!(Msisdn::parse(&"1".repeat(Msisdn::MAX_DIGITS)).is_ok());
        assert!(Msisdn::parse(&"1".repeat(Msisdn::MAX_DIGITS + 1)).is_err());
    }

    #[test]
    fn sequence_numbers_stay_within_the_range_of_the_specification() {
        assert!(SequenceNumber::new(0).is_err());
        assert_eq!(SequenceNumber::new(1).unwrap().get(), 1);
        assert_eq!(SequenceNumber::new(0x7FFF_FFFF).unwrap().get(), 0x7FFF_FFFF);
        assert!(SequenceNumber::new(0x8000_0000).is_err());
        assert!(SequenceNumber::new(u32::MAX).is_err());
    }

    #[test]
    fn sequence_numbers_wrap_back_to_the_first_value() {
        assert_eq!(SequenceNumber::FIRST.get(), 1);
        assert_eq!(SequenceNumber::FIRST.next().get(), 2);
        assert_eq!(SequenceNumber::LAST.get(), 0x7FFF_FFFF);
        assert_eq!(SequenceNumber::LAST.next(), SequenceNumber::FIRST);
    }

    #[test]
    fn a_sequence_number_survives_a_trip_through_the_wire_type() {
        let mut current = SequenceNumber::FIRST;

        for _ in 0..1_000 {
            assert_eq!(SequenceNumber::new(current.get()).unwrap(), current);
            current = current.next();
        }
    }

    #[test]
    fn identifiers_are_random_and_round_trip_through_their_text_form() {
        let session = SessionId::new();
        assert_ne!(session, SessionId::new());
        assert_eq!(SessionId::parse(&session.to_string()).unwrap(), session);

        let message = ClientMessageId::new();
        assert_ne!(message, ClientMessageId::new());
        assert_eq!(
            ClientMessageId::parse(&message.to_string()).unwrap(),
            message
        );
    }

    #[test]
    fn a_malformed_identifier_is_rejected() {
        assert!(SessionId::parse("not-a-uuid").is_err());
        assert!(ClientMessageId::parse("").is_err());
    }
}
