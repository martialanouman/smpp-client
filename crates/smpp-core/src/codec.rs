//! PDU codec facade: bytes in, typed [`Command`] out, and back.
//!
//! # Scope
//!
//! This module performs **no I/O**. [`encode`] and [`decode`] are pure
//! functions over a byte buffer, which is what makes the property tests of
//! milestone 003 possible at all. Mounting [`CommandCodec`] on a socket
//! (`Framed<TcpStream, CommandCodec>`) is milestone 005's job — the type is
//! re-exported here because `smpp-core` is the only crate allowed to depend on
//! `rusmpp` (guide §4.2).
//!
//! # Framing
//!
//! A PDU is a 16-octet header followed by an optional body (spec §7.1), and
//! `command_length` counts the header. [`decode`] expects **exactly one whole
//! PDU**: a shorter buffer yields [`SmppError::Incomplete`], a longer one
//! [`SmppError::TrailingBytes`]. Resynchronising a stream is a stream concern,
//! and belongs to the layer that owns the socket.

use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use crate::error::SmppError;

pub use rusmpp::{
    tokio_codec::{CommandCodec, DecodeError as PduDecodeError, EncodeError as PduEncodeError},
    Command, Pdu,
};

/// Length of the PDU header, in octets (spec §7.1).
pub const HEADER_LENGTH: usize = 16;

/// Largest `command_length` this client accepts, in octets.
///
/// Spec §7.5 caps `message_payload` at 64 KB; the rest of the budget covers the
/// mandatory fields and the optional parameters that travel with it. The bound
/// exists so a broken or hostile peer cannot make the client reserve an
/// arbitrary buffer by announcing a four-gigabyte PDU.
pub const MAX_COMMAND_LENGTH: usize = 128 * 1024;

/// Encodes a command into its wire representation, header included.
///
/// # Errors
///
/// Returns [`SmppError::CommandTooLarge`] if the encoded PDU exceeds
/// [`MAX_COMMAND_LENGTH`], and [`SmppError::Encode`] if the underlying encoder
/// fails.
pub fn encode(command: &Command) -> Result<Vec<u8>, SmppError> {
    let mut codec = CommandCodec::new().with_max_length(MAX_COMMAND_LENGTH);
    let mut buffer = BytesMut::new();

    Encoder::<&Command>::encode(&mut codec, command, &mut buffer)?;

    if buffer.len() > MAX_COMMAND_LENGTH {
        return Err(SmppError::CommandTooLarge {
            actual: buffer.len(),
            max: MAX_COMMAND_LENGTH,
        });
    }

    Ok(buffer.to_vec())
}

