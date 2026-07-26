//! Error type for this crate.

use crate::values::CommandId;

/// Why a value was refused at construction time.
///
/// Deliberately a closed set of reasons rather than a free-form message: the
/// rejected input is often personal data (an MSISDN) or a secret, and CLAUDE.md
/// §8 forbids either from reaching a log, an export or the IPC boundary. The
/// caller learns *what* was wrong, never *with what*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FieldRejection {
    /// The value was empty once separators were removed.
    Empty,
    /// The value contains a character the field does not allow.
    IllegalCharacter,
    /// The value is shorter than the minimum the protocol accepts.
    TooShort,
    /// The value is longer than the field can carry.
    TooLong,
    /// The value is syntactically well formed but outside the allowed range.
    OutOfRange,
    /// The value is not a well-formed UUID.
    MalformedUuid,
}

impl core::fmt::Display for FieldRejection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let reason = match self {
            Self::Empty => "value is empty",
            Self::IllegalCharacter => "value contains an illegal character",
            Self::TooShort => "value is too short",
            Self::TooLong => "value is too long",
            Self::OutOfRange => "value is out of range",
            Self::MalformedUuid => "value is not a well-formed UUID",
        };

        f.write_str(reason)
    }
}

/// Errors produced by this crate.
///
/// Per guide §6.1, every crate exposes **one** exhaustive `thiserror` type. No
/// public API returns a `Box<dyn Error>`: callers must be able to discriminate
/// between cases — milestone 005 branches on [`SmppError::UnexpectedPdu`] the
/// way milestone 010 branches on the status classification.
///
/// # Contents of the messages
///
/// Messages name the *field* and the *reason*, never the offending value. A
/// decode error carries the chain of protocol fields that failed (`rusmpp`'s
/// `verbose` feature) but no payload, so it stays safe to log and to hand to
/// the UI.
///
/// `#[non_exhaustive]` lets later milestones add variants without breaking
/// `match` expressions in calling crates.
/// # Why no `PartialEq`
///
/// Comparing two errors would be convenient in tests, but `rusmpp` derives
/// neither `PartialEq` nor `Eq` on its `DecodeError` and `EncodeError`, and
/// those are carried verbatim by the two wrapping variants. Tests therefore
/// assert on the rendered message or on the matched variant, which is the more
/// durable check anyway: it survives a new field appearing inside an error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SmppError {
    /// The byte stream is not a valid PDU.
    ///
    /// Covers a malformed body, an unterminated C-Octet String, a truncated
    /// TLV, and an inconsistent `command_length`.
    #[error("PDU decoding failed: {0}")]
    Decode(#[from] crate::codec::PduDecodeError),

    /// The command could not be serialised to its wire representation.
    ///
    /// Unlike a decoding failure — which a hostile or broken peer can trigger —
    /// this one means *we* built an invalid command, so it is a bug on our
    /// side rather than a protocol event.
    #[error("PDU encoding failed: {0}")]
    Encode(#[from] crate::codec::PduEncodeError),

    /// Fewer bytes were supplied than the PDU header announces.
    ///
    /// On a stream this only means "read more"; for the one-shot decoding this
    /// crate exposes it is an error, because the caller claimed to hand over a
    /// whole PDU.
    #[error("incomplete PDU: {available} byte(s) supplied, {needed} announced")]
    Incomplete {
        /// Number of bytes supplied.
        available: usize,
        /// Value of the `command_length` header field.
        needed: usize,
    },

    /// A complete PDU was decoded but bytes remain after it.
    #[error("{count} byte(s) left over after the PDU")]
    TrailingBytes {
        /// Number of bytes left over.
        count: usize,
    },

    /// The announced `command_length` exceeds the accepted maximum.
    ///
    /// The bound exists so a hostile or broken peer cannot make the client
    /// allocate an arbitrary buffer.
    #[error("command_length {actual} exceeds the maximum of {max}")]
    CommandTooLarge {
        /// Value of the `command_length` header field.
        actual: usize,
        /// Maximum accepted, see [`crate::codec::MAX_COMMAND_LENGTH`].
        max: usize,
    },

    /// A well-formed PDU arrived where another operation was expected.
    #[error("unexpected PDU: expected {expected:?}, received {actual:?}")]
    UnexpectedPdu {
        /// The operation the state machine was waiting for.
        expected: CommandId,
        /// The operation actually received.
        actual: CommandId,
    },

    /// The `interface_version` octet is neither v3.4 nor v5.0.
    #[error("unsupported SMPP interface version: {octet:#04X}")]
    UnsupportedInterfaceVersion {
        /// The rejected octet.
        octet: u8,
    },

    /// A domain value was refused at construction time.
    #[error("invalid value for `{field}`: {reason}")]
    InvalidField {
        /// Name of the field, as the specification spells it.
        field: &'static str,
        /// Why it was refused.
        reason: FieldRejection,
    },
}

impl SmppError {
    /// Builds an [`SmppError::InvalidField`].
    pub(crate) const fn invalid_field(field: &'static str, reason: FieldRejection) -> Self {
        Self::InvalidField { field, reason }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_field_names_the_field_and_the_reason() {
        let error = SmppError::invalid_field("destination_addr", FieldRejection::TooLong);

        assert_eq!(
            error.to_string(),
            "invalid value for `destination_addr`: value is too long"
        );
    }

    #[test]
    fn an_unsupported_version_reports_the_offending_octet() {
        let error = SmppError::UnsupportedInterfaceVersion { octet: 0x33 };

        assert_eq!(
            error.to_string(),
            "unsupported SMPP interface version: 0x33"
        );
    }
}
