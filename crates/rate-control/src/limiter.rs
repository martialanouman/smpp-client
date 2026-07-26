//! The throughput limiter: at most `throughput_tps` PDUs per second.
//!
//! Deliverable L-007-01, spec §9.2 mechanism 1. GCRA, through `governor`, as
//! spec §6.1 prescribes.
//!
//! # Three decisions the fiche asks to make explicitly
//!
//! **No initial burst.** `governor`'s `Quota::per_second(n)` allows a burst of
//! `n` by default: a limiter that has just been built is *full*, so the first
//! `n` requests leave in the same instant and only then does the pacing start.
//! Against a message centre that measures a sliding second, a `throughput_tps`
//! of 100 then produces up to 199 submissions inside the first second — twice
//! the agreed rate, on the very first second of a campaign, which is exactly
//! when a provider is most likely to be watching. The quota is therefore built
//! with `allow_burst(1)`: cells are spaced by `1/tps` and nothing accumulates.
//! The cost is that a genuinely idle session cannot "save up" capacity, which
//! is the point.
//!
//! Note what this does **not** do: it does not make the average rate correct.
//! An average over ten seconds is correct with the burst too. Only a *sliding*
//! window sees the difference, which is why CA-007-01 is stated as "no sliding
//! second exceeds 100" and the test measures it that way.
//!
//! **Zero means unlimited.** Not "zero per second", which would stop the
//! session dead. There is then no `governor` limiter at all and no timer on
//! the path — CA-007-04 asks for no artificial delay, not for a very short
//! one.
//!
//! **The clock is Tokio's.** `governor`'s default clock reads the platform
//! monotonic counter, which `tokio::time::pause()` cannot move; a test would
//! then have to sleep in real time to observe pacing, and CLAUDE.md §7 rules
//! that out. [`TokioClock`] reports the virtual instant, so the whole of the
//! rate control is exercised under `start_paused = true` at no wall-clock
//! cost. Waiting is `tokio::time::sleep`, never `until_ready`, for the same
//! reason: `governor`'s own async wait uses its own timer.
//!
//! # Congestion, and what belongs to milestone 012
//!
//! Spec §9.4 asks for AIMD: a multiplicative cut on `ESME_RTHROTTLED` or a
//! `congestion_state` above 90, then an additive climb back. The fiche puts
//! that at milestone 012 and asks this one to apply a **constant factor of
//! 1.0** while exposing the attachment points. So:
//!
//! * [`AdaptiveFactor`] exists, is settable, and multiplies the target —
//!   nothing in this milestone ever moves it off [`AdaptiveFactor::NEUTRAL`];
//! * [`ThroughputConfig::min_tps`] is the floor of the clamp of spec §9.4;
//! * [`RateLimiter::penalise`] is the **immediate** half of the reaction to
//!   `ESME_RTHROTTLED` — a bounded cooling-off period during which nothing is
//!   admitted. It is not AIMD, it does not touch the factor, and it is what
//!   makes "the message centre said slow down" have an effect on the wire at
//!   this milestone rather than at the next one.

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};

use core::time::Duration;

use governor::clock::Clock;
use governor::middleware::NoOpMiddleware;
use governor::nanos::Nanos;
use governor::state::{InMemoryState, NotKeyed};
use governor::Quota;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::error::RateControlError;

/// A GCRA limiter reading the Tokio clock.
type Gcra = governor::RateLimiter<NotKeyed, InMemoryState, TokioClock, NoOpMiddleware<Nanos>>;

/// How many times [`RateLimiter::acquire`] re-checks the quota before giving
/// up on pacing.
///
/// A correct clock needs exactly one: the wait `governor` reports is how long
/// until the cell is available, and after sleeping it the next check passes.
/// The bound is here so that a clock that ever failed to advance produces a
/// warning and a late PDU rather than a task that spins for ever, which is the
/// busy-wait CLAUDE.md §4 forbids.
const MAX_PACING_ROUNDS: u32 = 8;

