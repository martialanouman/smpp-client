//! The closed sets stored as text.
//!
//! Guide §5.6 and CLAUDE.md §4: a constrained field is an enum, never a bare
//! `String`. Each type here owns both halves of its mapping — the text SQLite
//! stores and the parse that reads it back — so the two cannot drift, and the
//! round-trip tests at the bottom of the file hold every variant to it.

use crate::PersistenceError;

/// Generates the text mapping of a stored enum.
macro_rules! stored_enum {
    (
        $(#[$meta:meta])*
        $name:ident, $table:literal, $column:literal, $expected:literal {
            $($(#[$variant_meta:meta])* $variant:ident => $text:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[non_exhaustive]
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The text form stored in SQLite.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// Parses the text form read from SQLite.
            ///
            /// # Errors
            ///
            /// [`PersistenceError::MalformedRow`] if the text is not one of
            /// the accepted values.
            pub fn parse(raw: &str) -> Result<Self, PersistenceError> {
                match raw {
                    $($text => Ok(Self::$variant),)+
                    _ => Err(PersistenceError::MalformedRow {
                        table: $table,
                        column: $column,
                        expected: $expected,
                    }),
                }
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

stored_enum!(
    /// How an ESME binds to the SMSC (spec §14.2, §7.2).
    BindType,
    "session_profiles",
    "bind_type",
    "transmitter, receiver or transceiver"
    {
        /// Sends only.
        Transmitter => "transmitter",
        /// Receives only.
        Receiver => "receiver",
        /// Sends and receives on the same connection.
        Transceiver => "transceiver",
    }
);

stored_enum!(
    /// Where a campaign stands in the lifecycle of spec §10.3.
    ///
    /// Unlike `MessageState` this set is **not** mirrored by a `CHECK`
    /// constraint: spec §14.2 writes the column's domain as
    /// `CREATED|RUNNING|PAUSED|COMPLETED|...`, and freezing an open list into
    /// the file format would turn a future status into a migration.
    CampaignStatus,
    "campaigns",
    "status",
    "one of CREATED, VALIDATED, RUNNING, PAUSED, COMPLETED, CANCELLED, FAILED"
    {
        /// Created, recipients not resolved yet.
        Created => "CREATED",
        /// Recipients and template checked, ready to start.
        Validated => "VALIDATED",
        /// Sending.
        Running => "RUNNING",
        /// Feeding suspended; the in-flight window drains normally.
        Paused => "PAUSED",
        /// Every message reached a terminal state.
        Completed => "COMPLETED",
        /// Stopped by the operator.
        Cancelled => "CANCELLED",
        /// Stopped by an error the campaign could not recover from.
        Failed => "FAILED",
    }
);

stored_enum!(
    /// Which way a logged PDU travelled (spec §14.2, `pdu_log.direction`).
    PduDirection,
    "pdu_log",
    "direction",
    "in or out"
    {
        /// Received from the SMSC.
        Inbound => "in",
        /// Sent to the SMSC.
        Outbound => "out",
    }
);

#[cfg(test)]
mod tests {
    use super::{BindType, CampaignStatus, PduDirection};

    /// A variant whose text form does not parse back is a row this version
    /// wrote and cannot read. Checking every variant of every stored enum is
    /// cheap; discovering the hole in production is not.
    macro_rules! assert_round_trips {
        ($name:ident) => {
            for variant in $name::ALL {
                assert_eq!(
                    $name::parse(variant.as_str()).expect("own output parses"),
                    *variant
                );
            }
        };
    }

    #[test]
    fn every_stored_variant_parses_back() {
        assert_round_trips!(BindType);
        assert_round_trips!(CampaignStatus);
        assert_round_trips!(PduDirection);
    }

    #[test]
    fn the_stored_text_matches_the_specification() {
        assert_eq!(BindType::Transceiver.as_str(), "transceiver");
        assert_eq!(PduDirection::Inbound.as_str(), "in");
    }

    #[test]
    fn an_unknown_value_is_rejected_without_being_echoed() {
        let rejection = CampaignStatus::parse("PENDING").expect_err("must be rejected");

        let rendered = rejection.to_string();
        assert!(rendered.contains("campaigns.status"), "{rendered}");
        assert!(!rendered.contains("PENDING"), "{rendered}");
    }
}
