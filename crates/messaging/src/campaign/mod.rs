//! Campaign lifecycle (deliverable L-010-01).
//!
//! The machine of spec §10.3, stated once:
//!
//! ```text
//! CREATED → VALIDATED → RUNNING → (PAUSED ⇄ RUNNING) → COMPLETED
//!                          │
//!                          └──► CANCELLED / FAILED
//! ```
//!
//! # Why this lives here and not in `persistence`
//!
//! [`CampaignStatus`] was written at milestone 002 next to the SQLx code that
//! stores it, as one more `stored_enum!`, and ADR 0007 said so explicitly: the
//! consuming crate was an empty shell. Milestone 010 is the crate that owns the
//! lifecycle, so it owns the type that carries it — the same move ADR 0010 made
//! for `MessageState` and ADR 0012 for `Contact`, and ADR 0013 records this
//! one. `persistence` re-exports it, so `persistence::CampaignStatus` still
//! resolves and no call site outside the two crates changed.
//!
//! The move is what makes the machine expressible at all: `messaging` sits
//! **above** `persistence` and cannot depend on it, so a state machine written
//! here over a type declared there is not a thing that can compile. The
//! alternative — a second enum here, converted at the boundary — is two sets of
//! statuses that agree only as long as somebody keeps them in step.
//!
//! # Why this machine is step-by-step where the message one is monotone
//!
//! [`crate::message::MessageState`] had to be made **monotone** — a state moves
//! to itself or to any state further along — because its transitions are
//! *reports about the past* arriving over a network: a `submit_sm_resp`
//! journalled before the `SENT` transition, a delivery receipt overtaking the
//! response. A step-by-step machine refused `QUEUED → ACCEPTED` and froze
//! messages that had in fact completed.
//!
//! The same tension does not exist here, for two reasons, and both are about
//! where the transitions come from:
//!
//! * a campaign transition is a **local decision** — an operator command, or
//!   the runner observing that nothing is left to feed. It is applied by the
//!   task that owns the campaign, in the order it was decided, and there is no
//!   second source that could deliver it out of order;
//! * the lifecycle is not a line. `PAUSED ⇄ RUNNING` is a genuine cycle, so no
//!   ordering of the statuses exists in which "forward" means anything —
//!   monotone is not a stricter machine here, it is an impossible one.
//!
//! So the transitions are enumerated, and the ones that are not are refused
//! with a [`Result`] naming both ends rather than a bare `false`.
//!
//! What **is** kept from the message machine is the pair of properties that had
//! nothing to do with ordering:
//!
//! * every status may move to **itself**, so a command applied twice — a double
//!   click, a resume that finds the campaign already `RUNNING` after a cold
//!   restart — is a no-op and not a rejection (CLAUDE.md §4 asks for idempotent
//!   transitions);
//! * a **terminal** status has no successor but itself: a cancelled campaign
//!   that could be resumed would send the messages the operator stopped.

//! # What else is in this module
//!
//! The lifecycle above is the *statement*; the modules beside it are what runs
//! one campaign against it:
//!
//! | Module | Deliverable | What it owns |
//! |---|---|---|
//! | [`feeder`] | L-010-02 | reading the recipients in streaming into a bounded queue |
//! | [`resume`] | L-010-04 | **the invariant** — at most one accepted message per recipient — and the arbitration on a crash |
//! | [`control`] | — | start, pause, resume, cancel, carried to every task of one campaign |
//! | [`schedule`] | — | when a campaign may send (CA-010-10) |
//! | [`runner`] | — | the loop that ties them together and counts what happened |
//! | [`progress`] | L-010-07 | what an observer may read of a campaign that has not finished |
//!
//! [`resume`] is the one to read first: it states the invariant this milestone
//! exists to hold, and everything else is arranged around it.

pub mod control;
pub mod feeder;
pub mod progress;
pub mod resume;
pub mod runner;
pub mod schedule;