/// How the effective rate is derived from the user's target (spec §9.4).
///
/// Stored in **per mille** rather than as an `f64`: the factor multiplies a
/// message rate, the result has to be an integer, and integer arithmetic makes
/// that exact instead of "whatever the rounding did". A workspace that denies
/// `cast_possible_truncation` has no cheap `f64 -> u32` anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AdaptiveFactor(u16);

impl AdaptiveFactor {
    /// One thousand per mille: the target is applied as the user set it.
    ///
    /// **The only value this milestone ever uses.** See the module header.
    pub const NEUTRAL: Self = Self(1_000);

    /// A factor of `permille / 1000`, capped at [`Self::NEUTRAL`].
    ///
    /// Capped rather than refused: spec §9.4 climbs back "towards the user's
    /// target", so 1.0 is a ceiling by definition and a caller asking for more
    /// is asking for the ceiling.
    #[must_use]
    pub const fn from_permille(permille: u16) -> Self {
        if permille > 1_000 {
            Self::NEUTRAL
        } else {
            Self(permille)
        }
    }

    /// The factor in per mille.
    #[must_use]
    pub const fn permille(self) -> u16 {
        self.0
    }

    /// The factor as a fraction, for display.
    #[must_use]
    pub fn as_fraction(self) -> f64 {
        f64::from(self.0) / 1_000.0
    }
}

impl Default for AdaptiveFactor {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// The throughput settings of spec §9.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThroughputConfig {
    /// Messages per second the operator asked for. **Zero means unlimited.**
    pub target_tps: u32,
    /// The floor the adaptation may never go below (spec §9.4).
    ///
    /// Ignored when `target_tps` is zero: there is no rate to clamp.
    pub min_tps: u32,
    /// How long nothing is admitted after an `ESME_RTHROTTLED`.
    ///
    /// The immediate half of spec §9.4's reaction. Milestone 012 adds the
    /// multiplicative cut and the additive recovery around it.
    pub throttle_cooldown: Duration,
}

/// The default cooling-off period after a throttling status.
///
/// One second: long enough that the message centre sees the sending actually
/// stop — a shorter pause at 100 TPS would be lost in the pacing noise — and
/// short enough that a single spurious `ESME_RTHROTTLED` costs a campaign one
/// second rather than a visible stall.
pub const DEFAULT_THROTTLE_COOLDOWN: Duration = Duration::from_secs(1);

impl Default for ThroughputConfig {
    /// The prudent defaults of spec §9.5.
    fn default() -> Self {
        Self {
            target_tps: 100,
            min_tps: 1,
            throttle_cooldown: DEFAULT_THROTTLE_COOLDOWN,
        }
    }
}

impl ThroughputConfig {
    /// A configuration for `target_tps`, with the other defaults.
    #[must_use]
    pub fn at(target_tps: u32) -> Self {
        Self {
            target_tps,
            ..Self::default()
        }
    }

    /// The same configuration with another floor.
    #[must_use]
    pub const fn with_min_tps(mut self, min_tps: u32) -> Self {
        self.min_tps = min_tps;
        self
    }

    /// The same configuration with another cooling-off period.
    #[must_use]
    pub const fn with_throttle_cooldown(mut self, cooldown: Duration) -> Self {
        self.throttle_cooldown = cooldown;
        self
    }
}

/// The mutable half of the limiter, behind the pacing lock.
struct Inner {
    /// `None` when the target is zero — unlimited.
    gcra: Option<Gcra>,
    /// What the target and the factor currently work out to.
    effective_tps: u32,
    /// The factor of spec §9.4. Always [`AdaptiveFactor::NEUTRAL`] here.
    factor: AdaptiveFactor,
}

