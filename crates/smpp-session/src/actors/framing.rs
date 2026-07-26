//! The codec mounted on the socket.
//!
//! # Why this is not `rusmpp`'s `CommandCodec` directly
//!
//! Milestone 003 left a note that reads like a prediction, and it was one:
//!
//! > the same codec is meant to sit on a `Framed` at milestone 005. There,
//! > those octets do not raise anything: they stay in the read buffer and get
//! > read as the beginning of the next PDU. That is a silent framing
//! > desynchronisation, the kind of bug that surfaces days later as "responses
//! > no longer match requests".
//!
//! Two distinct problems come out of mounting `CommandCodec` on a stream.
//!
//! **It does not consume by `command_length`.** Several PDUs — the three
//! `bind_*`, `outbind`, `query_sm`, `cancel_sm` — are parsed without a length,
//! so any octet the sender put *inside* `command_length` that the body parser
//! did not want stays in the buffer and is read as the start of the next PDU.
//! A vendor TLV appended to a bind is the ordinary case, not a hostile one.
//!
//! **A decode error leaves the buffer in an unknown state.** `CommandCodec`
//! resets its own state machine but has already handed the body to the parser,
//! which consumed an unspecified amount of it. There is no way to know where
//! the next PDU begins, so a malformed PDU would have to end the connection.
//! CA-005-07 asks for the opposite: a malformed PDU must not kill the session.
//!
//! [`SessionCodec`] fixes both by deciding the frame boundary itself, from
//! `command_length`, and handing exactly that many octets to
//! [`smpp_core::codec::decode`] — the function milestone 003 covered with a
//! fuzzing test and a no-panic contract. A bad frame therefore comes out as an
//! **item** (`Err(SmppError)`), not as a stream error: the reader answers
//! `generic_nack`, logs it, and reads the next PDU from a buffer that is still
//! aligned.

use smpp_core::codec::{self, Command, HEADER_LENGTH, MAX_COMMAND_LENGTH};
use smpp_core::SmppError;
use tokio_util::bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

/// Octets of the `command_length` field.
const LENGTH_FIELD_OCTETS: usize = 4;

/// What one frame decoded to.
///
/// `Err` is a **well-framed PDU that would not parse**, which is a protocol
/// incident and not a transport failure. The distinction is the whole reason
/// this type exists.
pub(crate) type DecodedFrame = Result<Command, SmppError>;

/// Length-prefixed SMPP framing over [`smpp_core::codec`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionCodec {
    max_length: usize,
}

impl SessionCodec {
    /// A codec refusing any PDU beyond [`MAX_COMMAND_LENGTH`].
    pub(crate) const fn new() -> Self {
        Self {
            max_length: MAX_COMMAND_LENGTH,
        }
    }

    /// Reads `command_length` without consuming it.
    fn announced_length(src: &BytesMut) -> Option<usize> {
        let header = src.get(..LENGTH_FIELD_OCTETS)?;
        let raw = u32::from_be_bytes([
            *header.first()?,
            *header.get(1)?,
            *header.get(2)?,
            *header.get(3)?,
        ]);

        usize::try_from(raw).ok()
    }
}

impl Decoder for SessionCodec {
    type Item = DecodedFrame;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let Some(announced) = Self::announced_length(src) else {
            return Ok(None);
        };

        // Both of these end the connection, and there is no alternative: a
        // `command_length` that is self-contradictory or beyond the buffer we
        // are willing to reserve gives us no way to find where the next PDU
        // starts. Skipping a length we do not trust is guessing.
        if announced < HEADER_LENGTH {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("command_length {announced} is below the {HEADER_LENGTH}-byte header"),
            ));
        }

        if announced > self.max_length {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "command_length {announced} exceeds the maximum of {}",
                    self.max_length
                ),
            ));
        }

        if src.len() < announced {
            src.reserve(announced - src.len());

            return Ok(None);
        }

        // The frame leaves the buffer whether or not it parses. That single
        // line is what keeps the stream aligned across a malformed PDU.
        let frame = src.split_to(announced);

        Ok(Some(codec::decode(&frame)))
    }
}

impl Encoder<Command> for SessionCodec {
    type Error = std::io::Error;

