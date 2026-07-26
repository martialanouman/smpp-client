//! Conversions between stored columns and domain types.
//!
//! SQLite has four storage classes, and `INTEGER` is a signed 64-bit one — so
//! every unsigned field of a record crosses this module on the way in and on
//! the way out. The narrowing conversions are all `try_from`: the workspace
//! denies `cast_possible_truncation` precisely so that a `window_size` of
//! 5 000 000 000 in a hand-edited file becomes an error rather than a
//! plausible-looking small number.

use smpp_core::types::{ClientMessageId, Msisdn, SessionId};
use smpp_core::values::{
    CommandStatus, DataCoding, Gsm7BitCharset, Gsm7BitPacking, Npi, SmppVersion, Ton,
};

use crate::records::{CampaignId, ContactId, ListId};
use crate::{PersistenceError, Timestamp};

/// Widens an unsigned count to the signed integer SQLite stores.
///
/// Infallible, and the direction that never needs a check.
pub(crate) fn store_u32(value: u32) -> i64 {
    i64::from(value)
}

/// Widens a port number to the signed integer SQLite stores.
pub(crate) fn store_u16(value: u16) -> i64 {
    i64::from(value)
}

/// Widens a protocol octet to the signed integer SQLite stores.
pub(crate) fn store_u8(value: u8) -> i64 {
    i64::from(value)
}

/// Narrows a stored integer back to a count.
pub(crate) fn read_u32(
    value: i64,
    table: &'static str,
    column: &'static str,
) -> Result<u32, PersistenceError> {
    u32::try_from(value).map_err(|_| PersistenceError::MalformedRow {
        table,
        column,
        expected: "a non-negative 32-bit integer",
    })
}

/// Narrows a stored integer back to a port number.
pub(crate) fn read_u16(
    value: i64,
    table: &'static str,
    column: &'static str,
) -> Result<u16, PersistenceError> {
    u16::try_from(value).map_err(|_| PersistenceError::MalformedRow {
        table,
        column,
        expected: "a TCP port between 0 and 65535",
    })
}

/// Narrows a stored integer back to a protocol octet.
fn read_u8(value: i64, table: &'static str, column: &'static str) -> Result<u8, PersistenceError> {
    u8::try_from(value).map_err(|_| PersistenceError::MalformedRow {
        table,
        column,
        expected: "a single octet between 0 and 255",
    })
}

