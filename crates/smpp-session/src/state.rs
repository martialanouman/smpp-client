//! The session state machine of spec §7.9, and the edges it allows.
//!
//! ```text
//!   CLOSED ──connect──► CONNECTING ──bind_*──► BINDING ──bind_resp ok──► BOUND
//!      ▲                    │                     │                        │
//!      │                    │ TCP failure         │ recoverable status     │ link lost
//!      │                    ▼                     ▼                        ▼
//!      │               RECONNECTING ◄─────────────┴────────────────────────┘
//!      │                    │  back-off elapsed
//!      │                    └──────────────► CONNECTING
//!      │                                          fatal status
//!   UNBOUND ◄──unbind_resp── BOUND               BINDING ──────────► FAILED
//! ```
//!
//! # Why this is a type and not a `String` in a `watch`
//!
//! The supervisor, the IPC layer and the interface all read this value, and
//! two of the milestone's acceptance criteria are statements *about the
//! edges*: a fatal bind rejection must reach [`SessionState::Failed`] and not
//! [`SessionState::Reconnecting`] (CA-005-03), and a lost `enquire_link_resp`
//! must reach [`SessionState::Reconnecting`] and not stay
//! [`SessionState::Bound`] (CA-005-04). [`SessionState::try_transition`] makes
//! an illegal edge a `Result`, so a mistake in the actors surfaces in a unit
//! test rather than as a stuck banner.

use smpp_core::values::CommandId;

use crate::error::SessionError;

/// Which operations a bound session may perform.
///
/// Spec §7.2: a `bind_transmitter` session cannot receive `deliver_sm`, a
/// `bind_receiver` session cannot submit. Asking anyway earns an
/// `ESME_RINVBNDSTS` from the SMSC; CA-005-02 requires the client to answer
/// with a typed error *before* the PDU leaves, which is what
/// [`BindMode::allows`] is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BindMode {
    /// `bind_transmitter`: submissions only.
    Transmitter,
    /// `bind_receiver`: deliveries only.
    Receiver,
    /// `bind_transceiver`: both, on one connection.
    Transceiver,
}

impl BindMode {
    /// Whether this mode may submit messages.
    #[must_use]
    pub const fn can_transmit(self) -> bool {
        matches!(self, Self::Transmitter | Self::Transceiver)
    }

    /// Whether this mode may receive deliveries.
    #[must_use]
    pub const fn can_receive(self) -> bool {
        matches!(self, Self::Receiver | Self::Transceiver)
    }

    /// Whether an operation is legal on a session bound in this mode.
    ///
    /// Only the operations whose direction the bind constrains are listed;
    /// anything else — `enquire_link`, `unbind`, and their responses — is
    /// legal on every bind, which is precisely why a receiver session can
    /// still be kept alive and closed cleanly.
    #[must_use]
    pub const fn allows(self, operation: CommandId) -> bool {
        match operation {
            CommandId::SubmitSm
            | CommandId::SubmitMulti
            | CommandId::DataSm
            | CommandId::BroadcastSm
            | CommandId::CancelSm
            | CommandId::QuerySm
            | CommandId::ReplaceSm => self.can_transmit(),
            CommandId::DeliverSmResp => self.can_receive(),
            _ => true,
        }
    }

    /// The bind operation that opens a session in this mode.
    #[must_use]
    pub const fn bind_operation(self) -> CommandId {
        match self {
            Self::Transmitter => CommandId::BindTransmitter,
            Self::Receiver => CommandId::BindReceiver,
            Self::Transceiver => CommandId::BindTransceiver,
        }
    }
}

/// Where a session stands, as spec §7.9 draws it.
///
/// The spec's `ERROR` node is named [`SessionState::Failed`] here: `Error` next
/// to a crate-wide `SessionError` reads as "an error happened", when what this
/// variant means is narrower and load-bearing — *the session stopped and will
/// not retry on its own*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum SessionState {
    /// No socket. The state a profile sits in until someone binds it.
    #[default]
    Closed,
    /// Opening the TCP connection.
    Connecting,
    /// Connected, waiting for `bind_*_resp`.
    Binding,
    /// Bound. The only state that may submit (spec §7.9).
    Bound(BindMode),
    /// `unbind` completed, or the peer unbound us. Terminal until rebound.
    Unbound,
    /// Waiting out the back-off before the next attempt.
    Reconnecting,
    /// Stopped on an error retrying cannot fix — the spec's `ERROR`.
    ///
    /// Reached from a `command_status` that [`smpp_core::status_codes`]
    /// classifies [`smpp_core::status_codes::StatusClass::Fatal`]: bad
    /// password, unknown `system_id`. CA-005-03 is the statement that this
    /// state does **not** lead back to [`SessionState::Connecting`] on its
    /// own.
    Failed,
}

impl SessionState {
    /// Whether the session may submit right now.
    #[must_use]
    pub const fn is_bound(self) -> bool {
        matches!(self, Self::Bound(_))
    }

