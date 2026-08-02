//! Replay policy, decided by error code (deliverable L-010-05).
//!
//! Spec §10.7 asks for three settings — how many attempts, how long between
//! them, and which codes are worth replaying — and CA-010-07 names the two
//! cases that must not be got wrong: an `ESME_RINVDSTADR` is never replayed, an
//! `ESME_RTHROTTLED` is, after a delay and within the budget.
//!
//! # Whether to replay is not decided here
//!
//! It is read from the classification of milestone 003
//! ([`smpp_core::status_codes::StatusClass`]), exactly as the reconnection
//! policy of milestone 005 does. That table is the single place where "may this
//! request be sent again as is" is answered for a `command_status`, it is
//! exhaustive over both specifications, and its regression tests guard the
//! codes this module cares about. A second reading here would be a second
//! answer, and the two would drift on the day somebody reclassifies a code.
//!
//! What this module adds is the part the table cannot know: the failures that
//! carry **no** `command_status` at all — a response that never came, a session
//! that closed mid-flight. Spec §10.7 names the timeout beside the two status
//! codes, and it is the only case where "the message may already have been
//! accepted" is true, which is what makes
//! [`crate::message::SmscMessageIdUpdate`] a tri-state.
//!
//! # No sleeping, no clock, no generator
//!
//! [`RetryPolicy::delay_for`] is a pure function of the attempt number. The
//! waiting belongs to the campaign runner, which owns the cancellation token
//! that must interrupt it (CA-010-09: cancelling stops sending in under a
//! second, and a task parked in a `sleep` that nothing can wake does not).
//!
//! There is deliberately **no jitter** here, unlike the reconnection back-off.
//! Jitter exists to stop independent clients retrying in lockstep after a
//! common outage; the retries of one campaign are issued by one feeder through
//! one rate limiter, so there is no herd to spread — and an `Rng` parameter
//! would have to be threaded through the whole runner to reach a decision that
//! is otherwise pure.
//!
//! # The attempt budget is consumed uniformly
//!
//! Including by failures that never reached the wire
//! ([`SubmitError::prevented_emission`]). The alternative — not counting an
//! attempt that produced no PDU — leaves the loop unbounded exactly when the
//! message centre is unreachable, which is the moment a bound matters most.
//!
//! The consequence is stated rather than hidden: the `attempts` column of the
//! journal counts **emissions** (it is written with `sent_at`, and a message
//! that never left carries no departure instant), so the counter passed to
//! [`RetryPolicy::decide`] is the runner's own and is not the column. They
//! differ by the attempts that were refused before the socket, and a cold
//! restart resets the runner's counter — a resumed campaign gives a message its
//! full budget again, which is what a human would do by hand and is preferable
//! to abandoning a recipient because a session flapped an hour ago.

use core::time::Duration;

use smpp_core::status_codes::{self, StatusClass};
use smpp_core::values::CommandStatus;

use crate::ports::SubmitError;

/// Why a send attempt did not end with an accepted message.
///
/// The two arms are the two shapes a failure comes in, and the distinction is
/// the one [`crate::ports::SmscSession::submit`] already makes: a refusal is an
/// answer and carries a `command_status`; the rest is the absence of one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SendFailure {
    /// The message centre answered, refusing the message.
    Rejected(CommandStatus),
    /// No usable answer came back.
    NoResponse(SubmitError),
}

/// What the campaign must do after a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryDecision {
    /// Send again, as attempt `attempt`, after waiting `delay`.
    RetryAfter {
        /// Number of the attempt about to be made, counting from 1.
        attempt: u32,
        /// How long to wait first.
        delay: Duration,
    },
    /// Do not send again.
    GiveUp(GiveUpReason),
}

/// Why the policy stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GiveUpReason {
    /// Repeating the same submission cannot succeed.
    Fatal,
    /// The failure was transient, and the attempt budget is spent.
    AttemptsExhausted,
    /// The attempt did not fail.
    NothingToRetry,
}

/// How the wait grows from one attempt to the next.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RetryBackoff {
    /// The same wait every time.
    Fixed,
    /// Doubling, capped.
    #[default]
    Exponential,
}

