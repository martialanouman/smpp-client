//! Error type for this crate.

use core::time::Duration;

use smpp_core::status_codes::StatusClass;
use smpp_core::types::SequenceNumber;
use smpp_core::values::{CommandId, CommandStatus};

use crate::state::BindMode;

/// Why a session profile was refused at construction time.
///
/// A closed set rather than a free-form message, for the reason
/// [`smpp_core::FieldRejection`] is one: the rejected value is often a
/// hostname or a `system_id`, and the message crosses the IPC boundary
/// (CLAUDE.md §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProfileRejection {
    /// The value was empty, or only whitespace.
    Empty,
    /// The value is longer than the protocol field can carry.
    TooLong,
    /// The value is outside the range the specification allows.
    OutOfRange,
    /// The value contains a character the field does not allow — a NUL in a
    /// C-Octet String, most often.
    IllegalCharacter,
    /// Two settings that cannot both hold were asked for at once.
    Contradictory,
}

impl core::fmt::Display for ProfileRejection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let reason = match self {
            Self::Empty => "value is empty",
            Self::TooLong => "value is too long",
            Self::OutOfRange => "value is out of range",
            Self::IllegalCharacter => "value contains an illegal character",
            Self::Contradictory => "value contradicts another setting",
        };

        formatter.write_str(reason)
    }
}

