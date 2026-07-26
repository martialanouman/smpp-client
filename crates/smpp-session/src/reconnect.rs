//! Reconnection policy: when to try again, how long to wait, when to stop.
//!
//! Two decisions live here, and they are independent.
//!
//! **Whether to retry at all** is not a judgement this module makes: it reads
//! the `command_status` classification produced at milestone 003
//! ([`smpp_core::status_codes::StatusClass`]). A bind refused with
//! `ESME_RINVPASWD` is [`StatusClass::Fatal`], and a fatal refusal stops the
//! session (CA-005-03) — retrying identical credentials forever hammers an
//! SMSC that will keep saying no, and buries the one message the operator
//! needs to see.
//!
//! **How long to wait** is an exponential back-off, capped, **with jitter**.
//! The jitter is not a refinement: milestone 011 runs several sessions at
//! once, and sessions that drop together — a network blip, an SMSC restart —
//! would otherwise retry in lockstep for ever, turning one outage into a
//! synchronised load spike on the way back up.

use core::time::Duration;

use rand::Rng;
use smpp_core::status_codes::StatusClass;

use crate::error::{ProfileRejection, SessionError};

/// Lowest back-off the policy accepts, in seconds (spec §8.2).
const MIN_BACKOFF_FLOOR_S: u32 = 1;

/// Highest back-off the policy accepts, in seconds.
///
/// Spec §8.2 gives 60 as the default ceiling. The hard limit is an hour: past
/// that a session is not "reconnecting", it is down, and the operator should
/// be told rather than left with a spinner.
const MAX_BACKOFF_CEILING_S: u32 = 3_600;

/// What the supervisor must do after a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReconnectDecision {
    /// Wait this long, then connect again.
    RetryAfter(Duration),
    /// Stop. The session moves to [`crate::state::SessionState::Failed`].
    GiveUp(GiveUpReason),
}

/// Why the supervisor stopped retrying.
///
/// Carried all the way to the interface: "wrong password" and "reconnection
/// turned off" are the same state on screen but not the same thing to do
/// about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GiveUpReason {
    /// The SMSC refused in a way that repeating cannot fix.
    FatalStatus,
    /// The profile has reconnection disabled.
    Disabled,
}

impl GiveUpReason {
    /// A stable machine-readable name, for the IPC contract.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::FatalStatus => "FATAL_STATUS",
            Self::Disabled => "RECONNECT_DISABLED",
        }
    }
}

/// The reconnection settings of a session profile (spec §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReconnectPolicy {
    enabled: bool,
    min_backoff: Duration,
    max_backoff: Duration,
    jitter: bool,
}

impl Default for ReconnectPolicy {
    /// The defaults of spec §8.2: enabled, 1 s to 60 s, jitter on.
    fn default() -> Self {
        Self {
            enabled: true,
            min_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            jitter: true,
        }
    }
}