/// Why a retry policy was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RetryPolicyError {
    /// Fewer than one attempt, or more than the ceiling.
    #[error("a campaign makes between 1 and {maximum} attempts per message")]
    AttemptsOutOfRange {
        /// The ceiling.
        maximum: u32,
    },
    /// The ceiling is below the base delay.
    #[error("the maximum delay is below the base delay")]
    DelayBoundsInverted,
    /// The ceiling is above what a campaign may wait.
    #[error("a retry may not be delayed by more than {maximum_s} seconds")]
    DelayTooLong {
        /// The ceiling, in seconds.
        maximum_s: u64,
    },
}

/// The replay policy of a campaign (spec §10.7).
///
/// ```
/// use core::time::Duration;
/// use messaging::retry::{GiveUpReason, RetryDecision, RetryPolicy, SendFailure};
/// use smpp_core::values::CommandStatus;
///
/// let policy = RetryPolicy::default();
///
/// // Throttled: replayed, after a delay (CA-010-07).
/// assert_eq!(
///     policy.decide(&SendFailure::Rejected(CommandStatus::EsmeRthrottled), 1),
///     RetryDecision::RetryAfter { attempt: 2, delay: Duration::from_secs(5) },
/// );
///
/// // Invalid destination: never replayed, whatever the budget left.
/// assert_eq!(
///     policy.decide(&SendFailure::Rejected(CommandStatus::EsmeRinvdstadr), 1),
///     RetryDecision::GiveUp(GiveUpReason::Fatal),
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
    backoff: RetryBackoff,
}

impl RetryPolicy {
    /// Most attempts a policy may ask for.
    ///
    /// A guard-rail, not a protocol limit (CLAUDE.md §8 asks for caps on
    /// volume): a campaign of half a million recipients configured with a
    /// hundred attempts is a way to send fifty million PDUs to a message centre
    /// that is already refusing them, and no operator means that.
    pub const MAX_ATTEMPTS: u32 = 10;

    /// Longest a retry may be held back, in seconds.
    ///
    /// The same hour as the reconnection ceiling of milestone 005, for the same
    /// reason: past that, a message is not "being retried", it is stuck, and
    /// the operator should be told rather than left with a spinner.
    pub const MAX_DELAY_S: u64 = 3_600;

    /// Builds a policy.
    ///
    /// # Errors
    ///
    /// [`RetryPolicyError::AttemptsOutOfRange`] outside 1..=[`Self::MAX_ATTEMPTS`]
    /// — zero attempts is a campaign that sends nothing, which is a
    /// configuration mistake and not a policy —,
    /// [`RetryPolicyError::DelayTooLong`] above [`Self::MAX_DELAY_S`], and
    /// [`RetryPolicyError::DelayBoundsInverted`] when the ceiling is below the
    /// base delay.
    pub fn new(
        max_attempts: u32,
        base_delay: Duration,
        max_delay: Duration,
        backoff: RetryBackoff,
    ) -> Result<Self, RetryPolicyError> {
        if !(1..=Self::MAX_ATTEMPTS).contains(&max_attempts) {
            return Err(RetryPolicyError::AttemptsOutOfRange {
                maximum: Self::MAX_ATTEMPTS,
            });
        }

        if max_delay > Duration::from_secs(Self::MAX_DELAY_S) {
            return Err(RetryPolicyError::DelayTooLong {
                maximum_s: Self::MAX_DELAY_S,
            });
        }

        if max_delay < base_delay {
            return Err(RetryPolicyError::DelayBoundsInverted);
        }

        Ok(Self {
            max_attempts,
            base_delay,
            max_delay,
            backoff,
        })
    }

    /// How many times a message is sent, first attempt included.
    ///
    /// One means "no replay": the message is sent once and its verdict is
    /// final.
    #[must_use]
    pub const fn max_attempts(self) -> u32 {
        self.max_attempts
    }

    /// The base wait, before any growth.
    #[must_use]
    pub const fn base_delay(self) -> Duration {
        self.base_delay
    }

    /// The longest wait this policy ever returns.
    #[must_use]
    pub const fn max_delay(self) -> Duration {
        self.max_delay
    }

    /// How the wait grows.
    #[must_use]
    pub const fn backoff(self) -> RetryBackoff {
        self.backoff
    }