/// Errors produced by this crate.
///
/// Per guide §6.1, every crate exposes **one** exhaustive `thiserror` type. No
/// public API returns a `Box<dyn Error>`, and no message ever carries a
/// password: [`SessionError::BindRejected`] names the status and its symbol,
/// never the credentials that earned it.
///
/// # Why no `PartialEq`
///
/// [`SessionError::Transport`] carries a `std::io::Error`, which is not
/// comparable, and dropping the source to gain the derive would lose the
/// origin of the failure — the one thing guide §6.3 asks never to lose. Tests
/// therefore match on the variant or assert on the rendered message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// An actor asked for a state change spec §7.9 does not draw.
    ///
    /// An internal invariant surfacing as a value rather than as a panic.
    #[error("illegal session state transition: {from} → {to}")]
    IllegalTransition {
        /// State the session was in.
        from: &'static str,
        /// State that was asked for.
        to: &'static str,
    },

    /// The peer sent something that is not a valid PDU, or we failed to build
    /// one.
    #[error(transparent)]
    Protocol(#[from] smpp_core::SmppError),

    /// The socket failed.
    ///
    /// `operation` names what was being done — `connect`, `write`, `read`,
    /// `shutdown` — because an `io::Error` on its own rarely says where it
    /// came from.
    #[error("transport failure while {operation}: {source}")]
    Transport {
        /// What the session was doing.
        operation: &'static str,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// The SMSC refused the bind.
    ///
    /// `class` is what decides whether the supervisor retries: a
    /// [`StatusClass::Fatal`] rejection — a wrong password — must not open a
    /// reconnection loop (CA-005-03).
    #[error("bind refused by the SMSC: {symbol} ({status:?})")]
    BindRejected {
        /// The bind operation that was refused.
        operation: CommandId,
        /// The status the SMSC answered with.
        status: CommandStatus,
        /// Its symbolic name, e.g. `ESME_RINVPASWD`.
        symbol: &'static str,
        /// How the engine must react.
        class: StatusClass,
    },

    /// An operation was requested on a session that is not bound.
    #[error("session is not bound (state: {state})")]
    NotBound {
        /// The state the session was actually in.
        state: &'static str,
    },

    /// The operation is legal SMPP but not on this bind type.
    ///
    /// CA-005-02: submitting on a receiver session is refused here, before
    /// the PDU leaves, rather than by an `ESME_RINVBNDSTS` from the SMSC.
    #[error("operation {operation:?} is not allowed on a {mode:?} bind")]
    OperationNotAllowed {
        /// The operation that was refused.
        operation: CommandId,
        /// The bind mode in force.
        mode: BindMode,
    },

    /// No response came back within `response_timeout`.
    #[error("no response to {operation:?} (sequence {sequence}) within {timeout:?}")]
    ResponseTimeout {
        /// The request that went unanswered.
        operation: CommandId,
        /// Its `sequence_number`.
        sequence: SequenceNumber,
        /// How long we waited.
        timeout: Duration,
    },

    /// A response came back, but for another operation than the one asked.
    #[error("expected a {expected:?}, received a {actual:?}")]
    UnexpectedResponse {
        /// The response the request called for.
        expected: CommandId,
        /// What arrived instead.
        actual: CommandId,
    },

    /// Every `sequence_number` of `1..=0x7FFFFFFF` is currently in flight.
    ///
    /// Unreachable in practice — it would take more than two billion
    /// unanswered requests — but reusing a number still in the correlation
    /// table would attribute a response to the wrong message, so the case is
    /// an error rather than a wrap.
    #[error("no free sequence_number: {in_flight} request(s) in flight")]
    SequenceSpaceExhausted {
        /// How many entries the correlation table held.
        in_flight: usize,
    },

    /// The session shut down while the request was in flight.
    #[error("session shut down while the request was in flight")]
    Cancelled,

    /// The session actors are gone: the request has nowhere to go.
    #[error("session is closed")]
    Closed,

    /// A session profile field was refused.
    #[error("invalid value for `{field}`: {reason}")]
    InvalidProfile {
        /// Name of the field, as spec §8.2 spells it.
        field: &'static str,
        /// Why it was refused.
        reason: ProfileRejection,
    },

    /// Reading or writing a session profile failed.
    #[error(transparent)]
    Persistence(#[from] persistence::PersistenceError),
}

impl SessionError {
    /// Builds an [`SessionError::InvalidProfile`].
    pub(crate) const fn invalid_profile(field: &'static str, reason: ProfileRejection) -> Self {
        Self::InvalidProfile { field, reason }
    }

    /// Whether retrying the operation that produced this error can succeed.
    ///
    /// The single place the classification of milestone 003 turns into a
    /// decision for the supervisor. Anything that is not a refusal by the
    /// SMSC — a socket failure, a timeout — is retryable: the peer never got
    /// a chance to say otherwise.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::BindRejected { class, .. } => class.is_retryable(),
            Self::Transport { .. } | Self::ResponseTimeout { .. } | Self::Protocol(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_illegal_transition_names_both_ends() {
        let error = SessionError::IllegalTransition {
            from: "CLOSED",
            to: "BOUND",
        };

        assert_eq!(
            error.to_string(),
            "illegal session state transition: CLOSED → BOUND"
        );
    }

    /// CA-005-03, at the level of the error type: the classification of
    /// milestone 003 is what answers, not a guess about the variant.
    #[test]
    fn a_fatal_bind_rejection_is_not_retryable_and_a_transient_one_is() {
        let fatal = SessionError::BindRejected {
            operation: CommandId::BindTransceiver,
            status: CommandStatus::EsmeRinvpaswd,
            symbol: "ESME_RINVPASWD",
            class: StatusClass::Fatal,
        };
        assert!(!fatal.is_retryable());

        let transient = SessionError::BindRejected {
            operation: CommandId::BindTransceiver,
            status: CommandStatus::EsmeRalybnd,
            symbol: "ESME_RALYBND",
            class: StatusClass::Recoverable,
        };
        assert!(transient.is_retryable());
    }

    #[test]
    fn a_socket_failure_is_always_worth_retrying() {
        let error = SessionError::Transport {
            operation: "connect",
            source: std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        };

        assert!(error.is_retryable());
        assert!(error
            .to_string()
            .starts_with("transport failure while connect"));
    }

    /// CLAUDE.md §8 — the rejection message names the status, never what was
    /// sent to earn it.
    #[test]
    fn a_bind_rejection_never_echoes_the_credentials() {
        let error = SessionError::BindRejected {
            operation: CommandId::BindTransceiver,
            status: CommandStatus::EsmeRinvpaswd,
            symbol: "ESME_RINVPASWD",
            class: StatusClass::Fatal,
        };

        let rendered = error.to_string();
        assert!(rendered.contains("ESME_RINVPASWD"));
        assert!(!rendered.contains("password"));
    }

    #[test]
    fn a_profile_rejection_names_the_field_and_the_reason() {
        assert_eq!(
            SessionError::invalid_profile("host", ProfileRejection::Empty).to_string(),
            "invalid value for `host`: value is empty"
        );
        assert_eq!(
            SessionError::invalid_profile("gsm7_charset", ProfileRejection::Contradictory)
                .to_string(),
            "invalid value for `gsm7_charset`: value contradicts another setting"
        );
    }
}
