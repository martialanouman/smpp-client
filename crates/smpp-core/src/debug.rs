//! Hexadecimal dump of a PDU, for explicit debug mode.
//!
//! # Why this module is awkward to use, on purpose
//!
//! CLAUDE.md §8 states it plainly: no secret in a log, *even at `trace`*, and
//! the PDU hex dump is **reserved for explicit debug mode**. A PDU body is not
//! innocuous — a `bind_transmitter` carries the SMSC password in clear, a
//! `submit_sm` carries the message text and the subscriber's number.
//!
//! So the full dump is not simply "a function one should be careful with": it
//! cannot be reached without naming [`DebugDumpAuthorisation::granted`], which
//! is a single greppable call site. There is no `Display`, no `Debug`, no
//! `From` that would print the body as a side effect of formatting something
//! else.
//!
//! What is freely available is [`header_dump`], which renders the 16 header
//! octets only — `command_length`, `command_id`, `command_status`,
//! `sequence_number`. Those four fields hold no user data and are what one
//! actually needs to diagnose a framing problem.
//!
//! ```
//! use smpp_core::debug;
//!
//! let pdu = [
//!     0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x15,
//!     0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
//! ];
//!
//! assert_eq!(
//!     debug::header_dump(&pdu),
//!     "00000010 00000015 00000000 00000001"
//! );
//! ```

use crate::codec::HEADER_LENGTH;

/// Number of octets rendered per line by [`full_dump`].
const OCTETS_PER_LINE: usize = 16;

/// Proof that revealing PDU payloads was an explicit decision.
///
/// Obtained only through [`DebugDumpAuthorisation::granted`]. It carries no
/// data: its whole purpose is to make the call site of a full dump visible in
/// a review and in a `grep`.
///
/// The caller is expected to build it from the user's debug-mode setting, and
/// nowhere else:
///
/// ```
/// use smpp_core::debug::{self, DebugDumpAuthorisation};
///
/// # let debug_mode_enabled_by_the_user = true;
/// let dump = if debug_mode_enabled_by_the_user {
///     Some(debug::full_dump(&[0x00, 0x01], DebugDumpAuthorisation::granted()))
/// } else {
///     None
/// };
/// # assert!(dump.is_some());
/// ```
#[derive(Debug, Clone, Copy)]
pub struct DebugDumpAuthorisation(());

impl DebugDumpAuthorisation {
    /// Grants the authorisation.
    ///
    /// Call this **only** where the user has explicitly turned debug mode on.
    /// Every call site is a place where a password may reach a log file.
    #[must_use]
    pub const fn granted() -> Self {
        Self(())
    }
}

/// Renders the four header fields of a PDU, and nothing else.
///
/// Always safe: spec §7.1 puts no user data in the header. Returns what is
/// available when the buffer is shorter than a header, so a truncated frame can
/// still be diagnosed.
#[must_use]
pub fn header_dump(bytes: &[u8]) -> String {
    let header = bytes.get(..HEADER_LENGTH).unwrap_or(bytes);

    header
        .chunks(4)
        .map(|word| word.iter().map(|octet| format!("{octet:02X}")).collect())
        .collect::<Vec<String>>()
        .join(" ")
}

/// Renders the whole PDU, body included, as an offset/hex/ASCII dump.
///
/// # Authorisation
///
/// Requires a [`DebugDumpAuthorisation`]. The output **may contain a password,
/// a message text or a subscriber number**: it must never reach a shared log,
/// an export or a bug report without the user knowing.
#[must_use]
pub fn full_dump(bytes: &[u8], _authorisation: DebugDumpAuthorisation) -> String {
    let mut rendered = String::new();

    for (index, chunk) in bytes.chunks(OCTETS_PER_LINE).enumerate() {
        let offset = index * OCTETS_PER_LINE;
        let hex: Vec<String> = chunk.iter().map(|octet| format!("{octet:02X}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|octet| {
                if octet.is_ascii_graphic() || *octet == b' ' {
                    char::from(*octet)
                } else {
                    '.'
                }
            })
            .collect();

        rendered.push_str(&format!("{offset:08X}  {:<47}  |{ascii}|\n", hex.join(" ")));
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENQUIRE_LINK: [u8; 16] = [
        0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x15, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01,
    ];

    #[test]
    fn the_header_dump_shows_the_four_fields_of_the_specification() {
        assert_eq!(
            header_dump(&ENQUIRE_LINK),
            "00000010 00000015 00000000 00000001"
        );
    }

    #[test]
    fn the_header_dump_ignores_the_body() {
        let mut bytes = ENQUIRE_LINK.to_vec();
        bytes.extend_from_slice(b"secret08");

        assert!(
            !header_dump(&bytes).contains("73"),
            "the body must not leak into the header dump"
        );
    }

    #[test]
    fn the_header_dump_survives_a_truncated_frame() {
        assert_eq!(header_dump(&[0x00, 0x00, 0x00]), "000000");
        assert_eq!(header_dump(&[]), "");
    }

    #[test]
    fn the_full_dump_renders_offsets_hex_and_ascii() {
        let dump = full_dump(b"SMPP3TEST", DebugDumpAuthorisation::granted());

        assert_eq!(
            dump,
            "00000000  53 4D 50 50 33 54 45 53 54                       |SMPP3TEST|\n"
        );
    }

    #[test]
    fn the_full_dump_wraps_every_sixteen_octets() {
        let dump = full_dump(&[0x41; 20], DebugDumpAuthorisation::granted());

        assert_eq!(dump.lines().count(), 2);
        assert!(dump.contains("00000010"));
    }

    #[test]
    fn the_full_dump_of_nothing_is_empty() {
        assert!(full_dump(&[], DebugDumpAuthorisation::granted()).is_empty());
    }
}
