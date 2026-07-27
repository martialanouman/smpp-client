//! What one session is doing right now (deliverable L-007-04).
//!
//! Spec §9.6 and §18.1: instantaneous throughput as a sliding 1 s and 10 s
//! average, average and peak throughput, window occupancy, response round-trip
//! time, reconnection count, uptime, and counters per outcome.
//!
//! # The averages are computed here, not in the interface
//!
//! The fiche is explicit about it, and the reason is worth stating: the
//! interface receives `metrics:tick` at 4 Hz *at most*, and a sliding second
//! reconstructed from four samples a second is not a sliding second — it is
//! whatever the throttling let through. Worse, its accuracy would then depend
//! on the display rate, so tightening the throttle to protect the bridge would
//! silently degrade the numbers.
//!
//! Every send and every response is recorded here, at full rate. The tick only
//! *reads*.
//!
//! # The ring
//!
//! One hundred buckets of 100 ms, covering ten seconds. A send lands in the
//! bucket its instant falls in; reading advances the ring first, clearing
//! whatever the gap skipped over, so a session that stopped sending reports
//! zero rather than the last rate it ever reached.
//!
//! Ten seconds of `u32` counters and `u64` accumulators is under three
//! kilobytes per session, fixed for the life of the session. There is no
//! growth term: CA-007-06 asks for stable memory under sustained load, and a
//! metrics collector that kept a sample per message would be the first thing
//! to break it.
//!
//! # The clock
//!
//! `tokio::time::Instant`, so `tokio::time::pause()` moves it. Every assertion
//! about a sliding average is then exact instead of "about right", which is
//! what CA-007-08 needs to be checkable at all.

use core::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Width of one bucket of the sliding window.
const BUCKET: Duration = Duration::from_millis(100);

/// Buckets kept — ten seconds' worth.
const BUCKETS: usize = 100;

/// Buckets covering one second.
const BUCKETS_PER_SECOND: usize = 10;

/// What a snapshot of a session's metrics carries (spec §18.1).
///
/// A plain value: the interface layer projects it onto its DTO, and nothing
/// here knows that an interface exists.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MetricsSnapshot {
    /// Submissions per second over the last second.
    pub tps_1s: f64,
    /// Submissions per second over the last ten seconds.
    pub tps_10s: f64,
    /// Submissions per second since the session was first bound.
    pub tps_average: f64,
    /// The highest [`Self::tps_1s`] ever observed on this session.
    pub tps_peak: f64,
    /// Slots the window has in total.
    pub window_size: u32,
    /// Slots occupied right now.
    pub window_in_use: u32,
    /// [`Self::window_in_use`] over [`Self::window_size`], in `0.0..=1.0`.
    pub window_occupancy: f64,
    /// Mean round-trip time of the responses of the last ten seconds, in
    /// milliseconds. Zero when nothing has been answered in that window.
    pub rtt_ms: f64,
    /// How many times the session has reconnected since it was created.
    pub reconnects: u32,
    /// How long the session has been bound, in seconds. Zero when it is not.
    pub uptime_s: u64,
    /// Submissions handed to the writer.
    pub submitted: u64,
    /// Submissions the message centre accepted.
    pub accepted: u64,
    /// Submissions the message centre refused.
    pub rejected: u64,
    /// Submissions that never got an answer.
    pub timed_out: u64,
    /// Responses carrying a throttling status (spec §9.4).
    pub throttled: u64,
    /// The adaptive factor in force, in per mille. 1 000 at this milestone.
    pub adaptive_permille: u16,
    /// The target the operator configured, in messages per second.
    ///
    /// Zero means unlimited. Carried in the snapshot because it is what the
    /// gauge of spec §9.6 is drawn against — a throughput reading with no
    /// scale is a number, not a gauge.
    pub target_tps: u32,
    /// Whether submissions are held back by a throttling penalty right now.
    ///
    /// The visible half of spec §9.4's immediate reaction: an operator seeing
    /// the throughput drop needs to know it is the message centre asking, not
    /// the client stalling.
    pub backing_off: bool,
}

