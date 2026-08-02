//! Source and destination addresses (deliverable L-006-03).
//!
//! Spec §7.3 gives both addresses the same wire shape — a `COctetString<1, 21>`
//! with a TON and an NPI beside it — and completely different rules. A
//! destination is a subscriber number. A source is very often not: a sender ID
//! is letters, and letters mean `source_addr_ton = 5` (`Alphanumeric`), a
//! constraint the operator has to be told about rather than discover in a
//! rejection (fiche §6).
//!
//! Everything here runs **before** anything is persisted or sent, which is
//! what CA-006-07 asks for: an invalid recipient leaves no row behind.
//!
//! # How far the E.164 normalisation goes, and where it stops
//!
//! A destination is parsed by [`smpp_core::types::Msisdn`]: separators
//! removed, one optional leading `+` accepted, digits only, 3 to 20 of them.
//! What comes out is the international form the `destination_addr` field
//! carries, and [`Msisdn::to_e164`] renders it back with its `+`.
//!
//! What is **not** done here is country-plan validation — that `+225 01…` is a
//! real Ivorian mobile prefix, that the length matches that plan, that the
//! line is not a fixed line. That needs a numbering-plan database
//! (`phonenumber`), a default region to resolve national forms against, and a
//! decision about what to do with a number the plan dislikes but the SMSC
//! would route anyway. All three belong with the import and validation of
//! milestone 009, which owns `contacts` and its default-region setting; doing
//! half of it here would mean two normalisations that disagree.
//!
//! The consequence is stated rather than hidden: a **national** form typed
//! without a country code — `0102030405` — is accepted here and will be
//! rejected by the SMSC, not by this application. The interface says so next
//! to the field.

use core::str::FromStr as _;

use smpp_core::octets::COctetString;
use smpp_core::types::Msisdn;
use smpp_core::values::{Npi, Ton};

/// Longest alphanumeric source address the GSM network carries.
///
/// Eleven characters: the limit is the 3GPP TS 23.040 alphanumeric address
/// field, not SMPP, whose `source_addr` would hold twenty. A twelve-character
/// sender ID is accepted by some SMSCs and truncated on the handset, which is
/// worse than a rejection.
pub const MAX_ALPHANUMERIC_SOURCE: usize = 11;

/// Why an address was refused.
///
/// The rejected value never appears in the message: an MSISDN is personal data
/// (CLAUDE.md §8), and a sender ID is close enough to a customer name to be
/// treated the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AddressError {
    /// The recipient is not a number this client can put on the wire.
    #[error("the recipient is not a valid subscriber number")]
    InvalidDestination,

    /// The recipient was empty.
    #[error("a recipient is required")]
    MissingDestination,

    /// The source address was empty.
    ///
    /// An empty `source_addr` is legal SMPP — the message centre substitutes
    /// its own — but only when the operator meant it. The interface therefore
    /// sends `None` rather than an empty string, and an empty string is a
    /// mistake.
    #[error("the source address is empty; omit it to let the message centre choose")]
    EmptySource,

    /// An alphanumeric source address longer than 11 characters.
    #[error("an alphanumeric source address holds at most {maximum} characters")]
    SourceTooLong {
        /// The ceiling, so the interface can show it.
        maximum: usize,
    },

    /// A numeric source address too long for the protocol field.
    #[error("the source address is not a valid number")]
    InvalidSource,

    /// The source address holds a character no SMSC accepts.
    ///
    /// Letters, digits, spaces, `.` and `-` are the portable set. Anything
    /// else — an accent, an emoji, a `@` — is rejected by most message centres
    /// and mangled by the rest.
    #[error("the source address holds a character message centres do not accept")]
    IllegalSourceCharacter,
}

/// A validated recipient, with the TON and NPI that describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    number: Msisdn,
    ton: Ton,
    npi: Npi,
}

/// The number [`Destination::campaign_placeholder`] is built from.
///
/// Unallocated by construction — see that constructor for why a campaign needs
/// one at all, and why a message that ever carried it would be refused by the
/// message centre rather than delivered to somebody.
const CAMPAIGN_PLACEHOLDER: &str = "+10000000000";