impl ReconnectPolicy {
    /// Builds a policy from the seconds a profile stores.
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidProfile`] when `min_backoff_s` is below one
    /// second, when `max_backoff_s` exceeds an hour, or when the two bounds
    /// are in the wrong order.
    pub fn new(
        enabled: bool,
        min_backoff_s: u32,
        max_backoff_s: u32,
        jitter: bool,
    ) -> Result<Self, SessionError> {
        if min_backoff_s < MIN_BACKOFF_FLOOR_S {
            return Err(SessionError::invalid_profile(
                "min_backoff_s",
                ProfileRejection::OutOfRange,
            ));
        }

        if max_backoff_s > MAX_BACKOFF_CEILING_S {
            return Err(SessionError::invalid_profile(
                "max_backoff_s",
                ProfileRejection::OutOfRange,
            ));
        }

        if max_backoff_s < min_backoff_s {
            return Err(SessionError::invalid_profile(
                "max_backoff_s",
                ProfileRejection::Contradictory,
            ));
        }

        Ok(Self {
            enabled,
            min_backoff: Duration::from_secs(u64::from(min_backoff_s)),
            max_backoff: Duration::from_secs(u64::from(max_backoff_s)),
            jitter,
        })
    }

    /// Whether the session reconnects on its own at all.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Shortest wait, before jitter.
    #[must_use]
    pub const fn min_backoff(self) -> Duration {
        self.min_backoff
    }

    /// Longest wait. No delay this function returns ever exceeds it.
    #[must_use]
    pub const fn max_backoff(self) -> Duration {
        self.max_backoff
    }

    /// Whether the wait is spread out.
    #[must_use]
    pub const fn has_jitter(self) -> bool {
        self.jitter
    }

    /// How long to wait before attempt number `attempt`.
    ///
    /// `attempt` counts from 1 for the first retry. The undelayed base is
    /// `min_backoff * 2^(attempt - 1)`, capped at [`Self::max_backoff`].
    ///
    /// # The jitter is "equal jitter", not "full jitter"
    ///
    /// The returned delay is drawn uniformly from `[base / 2, base]` rather
    /// than from `[0, base]`. Full jitter spreads better but destroys the
    /// growth: a fifth attempt can legitimately draw a shorter wait than the
    /// first, so a session that keeps failing keeps hammering. Equal jitter
    /// keeps the sequence non-decreasing **while the base is still doubling**
    /// — attempt *n* waits at least `base(n) / 2 = base(n - 1)`, which is at
    /// least what attempt *n − 1* waited. CA-005-05 asks for growth and for a
    /// spread at once, and only this shape has both.
    ///
    /// The guarantee stops at the ceiling, and deliberately: once
    /// `base(n) = base(n - 1) = max_backoff`, consecutive draws are two
    /// independent samples of `[max / 2, max]` and the later one can be the
    /// shorter. That is the point of the jitter — a fleet stuck at the ceiling
    /// is exactly the fleet that must not retry in step — and monotonicity
    /// there would mean no spread at all.
    ///
    /// The generator is a parameter rather than a thread-local so tests can
    /// seed it: a jitter assertion that cannot be reproduced is not a test.
    pub fn delay_for<R: Rng + ?Sized>(self, attempt: u32, rng: &mut R) -> Duration {
        let base = self.base_delay(attempt);

        if !self.jitter {
            return base;
        }

        let half = base / 2;
        let spread = base.saturating_sub(half);

        // `Duration::subsec_nanos` fits a `u32` by construction and the whole
        // seconds of a back-off are a handful, so milliseconds are ample
        // precision and never overflow a `u64`.
        let spread_ms = u64::try_from(spread.as_millis()).unwrap_or(u64::MAX);

        if spread_ms == 0 {
            return base;
        }

        half + Duration::from_millis(rng.random_range(0..=spread_ms))
    }

    /// The undelayed exponential base for `attempt`, capped.
    ///
    /// Separate from [`Self::delay_for`] so the cap can be asserted without a
    /// generator in the way.
    #[must_use]
    pub fn base_delay(self, attempt: u32) -> Duration {
        // Clamped rather than wrapped: `1 << 64` is undefined for a `u64`, and
        // an attempt counter that ran away must widen the wait, not reset it.
        let exponent = attempt.saturating_sub(1).min(u64::BITS - 1);
        let factor = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
        let min_ms = u64::try_from(self.min_backoff.as_millis()).unwrap_or(u64::MAX);

        Duration::from_millis(min_ms.saturating_mul(factor)).min(self.max_backoff)
    }

    /// What to do after a failed attempt.
    ///
    /// The classification of milestone 003 is the whole of the judgement —
    /// see [`SessionError::is_retryable`].
    pub fn decide<R: Rng + ?Sized>(
        self,
        error: &SessionError,
        attempt: u32,
        rng: &mut R,
    ) -> ReconnectDecision {
        if !error.is_retryable() {
            return ReconnectDecision::GiveUp(GiveUpReason::FatalStatus);
        }

        if !self.enabled {
            return ReconnectDecision::GiveUp(GiveUpReason::Disabled);
        }

        ReconnectDecision::RetryAfter(self.delay_for(attempt, rng))
    }
}

/// Whether a status classification lets the session try again.
///
/// A one-line free function so the rule has a name and one call site rather
/// than being spelled out wherever a bind response is handled.
#[must_use]
pub const fn may_retry(class: StatusClass) -> bool {
    class.is_retryable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, SeedableRng as _};
    use smpp_core::values::{CommandId, CommandStatus};

    /// A generator seeded to a fixed value: the jitter is random, the test is
    /// not (CLAUDE.md §7).
    fn seeded() -> StdRng {
        StdRng::seed_from_u64(0x5_1_B_1)
    }

    fn fatal_bind() -> SessionError {
        SessionError::BindRejected {
            operation: CommandId::BindTransceiver,
            status: CommandStatus::EsmeRinvpaswd,
            symbol: "ESME_RINVPASWD",
            class: StatusClass::Fatal,
        }
    }

    fn dropped_socket() -> SessionError {
        SessionError::Transport {
            operation: "read",
            source: std::io::Error::from(std::io::ErrorKind::ConnectionReset),
        }
    }

    #[test]
    fn the_defaults_are_the_ones_of_the_specification() {
        let policy = ReconnectPolicy::default();

        assert!(policy.is_enabled());
        assert!(policy.has_jitter());
        assert_eq!(policy.min_backoff(), Duration::from_secs(1));
        assert_eq!(policy.max_backoff(), Duration::from_secs(60));
    }

    #[test]
    fn the_base_delay_doubles_and_then_stops_at_the_ceiling() {
        let policy = ReconnectPolicy::new(true, 1, 60, false).expect("valid bounds");

        assert_eq!(policy.base_delay(1), Duration::from_secs(1));
        assert_eq!(policy.base_delay(2), Duration::from_secs(2));
        assert_eq!(policy.base_delay(3), Duration::from_secs(4));
        assert_eq!(policy.base_delay(4), Duration::from_secs(8));
        assert_eq!(policy.base_delay(5), Duration::from_secs(16));
        assert_eq!(policy.base_delay(6), Duration::from_secs(32));
        assert_eq!(policy.base_delay(7), Duration::from_secs(60));
        assert_eq!(policy.base_delay(64), Duration::from_secs(60));
        // The exponent is clamped, so no attempt number can overflow it.
        assert_eq!(policy.base_delay(u32::MAX), Duration::from_secs(60));
    }

    /// CA-005-05, the three properties in one place: growth, bound, spread.
    #[test]
    fn the_back_off_grows_stays_bounded_and_is_not_twice_the_same() {
        let policy = ReconnectPolicy::default();
        let mut rng = seeded();

        let delays: Vec<Duration> = (1..=8).map(|n| policy.delay_for(n, &mut rng)).collect();

        // Growth, for as long as the base is still doubling. Past the ceiling
        // two consecutive draws are independent samples of the same window and
        // the later one may be the shorter — see `delay_for`.
        for (index, pair) in delays.windows(2).enumerate() {
            let attempt = u32::try_from(index).expect("eight attempts fit a u32") + 2;

            if policy.base_delay(attempt) == policy.base_delay(attempt - 1) {
                break;
            }

            let (previous, next) = (pair[0], pair[1]);
            assert!(
                next >= previous,
                "the back-off must not shrink before the ceiling: {previous:?} then {next:?}"
            );
        }

        for delay in &delays {
            assert!(
                *delay <= policy.max_backoff(),
                "{delay:?} exceeds the ceiling of {:?}",
                policy.max_backoff()
            );
            assert!(*delay >= policy.min_backoff() / 2);
        }

        // Two sessions dropping at the same instant must not come back at the
        // same instant. Drawing the same attempt repeatedly is exactly that
        // situation.
        let mut other = StdRng::seed_from_u64(7);
        let herd: Vec<Duration> = (0..16).map(|_| policy.delay_for(6, &mut other)).collect();
        assert!(
            herd.windows(2).any(|pair| pair[0] != pair[1]),
            "without jitter, sixteen sessions would all wait {:?}",
            herd.first()
        );
    }

    #[test]
    fn jitter_can_be_turned_off_and_then_the_delay_is_exactly_the_base() {
        let policy = ReconnectPolicy::new(true, 2, 60, false).expect("valid bounds");
        let mut rng = seeded();

        for attempt in 1..=6 {
            assert_eq!(
                policy.delay_for(attempt, &mut rng),
                policy.base_delay(attempt)
            );
        }
    }

    #[test]
    fn a_jittered_delay_never_leaves_the_half_open_window_of_its_base() {
        let policy = ReconnectPolicy::default();
        let mut rng = seeded();

        for attempt in 1..=10 {
            let base = policy.base_delay(attempt);

            for _ in 0..64 {
                let delay = policy.delay_for(attempt, &mut rng);
                assert!(delay >= base / 2, "{delay:?} is below half of {base:?}");
                assert!(delay <= base, "{delay:?} is above {base:?}");
            }
        }
    }

    /// CA-005-03 — the decision, not the delay.
    #[test]
    fn a_fatal_bind_rejection_stops_the_loop_whatever_the_policy_says() {
        let policy = ReconnectPolicy::default();
        let mut rng = seeded();

        assert_eq!(
            policy.decide(&fatal_bind(), 1, &mut rng),
            ReconnectDecision::GiveUp(GiveUpReason::FatalStatus)
        );

        // And it stays refused however many attempts have gone by.
        assert_eq!(
            policy.decide(&fatal_bind(), 99, &mut rng),
            ReconnectDecision::GiveUp(GiveUpReason::FatalStatus)
        );
    }

    #[test]
    fn a_dropped_socket_is_retried_after_a_delay() {
        let policy = ReconnectPolicy::default();
        let mut rng = seeded();

        let ReconnectDecision::RetryAfter(delay) = policy.decide(&dropped_socket(), 1, &mut rng)
        else {
            panic!("a dropped socket must be retried");
        };

        assert!(delay <= policy.max_backoff());
    }

    #[test]
    fn a_profile_with_reconnection_off_never_retries_even_a_transient_failure() {
        let policy = ReconnectPolicy::new(false, 1, 60, true).expect("valid bounds");
        let mut rng = seeded();

        assert_eq!(
            policy.decide(&dropped_socket(), 1, &mut rng),
            ReconnectDecision::GiveUp(GiveUpReason::Disabled)
        );
    }

    /// A `Recoverable` bind status — the SMSC has not reaped the previous
    /// session yet — must be retried. Classifying it `Fatal` was the
    /// regression milestone 003 pinned down; this is the session-side half of
    /// that guard.
    #[test]
    fn an_already_bound_rejection_is_retried_rather_than_abandoned() {
        let policy = ReconnectPolicy::default();
        let mut rng = seeded();
        let error = SessionError::BindRejected {
            operation: CommandId::BindTransceiver,
            status: CommandStatus::EsmeRalybnd,
            symbol: "ESME_RALYBND",
            class: StatusClass::Recoverable,
        };

        assert!(matches!(
            policy.decide(&error, 1, &mut rng),
            ReconnectDecision::RetryAfter(_)
        ));
    }

    #[test]
    fn the_bounds_of_the_policy_are_validated() {
        assert!(ReconnectPolicy::new(true, 0, 60, true).is_err());
        assert!(ReconnectPolicy::new(true, 1, 3_601, true).is_err());
        assert!(ReconnectPolicy::new(true, 30, 10, true).is_err());
        assert!(ReconnectPolicy::new(true, 1, 1, true).is_ok());
    }

    #[test]
    fn the_give_up_reasons_carry_a_stable_code() {
        assert_eq!(GiveUpReason::FatalStatus.code(), "FATAL_STATUS");
        assert_eq!(GiveUpReason::Disabled.code(), "RECONNECT_DISABLED");
    }

    #[test]
    fn the_retry_predicate_is_the_classification_of_milestone_003() {
        assert!(!may_retry(StatusClass::Fatal));
        assert!(!may_retry(StatusClass::Success));
        assert!(may_retry(StatusClass::Recoverable));
        assert!(may_retry(StatusClass::Throttling));
    }
}