/// Reads an optional type-of-number column.
pub(crate) fn read_ton(
    value: Option<i64>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<Ton>, PersistenceError> {
    value
        .map(|raw| read_u8(raw, table, column).map(Ton::from))
        .transpose()
}

/// Reads an optional numbering-plan column.
pub(crate) fn read_npi(
    value: Option<i64>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<Npi>, PersistenceError> {
    value
        .map(|raw| read_u8(raw, table, column).map(Npi::from))
        .transpose()
}

/// Reads an optional data-coding column.
pub(crate) fn read_data_coding(
    value: Option<i64>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<DataCoding>, PersistenceError> {
    value
        .map(|raw| read_u8(raw, table, column).map(DataCoding::from))
        .transpose()
}

/// Reads an optional `command_status` column.
///
/// Every 32-bit value is a legal `command_status`: the specification reserves
/// whole ranges for vendors, and `CommandStatus::Other` preserves the ones this
/// build does not know (spec §7.6).
pub(crate) fn read_command_status(
    value: Option<i64>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<CommandStatus>, PersistenceError> {
    value
        .map(|raw| read_u32(raw, table, column).map(CommandStatus::from))
        .transpose()
}

/// Stores a `command_status`.
pub(crate) fn store_command_status(status: CommandStatus) -> i64 {
    store_u32(u32::from(status))
}

/// Reads a required timestamp column.
pub(crate) fn read_timestamp(
    raw: &str,
    table: &'static str,
    column: &'static str,
) -> Result<Timestamp, PersistenceError> {
    Timestamp::parse(raw).map_err(|_| PersistenceError::MalformedRow {
        table,
        column,
        expected: "an RFC 3339 instant such as 2026-07-26T12:00:00Z",
    })
}

/// Reads an optional timestamp column.
pub(crate) fn read_optional_timestamp(
    raw: Option<&str>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<Timestamp>, PersistenceError> {
    raw.map(|value| read_timestamp(value, table, column))
        .transpose()
}

/// Reads a required session identifier column.
pub(crate) fn read_session_id(
    raw: &str,
    table: &'static str,
    column: &'static str,
) -> Result<SessionId, PersistenceError> {
    SessionId::parse(raw).map_err(|_| PersistenceError::MalformedRow {
        table,
        column,
        expected: "a UUID in canonical form",
    })
}

/// Reads an optional session identifier column.
pub(crate) fn read_optional_session_id(
    raw: Option<&str>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<SessionId>, PersistenceError> {
    raw.map(|value| read_session_id(value, table, column))
        .transpose()
}

/// Reads a required client message identifier column.
pub(crate) fn read_client_message_id(
    raw: &str,
    table: &'static str,
    column: &'static str,
) -> Result<ClientMessageId, PersistenceError> {
    ClientMessageId::parse(raw).map_err(|_| PersistenceError::MalformedRow {
        table,
        column,
        expected: "a UUID in canonical form",
    })
}

/// Reads a required campaign identifier column.
///
/// `CampaignId` moved to `smpp-core` at milestone 006 (ADR 0010), so its
/// `parse` reports an `SmppError`; the column context that makes a malformed
/// row actionable is restored here, where it is known.
pub(crate) fn read_campaign_id(raw: &str) -> Result<CampaignId, PersistenceError> {
    CampaignId::parse(raw).map_err(|_| PersistenceError::MalformedRow {
        table: "campaigns",
        column: "campaign_id",
        expected: "a UUID in canonical form",
    })
}

/// Reads an optional campaign identifier column.
pub(crate) fn read_optional_campaign_id(
    raw: Option<&str>,
) -> Result<Option<CampaignId>, PersistenceError> {
    raw.map(read_campaign_id).transpose()
}

/// Reads a contact identifier column.
pub(crate) fn read_contact_id(raw: &str) -> Result<ContactId, PersistenceError> {
    ContactId::parse(raw)
}

/// Reads a contact list identifier column.
pub(crate) fn read_list_id(raw: &str) -> Result<ListId, PersistenceError> {
    ListId::parse(raw)
}

/// Reads a subscriber number column.
pub(crate) fn read_msisdn(
    raw: &str,
    table: &'static str,
    column: &'static str,
) -> Result<Msisdn, PersistenceError> {
    Msisdn::parse(raw).map_err(|_| PersistenceError::MalformedRow {
        table,
        column,
        expected: "3 to 20 digits, optionally prefixed with +",
    })
}

/// Stores the protocol version as the text of the schema's `CHECK`.
///
/// Matching on the octet rather than the variants: `SmppVersion` is
/// `#[non_exhaustive]`, so a `match` on it needs a wildcard arm anyway, and a
/// wildcard on the octet at least says what it means.
pub(crate) fn store_interface_version(version: SmppVersion) -> &'static str {
    match version.as_octet() {
        0x34 => "v3.4",
        _ => "v5.0",
    }
}

/// Reads the protocol version column.
pub(crate) fn read_interface_version(raw: &str) -> Result<SmppVersion, PersistenceError> {
    match raw {
        "v3.4" => Ok(SmppVersion::V3_4),
        "v5.0" => Ok(SmppVersion::V5_0),
        _ => Err(PersistenceError::MalformedRow {
            table: "session_profiles",
            column: "interface_version",
            expected: "v3.4 or v5.0",
        }),
    }
}

/// Reads the GSM 7-bit packing column (ADR 0008, ADR 0009).
pub(crate) fn read_gsm7_packing(raw: &str) -> Result<Gsm7BitPacking, PersistenceError> {
    Gsm7BitPacking::parse(raw).ok_or(PersistenceError::MalformedRow {
        table: "session_profiles",
        column: "gsm7_packing",
        expected: "unpacked or packed",
    })
}

/// Reads the GSM 7-bit charset column (ADR 0009).
pub(crate) fn read_gsm7_charset(raw: &str) -> Result<Gsm7BitCharset, PersistenceError> {
    Gsm7BitCharset::parse(raw).ok_or(PersistenceError::MalformedRow {
        table: "session_profiles",
        column: "gsm7_charset",
        expected: "gsm0338 or latin1",
    })
}

#[cfg(test)]
mod tests {
    use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking, SmppVersion};

    use super::{
        read_gsm7_charset, read_gsm7_packing, read_interface_version, read_u16, read_u32,
        store_interface_version,
    };

    #[test]
    fn a_negative_count_is_rejected_rather_than_wrapped() {
        assert!(read_u32(-1, "messages", "attempts").is_err());
    }

    #[test]
    fn a_port_beyond_the_tcp_range_is_rejected() {
        assert!(read_u16(70_000, "session_profiles", "port").is_err());
        assert_eq!(
            read_u16(2775, "session_profiles", "port").expect("in range"),
            2775
        );
    }

    #[test]
    fn the_protocol_version_round_trips_through_its_stored_text() {
        for version in [SmppVersion::V3_4, SmppVersion::V5_0] {
            assert_eq!(
                read_interface_version(store_interface_version(version)).expect("own output"),
                version
            );
        }
    }

    #[test]
    fn the_gsm7_layout_columns_round_trip_through_their_stored_text() {
        for packing in [Gsm7BitPacking::Unpacked, Gsm7BitPacking::Packed] {
            assert_eq!(
                read_gsm7_packing(packing.code()).expect("own output"),
                packing
            );
        }

        for charset in [Gsm7BitCharset::Gsm0338, Gsm7BitCharset::Latin1] {
            assert_eq!(
                read_gsm7_charset(charset.code()).expect("own output"),
                charset
            );
        }

        assert!(read_gsm7_packing("PACKED").is_err());
        assert!(read_gsm7_charset("iso-8859-1").is_err());
    }
}