/// The sliding ring, plus the totals.
#[derive(Debug)]
struct Inner {
    /// Submissions per bucket.
    sent: [u32; BUCKETS],
    /// Accumulated round-trip nanoseconds per bucket.
    rtt_nanos: [u64; BUCKETS],
    /// Responses counted per bucket.
    answered: [u32; BUCKETS],
    /// Absolute index of the newest bucket, counting from [`Inner::origin`].
    newest: u64,
    /// When bucket zero starts.
    origin: Instant,
    /// When the session became bound, while it is.
    bound_since: Option<Instant>,
    /// Seconds already spent bound on previous connections.
    bound_before: Duration,
    /// The highest one-second rate ever seen.
    peak: f64,
    reconnects: u32,
    submitted: u64,
    accepted: u64,
    rejected: u64,
    timed_out: u64,
    throttled: u64,
}

/// The live metrics of one session.
///
/// Shared between the send path, the supervisor and whoever reads the tick.
/// `tokio::sync::Mutex` for the reason the rest of this crate uses it: the
/// workspace `clippy.toml` refuses the `std` one outright, and every critical
/// section here is arithmetic that never awaits.
#[derive(Debug)]
pub struct SessionMetrics {
    inner: Mutex<Inner>,
}

impl SessionMetrics {
    /// A collector whose clock starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                sent: [0; BUCKETS],
                rtt_nanos: [0; BUCKETS],
                answered: [0; BUCKETS],
                newest: 0,
                origin: Instant::now(),
                bound_since: None,
                bound_before: Duration::ZERO,
                peak: 0.0,
                reconnects: 0,
                submitted: 0,
                accepted: 0,
                rejected: 0,
                timed_out: 0,
                throttled: 0,
            }),
        }
    }

    /// Records one PDU handed to the writer.
    pub async fn record_submitted(&self) {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();

        inner.advance(now);
        inner.submitted = inner.submitted.saturating_add(1);

        let slot = Inner::slot(inner.newest);
        inner.sent[slot] = inner.sent[slot].saturating_add(1);

        // The peak is taken here rather than at read time. A burst that both
        // starts and ends between two ticks is invisible to a reader sampling
        // at 4 Hz, and the peak is precisely the number that must not be.
        let rate = inner.rate_over(BUCKETS_PER_SECOND, now);

        if rate > inner.peak {
            inner.peak = rate;
        }
    }

    /// Records one response and how long it took.
    pub async fn record_response(&self, outcome: ResponseOutcome, round_trip: Duration) {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();

        inner.advance(now);

        match outcome {
            ResponseOutcome::Accepted => inner.accepted = inner.accepted.saturating_add(1),
            ResponseOutcome::Rejected => inner.rejected = inner.rejected.saturating_add(1),
            ResponseOutcome::Throttled => {
                inner.rejected = inner.rejected.saturating_add(1);
                inner.throttled = inner.throttled.saturating_add(1);
            }
            ResponseOutcome::Unanswered => {
                inner.timed_out = inner.timed_out.saturating_add(1);

                // An unanswered request has no round-trip time. Folding the
                // timeout in would make the average say "10 000 ms" for a
                // message centre that simply went quiet, which reads as
                // catastrophic latency rather than as a dead link.
                return;
            }
        }

        let slot = Inner::slot(inner.newest);
        let nanos = u64::try_from(round_trip.as_nanos()).unwrap_or(u64::MAX);

        inner.rtt_nanos[slot] = inner.rtt_nanos[slot].saturating_add(nanos);
        inner.answered[slot] = inner.answered[slot].saturating_add(1);
    }

    /// The session is bound: start counting uptime.
    pub async fn mark_bound(&self) {
        let mut inner = self.inner.lock().await;

        if inner.bound_since.is_none() {
            inner.bound_since = Some(Instant::now());
        }
    }

    /// The session lost its connection: bank the uptime and count the loss.
    ///
    /// `reconnecting` distinguishes a link that dropped — which will be
    /// reconnected, and is what spec §18.1 counts — from a deliberate unbind,
    /// which is not a reconnection.
    pub async fn mark_unbound(&self, reconnecting: bool) {
        let mut inner = self.inner.lock().await;

        if let Some(since) = inner.bound_since.take() {
            let held = Instant::now().saturating_duration_since(since);

            inner.bound_before = inner.bound_before.saturating_add(held);
        }

        if reconnecting {
            inner.reconnects = inner.reconnects.saturating_add(1);
        }
    }

    /// Everything spec §18.1 lists, read at this instant.
    ///
    /// `window` is passed in rather than held here: the window is the
    /// authority on its own occupancy, and a copy of that number kept
    /// alongside it is a copy that can disagree with it (CA-007-08).
    pub async fn snapshot(&self, window: &rate_control::SendWindow) -> MetricsSnapshot {
        self.snapshot_with(window, rate_control::AdaptiveFactor::NEUTRAL)
            .await
    }

    /// The same snapshot, carrying the adaptive factor in force.
    pub async fn snapshot_with(
        &self,
        window: &rate_control::SendWindow,
        factor: rate_control::AdaptiveFactor,
    ) -> MetricsSnapshot {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();

        inner.advance(now);

        let uptime = inner.uptime(now);
        let uptime_s = uptime.as_secs();

        MetricsSnapshot {
            tps_1s: inner.rate_over(BUCKETS_PER_SECOND, now),
            tps_10s: inner.rate_over(BUCKETS, now),
            tps_average: rate(inner.submitted, uptime),
            tps_peak: inner.peak,
            window_size: window.size(),
            window_in_use: window.in_use(),
            window_occupancy: window.occupancy(),
            rtt_ms: inner.mean_rtt_ms(),
            reconnects: inner.reconnects,
            uptime_s,
            submitted: inner.submitted,
            accepted: inner.accepted,
            rejected: inner.rejected,
            timed_out: inner.timed_out,
            throttled: inner.throttled,
            adaptive_permille: factor.permille(),
            // Filled in by whoever owns the limiter — see
            // `crate::actors::writer::SendGate::snapshot`. Neither is a
            // property of the meter, and inventing a second copy of them here
            // is exactly how a display disagrees with the thing it displays.
            target_tps: 0,
            backing_off: false,
        }
    }
}