/// Paces submissions to the configured throughput.
///
/// # The lock is the queue
///
/// One `tokio::sync::Mutex` around the quota, held across the pacing sleep.
/// That is deliberate, and it is not the "lock held across an await" CLAUDE.md
/// §4 warns about — that rule is about `std::sync`, whose guard blocks a
/// runtime thread. Here the lock **is** the waiting line: Tokio's mutex is
/// FIFO-fair, so senders are admitted in the order they arrived, and exactly
/// one of them is ever sleeping on the pacing timer.
///
/// The alternative — every sender checking the quota and sleeping on its own
/// refusal — makes all of them wake at the same instant, one win and the rest
/// re-sleep. That is a thundering herd on the hot path of a campaign, and it
/// makes the admission order arbitrary.
pub struct RateLimiter {
    config: ThroughputConfig,
    clock: TokioClock,
    inner: Mutex<Inner>,
    /// Nanoseconds since [`TokioClock::base`] before which nothing is
    /// admitted, set by [`RateLimiter::penalise`].
    ///
    /// An atomic rather than a field of [`Inner`], and that matters: the
    /// penalty is applied from the **response** path, while the send path may
    /// be asleep on the pacing timer holding the lock. Behind the lock, a
    /// throttling response would have to wait for a pacing interval before it
    /// could even be recorded — the one place in this file where the reaction
    /// has to be immediate.
    not_before_nanos: AtomicU64,
}

impl core::fmt::Debug for RateLimiter {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RateLimiter")
            .field("target_tps", &self.config.target_tps)
            .field("min_tps", &self.config.min_tps)
            .finish_non_exhaustive()
    }
}

impl RateLimiter {
    /// A limiter for `config`, starting at [`AdaptiveFactor::NEUTRAL`].
    ///
    /// # Errors
    ///
    /// [`RateControlError::ThroughputBandEmpty`] when `min_tps` is above a
    /// non-zero `target_tps`.
    pub fn new(config: ThroughputConfig) -> Result<Self, RateControlError> {
        if config.target_tps != 0 && config.min_tps > config.target_tps {
            return Err(RateControlError::ThroughputBandEmpty {
                target_tps: config.target_tps,
                min_tps: config.min_tps,
            });
        }

        let clock = TokioClock::starting_now();
        let factor = AdaptiveFactor::NEUTRAL;
        let effective_tps = effective_tps(&config, factor);

        Ok(Self {
            config,
            inner: Mutex::new(Inner {
                gcra: gcra(effective_tps, &clock),
                effective_tps,
                factor,
            }),
            clock,
            not_before_nanos: AtomicU64::new(0),
        })
    }

    /// A limiter at `target_tps`, with the other settings at their defaults.
    ///
    /// # Errors
    ///
    /// Same as [`Self::new`]; unreachable with the default floor of 1 unless
    /// `target_tps` is zero, which is legal.
    pub fn at(target_tps: u32) -> Result<Self, RateControlError> {
        Self::new(ThroughputConfig::at(target_tps))
    }

    /// A limiter that never delays anything.
    ///
    /// Infallible, which is why it exists: a caller that cannot fail — a
    /// session being spawned — needs a fallback, and "send unpaced" is the
    /// only one that keeps the session usable. An unlimited target has no
    /// band to be empty, so [`Self::new`] cannot refuse it.
    #[must_use]
    pub fn unlimited() -> Self {
        let clock = TokioClock::starting_now();

        Self {
            config: ThroughputConfig {
                target_tps: 0,
                min_tps: 0,
                throttle_cooldown: DEFAULT_THROTTLE_COOLDOWN,
            },
            inner: Mutex::new(Inner {
                gcra: None,
                effective_tps: 0,
                factor: AdaptiveFactor::NEUTRAL,
            }),
            clock,
            not_before_nanos: AtomicU64::new(0),
        }
    }

    /// Waits until one PDU may be sent, then accounts for it.
    ///
    /// Returns as soon as the quota allows, which for an unlimited limiter is
    /// immediately and without touching a timer (CA-007-04).
    pub async fn acquire(&self) {
        self.serve_penalty().await;

        let mut inner = self.inner.lock().await;

        let Some(gcra) = inner.gcra.as_mut() else {
            return;
        };

        for _ in 0..MAX_PACING_ROUNDS {
            let Err(not_until) = gcra.check() else {
                return;
            };

            let wait = not_until.wait_time_from(self.clock.now());

            // `governor` reports a zero wait when the cell is due at exactly
            // this instant; sleeping zero would spin. One nanosecond is enough
            // to make the loop advance under a virtual clock and rounds up to
            // the timer's granularity under a real one.
            tokio::time::sleep(wait.max(Duration::from_nanos(1))).await;
        }

        // Not reachable with a monotonic clock: the wait above is exactly how
        // long the cell needs. Admitting rather than looping is the deliberate
        // choice — a spinning task would take a runtime thread with it, and a
        // campaign that stops for ever is worse than one PDU sent early.
        tracing::warn!(
            rounds = MAX_PACING_ROUNDS,
            "the rate limiter could not settle; admitting the PDU unpaced"
        );
    }