    /// How long to wait before retry number `retry`, counting from 1.
    ///
    /// A pure function of the attempt number: nothing sleeps here, and the
    /// caller is what owns the clock and the cancellation token.
    ///
    /// Under [`RetryBackoff::Exponential`] the wait is
    /// `base_delay * 2^(retry - 1)`, capped at [`Self::max_delay`]. A runaway
    /// counter saturates at the ceiling rather than wrapping round to nothing:
    /// `1 << 64` is undefined for a `u64`, and an attempt number nobody expected
    /// must widen the wait, not remove it.
    #[must_use]
    pub fn delay_for(self, retry: u32) -> Duration {
        if self.backoff == RetryBackoff::Fixed {
            return self.base_delay.min(self.max_delay);
        }

        let exponent = retry.saturating_sub(1).min(u64::BITS - 1);
        let factor = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
        let base_ms = u64::try_from(self.base_delay.as_millis()).unwrap_or(u64::MAX);

        Duration::from_millis(base_ms.saturating_mul(factor)).min(self.max_delay)
    }

    /// What to do after a failed attempt.
    ///
    /// `attempts_made` counts the attempts already made for this message,
    /// **including** the one that just failed, so it is at least 1 when a
    /// failure exists. The returned `attempt` is the number of the attempt to
    /// make next, which is what the journal records beside `sent_at`
    /// ([`crate::message::MessageStateUpdate::sent_at`]).
    ///
    /// The order of the three answers is not arbitrary: a fatal failure gives
    /// up **before** the budget is looked at, so a message rejected for an
    /// invalid destination on its last allowed attempt is reported as invalid
    /// and not as exhausted. The two call for very different things — fix the
    /// number, or try again later — and the interface shows the difference.
    #[must_use]
    pub fn decide(self, failure: &SendFailure, attempts_made: u32) -> RetryDecision {
        if let Some(StatusClass::Success) = failure.status_class() {
            return RetryDecision::GiveUp(GiveUpReason::NothingToRetry);
        }

        if !failure.is_retryable() {
            return RetryDecision::GiveUp(GiveUpReason::Fatal);
        }

        if attempts_made >= self.max_attempts {
            return RetryDecision::GiveUp(GiveUpReason::AttemptsExhausted);
        }

        RetryDecision::RetryAfter {
            attempt: attempts_made.saturating_add(1),
            delay: self.delay_for(attempts_made.max(1)),
        }
    }
}

impl Default for RetryPolicy {
    /// Three attempts, five seconds apart, doubling, capped at a minute.
    ///
    /// Spec §10.7 names the settings and not their values. Three attempts is
    /// what covers the failures this policy is for — a queue that fills, a
    /// window that throttles, a response lost in a reconnection — without
    /// turning a message centre's bad ten minutes into three times the traffic.
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(60),
            backoff: RetryBackoff::Exponential,
        }
    }
}

impl SendFailure {
    /// How milestone 003 classifies this failure, when it carries a status.
    ///
    /// `None` for [`Self::NoResponse`]: there is no `command_status` to
    /// classify, and inventing one would put a code in the journal that no
    /// message centre sent.
    #[must_use]
    pub fn status_class(&self) -> Option<StatusClass> {
        match self {
            Self::Rejected(status) => Some(status_codes::classify(*status)),
            Self::NoResponse(_) => None,
        }
    }

    /// Whether sending the same message again may succeed.
    ///
    /// For a refusal, this is [`StatusClass::is_retryable`] and nothing else —
    /// including for a code no specification documents, which milestone 003
    /// reads as fatal on purpose.
    ///
    /// For the failures that carry no status, the table below is the whole
    /// judgement. Only one of them is permanent:
    ///
    /// | [`SubmitError`] | Replayed | Why |
    /// |---|---|---|
    /// | `ResponseTimeout` | yes | Spec §10.7 names it. The message centre may have accepted the message and lost the answer, which is why a retry gets a new `smsc_message_id`. |
    /// | `Closed` | yes | The session ended mid-flight; milestone 005 rebinds and the message goes out on the next attempt. |
    /// | `Transport` | yes | A socket or codec failure on one PDU says nothing about the next. |
    /// | `NotBound` | yes | The session is reconnecting. Nothing about the message is wrong. |
    /// | `OperationNotAllowed` | **no** | A receiver bind will refuse the next submission exactly as it refused this one. Replaying a configuration mistake is a loop, and the operator needs to be told. |
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Rejected(status) => status_codes::classify(*status).is_retryable(),
            Self::NoResponse(error) => !matches!(error, SubmitError::OperationNotAllowed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GiveUpReason, RetryBackoff, RetryDecision, RetryPolicy, RetryPolicyError, SendFailure,
    };
    use crate::ports::SubmitError;
    use core::time::Duration;
    use smpp_core::values::CommandStatus;

