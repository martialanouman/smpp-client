//! Errors raised while encoding or segmenting a message.

use crate::encoding::Encoding;

/// Everything that can go wrong turning a text into segments.
///
/// Deliverable L-004-05. It is a type of its own rather than a set of variants
/// on [`MessagingError`](crate::MessagingError) because the callers that need
/// to *discriminate* — the live preview, the campaign validator — only ever
/// deal with encoding failures, and matching on a crate-wide enum would force
/// them to handle repository and session cases that cannot occur here.
/// Guide §6.1 is still satisfied: `MessagingError` remains the single error
/// type of the crate and carries this one as a source.
///
/// # Message content and confidentiality
///
/// [`Self::UnrepresentableCharacter`] carries the offending character. That is
/// one character out of a body, and the user interface cannot point at the
/// problem without it (CA-004-04). Everything else is a count. No variant ever
/// carries the message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EncodingError {
    /// The requested encoding cannot represent this character.
    ///
    /// Only reachable when the encoding is *forced*: automatic detection falls
    /// back to UCS2, which covers the whole of Unicode.
    #[error("character {character:?} at position {index} cannot be represented in {encoding}")]
    UnrepresentableCharacter {
        /// The character that has no representation in `encoding`.
        character: char,
        /// Its position in the text, counted in characters, not in bytes.
        index: usize,
        /// The encoding that rejected it.
        encoding: Encoding,
    },

    /// The GSM 7-bit layout asks for two settings that cannot both hold.
    ///
    /// ADR 0009 §7: [`Gsm7BitCharset::Latin1`](smpp_core::values::Gsm7BitCharset::Latin1)
    /// octets use all eight bits — `é` is `0xE9` — and
    /// [`Gsm7BitPacking::Packed`](smpp_core::values::Gsm7BitPacking::Packed)
    /// masks the top bit off every one of them. The result is not slightly
    /// wrong, it is unrecoverable, and it is silent: the message centre
    /// answers `ESME_ROK` and the handset shows something else.
    ///
    /// The session profile refuses the pair too. This exists because a
    /// `SegmentationOptions` can be built without one, and an invariant that
    /// only one of its two entry points enforces is an invariant with a hole
    /// in it.
    #[error("GSM 7-bit charset {charset} cannot be combined with {packing} packing")]
    IncompatibleGsm7Layout {
        /// The charset that was asked for.
        charset: &'static str,
        /// The packing that was asked for.
        packing: &'static str,
    },

    /// The text needs more segments than a concatenation can address.
    ///
    /// Both the UDH part number and `sar_total_segments` are a single octet,
    /// so 255 is a protocol ceiling, not a policy.
    #[error("message needs {segments} segments, the concatenation limit is {maximum}")]
    TooManySegments {
        /// Segments the text requires.
        segments: usize,
        /// The highest addressable segment number.
        maximum: usize,
    },

    /// The body exceeds what the `message_payload` TLV can hold.
    ///
    /// The TLV length field is 16 bits, hence the 64 KiB of spec §7.5.
    #[error("message payload is {octets} octets, the maximum is {maximum}")]
    PayloadTooLarge {
        /// Size of the encoded body.
        octets: usize,
        /// The largest body the TLV can carry.
        maximum: usize,
    },

    /// The concatenation header of a segment could not be built.
    ///
    /// Unreachable through [`segment`](crate::segmentation::segment), which
    /// numbers the parts itself. It exists so that the *validating*
    /// constructor can be used rather than the unchecked one: should the
    /// numbering ever change, a malformed UDH becomes an error here instead of
    /// six wrong octets on the wire.
    #[error(
        "cannot build the concatenation header of segment {sequence_number} of {total_segments}"
    )]
    InvalidConcatenationHeader {
        /// The segment at fault, 1-based.
        sequence_number: u8,
        /// The total it was numbered against.
        total_segments: u8,
    },

    /// Segments handed to the reassembler do not form one message.
    ///
    /// Raised on a missing or duplicated part, on parts that disagree about
    /// the total, or on an empty slice.
    #[error("segments do not form a complete message: {reason}")]
    IncompleteConcatenation {
        /// What exactly was inconsistent.
        reason: &'static str,
    },

    /// The octets of a segment are not valid for its declared encoding.
    ///
    /// Reachable on reassembly of segments that did not come out of the
    /// segmenter — a truncated UCS2 body, for instance.
    #[error("segment {sequence_number} is not valid {encoding}: {reason}")]
    MalformedUserData {
        /// The segment at fault, 1-based.
        sequence_number: u8,
        /// Its declared encoding.
        encoding: Encoding,
        /// What exactly was malformed.
        reason: &'static str,
    },
}