    /// Whether the session has stopped and will not move again unaided.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Unbound | Self::Failed | Self::Closed)
    }

    /// The bind mode, when the session is bound.
    #[must_use]
    pub const fn bind_mode(self) -> Option<BindMode> {
        match self {
            Self::Bound(mode) => Some(mode),
            _ => None,
        }
    }

    /// A stable machine-readable name, for the IPC contract and the logs.
    ///
    /// Deliberately not `Display`-derived from the variant name: this string
    /// crosses the IPC boundary and is keyed off by the interface, so it is a
    /// contract and renaming a variant must not silently change it.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Closed => "CLOSED",
            Self::Connecting => "CONNECTING",
            Self::Binding => "BINDING",
            Self::Bound(_) => "BOUND",
            Self::Unbound => "UNBOUND",
            Self::Reconnecting => "RECONNECT",
            Self::Failed => "ERROR",
        }
    }

    /// Whether moving from `self` to `next` is an edge of spec §7.9.
    #[must_use]
    pub const fn allows(self, next: Self) -> bool {
        match (self, next) {
            // A no-op is always legal: the supervisor republishes the current
            // state on every health tick, and refusing that would make an
            // idle session an error.
            (Self::Closed, Self::Closed)
            | (Self::Connecting, Self::Connecting)
            | (Self::Binding, Self::Binding)
            | (Self::Unbound, Self::Unbound)
            | (Self::Reconnecting, Self::Reconnecting)
            | (Self::Failed, Self::Failed) => true,
            (Self::Bound(left), Self::Bound(right)) => matches!(
                (left, right),
                (BindMode::Transmitter, BindMode::Transmitter)
                    | (BindMode::Receiver, BindMode::Receiver)
                    | (BindMode::Transceiver, BindMode::Transceiver)
            ),

            // The nominal path.
            (Self::Closed | Self::Reconnecting, Self::Connecting)
            | (Self::Connecting, Self::Binding)
            | (Self::Binding, Self::Bound(_)) => true,

            // Failure paths that keep the session alive.
            (Self::Connecting | Self::Binding | Self::Bound(_), Self::Reconnecting) => true,

            // Failure paths that stop it for good.
            (
                Self::Connecting | Self::Binding | Self::Bound(_) | Self::Reconnecting,
                Self::Failed,
            ) => true,

            // Clean shutdown. Reachable from every non-terminal state: a
            // cancellation may land while the back-off is still running or
            // while the bind response has not come back.
            (
                Self::Connecting | Self::Binding | Self::Bound(_) | Self::Reconnecting,
                Self::Unbound,
            ) => true,

            // Rebinding a session that was unbound or that failed starts the
            // whole machine again, from CLOSED.
            (Self::Unbound | Self::Failed, Self::Closed) => true,

            _ => false,
        }
    }

    /// Applies a transition, refusing the edges spec §7.9 does not draw.
    ///
    /// # Errors
    ///
    /// [`SessionError::IllegalTransition`] when the edge does not exist. The
    /// caller is always one of this crate's actors, so this is an internal
    /// invariant surfacing as a value rather than as a panic.
    pub const fn try_transition(self, next: Self) -> Result<Self, SessionError> {
        if self.allows(next) {
            Ok(next)
        } else {
            Err(SessionError::IllegalTransition {
                from: self.code(),
                to: next.code(),
            })
        }
    }
}

impl core::fmt::Display for SessionState {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every edge the diagram of spec §7.9 draws, and nothing else.
    #[test]
    fn the_nominal_path_of_the_specification_is_walkable() {
        let bound = SessionState::Closed
            .try_transition(SessionState::Connecting)
            .and_then(|state| state.try_transition(SessionState::Binding))
            .and_then(|state| state.try_transition(SessionState::Bound(BindMode::Transceiver)))
            .expect("the nominal path must exist");

        assert_eq!(bound, SessionState::Bound(BindMode::Transceiver));

        let closed = bound
            .try_transition(SessionState::Unbound)
            .and_then(|state| state.try_transition(SessionState::Closed))
            .expect("a bound session must close cleanly");

        assert_eq!(closed, SessionState::Closed);
    }

    #[test]
    fn a_lost_link_sends_a_bound_session_back_through_the_back_off() {
        let reconnecting = SessionState::Bound(BindMode::Transceiver)
            .try_transition(SessionState::Reconnecting)
            .expect("CA-005-04: a dead link leaves BOUND");

        assert_eq!(
            reconnecting
                .try_transition(SessionState::Connecting)
                .expect("the back-off must lead back to a new attempt"),
            SessionState::Connecting
        );
    }

    /// CA-005-03 — the edge that must exist, and the one that must not.
    #[test]
    fn a_failed_session_does_not_walk_back_into_a_connection_attempt() {
        let failed = SessionState::Binding
            .try_transition(SessionState::Failed)
            .expect("a fatal bind status stops the session");

        assert!(
            failed.try_transition(SessionState::Connecting).is_err(),
            "ERROR must not lead to CONNECTING: that is the reconnection loop CA-005-03 forbids"
        );
        assert!(
            failed.try_transition(SessionState::Reconnecting).is_err(),
            "ERROR must not lead to RECONNECT either"
        );
        assert_eq!(
            failed
                .try_transition(SessionState::Closed)
                .expect("an explicit rebind is the only way out, and it restarts from CLOSED"),
            SessionState::Closed
        );
    }