impl Default for SessionMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// What became of one submission, as the counters group it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResponseOutcome {
    /// `ESME_ROK`.
    Accepted,
    /// Any other status.
    Rejected,
    /// A status of class [`smpp_core::status_codes::StatusClass::Throttling`].
    ///
    /// Counted as a rejection *and* under its own name: spec §9.4 acts on it,
    /// and an operator reading the screen needs to tell "the centre is full"
    /// from "the destination is invalid".
    Throttled,
    /// No response: a timeout, a lost link, a refused operation.
    Unanswered,
}

impl Inner {
    /// The ring position of an absolute bucket index.
    ///
    /// `try_from` rather than `as` throughout: `cast_possible_truncation` is
    /// denied workspace-wide, and the fallbacks are unreachable — the modulus
    /// is [`BUCKETS`], which is a hundred.
    fn slot(index: u64) -> usize {
        let modulus = u64::try_from(BUCKETS).unwrap_or(u64::MAX);

        usize::try_from(index % modulus).unwrap_or(0)
    }

    /// Moves the ring forward to `now`, clearing the buckets that were skipped.
    ///
    /// Without the clearing, a session that goes quiet for a minute and then
    /// sends one message would report the rate it had a minute ago: the stale
    /// counters are still in the ring, and `newest` has wrapped back onto them.
    fn advance(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.origin);
        let index = u64::try_from(elapsed.as_nanos() / BUCKET.as_nanos()).unwrap_or(u64::MAX);

        if index <= self.newest {
            return;
        }

        let skipped = index.saturating_sub(self.newest);
        let to_clear = skipped.min(u64::try_from(BUCKETS).unwrap_or(u64::MAX));

        for offset in 1..=to_clear {
            let slot = Self::slot(self.newest.saturating_add(offset));

            self.sent[slot] = 0;
            self.rtt_nanos[slot] = 0;
            self.answered[slot] = 0;
        }