impl Destination {
    /// Parses a recipient, defaulting TON and NPI to the safe pair.
    ///
    /// `International` / `Isdn` (`1` / `1`) is what spec §23.3 prescribes and
    /// what every operator routes: a number normalised to its international
    /// form and announced as anything else is the commonest cause of an
    /// `ESME_RINVDSTADR`.
    ///
    /// # Errors
    ///
    /// [`AddressError::MissingDestination`] on an empty input,
    /// [`AddressError::InvalidDestination`] otherwise.
    pub fn parse(raw: &str) -> Result<Self, AddressError> {
        Self::parse_with(raw, Ton::International, Npi::Isdn)
    }

    /// Parses a recipient the operator described explicitly.
    ///
    /// TON and NPI are taken as given rather than inferred: an operator
    /// sending to a short code knows it is `NetworkSpecific`, and this crate
    /// has no way of telling.
    ///
    /// # Errors
    ///
    /// Same as [`Self::parse`].
    pub fn parse_with(raw: &str, ton: Ton, npi: Npi) -> Result<Self, AddressError> {
        if raw.trim().is_empty() {
            return Err(AddressError::MissingDestination);
        }

        let number = Msisdn::parse(raw).map_err(|_| AddressError::InvalidDestination)?;

        Ok(Self { number, ton, npi })
    }

    /// The stand-in a campaign's [`crate::submit::SubmitOptions`] carries.
    ///
    /// # Why a fabricated number exists at all
    ///
    /// `SubmitOptions` carries **one** recipient and a campaign has one per
    /// message, so the options a campaign is configured with hold a placeholder
    /// that [`crate::campaign::runner::CampaignPlan`] replaces for every
    /// recipient the feeder resolves. That type's own header records why the
    /// alternative — a second options type with the field removed — was
    /// rejected.
    ///
    /// # Why it is here and not at the boundary
    ///
    /// It was a `const` in the IPC layer, which put a manufactured MSISDN in a
    /// layer CLAUDE.md §3 keeps free of business decisions, and left the number
    /// beside the form rather than beside the type that explains it.
    ///
    /// The `ton` and `npi` are the campaign's own, and they are **validated**:
    /// a combination the address rules refuse fails when the campaign is
    /// created rather than on the first of two hundred thousand messages.
    ///
    /// # The number
    ///
    /// `+1 000 000 0000` — the `+1` country code with a national number of
    /// zeroes. It is syntactically an E.164 number and it is allocated to
    /// nobody, so a bug that let it reach a message centre produces an
    /// `ESME_RINVDSTADR` rather than a message to a stranger.
    ///
    /// # Errors
    ///
    /// [`AddressError::InvalidDestination`] if `ton` and `npi` cannot describe
    /// it.
    pub fn campaign_placeholder(ton: Ton, npi: Npi) -> Result<Self, AddressError> {
        Self::parse_with(CAMPAIGN_PLACEHOLDER, ton, npi)
    }

    /// The normalised number, digits only.
    #[must_use]
    pub const fn number(&self) -> &Msisdn {
        &self.number
    }

    /// `dest_addr_ton`.
    #[must_use]
    pub const fn ton(&self) -> Ton {
        self.ton
    }

    /// `dest_addr_npi`.
    #[must_use]
    pub const fn npi(&self) -> Npi {
        self.npi
    }

    /// The `destination_addr` field value.
    ///
    /// # Errors
    ///
    /// [`AddressError::InvalidDestination`] if the normalised number does not
    /// fit the 21-octet C-Octet String — unreachable, since `Msisdn` caps at
    /// twenty digits, and checked rather than asserted because an `expect`
    /// here would be a `panic!` in production code.
    pub fn to_field(&self) -> Result<COctetString<1, 21>, AddressError> {
        COctetString::from_str(self.number.as_str()).map_err(|_| AddressError::InvalidDestination)
    }
}

/// What kind of thing a source address is.
///
/// The distinction decides `source_addr_ton`, and the interface warns about
/// the second: an alphanumeric sender is refused outright by some operators
/// and silently replaced by others (fiche §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// Digits: a real originating number.
    Numeric,
    /// Letters: a sender ID, which forces `source_addr_ton = 5`.
    Alphanumeric,
}

/// A validated sender address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAddress {
    value: String,
    kind: SourceKind,
    ton: Ton,
    npi: Npi,
}

impl SourceAddress {
    /// Characters a source address may hold beyond letters and digits.
    ///
    /// The portable set. A `+` is deliberately absent: it is a presentation
    /// prefix, removed on the way in, and an SMSC that received one would
    /// treat the whole address as alphanumeric.
    const EXTRA_CHARACTERS: [char; 3] = [' ', '.', '-'];

