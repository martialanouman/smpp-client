//! Identifier newtypes owned by this crate.
//!
//! # Why they are here and not in `smpp-core`
//!
//! `smpp-core` already defines [`smpp_core::types::SessionId`] and
//! [`smpp_core::types::ClientMessageId`], and this crate uses those. It stops
//! there on purpose: a campaign, a contact and a contact list are not protocol
//! objects, and `smpp-core` is the crate that must stay free of anything the
//! wire format does not know about.
//!
//! Their long-term home is the crate that owns the aggregate — `messaging` for
//! campaigns, `contacts` for contacts and lists. Neither exists yet
//! (milestones 004 and 006), and putting them there today would make
//! `persistence` depend on two crates that are empty shells. They live here
//! until then; moving them is a re-export away, and the type identity of a
//! `CampaignId` does not change when it does.

use uuid::Uuid;

use crate::PersistenceError;

/// Generates a UUID-backed identifier newtype.
///
/// Same shape as the `uuid_newtype!` of `smpp-core`, deliberately: three more
/// identifiers written out by hand would be three chances for them to drift.
/// It is a private macro in both crates rather than a shared one, because the
/// crate that would export it is the one that must not grow domain concepts.
macro_rules! uuid_newtype {
    ($(#[$meta:meta])* $name:ident, $table:literal, $column:literal) => {
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
            /// # Errors
            ///
            /// [`PersistenceError::MalformedRow`] if the text is not a UUID.
            pub fn parse(raw: &str) -> Result<Self, PersistenceError> {
                Uuid::parse_str(raw)
                    .map(Self)
                    .map_err(|_| PersistenceError::MalformedRow {
                        table: $table,
                        column: $column,
                        expected: "a UUID in canonical form",
                    })
            }
        }

        // NO `impl Default`. `default()` would mint a fresh UUID, so a
        // `..Default::default()` in a struct literal would silently fabricate
        // a brand-new identifier instead of reusing one — the same trap
        // `smpp-core` calls out on `ClientMessageId`.

        impl core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(formatter, "{}", self.0)
            }
        }
    };
}

uuid_newtype!(
    /// Identifies a bulk-send campaign (spec §14.2, `campaigns.campaign_id`).
    CampaignId,
    "campaigns",
    "campaign_id"
);

uuid_newtype!(
    /// Identifies a contact (spec §14.2, `contacts.contact_id`).
    ContactId,
    "contacts",
    "contact_id"
);

uuid_newtype!(
    /// Identifies a contact list (spec §14.2, `contact_lists.list_id`).
    ListId,
    "contact_lists",
    "list_id"
);

#[cfg(test)]
mod tests {
    use super::{CampaignId, ContactId, ListId};

    #[test]
    fn an_identifier_survives_a_round_trip_through_its_text_form() {
        let identifier = CampaignId::new();

        assert_eq!(
            CampaignId::parse(&identifier.to_string()).expect("own output parses"),
            identifier
        );
    }

    #[test]
    fn two_fresh_identifiers_differ() {
        assert_ne!(ContactId::new(), ContactId::new());
    }

    #[test]
    fn a_malformed_identifier_names_its_column_without_echoing_the_value() {
        let rejection = ListId::parse("not-a-uuid").expect_err("must be rejected");

        let rendered = rejection.to_string();
        assert!(rendered.contains("contact_lists.list_id"), "{rendered}");
        assert!(!rendered.contains("not-a-uuid"), "{rendered}");
    }
}