    #[test]
    fn the_edges_the_diagram_does_not_draw_are_refused() {
        for (from, to) in [
            (SessionState::Closed, SessionState::Binding),
            (
                SessionState::Closed,
                SessionState::Bound(BindMode::Receiver),
            ),
            (
                SessionState::Connecting,
                SessionState::Bound(BindMode::Receiver),
            ),
            (
                SessionState::Unbound,
                SessionState::Bound(BindMode::Receiver),
            ),
            (SessionState::Unbound, SessionState::Connecting),
            (SessionState::Failed, SessionState::Unbound),
            (
                SessionState::Bound(BindMode::Transmitter),
                SessionState::Bound(BindMode::Receiver),
            ),
        ] {
            assert!(
                from.try_transition(to).is_err(),
                "{from} → {to} is not an edge of spec §7.9"
            );
        }
    }

    #[test]
    fn republishing_the_current_state_is_not_a_transition_error() {
        for state in [
            SessionState::Closed,
            SessionState::Connecting,
            SessionState::Binding,
            SessionState::Bound(BindMode::Transceiver),
            SessionState::Unbound,
            SessionState::Reconnecting,
            SessionState::Failed,
        ] {
            assert_eq!(
                state
                    .try_transition(state)
                    .expect("republishing a state is not a transition"),
                state
            );
        }
    }

    #[test]
    fn the_illegal_transition_error_names_both_ends() {
        let error = SessionState::Closed
            .try_transition(SessionState::Bound(BindMode::Transceiver))
            .expect_err("not an edge");

        assert_eq!(
            error.to_string(),
            "illegal session state transition: CLOSED → BOUND"
        );
    }

    /// CA-005-02 — the check that keeps a `submit_sm` off a receiver session.
    #[test]
    fn a_receiver_bind_refuses_to_submit_and_a_transmitter_bind_refuses_to_deliver() {
        assert!(!BindMode::Receiver.allows(CommandId::SubmitSm));
        assert!(!BindMode::Receiver.allows(CommandId::SubmitMulti));
        assert!(!BindMode::Receiver.allows(CommandId::DataSm));
        assert!(BindMode::Receiver.allows(CommandId::DeliverSmResp));

        assert!(BindMode::Transmitter.allows(CommandId::SubmitSm));
        assert!(!BindMode::Transmitter.allows(CommandId::DeliverSmResp));

        assert!(BindMode::Transceiver.allows(CommandId::SubmitSm));
        assert!(BindMode::Transceiver.allows(CommandId::DeliverSmResp));
    }

    /// Keeping a session alive and closing it must work on every bind type,
    /// receiver included — otherwise a receiver session could never be
    /// unbound cleanly, which CA-005-08 requires of all of them.
    #[test]
    fn keep_alive_and_shutdown_are_legal_on_every_bind_mode() {
        for mode in [
            BindMode::Transmitter,
            BindMode::Receiver,
            BindMode::Transceiver,
        ] {
            assert!(mode.allows(CommandId::EnquireLink));
            assert!(mode.allows(CommandId::EnquireLinkResp));
            assert!(mode.allows(CommandId::Unbind));
            assert!(mode.allows(CommandId::UnbindResp));
            assert!(mode.allows(CommandId::GenericNack));
        }
    }

    #[test]
    fn each_bind_mode_names_its_own_bind_operation() {
        assert_eq!(
            BindMode::Transmitter.bind_operation(),
            CommandId::BindTransmitter
        );
        assert_eq!(BindMode::Receiver.bind_operation(), CommandId::BindReceiver);
        assert_eq!(
            BindMode::Transceiver.bind_operation(),
            CommandId::BindTransceiver
        );
    }

    /// The codes cross the IPC boundary; the interface keys off them.
    #[test]
    fn the_state_codes_are_the_names_of_the_specification() {
        assert_eq!(SessionState::Closed.code(), "CLOSED");
        assert_eq!(SessionState::Connecting.code(), "CONNECTING");
        assert_eq!(SessionState::Binding.code(), "BINDING");
        assert_eq!(SessionState::Bound(BindMode::Receiver).code(), "BOUND");
        assert_eq!(SessionState::Unbound.code(), "UNBOUND");
        assert_eq!(SessionState::Reconnecting.code(), "RECONNECT");
        assert_eq!(SessionState::Failed.code(), "ERROR");
    }

    #[test]
    fn only_a_bound_session_reports_a_bind_mode() {
        assert_eq!(
            SessionState::Bound(BindMode::Receiver).bind_mode(),
            Some(BindMode::Receiver)
        );
        assert_eq!(SessionState::Binding.bind_mode(), None);
        assert!(SessionState::Bound(BindMode::Receiver).is_bound());
        assert!(!SessionState::Binding.is_bound());
        assert!(SessionState::Failed.is_terminal());
        assert!(SessionState::Unbound.is_terminal());
        assert!(!SessionState::Reconnecting.is_terminal());
    }
}