/// Where a campaign stands in the lifecycle of spec §10.3.
///
/// **No `PartialOrd`/`Ord`**, deliberately, where every other stored enum of
/// this workspace derives them. It came free from the `stored_enum!` macro this
/// type used to be built by, and it contradicts the machine below: `PAUSED ⇄
/// RUNNING` is a cycle, so no ordering of these seven statuses means anything,
/// and a derived one would let `status > CampaignStatus::Running` compile and
/// read as "further along" — which is exactly the reasoning the header rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CampaignStatus {
    /// Created, recipients not resolved yet.
    Created,
    /// Recipients and template checked, ready to start.
    Validated,
    /// Sending.
    Running,
    /// Feeding suspended; the in-flight window drains normally.
    Paused,
    /// Every message reached a terminal state.
    Completed,
    /// Stopped by the operator.
    Cancelled,
    /// Stopped by an error the campaign could not recover from.
    Failed,
}

/// A transition the lifecycle of spec §10.3 does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a campaign in {from} cannot move to {to}")]
pub struct InvalidCampaignTransition {
    /// Where the campaign stands.
    pub from: CampaignStatus,
    /// Where the caller tried to take it.
    pub to: CampaignStatus,
}

impl CampaignStatus {
    /// Every variant, in lifecycle order.
    pub const ALL: &'static [Self] = &[
        Self::Created,
        Self::Validated,
        Self::Running,
        Self::Paused,
        Self::Completed,
        Self::Cancelled,
        Self::Failed,
    ];

    /// The text form stored in SQLite (spec §14.2) and shown by the interface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Validated => "VALIDATED",
            Self::Running => "RUNNING",
            Self::Paused => "PAUSED",
            Self::Completed => "COMPLETED",
            Self::Cancelled => "CANCELLED",
            Self::Failed => "FAILED",
        }
    }

    /// Parses the text form, or `None` when the text names no known status.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|status| status.as_str() == raw)
    }

    /// Reports whether no further transition is expected.
    ///
    /// The three ways a campaign ends. A resumed application (spec §10.5)
    /// restarts the campaigns that are *not* terminal, and nothing else.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    /// Reports whether `next` is a legal successor of this status.
    ///
    /// The machine of spec §10.3, enumerated. See the module header for why it
    /// is enumerated rather than ordered, and for the two properties it keeps
    /// from the message machine: a status always moves to itself, and a
    /// terminal one moves nowhere else.
    #[must_use]
    pub const fn can_move_to(self, next: Self) -> bool {
        // A replayed command is a no-op, in every status including the
        // terminal ones.
        if matches!(
            (self, next),
            (Self::Created, Self::Created)
                | (Self::Validated, Self::Validated)
                | (Self::Running, Self::Running)
                | (Self::Paused, Self::Paused)
                | (Self::Completed, Self::Completed)
                | (Self::Cancelled, Self::Cancelled)
                | (Self::Failed, Self::Failed)
        ) {
            return true;
        }

        // Nothing leaves a terminal status. Written before the table below so
        // a line added there cannot resurrect a campaign by accident.
        if self.is_terminal() {
            return false;
        }

        // Stopping is always available while the campaign is alive: the
        // operator may cancel a campaign that has not started, and a validation
        // or session failure may end one that never ran.
        if matches!(next, Self::Cancelled | Self::Failed) {
            return true;
        }

        matches!(
            (self, next),
            // Recipients and template checked.
            (Self::Created, Self::Validated)
                // The operator starts it.
                | (Self::Validated, Self::Running)
                // Pause suspends the feeding; resume puts it back.
                | (Self::Running, Self::Paused)
                | (Self::Paused, Self::Running)
                // Nothing left to feed and nothing in flight. Legal from
                // PAUSED too: a campaign whose last message was accepted while
                // suspended is finished, and requiring a resume first would
                // leave it PAUSED for ever while its counters said otherwise.
                | (Self::Running, Self::Completed)
                | (Self::Paused, Self::Completed)
        )
    }

    /// Moves to `next`, or says why it cannot.
    ///
    /// A [`Result`] rather than a `bool`: a refused command has to reach the
    /// operator as *what* was refused, and a caller that ignores a bare `false`
    /// carries on with a status the campaign never took.
    ///
    /// # Errors
    ///
    /// [`InvalidCampaignTransition`] when the lifecycle does not allow it.
    pub const fn try_move_to(self, next: Self) -> Result<Self, InvalidCampaignTransition> {
        if self.can_move_to(next) {
            Ok(next)
        } else {
            Err(InvalidCampaignTransition {
                from: self,
                to: next,
            })
        }
    }
}