    /// Parses a sender address, deriving TON and NPI from what it is.
    ///
    /// * digits → `International` / `Isdn`, the same reasoning as a
    ///   destination;
    /// * anything else → `Alphanumeric` / `Unknown`, which is what spec §7.4
    ///   prescribes and what the `MAX_ALPHANUMERIC_SOURCE` ceiling goes with.
    ///
    /// # Errors
    ///
    /// [`AddressError::EmptySource`] on an empty input,
    /// [`AddressError::SourceTooLong`] past eleven alphanumeric characters,
    /// [`AddressError::InvalidSource`] on an unusable number,
    /// [`AddressError::IllegalSourceCharacter`] on a character outside the
    /// portable set.
    pub fn parse(raw: &str) -> Result<Self, AddressError> {
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            return Err(AddressError::EmptySource);
        }

        let numeric = trimmed
            .strip_prefix('+')
            .unwrap_or(trimmed)
            .chars()
            .all(|character| character.is_ascii_digit());

        if numeric {
            let number = Msisdn::parse(trimmed).map_err(|_| AddressError::InvalidSource)?;

            return Ok(Self {
                value: number.as_str().to_owned(),
                kind: SourceKind::Numeric,
                ton: Ton::International,
                npi: Npi::Isdn,
            });
        }

        if !trimmed
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || Self::is_extra(character))
        {
            return Err(AddressError::IllegalSourceCharacter);
        }

        if trimmed.chars().count() > MAX_ALPHANUMERIC_SOURCE {
            return Err(AddressError::SourceTooLong {
                maximum: MAX_ALPHANUMERIC_SOURCE,
            });
        }

        Ok(Self {
            value: trimmed.to_owned(),
            kind: SourceKind::Alphanumeric,
            // Spec §7.4: an alphanumeric address is TON 5, and the numbering
            // plan is meaningless for it.
            ton: Ton::Alphanumeric,
            npi: Npi::Unknown,
        })
    }

    /// Parses a sender address whose TON and NPI the operator chose.
    ///
    /// The kind is still derived from the characters, because it is a fact
    /// about the value rather than an opinion — but the announced TON and NPI
    /// are the operator's, since some message centres expect `National` for a
    /// short code.
    ///
    /// # Errors
    ///
    /// Same as [`Self::parse`].
    pub fn parse_with(raw: &str, ton: Ton, npi: Npi) -> Result<Self, AddressError> {
        Ok(Self::parse(raw)?.with_ton(ton).with_npi(npi))
    }

    /// The same address announced under another type of number.
    ///
    /// Separate from [`Self::with_npi`] so a caller can override **one** of the
    /// two. The interface offers them as two independent selectors, and
    /// requiring both to be set before either is honoured means silently
    /// discarding a choice the operator made — which CA-006-06 forbids.
    #[must_use]
    pub const fn with_ton(mut self, ton: Ton) -> Self {
        self.ton = ton;
        self
    }

    /// The same address announced under another numbering plan.
    #[must_use]
    pub const fn with_npi(mut self, npi: Npi) -> Self {
        self.npi = npi;
        self
    }

    /// Reports whether `character` is one of the accepted separators.
    fn is_extra(character: char) -> bool {
        Self::EXTRA_CHARACTERS.contains(&character)
    }

    /// The address as it goes on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Whether this is a number or a sender ID.
    #[must_use]
    pub const fn kind(&self) -> SourceKind {
        self.kind
    }

    /// Whether the interface should warn about operator support.
    #[must_use]
    pub const fn needs_alphanumeric_warning(&self) -> bool {
        matches!(self.kind, SourceKind::Alphanumeric)
    }

    /// `source_addr_ton`.
    #[must_use]
    pub const fn ton(&self) -> Ton {
        self.ton
    }

    /// `source_addr_npi`.
    #[must_use]
    pub const fn npi(&self) -> Npi {
        self.npi
    }

    /// The `source_addr` field value.
    ///
    /// # Errors
    ///
    /// [`AddressError::InvalidSource`] if the value does not fit the 21-octet
    /// C-Octet String.
    pub fn to_field(&self) -> Result<COctetString<1, 21>, AddressError> {
        COctetString::from_str(&self.value).map_err(|_| AddressError::InvalidSource)
    }
}