    fn encode(&mut self, command: Command, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let bytes = codec::encode(&command)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;

        dst.extend_from_slice(&bytes);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smpp_core::codec::Pdu;
    use smpp_core::values::{CommandId, CommandStatus};

    fn enquire_link(sequence: u32) -> Command {
        Command::new(CommandStatus::EsmeRok, sequence, Pdu::EnquireLink)
    }

    fn encoded(command: &Command) -> Vec<u8> {
        codec::encode(command).expect("a well-formed command encodes")
    }

    #[test]
    fn a_whole_pdu_decodes_and_leaves_the_buffer_empty() {
        let mut codec = SessionCodec::new();
        let mut buffer = BytesMut::from(&encoded(&enquire_link(7))[..]);

        let decoded = codec
            .decode(&mut buffer)
            .expect("no transport failure")
            .expect("one whole PDU")
            .expect("it parses");

        assert_eq!(decoded.id(), CommandId::EnquireLink);
        assert_eq!(decoded.sequence_number(), 7);
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_partial_pdu_waits_rather_than_failing() {
        let mut codec = SessionCodec::new();
        let bytes = encoded(&enquire_link(1));
        let mut buffer = BytesMut::from(&bytes[..8]);

        assert!(codec.decode(&mut buffer).expect("not a failure").is_none());

        buffer.extend_from_slice(&bytes[8..]);

        assert!(codec
            .decode(&mut buffer)
            .expect("not a failure")
            .expect("now complete")
            .is_ok());
    }

    /// **The reason this codec exists.** A malformed PDU comes back as an
    /// item, the frame leaves the buffer, and the PDU behind it decodes
    /// normally. Mounting `CommandCodec` directly loses the second half.
    #[test]
    fn a_malformed_pdu_does_not_desynchronise_the_frames_behind_it() {
        let mut codec = SessionCodec::new();
        let mut buffer = BytesMut::new();

        // A `submit_sm` whose body is nothing but a length: the mandatory
        // C-Octet Strings are missing, so the parse fails inside a frame whose
        // boundary is perfectly well announced.
        buffer.extend_from_slice(&[0x00, 0x00, 0x00, 0x14]); // command_length = 20
        buffer.extend_from_slice(&[0x00, 0x00, 0x00, 0x04]); // submit_sm
        buffer.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // ESME_ROK
        buffer.extend_from_slice(&[0x00, 0x00, 0x00, 0x63]); // sequence 99
        buffer.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // a body that is not one
        buffer.extend_from_slice(&encoded(&enquire_link(100)));

        let malformed = codec
            .decode(&mut buffer)
            .expect("a bad body is not a transport failure")
            .expect("the frame is complete");
        assert!(malformed.is_err(), "the body must not parse");

        let next = codec
            .decode(&mut buffer)
            .expect("still aligned")
            .expect("the next frame is complete")
            .expect("and it parses");

        assert_eq!(next.sequence_number(), 100);
        assert!(buffer.is_empty());
    }

    /// A vendor TLV appended to a bind is inside `command_length` but is not
    /// consumed by the body parser. Trusting the leftover rather than the
    /// header would report it as the start of the next PDU.
    #[test]
    fn octets_the_body_parser_ignores_stay_inside_their_own_frame() {
        let mut codec = SessionCodec::new();
        let mut bytes = encoded(&enquire_link(5));

        // Extend the announced length by four octets of padding.
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        let length = u32::try_from(bytes.len()).expect("small");
        bytes[..4].copy_from_slice(&length.to_be_bytes());

        let mut buffer = BytesMut::from(&bytes[..]);
        buffer.extend_from_slice(&encoded(&enquire_link(6)));

        let first = codec
            .decode(&mut buffer)
            .expect("not a failure")
            .expect("complete");
        // Whether `smpp-core` accepts the padding is its business; what
        // matters here is the boundary.
        let _ = first;

        let second = codec
            .decode(&mut buffer)
            .expect("still aligned")
            .expect("complete")
            .expect("parses");

        assert_eq!(second.sequence_number(), 6);
    }

    #[test]
    fn an_impossible_command_length_is_a_transport_failure() {
        let mut codec = SessionCodec::new();

        let mut too_short = BytesMut::from(&[0x00, 0x00, 0x00, 0x04][..]);
        too_short.extend_from_slice(&[0x00; 12]);
        assert!(codec.decode(&mut too_short).is_err());

        let mut too_long = BytesMut::from(&[0xFF, 0xFF, 0xFF, 0xFF][..]);
        too_long.extend_from_slice(&[0x00; 12]);
        assert!(codec.decode(&mut too_long).is_err());
    }

    #[test]
    fn a_command_survives_a_trip_through_the_encoder_and_the_decoder() {
        let mut codec = SessionCodec::new();
        let mut buffer = BytesMut::new();

        codec
            .encode(enquire_link(42), &mut buffer)
            .expect("encoding an enquire_link cannot fail");

        let decoded = codec
            .decode(&mut buffer)
            .expect("not a failure")
            .expect("complete")
            .expect("parses");

        assert_eq!(decoded.sequence_number(), 42);
        assert_eq!(decoded.id(), CommandId::EnquireLink);
    }
}