    /// Records an `ESME_RTHROTTLED` (or `ESME_RMSGQFUL`): stop for a while.
    ///
    /// Spec §9.4 asks for "an immediate multiplicative reduction **and** a
    /// back-off". This is the back-off, and it is the whole of the reaction at
    /// this milestone — the multiplicative reduction and its recovery are
    /// milestone 012's (fiche §2). Consecutive throttles push the resume
    /// instant further out rather than resetting it, so a message centre that
    /// keeps refusing keeps the sender stopped.
    pub fn penalise(&self) {
        let cooldown = self.config.throttle_cooldown;

        if cooldown.is_zero() {
            return;
        }

        let resume_at = self
            .clock
            .elapsed_nanos()
            .saturating_add(nanos_of(cooldown));

        self.not_before_nanos.fetch_max(resume_at, Ordering::SeqCst);

        tracing::warn!(
            cooldown_ms = cooldown.as_millis(),
            "the message centre asked us to slow down; pausing submissions"
        );
    }

    /// Waits out any pending throttling penalty.
    async fn serve_penalty(&self) {
        let resume_at = self.not_before_nanos.load(Ordering::SeqCst);

        if resume_at == 0 || resume_at <= self.clock.elapsed_nanos() {
            return;
        }

        tokio::time::sleep_until(self.clock.base + Duration::from_nanos(resume_at)).await;
    }

    /// Whether a throttling penalty is still in force.
    #[must_use]
    pub fn is_penalised(&self) -> bool {
        self.not_before_nanos.load(Ordering::SeqCst) > self.clock.elapsed_nanos()
    }

    /// The target the operator configured. Zero means unlimited.
    #[must_use]
    pub const fn target_tps(&self) -> u32 {
        self.config.target_tps
    }

    /// What the target and the adaptive factor currently work out to.
    ///
    /// Equal to [`Self::target_tps`] for the whole of this milestone.
    pub async fn effective_tps(&self) -> u32 {
        self.inner.lock().await.effective_tps
    }

    /// The adaptive factor of spec §9.4.
    pub async fn factor(&self) -> AdaptiveFactor {
        self.inner.lock().await.factor
    }

    /// Moves the adaptive factor and rebuilds the quota around it.
    ///
    /// The attachment point milestone 012 drives. **Nothing in this milestone
    /// calls it outside its tests**, which is what "a constant factor of 1.0"
    /// means in practice.
    pub async fn set_factor(&self, factor: AdaptiveFactor) {
        let mut inner = self.inner.lock().await;

        if inner.factor == factor {
            return;
        }

        let effective_tps = effective_tps(&self.config, factor);

        inner.factor = factor;
        inner.effective_tps = effective_tps;
        inner.gcra = gcra(effective_tps, &self.clock);
    }
}

/// What `target × factor` works out to, clamped into spec §9.4's band.
fn effective_tps(config: &ThroughputConfig, factor: AdaptiveFactor) -> u32 {
    if config.target_tps == 0 {
        return 0;
    }

    let scaled = u64::from(config.target_tps)
        .saturating_mul(u64::from(factor.permille()))
        .saturating_div(1_000);

    let floor = config.min_tps.max(1);

    u32::try_from(scaled)
        .unwrap_or(u32::MAX)
        .clamp(floor, config.target_tps)
}

/// The GCRA limiter for a rate, or `None` when unlimited.
///
/// `allow_burst(1)` is the no-initial-burst decision of the module header.
fn gcra(tps: u32, clock: &TokioClock) -> Option<Gcra> {
    let rate = NonZeroU32::new(tps)?;
    // INVARIANT: 1 is not zero. `NonZeroU32::new(1)` cannot return `None`, and
    // the `const` below makes the compiler agree without an `expect`.
    const ONE: NonZeroU32 = NonZeroU32::MIN;

    let quota = Quota::per_second(rate).allow_burst(ONE);

    Some(governor::RateLimiter::direct_with_clock(
        quota,
        clock.clone(),
    ))
}