/// The empty `source_addr` field, for a message that lets the SMSC choose.
///
/// # Errors
///
/// Never in practice: a `COctetString` of length zero is always
/// representable. The `Result` exists because the constructor is fallible and
/// an `expect` would be a `panic!` in production code.
pub(crate) fn empty_address() -> Result<COctetString<1, 21>, AddressError> {
    COctetString::from_str("").map_err(|_| AddressError::InvalidSource)
}

#[cfg(test)]
mod tests {
    use super::{AddressError, Destination, SourceAddress, SourceKind, MAX_ALPHANUMERIC_SOURCE};
    use smpp_core::values::{Npi, Ton};

    #[test]
    fn an_international_recipient_is_normalised_to_bare_digits() {
        let destination = Destination::parse("+225 01 02 03 04 05").expect("valid");

        assert_eq!(destination.number().as_str(), "2250102030405");
        assert_eq!(destination.number().to_e164(), "+2250102030405");
    }

    #[test]
    fn separators_and_surrounding_space_are_removed() {
        for raw in [
            "+225-01-02-03-04-05",
            "  +225 (01) 02.03.04.05 ",
            "2250102030405",
        ] {
            assert_eq!(
                Destination::parse(raw).expect("valid").number().as_str(),
                "2250102030405",
                "{raw:?}"
            );
        }
    }

    /// The safe defaults of spec §23.3, and the reason an operator almost
    /// never has to touch them.
    #[test]
    fn a_recipient_defaults_to_international_e164() {
        let destination = Destination::parse("+2250102030405").expect("valid");

        assert_eq!(destination.ton(), Ton::International);
        assert_eq!(destination.npi(), Npi::Isdn);
    }

    #[test]
    fn a_short_code_recipient_keeps_the_type_the_operator_chose() {
        let destination =
            Destination::parse_with("3615", Ton::NetworkSpecific, Npi::Unknown).expect("valid");

        assert_eq!(destination.ton(), Ton::NetworkSpecific);
        assert_eq!(destination.number().as_str(), "3615");
    }

    #[test]
    fn an_empty_recipient_is_distinguished_from_an_invalid_one() {
        assert_eq!(
            Destination::parse("   ").expect_err("empty"),
            AddressError::MissingDestination
        );
        assert_eq!(
            Destination::parse("+225ABC").expect_err("letters"),
            AddressError::InvalidDestination
        );
    }

    #[test]
    fn an_invalid_recipient_is_rejected() {
        for raw in ["12", "0123456789012345678901", "+225ABC0102030", "++225010"] {
            assert!(Destination::parse(raw).is_err(), "{raw:?} was accepted");
        }
    }

    /// CLAUDE.md §8: an MSISDN is personal data and never reaches a message.
    #[test]
    fn a_rejection_never_echoes_the_number() {
        let error = Destination::parse("+225ABC0102030").expect_err("letters");

        assert!(!error.to_string().contains("0102030"));
    }

    #[test]
    fn a_numeric_sender_is_announced_as_an_international_number() {
        let source = SourceAddress::parse("+2250102030405").expect("valid");

        assert_eq!(source.kind(), SourceKind::Numeric);
        assert_eq!(source.ton(), Ton::International);
        assert_eq!(source.npi(), Npi::Isdn);
        assert_eq!(source.as_str(), "2250102030405");
        assert!(!source.needs_alphanumeric_warning());
    }

    /// Fiche §6: a sender ID forces `source_addr_ton = 5`, and the interface
    /// has to say so rather than let the operator discover a rejection.
    #[test]
    fn a_sender_id_forces_the_alphanumeric_type_and_asks_for_a_warning() {
        let source = SourceAddress::parse("ShinobiSMS").expect("valid");

        assert_eq!(source.kind(), SourceKind::Alphanumeric);
        assert_eq!(source.ton(), Ton::Alphanumeric);
        assert_eq!(u8::from(source.ton()), 5);
        assert_eq!(source.npi(), Npi::Unknown);
        assert!(source.needs_alphanumeric_warning());
    }

    #[test]
    fn a_sender_id_is_capped_at_eleven_characters() {
        assert!(SourceAddress::parse(&"A".repeat(MAX_ALPHANUMERIC_SOURCE)).is_ok());

        assert_eq!(
            SourceAddress::parse(&"A".repeat(MAX_ALPHANUMERIC_SOURCE + 1)).expect_err("too long"),
            AddressError::SourceTooLong {
                maximum: MAX_ALPHANUMERIC_SOURCE
            }
        );
    }