impl core::fmt::Display for CampaignStatus {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{CampaignStatus, InvalidCampaignTransition};

    /// The lifecycle of spec §10.3, transcribed from the diagram rather than
    /// from the implementation: `CREATED → VALIDATED → RUNNING →
    /// (PAUSED ⇄ RUNNING) → COMPLETED`, with `CANCELLED` and `FAILED` reachable
    /// from anything still alive, and the three self-transitions of the
    /// replayed command.
    ///
    /// Read by the exhaustive test below, which holds all forty-nine pairs to
    /// it. The named tests around it say *why* the interesting ones are there.
    const ALLOWED: &[(CampaignStatus, &[CampaignStatus])] = &[
        (
            CampaignStatus::Created,
            &[
                CampaignStatus::Created,
                CampaignStatus::Validated,
                CampaignStatus::Cancelled,
                CampaignStatus::Failed,
            ],
        ),
        (
            CampaignStatus::Validated,
            &[
                CampaignStatus::Validated,
                CampaignStatus::Running,
                CampaignStatus::Cancelled,
                CampaignStatus::Failed,
            ],
        ),
        (
            CampaignStatus::Running,
            &[
                CampaignStatus::Running,
                CampaignStatus::Paused,
                CampaignStatus::Completed,
                CampaignStatus::Cancelled,
                CampaignStatus::Failed,
            ],
        ),
        (
            CampaignStatus::Paused,
            &[
                CampaignStatus::Paused,
                CampaignStatus::Running,
                CampaignStatus::Completed,
                CampaignStatus::Cancelled,
                CampaignStatus::Failed,
            ],
        ),
        (CampaignStatus::Completed, &[CampaignStatus::Completed]),
        (CampaignStatus::Cancelled, &[CampaignStatus::Cancelled]),
        (CampaignStatus::Failed, &[CampaignStatus::Failed]),
    ];

    /// Every valid transition, and the rejection of every invalid one — all
    /// forty-nine pairs, so a transition added to the machine without a line in
    /// the table above fails here.
    #[test]
    fn the_machine_allows_exactly_the_transitions_of_the_specification() {
        for (from, allowed) in ALLOWED {
            for to in CampaignStatus::ALL {
                let expected = allowed.contains(to);

                assert_eq!(from.can_move_to(*to), expected, "{from} -> {to}");
                assert_eq!(from.try_move_to(*to).is_ok(), expected, "{from} -> {to}");
            }
        }

        assert_eq!(ALLOWED.len(), CampaignStatus::ALL.len());
    }

    #[test]
    fn the_nominal_path_of_the_specification_is_legal() {
        let status = CampaignStatus::Created
            .try_move_to(CampaignStatus::Validated)
            .and_then(|status| status.try_move_to(CampaignStatus::Running))
            .and_then(|status| status.try_move_to(CampaignStatus::Completed));

        assert_eq!(status, Ok(CampaignStatus::Completed));
    }

    #[test]
    fn pausing_and_resuming_go_both_ways() {
        assert!(CampaignStatus::Running.can_move_to(CampaignStatus::Paused));
        assert!(CampaignStatus::Paused.can_move_to(CampaignStatus::Running));
    }