        self.newest = index;
    }

    /// Submissions per second over the last `buckets` buckets.
    ///
    /// # The span is measured, not assumed
    ///
    /// The obvious implementation divides the count by the *nominal* width of
    /// the window — ten buckets, one second. It is wrong, and wrong by
    /// exactly one bucket: the newest bucket has only partly elapsed, so at
    /// 100 TPS the sum is 90 to 100 depending on where in the bucket the read
    /// lands, and the figure sags to 90 TPS on a session sending exactly 100.
    /// A ten per cent error is twice what CA-007-08 allows, and it is
    /// *systematic* rather than noise: the display would read low all the
    /// time.
    ///
    /// So the divisor is how much of the window has actually happened —
    /// whole buckets plus the elapsed part of the newest one — which makes
    /// the reading exact at any offset within a bucket. It also handles a
    /// young session, whose window is shorter than it is nominally wide.
    ///
    /// Floored at one bucket width, so the first hundred milliseconds of a
    /// session cannot divide by something close to zero.
    fn rate_over(&self, buckets: usize, now: Instant) -> f64 {
        let mut total = 0_u64;
        let mut counted = 0_u64;

        for offset in 0..buckets {
            let index = self
                .newest
                .saturating_sub(u64::try_from(offset).unwrap_or(u64::MAX));

            total = total.saturating_add(u64::from(self.sent[Self::slot(index)]));
            counted = counted.saturating_add(1);

            // The ring has no history before its origin.
            if index == 0 {
                break;
            }
        }

        let bucket_nanos = BUCKET.as_nanos();
        let elapsed_nanos = now.saturating_duration_since(self.origin).as_nanos();
        let partial_nanos =
            elapsed_nanos.saturating_sub(u128::from(self.newest).saturating_mul(bucket_nanos));

        let span_nanos = u128::from(counted.saturating_sub(1))
            .saturating_mul(bucket_nanos)
            .saturating_add(partial_nanos)
            .max(bucket_nanos);

        as_f64(total) / (as_f64_wide(span_nanos) / 1_000_000_000.0)
    }

    /// Mean round-trip time of the ring, in milliseconds.
    fn mean_rtt_ms(&self) -> f64 {
        let mut nanos = 0_u64;
        let mut count = 0_u64;

        for slot in 0..BUCKETS {
            nanos = nanos.saturating_add(self.rtt_nanos[slot]);
            count = count.saturating_add(u64::from(self.answered[slot]));
        }

        if count == 0 {
            return 0.0;
        }

        (as_f64(nanos) / as_f64(count)) / 1_000_000.0
    }

    /// How long the session has been bound in total.
    fn uptime(&self, now: Instant) -> Duration {
        match self.bound_since {
            Some(since) => self
                .bound_before
                .saturating_add(now.saturating_duration_since(since)),
            None => self.bound_before,
        }
    }
}

/// A counter as a float.
///
/// Exact up to 2^53, which is more submissions than a session can make in a
/// human lifetime. Written out so the lossy step has one name and one comment
/// rather than five.
#[allow(clippy::cast_precision_loss)]
fn as_f64(count: u64) -> f64 {
    count as f64
}

/// A span of nanoseconds as a float. Same reasoning as [`as_f64`].
#[allow(clippy::cast_precision_loss)]
fn as_f64_wide(nanos: u128) -> f64 {
    nanos as f64
}

/// A count over a duration, in units per second.
fn rate(count: u64, over: Duration) -> f64 {
    let seconds = over.as_secs_f64();

    if seconds <= 0.0 {
        return 0.0;
    }

    as_f64(count) / seconds
}