/// Decodes exactly one whole PDU.
///
/// # Errors
///
/// * [`SmppError::Incomplete`] — fewer bytes than the header announces;
/// * [`SmppError::TrailingBytes`] — bytes left over after a complete PDU;
/// * [`SmppError::CommandTooLarge`] — `command_length` beyond
///   [`MAX_COMMAND_LENGTH`];
/// * [`SmppError::Decode`] — malformed body: unterminated C-Octet String,
///   truncated TLV, `command_length` below the header size.
///
/// This function never panics, whatever the input. That is a contract, not an
/// aspiration: milestone 003 covers it with a fuzzing test.
pub fn decode(bytes: &[u8]) -> Result<Command, SmppError> {
    // The HEADER is authoritative, not what the codec leaves behind.
    //
    // `CommandCodec::decode` discards the size `Pdu::decode` returns and never
    // advances the buffer to `command_length`. Several PDUs — the three
    // `bind_*`, `outbind`, `query_sm`, `query_sm_resp`, `cancel_sm` — are
    // parsed without a length, so any octet the sender included *inside*
    // `command_length` but that the body parser did not consume stays in the
    // buffer. Trusting the leftover would report those as `TrailingBytes`
    // although they belong to the PDU — a vendor TLV appended to a bind is the
    // ordinary case.
    //
    // Worse, the same codec is meant to sit on a `Framed` at milestone 005.
    // There, those octets do not raise anything: they stay in the read buffer
    // and get read as the beginning of the next PDU. That is a silent framing
    // desynchronisation, the kind of bug that surfaces days later as
    // "responses no longer match requests".
    //
    // So the boundary is decided here, from `command_length`, and the codec is
    // handed exactly that many octets. Whatever it fails to consume is inside
    // the PDU and is none of our business.
    let Some(announced) = announced_length(bytes) else {
        return Err(SmppError::Incomplete {
            available: bytes.len(),
            needed: HEADER_LENGTH,
        });
    };

    if announced > MAX_COMMAND_LENGTH {
        return Err(SmppError::CommandTooLarge {
            actual: announced,
            max: MAX_COMMAND_LENGTH,
        });
    }

    if announced < HEADER_LENGTH {
        // A `command_length` below the header size is incoherent. Reported as
        // a decoding error rather than `Incomplete`: no amount of extra bytes
        // would ever make this PDU valid.
        return Err(SmppError::Malformed {
            announced,
            minimum: HEADER_LENGTH,
        });
    }

    if bytes.len() < announced {
        return Err(SmppError::Incomplete {
            available: bytes.len(),
            needed: announced,
        });
    }

    if bytes.len() > announced {
        return Err(SmppError::TrailingBytes {
            count: bytes.len() - announced,
        });
    }

    let mut codec = CommandCodec::new().with_max_length(MAX_COMMAND_LENGTH);
    let mut buffer = BytesMut::from(bytes);

    match codec.decode(&mut buffer) {
        // The leftover is deliberately ignored: the boundary was settled above.
        Ok(Some(command)) => Ok(command),
        Ok(None) => Err(SmppError::Incomplete {
            available: bytes.len(),
            needed: announced,
        }),
        Err(PduDecodeError::MaxLength { actual, max }) => {
            Err(SmppError::CommandTooLarge { actual, max })
        }
        Err(error) => Err(SmppError::Decode(error)),
    }
}

/// Reads the `command_length` field without consuming anything.
///
/// Returns `None` when there are not even four octets to read it from, or when
/// the value does not fit a `usize` — which can only happen on a 16-bit target
/// and is reported as "unknown" rather than truncated.
fn announced_length(bytes: &[u8]) -> Option<usize> {
    let header = bytes.get(0..4)?;
    let raw = u32::from_be_bytes([
        *header.first()?,
        *header.get(1)?,
        *header.get(2)?,
        *header.get(3)?,
    ]);

    usize::try_from(raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusmpp::CommandStatus;

    #[test]
    fn an_empty_buffer_is_incomplete_not_a_panic() {
        assert!(matches!(
            decode(&[]),
            Err(SmppError::Incomplete {
                available: 0,
                needed: 16
            })
        ));
    }

    #[test]
    fn a_header_shorter_than_four_octets_reports_the_header_size() {
        assert!(matches!(
            decode(&[0x00, 0x00]),
            Err(SmppError::Incomplete { needed: 16, .. })
        ));
    }

    #[test]
    fn an_oversized_command_length_is_refused_before_any_allocation() {
        let mut bytes = vec![0xFF, 0xFF, 0xFF, 0xFF];
        bytes.extend_from_slice(&[0x00; 12]);

        assert!(matches!(
            decode(&bytes),
            Err(SmppError::CommandTooLarge { .. })
        ));
    }

    #[test]
    fn the_announced_length_is_reported_when_bytes_are_missing() {
        let mut bytes =
            encode(&Command::new(CommandStatus::EsmeRok, 1, Pdu::EnquireLink)).expect("encoding");
        bytes[3] = 0x40; // announce 64 octets, supply 16

        assert!(matches!(
            decode(&bytes),
            Err(SmppError::Incomplete {
                available: 16,
                needed: 64
            })
        ));
    }

    #[test]
    fn a_command_length_below_the_header_size_is_a_decode_error() {
        let bytes = vec![
            0x00, 0x00, 0x00, 0x04, // command_length = 4
            0x00, 0x00, 0x00, 0x15, //
            0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x01, //
        ];

        assert!(decode(&bytes).is_err());
    }
}