    fn policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy::new(
            max_attempts,
            Duration::from_secs(5),
            Duration::from_secs(60),
            RetryBackoff::Exponential,
        )
        .expect("the bounds are valid")
    }

    /// CA-010-07, first half: an invalid destination is never replayed, and no
    /// amount of remaining budget changes that.
    #[test]
    fn an_invalid_destination_is_never_replayed() {
        let failure = SendFailure::Rejected(CommandStatus::EsmeRinvdstadr);

        for attempts_made in 0..RetryPolicy::MAX_ATTEMPTS {
            assert_eq!(
                policy(RetryPolicy::MAX_ATTEMPTS).decide(&failure, attempts_made),
                RetryDecision::GiveUp(GiveUpReason::Fatal),
                "after {attempts_made} attempt(s)"
            );
        }
    }

    /// CA-010-07, second half: throttling is replayed after a delay, within the
    /// attempt budget — and the boundary is exact.
    #[test]
    fn throttling_is_replayed_until_the_budget_is_spent() {
        let failure = SendFailure::Rejected(CommandStatus::EsmeRthrottled);
        let policy = policy(3);

        assert_eq!(
            policy.decide(&failure, 1),
            RetryDecision::RetryAfter {
                attempt: 2,
                delay: Duration::from_secs(5),
            }
        );
        assert_eq!(
            policy.decide(&failure, 2),
            RetryDecision::RetryAfter {
                attempt: 3,
                delay: Duration::from_secs(10),
            }
        );
        assert_eq!(
            policy.decide(&failure, 3),
            RetryDecision::GiveUp(GiveUpReason::AttemptsExhausted)
        );
        assert_eq!(
            policy.decide(&failure, 4),
            RetryDecision::GiveUp(GiveUpReason::AttemptsExhausted)
        );
    }

    /// The two give-up reasons are not interchangeable: "this number is
    /// invalid" and "we tried three times" call for different things, and a
    /// message that is both must be reported as the first.
    #[test]
    fn a_fatal_failure_is_reported_as_fatal_even_once_the_budget_is_spent() {
        assert_eq!(
            policy(3).decide(&SendFailure::Rejected(CommandStatus::EsmeRinvdstadr), 3),
            RetryDecision::GiveUp(GiveUpReason::Fatal)
        );
    }

    #[test]
    fn a_full_message_queue_is_replayed() {
        assert!(SendFailure::Rejected(CommandStatus::EsmeRmsgqful).is_retryable());
    }

    #[test]
    fn a_transient_failure_of_the_message_centre_is_replayed() {
        assert!(SendFailure::Rejected(CommandStatus::EsmeRsubmitfail).is_retryable());
        assert!(SendFailure::Rejected(CommandStatus::EsmeRsyserr).is_retryable());
    }

    /// Spec §10.7 names the timeout beside the two status codes.
    #[test]
    fn a_response_that_never_came_is_replayed() {
        assert!(SendFailure::NoResponse(SubmitError::ResponseTimeout).is_retryable());
    }

    #[test]
    fn a_session_that_closed_or_stumbled_is_replayed() {
        assert!(SendFailure::NoResponse(SubmitError::Closed).is_retryable());
        assert!(SendFailure::NoResponse(SubmitError::Transport {
            reason: String::from("connection reset"),
        })
        .is_retryable());
        assert!(SendFailure::NoResponse(SubmitError::NotBound {
            state: String::from("RECONNECT"),
        })
        .is_retryable());
    }

    /// A receiver bind will refuse the next submission exactly as it refused
    /// this one: this is a configuration mistake, not a transient failure.
    #[test]
    fn a_session_that_may_not_submit_is_never_replayed() {
        let failure = SendFailure::NoResponse(SubmitError::OperationNotAllowed);

        assert!(!failure.is_retryable());
        assert_eq!(
            policy(3).decide(&failure, 1),
            RetryDecision::GiveUp(GiveUpReason::Fatal)
        );
    }

    #[test]
    fn a_success_is_not_something_to_replay() {
        assert_eq!(
            policy(3).decide(&SendFailure::Rejected(CommandStatus::EsmeRok), 1),
            RetryDecision::GiveUp(GiveUpReason::NothingToRetry)
        );
    }

    /// The classification of milestone 003 reads an unknown code as fatal, and
    /// this policy inherits that rather than deciding again.
    #[test]
    fn a_status_no_specification_documents_is_never_replayed() {
        let failure = SendFailure::Rejected(CommandStatus::from(0x0000_0400));

        assert!(!failure.is_retryable());
        assert_eq!(
            policy(3).decide(&failure, 1),
            RetryDecision::GiveUp(GiveUpReason::Fatal)
        );
    }

    #[test]
    fn the_delay_doubles_and_then_stops_at_the_ceiling() {
        let policy = policy(RetryPolicy::MAX_ATTEMPTS);

        assert_eq!(policy.delay_for(1), Duration::from_secs(5));
        assert_eq!(policy.delay_for(2), Duration::from_secs(10));
        assert_eq!(policy.delay_for(3), Duration::from_secs(20));
        assert_eq!(policy.delay_for(4), Duration::from_secs(40));
        assert_eq!(policy.delay_for(5), Duration::from_secs(60));
        assert_eq!(policy.delay_for(6), Duration::from_secs(60));
    }

    /// A runaway counter must widen the wait to the ceiling, not wrap round to
    /// nothing.
    #[test]
    fn an_absurd_attempt_number_still_yields_the_ceiling() {
        assert_eq!(
            policy(RetryPolicy::MAX_ATTEMPTS).delay_for(u32::MAX),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn a_fixed_backoff_waits_the_same_every_time() {
        let policy = RetryPolicy::new(
            3,
            Duration::from_secs(5),
            Duration::from_secs(60),
            RetryBackoff::Fixed,
        )
        .expect("the bounds are valid");

        assert_eq!(policy.delay_for(1), Duration::from_secs(5));
        assert_eq!(policy.delay_for(4), Duration::from_secs(5));
    }

    #[test]
    fn a_policy_that_never_sends_is_refused() {
        assert_eq!(
            RetryPolicy::new(
                0,
                Duration::from_secs(5),
                Duration::from_secs(60),
                RetryBackoff::Fixed,
            ),
            Err(RetryPolicyError::AttemptsOutOfRange {
                maximum: RetryPolicy::MAX_ATTEMPTS,
            })
        );
    }

    #[test]
    fn a_policy_above_the_attempt_ceiling_is_refused() {
        assert_eq!(
            RetryPolicy::new(
                RetryPolicy::MAX_ATTEMPTS + 1,
                Duration::from_secs(5),
                Duration::from_secs(60),
                RetryBackoff::Fixed,
            ),
            Err(RetryPolicyError::AttemptsOutOfRange {
                maximum: RetryPolicy::MAX_ATTEMPTS,
            })
        );
    }

    #[test]
    fn a_ceiling_below_the_base_delay_is_refused() {
        assert_eq!(
            RetryPolicy::new(
                3,
                Duration::from_secs(60),
                Duration::from_secs(5),
                RetryBackoff::Fixed,
            ),
            Err(RetryPolicyError::DelayBoundsInverted)
        );
    }

    #[test]
    fn a_ceiling_beyond_an_hour_is_refused() {
        assert_eq!(
            RetryPolicy::new(
                3,
                Duration::from_secs(5),
                Duration::from_secs(RetryPolicy::MAX_DELAY_S + 1),
                RetryBackoff::Fixed,
            ),
            Err(RetryPolicyError::DelayTooLong {
                maximum_s: RetryPolicy::MAX_DELAY_S,
            })
        );
    }

    /// A policy of one attempt is the way to turn replay off, and it must not
    /// be a policy that retries once.
    #[test]
    fn a_single_attempt_policy_never_replays() {
        assert_eq!(
            policy(1).decide(&SendFailure::Rejected(CommandStatus::EsmeRthrottled), 1),
            RetryDecision::GiveUp(GiveUpReason::AttemptsExhausted)
        );
    }

    #[test]
    fn the_default_policy_is_three_attempts_five_seconds_apart_and_doubling() {
        let policy = RetryPolicy::default();

        assert_eq!(policy.max_attempts(), 3);
        assert_eq!(policy.delay_for(1), Duration::from_secs(5));
        assert_eq!(policy.delay_for(2), Duration::from_secs(10));
    }
}
