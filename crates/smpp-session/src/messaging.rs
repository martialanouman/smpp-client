//! Satisfying the send port `messaging` declares.
//!
//! Guide §8.1: the port is defined by the layer that consumes it and
//! implemented by the layer below (ADR 0010). `messaging::ports::SmscSession`
//! is what the send orchestrator needs from a session — the two encoding
//! settings the message centre imposes, and *send this PDU, give me its
//! response* — and this file is the whole of the implementation.
//!
//! It is deliberately thin. Nothing decides anything here: the correlation,
//! the timeout and the bind-type check all live in
//! [`SessionHandle::request`], which already had them, and the only work this
//! adds is projecting [`SessionError`] onto the port's vocabulary.
//!
//! # Why the projection is not a `From` impl
//!
//! [`SubmitError`] names four outcomes a sender can act on;
//! [`SessionError`] has fifteen variants, several of which cannot arise on a
//! submission at all — a bind rejection, an invalid profile, a storage
//! failure. A blanket `From` would have to guess for those, and guessing at
//! the boundary is how a fatal condition ends up reported as retryable. Every
//! arm is written out, so a new `SessionError` variant is a compile error
//! here, which is where the decision belongs.

use messaging::ports::{SmscSession, SubmitError};
use smpp_core::codec::{Command, Pdu};
use smpp_core::types::SessionId;
use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking};

use crate::actors::SessionHandle;
use crate::error::SessionError;

impl SmscSession for SessionHandle {
    fn session_id(&self) -> SessionId {
        Self::session_id(self)
    }

    fn gsm7_packing(&self) -> Gsm7BitPacking {
        Self::gsm7_packing(self)
    }

    fn gsm7_charset(&self) -> Gsm7BitCharset {
        Self::gsm7_charset(self)
    }

    async fn submit(&self, pdu: Pdu) -> Result<Command, SubmitError> {
        self.request(pdu).await.map_err(submit_error)
    }
}

/// Projects a session failure onto the port's vocabulary.
///
/// The rendered `SessionError` goes into `reason` for the transport arm and
/// nowhere else. No variant of that type carries a credential — there is a
/// test in this crate — so the string is safe to show and to log.
fn submit_error(error: SessionError) -> SubmitError {
    match error {
        SessionError::NotBound { state } => SubmitError::NotBound {
            state: state.to_owned(),
        },
        SessionError::OperationNotAllowed { .. } => SubmitError::OperationNotAllowed,
        SessionError::ResponseTimeout { .. } => SubmitError::ResponseTimeout,
        SessionError::Closed | SessionError::Cancelled => SubmitError::Closed,
        // Everything else is a fault of the link or of this client, and the
        // sender treats all of them the same way: the message did not go out
        // and nothing says whether trying again would help.
        other => SubmitError::Transport {
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::submit_error;
    use crate::error::SessionError;
    use core::time::Duration;
    use messaging::ports::SubmitError;
    use smpp_core::types::SequenceNumber;
    use smpp_core::values::CommandId;

    #[test]
    fn a_timeout_keeps_its_identity_across_the_boundary() {
        let error = SessionError::ResponseTimeout {
            operation: CommandId::SubmitSm,
            sequence: SequenceNumber::FIRST,
            timeout: Duration::from_secs(10),
        };

        assert_eq!(submit_error(error), SubmitError::ResponseTimeout);
    }

    /// CA-005-02, seen from the send path: a receiver session refuses the
    /// submission here, and the sender must be able to tell that from a link
    /// failure — one is a configuration mistake, the other is not.
    #[test]
    fn a_refused_operation_is_not_reported_as_a_transport_failure() {
        let error = SessionError::OperationNotAllowed {
            operation: CommandId::SubmitSm,
            mode: crate::state::BindMode::Receiver,
        };

        assert_eq!(submit_error(error), SubmitError::OperationNotAllowed);
    }

    #[test]
    fn a_session_that_is_down_reports_the_state_it_is_in() {
        let error = SessionError::NotBound { state: "RECONNECT" };

        assert_eq!(
            submit_error(error),
            SubmitError::NotBound {
                state: String::from("RECONNECT")
            }
        );
    }

    #[test]
    fn a_cancelled_request_is_reported_as_a_closed_session() {
        assert_eq!(submit_error(SessionError::Cancelled), SubmitError::Closed);
        assert_eq!(submit_error(SessionError::Closed), SubmitError::Closed);
    }

    /// CLAUDE.md §8: nothing crossing this boundary may carry a credential.
    /// The transport arm is the only one that carries a rendered message, so
    /// it is the only one that could.
    #[test]
    fn the_transport_arm_carries_a_message_and_no_secret() {
        let error = SessionError::SequenceSpaceExhausted { in_flight: 12 };

        let SubmitError::Transport { reason } = submit_error(error) else {
            panic!("a sequence exhaustion is a transport failure");
        };

        assert!(reason.contains("sequence_number"), "{reason}");
    }
}
