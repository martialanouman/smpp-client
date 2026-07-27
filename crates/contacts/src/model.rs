//! The contact aggregate, its identifiers and its lists.
//!
//! # Why this lives here and not in `persistence`
//!
//! It was written at milestone 002 next to the SQLx code that stores it, and
//! ADR 0007 said so explicitly: the consuming crate was an empty shell, so
//! paying the cost of the inversion — an upward `persistence` → `contacts`
//! edge — would have bought nothing. **CA-009-13** is the deadline that ADR set
//! itself, and this is the move; ADR 0012 records it.
//!
//! What follows from the move is the point: `contacts` owns the import that
//! decides whether a number is valid and what kind of line it is, so it owns
//! the type carrying that verdict. `persistence` re-exports every type of this
//! module, so `persistence::Contact` still resolves and no call site outside
//! the two crates changed.

use smpp_core::time::Timestamp;
use smpp_core::types::Msisdn;
use uuid::Uuid;

/// Generates a UUID-backed identifier newtype.
///
/// Same shape as the `uuid_newtype!` of `smpp-core` and of the one that used
/// to sit in `persistence::records::ids`, deliberately: identifiers written
/// out by hand are chances for them to drift.
macro_rules! uuid_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            /// Draws a fresh random identifier (UUID v4).
            #[must_use]
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing UUID.
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
            /// Returns `None` rather than an error, for the reason
            /// `MessageState::parse` does: the two callers of this function —
            /// the storage reading a row back and the IPC layer validating an
            /// argument from the WebView — want different errors out of the
            /// same failure, and each names the column or the field it knows
            /// about.
            #[must_use]
            pub fn parse(raw: &str) -> Option<Self> {
                Uuid::parse_str(raw).ok().map(Self)
            }
        }

        // NO `impl Default`. `default()` would mint a fresh UUID, so a
        // `..Default::default()` in a struct literal would silently fabricate
        // a brand-new identifier instead of reusing one.

        impl core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(formatter, "{}", self.0)
            }
        }
    };
}

uuid_newtype!(
    /// Identifies a contact (spec §14.2, `contacts.contact_id`).
    ContactId
);

uuid_newtype!(
    /// Identifies a contact list (spec §14.2, `contact_lists.list_id`).
    ListId
);

uuid_newtype!(
    /// Identifies a saved column-mapping profile (CA-009-09).
    ProfileId
);

/// What kind of line a number reaches, as the numbering plan describes it.
///
/// A typed enum rather than the bare `String` the column used to carry
/// (CLAUDE.md §4). The distinction is not cosmetic: CA-009-06 asks for a
/// "mobiles only" switch that actually excludes fixed lines, and a switch that
/// compared strings would be one typo away from excluding nothing at all.
///
/// The variants are the subset of `phonenumber::Type` that changes a decision
/// here. Everything else — pager, personal number, UAN, voicemail — collapses
/// into [`Self::Other`]: they are neither a handset nor a landline, and
/// treating them as a third thing would multiply the cases without adding a
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LineType {
    /// A handset. The only type "mobiles only" keeps.
    Mobile,
    /// A landline.
    FixedLine,
    /// A range the plan allocates to both, and does not split.
    ///
    /// Kept by "mobiles only": in plans that do not separate the two — North
    /// America is the notable one — every mobile lands here, so excluding it
    /// would exclude every American mobile.
    FixedLineOrMobile,
    /// A number that is neither, or whose plan says nothing useful.
    Other,
    /// The plan has no opinion.
    Unknown,
}

impl LineType {
    /// The text form stored in SQLite and shown by the interface.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Mobile => "mobile",
            Self::FixedLine => "fixed_line",
            Self::FixedLineOrMobile => "fixed_line_or_mobile",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }

    /// Parses the stored text form.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "mobile" => Some(Self::Mobile),
            "fixed_line" => Some(Self::FixedLine),
            "fixed_line_or_mobile" => Some(Self::FixedLineOrMobile),
            "other" => Some(Self::Other),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Whether a "mobiles only" import keeps this line.
    ///
    /// [`Self::FixedLineOrMobile`] passes and [`Self::Unknown`] does not: the
    /// first is a plan that cannot tell mobiles from landlines, so rejecting
    /// it rejects every mobile of that country; the second is a number whose
    /// plan is not known at all, which is precisely what the filter is meant
    /// to catch.
    #[must_use]
    pub const fn is_mobile(self) -> bool {
        matches!(self, Self::Mobile | Self::FixedLineOrMobile)
    }
}

/// A contact (spec §14.2, `contacts`; spec §11.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    /// Primary key.
    pub contact_id: ContactId,
    /// Subscriber number, normalised to its international form.
    pub msisdn: Msisdn,
    /// ISO 3166-1 alpha-2 country, when it could be derived.
    pub country: Option<String>,
    /// Whether the number passed validation at import time.
    pub valid: bool,
    /// Line type reported by the numbering plan.
    pub line_type: Option<LineType>,
    /// Template variables, as an opaque JSON object.
    pub attributes: Option<String>,
    /// Where the contact came from (`import_csv`, `import_xlsx`…).
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

#[cfg(test)]
mod tests {
    use super::{ContactId, LineType, ListId};

    #[test]
    fn an_identifier_survives_a_round_trip_through_its_text_form() {
        let identifier = ListId::new();

        assert_eq!(
            ListId::parse(&identifier.to_string()).expect("own output parses"),
            identifier
        );
    }

    #[test]
    fn a_malformed_identifier_is_rejected_rather_than_defaulted() {
        assert!(ContactId::parse("not-a-uuid").is_none());
    }

    #[test]
    fn two_fresh_identifiers_differ() {
        assert_ne!(ContactId::new(), ContactId::new());
    }

    #[test]
    fn a_line_type_survives_a_round_trip_through_its_stored_text() {
        for line_type in [
            LineType::Mobile,
            LineType::FixedLine,
            LineType::FixedLineOrMobile,
            LineType::Other,
            LineType::Unknown,
        ] {
            assert_eq!(
                LineType::parse(line_type.code()).expect("own output parses"),
                line_type
            );
        }

        assert!(LineType::parse("MOBILE").is_none());
    }

    /// CA-009-06, and the two cases a naive `== Mobile` would get wrong.
    #[test]
    fn mobiles_only_keeps_ambiguous_plans_and_drops_unknown_ones() {
        assert!(LineType::Mobile.is_mobile());
        assert!(
            LineType::FixedLineOrMobile.is_mobile(),
            "a plan that does not split the two would lose every mobile"
        );
        assert!(!LineType::FixedLine.is_mobile());
        assert!(!LineType::Unknown.is_mobile());
        assert!(!LineType::Other.is_mobile());
    }
}