    /// A mixed address is alphanumeric, so the cap applies to it too — this is
    /// the case a "only reject if it is all letters" rule would have missed.
    #[test]
    fn a_mixed_sender_is_alphanumeric_and_capped_like_one() {
        let source = SourceAddress::parse("Shinobi2026").expect("eleven characters");
        assert_eq!(source.kind(), SourceKind::Alphanumeric);

        assert!(SourceAddress::parse("Shinobi20265").is_err());
    }

    #[test]
    fn a_sender_id_may_hold_spaces_dots_and_dashes() {
        for raw in ["Mon Shop", "S.A.R.L", "Bank-A"] {
            assert!(SourceAddress::parse(raw).is_ok(), "{raw:?} was rejected");
        }
    }

    #[test]
    fn a_sender_id_may_not_hold_an_accent_or_a_symbol() {
        for raw in ["Café", "A@B", "Shinobi™"] {
            assert_eq!(
                SourceAddress::parse(raw).expect_err("illegal"),
                AddressError::IllegalSourceCharacter,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn an_empty_sender_is_rejected_rather_than_silently_omitted() {
        assert_eq!(
            SourceAddress::parse("  ").expect_err("empty"),
            AddressError::EmptySource
        );
    }

    #[test]
    fn a_numeric_sender_too_long_for_the_field_is_rejected() {
        assert_eq!(
            SourceAddress::parse(&"1".repeat(21)).expect_err("too long"),
            AddressError::InvalidSource
        );
    }

    #[test]
    fn an_operator_may_override_the_type_of_a_sender_without_changing_its_kind() {
        let source =
            SourceAddress::parse_with("3615", Ton::National, Npi::National).expect("valid");

        assert_eq!(source.ton(), Ton::National);
        assert_eq!(source.npi(), Npi::National);
        assert_eq!(source.kind(), SourceKind::Numeric);
    }

    /// One of the two may be overridden on its own: the interface offers two
    /// independent selectors, and dropping a choice the operator made because
    /// the other one was left alone is what CA-006-06 forbids.
    #[test]
    fn either_of_the_two_sender_fields_may_be_overridden_alone() {
        let derived = SourceAddress::parse("ShinobiSMS").expect("valid");
        assert_eq!(derived.ton(), Ton::Alphanumeric);
        assert_eq!(derived.npi(), Npi::Unknown);

        let npi_only = derived.clone().with_npi(Npi::Isdn);
        assert_eq!(npi_only.ton(), Ton::Alphanumeric, "the derived TON stands");
        assert_eq!(npi_only.npi(), Npi::Isdn, "and the chosen NPI is honoured");

        let ton_only = derived.with_ton(Ton::National);
        assert_eq!(ton_only.ton(), Ton::National);
        assert_eq!(ton_only.npi(), Npi::Unknown);
    }

    #[test]
    fn a_validated_address_fits_the_protocol_field() {
        let destination = Destination::parse("+2250102030405").expect("valid");
        assert_eq!(
            destination.to_field().expect("fits").as_str(),
            "2250102030405"
        );

        let source = SourceAddress::parse("ShinobiSMS").expect("valid");
        assert_eq!(source.to_field().expect("fits").as_str(), "ShinobiSMS");
    }

    /// The placeholder carries the campaign's own TON and NPI, so a combination
    /// the address rules refuse is refused when the campaign is created and not
    /// on the first of two hundred thousand messages.
    #[test]
    fn the_campaign_placeholder_carries_the_campaign_type_of_number() {
        let placeholder = Destination::campaign_placeholder(Ton::NetworkSpecific, Npi::National)
            .expect("the placeholder parses");

        assert_eq!(placeholder.ton(), Ton::NetworkSpecific);
        assert_eq!(placeholder.npi(), Npi::National);
        assert!(placeholder.to_field().is_ok());
    }

    /// It must not be a number anybody has. A bug that let it reach a message
    /// centre has to produce an `ESME_RINVDSTADR`, not a message to a stranger.
    #[test]
    fn the_campaign_placeholder_is_not_an_allocated_number() {
        let placeholder = Destination::campaign_placeholder(Ton::International, Npi::Isdn)
            .expect("the placeholder parses");

        let digits = placeholder.number().as_str();

        assert!(digits.starts_with('1'), "{digits}");
        assert!(
            digits.chars().skip(1).all(|digit| digit == '0'),
            "the national part must be zeroes, not a routable number: {digits}"
        );
    }
}
