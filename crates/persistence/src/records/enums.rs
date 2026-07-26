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
    /// Where a message stands in the lifecycle of spec §14.3.
    ///
    /// `QUEUED` → `SENT` → `ACCEPTED` → `DELIVERED` | `FAILED` | `EXPIRED`,
    /// with a rejected `submit_sm_resp` jumping straight to `FAILED`.
    ///
    /// This crate stores the state it is given and does not police the
    /// transitions: the state machine belongs to `messaging` (milestone 004
    /// onwards). What it does guarantee is that no value outside this set can
    /// reach the column — the enum on the way in, a `CHECK` constraint on the
    /// file itself.
    MessageState,
    "messages",
    "state",
    "one of QUEUED, SENT, ACCEPTED, DELIVERED, FAILED, EXPIRED"
    {
        /// Persisted, not yet handed to a session. The write-ahead state.
        Queued => "QUEUED",
        /// `submit_sm` has left.
        Sent => "SENT",
        /// `submit_sm_resp` came back clean, with an SMSC message identifier.
        Accepted => "ACCEPTED",
        /// A delivery receipt reported success.
        Delivered => "DELIVERED",
        /// Rejected by the SMSC, or a delivery receipt reported failure.
        Failed => "FAILED",
        /// The SMSC gave up before the validity period ran out.
        Expired => "EXPIRED",
    }
);

impl MessageState {
    /// Reports whether no further transition is expected.
    ///
    /// A resumed campaign (spec §10.5) restarts from the messages that are
    /// *not* terminal; anything else would re-send what already went out.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Failed | Self::Expired)
    }
}

stored_enum!(
    /// Where a campaign stands in the lifecycle of spec §10.3.
    ///
    /// Unlike [`MessageState`] this set is **not** mirrored by a `CHECK`
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
    use super::{BindType, CampaignStatus, MessageState, PduDirection};

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
        assert_round_trips!(MessageState);
        assert_round_trips!(CampaignStatus);
        assert_round_trips!(PduDirection);
    }

    #[test]
    fn the_stored_text_matches_the_specification() {
        assert_eq!(MessageState::Queued.as_str(), "QUEUED");
        assert_eq!(BindType::Transceiver.as_str(), "transceiver");
        assert_eq!(PduDirection::Inbound.as_str(), "in");
    }

    #[test]
    fn an_unknown_value_is_rejected_without_being_echoed() {
        let rejection = MessageState::parse("PENDING").expect_err("must be rejected");

        let rendered = rejection.to_string();
        assert!(rendered.contains("messages.state"), "{rendered}");
        assert!(!rendered.contains("PENDING"), "{rendered}");
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
}