/// Whole nanoseconds of a duration, saturating rather than wrapping.
fn nanos_of(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// `governor`'s clock, reading Tokio's.
///
/// The single reason this type exists: `tokio::time::pause()` moves
/// `tokio::time::Instant` and nothing else, so a limiter reading any other
/// clock cannot be tested without sleeping in real time. Instants are reported
/// as nanoseconds since a base captured at construction, which is what
/// `governor::nanos::Nanos` is.
#[derive(Debug, Clone)]
struct TokioClock {
    /// The instant this clock calls zero.
    base: Instant,
}

impl TokioClock {
    /// A clock whose origin is now.
    fn starting_now() -> Self {
        Self {
            base: Instant::now(),
        }
    }

    /// Nanoseconds elapsed since the origin.
    fn elapsed_nanos(&self) -> u64 {
        nanos_of(Instant::now().saturating_duration_since(self.base))
    }
}

impl Clock for TokioClock {
    type Instant = Nanos;

    fn now(&self) -> Nanos {
        Nanos::new(self.elapsed_nanos())
    }
}

#[cfg(test)]
// `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
// reserves for "the binary entry point". A test harness is one.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    /// Admits `count` PDUs and returns the instant each was admitted at.
    async fn admissions(limiter: &RateLimiter, count: usize) -> Vec<Instant> {
        let mut instants = Vec::with_capacity(count);

        for _ in 0..count {
            limiter.acquire().await;
            instants.push(Instant::now());
        }

        instants
    }

    /// The largest number of admissions inside any window of `width`.
    ///
    /// A **sliding** window, not consecutive buckets: the burst this milestone
    /// is about straddles a bucket boundary as readily as it sits inside one,
    /// and a bucketed count would miss it half the time.
    fn busiest_window(instants: &[Instant], width: Duration) -> usize {
        instants
            .iter()
            .map(|start| {
                let end = *start + width;

                instants
                    .iter()
                    .filter(|at| **at >= *start && **at < end)
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    /// **The burst this milestone exists to prevent.**
    ///
    /// `governor`'s default quota is full at construction, so the first 100
    /// PDUs of a 100 TPS session leave at t=0 and the pacing only starts
    /// afterwards — the first sliding second then carries close to 200.
    ///
    /// Replace `allow_burst(ONE)` in [`gcra`] with the default quota and this
    /// test fails on the second assertion while the *average* stays perfect,
    /// which is the whole point of measuring a sliding window.
    #[tokio::test(start_paused = true)]
    async fn no_sliding_second_exceeds_the_target_not_even_the_first() {
        let limiter = RateLimiter::at(100).expect("a valid band");
        let instants = admissions(&limiter, 300).await;

        assert_eq!(
            busiest_window(&instants, Duration::from_secs(1)),
            100,
            "a sliding second must carry the target and not one PDU more"
        );

        // And the very first second in particular, which is where a full
        // bucket empties itself.
        let start = *instants.first().expect("300 admissions");
        let first_second = instants
            .iter()
            .filter(|at| **at < start + Duration::from_secs(1))
            .count();

        assert_eq!(first_second, 100, "the first second is not a free burst");
    }

    /// CA-007-01, at the level of the limiter: 1 000 PDUs at 100 TPS take ten
    /// seconds, to within five per cent.
    #[tokio::test(start_paused = true)]
    async fn the_configured_target_is_what_the_elapsed_time_says() {
        let limiter = RateLimiter::at(100).expect("a valid band");
        let started = Instant::now();

        for _ in 0..1_000 {
            limiter.acquire().await;
        }

        let elapsed = Instant::now().saturating_duration_since(started);

        // 999 intervals of 10 ms — the first PDU leaves immediately.
        assert!(
            elapsed >= Duration::from_millis(9_500) && elapsed <= Duration::from_millis(10_500),
            "1 000 PDUs at 100 TPS took {elapsed:?}, not ten seconds"
        );
    }

    /// CA-007-04 — zero is unlimited, and unlimited means no timer at all.
    #[tokio::test(start_paused = true)]
    async fn a_target_of_zero_introduces_no_delay_whatsoever() {
        let limiter = RateLimiter::at(0).expect("zero is legal");
        let started = Instant::now();

        for _ in 0..10_000 {
            limiter.acquire().await;
        }

        assert_eq!(
            Instant::now().saturating_duration_since(started),
            Duration::ZERO,
            "an unlimited limiter must not sleep, not even for a nanosecond"
        );
        assert_eq!(limiter.target_tps(), 0);
        assert_eq!(limiter.effective_tps().await, 0);
    }

    /// A slow target is paced just as exactly as a fast one, and the interval
    /// is the one the target implies.
    #[tokio::test(start_paused = true)]
    async fn admissions_are_spaced_by_the_reciprocal_of_the_target() {
        let limiter = RateLimiter::at(4).expect("a valid band");
        let instants = admissions(&limiter, 5).await;

        let first = *instants.first().expect("five admissions");

        for (index, at) in instants.iter().enumerate() {
            let expected = first + Duration::from_millis(250) * u32::try_from(index).unwrap_or(0);
            let drift = at.saturating_duration_since(expected);

            assert!(
                drift < Duration::from_millis(1),
                "admission {index} landed {drift:?} away from its slot"
            );
        }
    }

    /// **What point 3 of this milestone is about.** An `ESME_RTHROTTLED`
    /// must change the wire, not just a counter: after `penalise`, nothing is
    /// admitted until the cooling-off period has run.
    #[tokio::test(start_paused = true)]
    async fn a_throttling_status_stops_the_sending_for_the_cooling_off_period() {
        let limiter = RateLimiter::new(
            ThroughputConfig::at(0).with_throttle_cooldown(Duration::from_secs(2)),
        )
        .expect("a valid band");

        // Unlimited: without the penalty this would take no time at all, so
        // any elapsed time is the penalty and nothing else.
        limiter.acquire().await;

        let started = Instant::now();
        limiter.penalise();
        assert!(limiter.is_penalised());

        limiter.acquire().await;

        assert_eq!(
            Instant::now().saturating_duration_since(started),
            Duration::from_secs(2),
            "the sender must wait out the cooling-off period"
        );
        assert!(!limiter.is_penalised());

        // And once served, it does not come back.
        let resumed = Instant::now();
        limiter.acquire().await;
        assert_eq!(
            Instant::now().saturating_duration_since(resumed),
            Duration::ZERO
        );
    }

    /// A message centre that keeps refusing keeps the sender stopped: the
    /// second penalty extends the pause rather than restarting a shorter one.
    #[tokio::test(start_paused = true)]
    async fn consecutive_throttles_push_the_resume_instant_further_out() {
        let limiter = RateLimiter::new(
            ThroughputConfig::at(0).with_throttle_cooldown(Duration::from_secs(2)),
        )
        .expect("a valid band");

        let started = Instant::now();
        limiter.penalise();

        tokio::time::sleep(Duration::from_secs(1)).await;
        limiter.penalise();

        limiter.acquire().await;

        assert_eq!(
            Instant::now().saturating_duration_since(started),
            Duration::from_secs(3),
            "the second throttle must extend the pause, not replace it"
        );
    }

    /// A cooldown of zero disables the reaction outright, which is what an
    /// operator who wants the message centre's opinion ignored would set.
    #[tokio::test(start_paused = true)]
    async fn a_zero_cooldown_makes_a_throttling_status_cost_nothing() {
        let limiter =
            RateLimiter::new(ThroughputConfig::at(0).with_throttle_cooldown(Duration::ZERO))
                .expect("a valid band");

        limiter.penalise();

        assert!(!limiter.is_penalised());

        let started = Instant::now();
        limiter.acquire().await;

        assert_eq!(
            Instant::now().saturating_duration_since(started),
            Duration::ZERO
        );
    }

    /// The attachment point of milestone 012: the factor scales the target and
    /// the clamp of spec §9.4 keeps it inside its band.
    #[tokio::test(start_paused = true)]
    async fn the_adaptive_factor_scales_the_target_within_its_band() {
        let limiter =
            RateLimiter::new(ThroughputConfig::at(100).with_min_tps(50)).expect("50 is below 100");

        assert_eq!(limiter.factor().await, AdaptiveFactor::NEUTRAL);
        assert_eq!(limiter.effective_tps().await, 100);

        limiter.set_factor(AdaptiveFactor::from_permille(800)).await;
        assert_eq!(limiter.effective_tps().await, 80);

        // Below the floor, the clamp holds.
        limiter.set_factor(AdaptiveFactor::from_permille(100)).await;
        assert_eq!(limiter.effective_tps().await, 50);

        // Above 1.0 is not a way to exceed the user's target.
        limiter
            .set_factor(AdaptiveFactor::from_permille(5_000))
            .await;
        assert_eq!(limiter.effective_tps().await, 100);
    }

    /// A lowered factor is not bookkeeping: the pacing actually slows down.
    #[tokio::test(start_paused = true)]
    async fn lowering_the_factor_lengthens_the_interval_between_admissions() {
        let limiter =
            RateLimiter::new(ThroughputConfig::at(100).with_min_tps(1)).expect("a valid band");

        limiter.acquire().await;
        let at_full_rate = admissions(&limiter, 2).await;
        let full = at_full_rate[1].saturating_duration_since(at_full_rate[0]);

        limiter.set_factor(AdaptiveFactor::from_permille(500)).await;

        limiter.acquire().await;
        let at_half_rate = admissions(&limiter, 2).await;
        let half = at_half_rate[1].saturating_duration_since(at_half_rate[0]);

        assert_eq!(full, Duration::from_millis(10));
        assert_eq!(half, Duration::from_millis(20));
    }

    #[test]
    fn a_floor_above_the_target_is_refused_rather_than_silently_lowered() {
        assert_eq!(
            RateLimiter::new(ThroughputConfig::at(10).with_min_tps(50))
                .err()
                .map(|error| error.to_string()),
            Some("min_tps 50 is above the target of 10 messages per second".to_owned())
        );

        // The floor is meaningless when there is no rate to clamp.
        assert!(RateLimiter::new(ThroughputConfig::at(0).with_min_tps(50)).is_ok());
    }

    #[test]
    fn the_neutral_factor_is_the_ceiling_and_the_default() {
        assert_eq!(AdaptiveFactor::default(), AdaptiveFactor::NEUTRAL);
        assert_eq!(AdaptiveFactor::NEUTRAL.permille(), 1_000);
        assert_eq!(
            AdaptiveFactor::from_permille(u16::MAX),
            AdaptiveFactor::NEUTRAL
        );
        assert!((AdaptiveFactor::from_permille(250).as_fraction() - 0.25).abs() < f64::EPSILON);
    }

    /// The defaults spec §9.5 calls "prudent".
    #[test]
    fn the_defaults_are_the_ones_of_the_specification() {
        let config = ThroughputConfig::default();

        assert_eq!(config.target_tps, 100);
        assert_eq!(config.min_tps, 1);
        assert_eq!(config.throttle_cooldown, DEFAULT_THROTTLE_COOLDOWN);
    }

    /// Senders are admitted in the order they queued, not in whatever order
    /// they happened to wake up in.
    #[tokio::test(start_paused = true)]
    async fn concurrent_senders_are_admitted_in_the_order_they_arrived() {
        use std::sync::Arc;

        let limiter = Arc::new(RateLimiter::at(10).expect("a valid band"));
        let order = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let mut tasks = Vec::new();

        for index in 0_u32..5 {
            let limiter = Arc::clone(&limiter);
            let order = Arc::clone(&order);

            tasks.push(tokio::spawn(async move {
                limiter.acquire().await;
                order.lock().await.push(index);
            }));

            // Enough for the task to reach `acquire` and queue behind the
            // lock, and far less than the 100 ms pacing interval.
            tokio::task::yield_now().await;
        }

        for task in tasks {
            task.await.expect("the task ran");
        }

        assert_eq!(*order.lock().await, vec![0, 1, 2, 3, 4]);
    }
}