#[cfg(test)]
// `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
// reserves for "the binary entry point". A test harness is one.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use rate_control::SendWindow;

    fn a_window() -> SendWindow {
        SendWindow::new(10).expect("a valid size")
    }

    /// Sends `count` PDUs spaced by `every`.
    async fn send_at(metrics: &SessionMetrics, count: u32, every: Duration) {
        for _ in 0..count {
            metrics.record_submitted().await;
            tokio::time::sleep(every).await;
        }
    }

    /// CA-007-08 — the displayed rate is the real one, to within five per cent.
    #[tokio::test(start_paused = true)]
    async fn the_one_second_rate_matches_the_rate_actually_sent() {
        let metrics = SessionMetrics::new();
        metrics.mark_bound().await;

        // Two full seconds at exactly 100 TPS.
        send_at(&metrics, 200, Duration::from_millis(10)).await;

        let snapshot = metrics.snapshot(&a_window()).await;

        assert!(
            (snapshot.tps_1s - 100.0).abs() <= 5.0,
            "the sliding second says {}, not 100",
            snapshot.tps_1s
        );
        assert_eq!(snapshot.submitted, 200);
    }

    /// The ten-second window is a different figure from the one-second one, and
    /// it is the one that smooths a burst out.
    #[tokio::test(start_paused = true)]
    async fn the_ten_second_rate_smooths_what_the_one_second_rate_shows_raw() {
        let metrics = SessionMetrics::new();
        metrics.mark_bound().await;

        // One second of traffic, then four of silence: five seconds elapsed,
        // all of it still inside the ten-second ring.
        send_at(&metrics, 100, Duration::from_millis(10)).await;
        tokio::time::sleep(Duration::from_secs(4)).await;

        let snapshot = metrics.snapshot(&a_window()).await;

        assert!(
            snapshot.tps_1s < 1.0,
            "the last second was silent: {}",
            snapshot.tps_1s
        );
        assert!(
            (snapshot.tps_10s - 20.0).abs() <= 1.0,
            "100 messages over the five seconds that have elapsed is 20 TPS, not {}",
            snapshot.tps_10s
        );
    }

    /// **A session that stops sending must report zero.** The ring wraps, so a
    /// collector that did not clear the buckets it skipped would report the
    /// rate it had one ring ago — a screen showing 100 TPS on an idle session.
    #[tokio::test(start_paused = true)]
    async fn a_session_that_goes_quiet_reports_zero_rather_than_its_last_rate() {
        let metrics = SessionMetrics::new();
        metrics.mark_bound().await;

        send_at(&metrics, 100, Duration::from_millis(10)).await;
        assert!(metrics.snapshot(&a_window()).await.tps_1s > 50.0);

        // Well past a full ring, so every stale bucket is back under `newest`.
        tokio::time::sleep(Duration::from_secs(30)).await;

        let snapshot = metrics.snapshot(&a_window()).await;

        assert!(
            (snapshot.tps_1s - 0.0).abs() < f64::EPSILON,
            "an idle session reports {} TPS",
            snapshot.tps_1s
        );
        assert!((snapshot.tps_10s - 0.0).abs() < f64::EPSILON);

        // The totals are not a sliding window and must survive the silence.
        assert_eq!(snapshot.submitted, 100);
    }

    /// The peak survives the burst that produced it, which is the whole reason
    /// it is taken on the write path rather than at read time.
    #[tokio::test(start_paused = true)]
    async fn the_peak_records_a_burst_the_reader_never_sampled() {
        let metrics = SessionMetrics::new();
        metrics.mark_bound().await;

        send_at(&metrics, 200, Duration::from_millis(5)).await;
        tokio::time::sleep(Duration::from_secs(20)).await;

        let snapshot = metrics.snapshot(&a_window()).await;

        assert!(
            (snapshot.tps_peak - 200.0).abs() <= 10.0,
            "the 200 TPS burst peaked at {}",
            snapshot.tps_peak
        );
        assert!((snapshot.tps_1s - 0.0).abs() < f64::EPSILON);
    }

    /// CA-007-08 — the round-trip time reported is the latency actually seen.
    #[tokio::test(start_paused = true)]
    async fn the_round_trip_time_is_the_mean_of_what_was_recorded() {
        let metrics = SessionMetrics::new();

        for round_trip in [20, 40, 60] {
            metrics
                .record_response(ResponseOutcome::Accepted, Duration::from_millis(round_trip))
                .await;
        }

        let snapshot = metrics.snapshot(&a_window()).await;

        assert!(
            (snapshot.rtt_ms - 40.0).abs() < 0.001,
            "the mean of 20, 40 and 60 ms is 40 ms, not {}",
            snapshot.rtt_ms
        );
        assert_eq!(snapshot.accepted, 3);
    }

    /// An unanswered request contributes no round-trip time. Folding the
    /// timeout in would report the timeout as the latency.
    #[tokio::test(start_paused = true)]
    async fn a_request_that_was_never_answered_does_not_inflate_the_latency() {
        let metrics = SessionMetrics::new();

        metrics
            .record_response(ResponseOutcome::Accepted, Duration::from_millis(20))
            .await;
        metrics
            .record_response(ResponseOutcome::Unanswered, Duration::from_secs(10))
            .await;

        let snapshot = metrics.snapshot(&a_window()).await;

        assert!(
            (snapshot.rtt_ms - 20.0).abs() < 0.001,
            "the timeout leaked into the latency: {}",
            snapshot.rtt_ms
        );
        assert_eq!(snapshot.timed_out, 1);
        assert_eq!(snapshot.accepted, 1);
    }

    /// A throttling status is a rejection *and* its own counter: spec §9.4
    /// acts on the second, the operator reads both.
    #[tokio::test(start_paused = true)]
    async fn a_throttling_response_is_counted_twice_over() {
        let metrics = SessionMetrics::new();

        metrics
            .record_response(ResponseOutcome::Throttled, Duration::from_millis(5))
            .await;
        metrics
            .record_response(ResponseOutcome::Rejected, Duration::from_millis(5))
            .await;

        let snapshot = metrics.snapshot(&a_window()).await;

        assert_eq!(snapshot.rejected, 2);
        assert_eq!(snapshot.throttled, 1);
    }

    /// Occupancy is read from the window, so it cannot disagree with it.
    #[tokio::test]
    async fn the_window_occupancy_is_the_window_s_own_number() {
        let metrics = SessionMetrics::new();
        let window = a_window();

        let _first = window.acquire().await.expect("open");
        let _second = window.acquire().await.expect("open");

        let snapshot = metrics.snapshot(&window).await;

        assert_eq!(snapshot.window_size, 10);
        assert_eq!(snapshot.window_in_use, 2);
        assert!((snapshot.window_occupancy - 0.2).abs() < f64::EPSILON);
    }

    /// Uptime accumulates across connections, and a reconnection is counted
    /// once — a deliberate unbind is not one.
    #[tokio::test(start_paused = true)]
    async fn uptime_survives_a_reconnection_and_counts_it() {
        let metrics = SessionMetrics::new();

        metrics.mark_bound().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        metrics.mark_unbound(true).await;

        // Down for a while: the clock stops.
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(metrics.snapshot(&a_window()).await.uptime_s, 30);

        metrics.mark_bound().await;
        tokio::time::sleep(Duration::from_secs(10)).await;

        let snapshot = metrics.snapshot(&a_window()).await;

        assert_eq!(snapshot.uptime_s, 40);
        assert_eq!(snapshot.reconnects, 1);

        // A clean unbind is not a reconnection.
        metrics.mark_unbound(false).await;
        assert_eq!(metrics.snapshot(&a_window()).await.reconnects, 1);
    }

    /// The average is over the time the session was up, not over wall time: a
    /// session down for an hour has not been sending at zero for an hour.
    #[tokio::test(start_paused = true)]
    async fn the_average_rate_is_over_the_time_the_session_was_bound() {
        let metrics = SessionMetrics::new();

        metrics.mark_bound().await;
        send_at(&metrics, 100, Duration::from_millis(100)).await;

        let snapshot = metrics.snapshot(&a_window()).await;

        assert!(
            (snapshot.tps_average - 10.0).abs() <= 0.5,
            "100 messages over ten seconds is 10 TPS, not {}",
            snapshot.tps_average
        );
    }

    /// A collector that has never seen anything reports zeroes, not a division
    /// by zero.
    #[tokio::test]
    async fn a_fresh_collector_reports_zeroes() {
        let snapshot = SessionMetrics::new().snapshot(&a_window()).await;

        assert_eq!(
            snapshot,
            MetricsSnapshot {
                window_size: 10,
                adaptive_permille: 1_000,
                ..MetricsSnapshot::default()
            }
        );
    }
}