    /// Spec §10.3 gates sending behind validation: recipients and template are
    /// checked there, and a campaign that started without it would send a
    /// template nobody read (CA-010-06).
    #[test]
    fn a_campaign_cannot_start_before_it_is_validated() {
        assert_eq!(
            CampaignStatus::Created.try_move_to(CampaignStatus::Running),
            Err(InvalidCampaignTransition {
                from: CampaignStatus::Created,
                to: CampaignStatus::Running,
            })
        );
        assert!(!CampaignStatus::Created.can_move_to(CampaignStatus::Paused));
    }

    #[test]
    fn a_live_campaign_may_always_be_cancelled_or_fail() {
        for from in [
            CampaignStatus::Created,
            CampaignStatus::Validated,
            CampaignStatus::Running,
            CampaignStatus::Paused,
        ] {
            assert!(from.can_move_to(CampaignStatus::Cancelled), "{from}");
            assert!(from.can_move_to(CampaignStatus::Failed), "{from}");
        }
    }

    /// A campaign whose last message was accepted while the feeding was
    /// suspended has nothing left to do. Requiring a resume first would leave
    /// it `PAUSED` for ever while its counters said it was finished
    /// (CA-010-02).
    #[test]
    fn a_paused_campaign_with_nothing_left_may_complete() {
        assert!(CampaignStatus::Paused.can_move_to(CampaignStatus::Completed));
    }

    #[test]
    fn a_campaign_never_returns_to_an_earlier_status() {
        assert!(!CampaignStatus::Validated.can_move_to(CampaignStatus::Created));
        assert!(!CampaignStatus::Running.can_move_to(CampaignStatus::Validated));
        assert!(!CampaignStatus::Running.can_move_to(CampaignStatus::Created));
        assert!(!CampaignStatus::Paused.can_move_to(CampaignStatus::Validated));
    }

    /// Replaying a command that was already applied — the operator clicking
    /// twice, a resume after a restart that finds the campaign already
    /// `RUNNING` — must be a no-op and not a rejection.
    #[test]
    fn every_status_may_move_to_itself() {
        for status in CampaignStatus::ALL {
            assert_eq!(status.try_move_to(*status), Ok(*status), "{status}");
        }
    }

    #[test]
    fn a_terminal_status_has_no_successor_but_itself() {
        for terminal in [
            CampaignStatus::Completed,
            CampaignStatus::Cancelled,
            CampaignStatus::Failed,
        ] {
            for next in CampaignStatus::ALL {
                assert_eq!(
                    terminal.can_move_to(*next),
                    terminal == *next,
                    "{terminal} -> {next}"
                );
            }
        }
    }

    #[test]
    fn only_the_three_end_statuses_are_terminal() {
        assert!(CampaignStatus::Completed.is_terminal());
        assert!(CampaignStatus::Cancelled.is_terminal());
        assert!(CampaignStatus::Failed.is_terminal());

        assert!(!CampaignStatus::Created.is_terminal());
        assert!(!CampaignStatus::Validated.is_terminal());
        assert!(!CampaignStatus::Running.is_terminal());
        assert!(!CampaignStatus::Paused.is_terminal());
    }

    #[test]
    fn every_status_parses_back_from_its_stored_form() {
        for status in CampaignStatus::ALL {
            assert_eq!(CampaignStatus::parse(status.as_str()), Some(*status));
        }
    }

    #[test]
    fn the_stored_text_matches_the_specification() {
        assert_eq!(CampaignStatus::Created.as_str(), "CREATED");
        assert_eq!(CampaignStatus::Cancelled.as_str(), "CANCELLED");
    }

    #[test]
    fn an_unknown_status_is_not_parsed() {
        assert_eq!(CampaignStatus::parse("PENDING"), None);
        assert_eq!(CampaignStatus::parse("created"), None);
    }

    /// The rejection says where the campaign is and where it was taken, which
    /// is what the interface needs to explain a refused command.
    #[test]
    fn a_rejected_transition_names_both_ends() {
        let rejection = CampaignStatus::Completed
            .try_move_to(CampaignStatus::Running)
            .expect_err("a completed campaign does not restart");

        assert_eq!(
            rejection.to_string(),
            "a campaign in COMPLETED cannot move to RUNNING"
        );
    }
}
