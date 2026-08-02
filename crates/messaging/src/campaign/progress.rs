//! What a running campaign lets an observer read (part of L-010-07).
//!
//! [`super::runner`] counts what happens to every recipient in a
//! [`CampaignTally`] it owns and hands back once, at the end. That is exactly
//! what the runner needs and nothing of what a progress bar needs: a campaign
//! of half a million recipients is a future that does not resolve for an hour.
//!
//! This is the window into it. The runner publishes a [`CampaignReading`] after
//! **every** item; a reader samples the latest one whenever it wants to.
//!
//! # Why publishing on every item is not the throttle problem
//!
//! CA-010-11 asks for `campaign:progress` to be throttled, and this publishes at
//! the full message rate. The two are not in tension, because publishing here
//! costs one `watch::Sender::send_replace` — a lock, a move of a small `Copy`
//! struct, and no wake-up unless somebody is parked on it. What CA-010-11 is
//! about is the **IPC bridge**, and nothing here crosses it: the reader samples
//! at its own cadence and decides what to emit.
//!
//! That split is deliberate. A rate limit applied where the counters are
//! produced would make the figures depend on the display cadence — the mistake
//! `metrics:tick` avoided at milestone 007 for the same reason.
//!
//! # A whole reading, never a field at a time
//!
//! [`tokio::sync::watch`] over a `Copy` struct rather than a bag of atomics. The
//! difference is observable: relaxed counters can be read mid-update, so a
//! snapshot could hold an `accepted` that had been incremented beside a `failed`
//! that had not, and `CampaignTally::total()` — the figure the progress bar
//! divides by — would be off by one for a sampling period. A snapshot here is
//! always a reading the runner actually held.
//!
//! # The throughput is the **campaign's**, and it is measured here
//!
//! Spec §15.3 has `campaign:progress` carry "compteurs + débit", and the rate it
//! means is the rate of *that campaign*. It is measured by [`AcceptanceRate`],
//! out of the acceptances the runner counts, against the runner's **injected**
//! clock (CLAUDE.md §7).
//!
//! The session's own throughput — `metrics:tick`, milestone 007 — is a different
//! figure answering a different question: it counts every submission on the
//! link, so a unit send made while a campaign runs is inside it. Two numbers
//! shown side by side have to describe the same thing, and beside a campaign's
//! counters the honest one is the campaign's. `metrics:tick` keeps its place on
//! the Sessions and Dashboard screens, where it is labelled as the session's.
//!
//! Deriving the rate in the WebView from the four readings a second that reach
//! it was never on the table: the accuracy of the figure would then depend on
//! the display cadence, which is the dependency this whole module is arranged to
//! avoid.

use smpp_core::time::Timestamp;
use tokio::sync::watch;

use super::runner::CampaignTally;

/// Seconds the acceptance rate averages over.
///
/// Ten, like the `tps_10s` of `smpp_session::metrics`, and for the same reason:
/// a one-second window sampled four times a second is mostly noise, and an
/// operator watching a campaign wants to know whether it is moving rather than
/// what happened in the last 250 ms. It is also what stays readable when a
/// message centre answers in bursts.
pub const RATE_WINDOW_SECONDS: i64 = 10;

/// The same number as a length, for indexing the ring.
///
/// Written out rather than cast from [`RATE_WINDOW_SECONDS`]: the workspace
/// lints refuse both directions of that cast — truncating on a 32-bit target,
/// wrapping on a 64-bit one — and a `const` assertion below is a stronger tie
/// between the two than a cast would have been.
const RATE_WINDOW_SLOTS: usize = 10;

/// The two constants are one number. A `const` block, so they cannot drift past
/// a build.
const _: () = assert!(RATE_WINDOW_SLOTS as u64 == RATE_WINDOW_SECONDS as u64);

/// What a reader learns about a campaign that is still running.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CampaignReading {
    /// What has become of every recipient dealt with so far.
    pub tally: CampaignTally,

    /// Messages the message centre **accepted**, per second, over the last
    /// [`RATE_WINDOW_SECONDS`].
    ///
    /// Acceptances and not submissions: a message refused and replayed twice is
    /// one delivery's worth of work, and counting attempts would make a campaign
    /// look fastest exactly when the message centre is refusing everything.
    pub accepted_per_second: f64,
}

/// Acceptances per second over a sliding window.
///
/// # Pure, and answered against an injected instant
///
/// Nothing here reads a clock. [`Self::record`] and [`Self::per_second`] both
/// take the instant, and the runner passes the clock it was built with — which
/// is what makes a ten-second measurement a test that runs in microseconds
/// (CLAUDE.md §7), and what stops the figure depending on how fast the machine
/// is.
///
/// # Why each slot carries the second it stands for
///
/// A plain ring of counters has to be *cleared* as time moves, which means the
/// caller must keep recording for the clearing to happen — and a campaign whose
/// message centre goes quiet for a minute records nothing at all. Storing the
/// second beside the count makes a stale slot recognisable on read, so a silence
/// is a rate that falls to zero on its own rather than a stale figure that stays
/// high for ever.
#[derive(Debug, Clone, Copy)]
pub struct AcceptanceRate {
    /// `(the whole second this slot counts, how many it accepted)`.
    slots: [(i64, u64); RATE_WINDOW_SLOTS],
    /// The first instant the window ever saw — its divisor while it is young.
    started: Option<Timestamp>,
}

impl AcceptanceRate {
    /// An empty window.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [(i64::MIN, 0); RATE_WINDOW_SLOTS],
            started: None,
        }
    }

    /// Records `accepted` acceptances at `at`.
    pub fn record(&mut self, at: Timestamp, accepted: u64) {
        if accepted == 0 {
            return;
        }

        let started = *self.started.get_or_insert(at);
        // A negative second is a clock that stepped backwards — an NTP
        // correction mid-campaign. It is folded into the first second rather
        // than dropped: the acceptance happened, and losing it would make the
        // campaign's rate disagree with its own counters.
        let second = whole_seconds(started, at).max(0);

        // INVARIANT: `rem_euclid` by a positive constant lands in
        // `0..RATE_WINDOW_SECONDS`, which is exactly the length of `slots`, so
        // the index is in range and the narrowing cannot lose a bit.
        let slot = match usize::try_from(second.rem_euclid(RATE_WINDOW_SECONDS)) {
            Ok(index) if index < RATE_WINDOW_SLOTS => &mut self.slots[index],
            // Unreachable, and it costs nothing to say so without a panic: the
            // acceptance is folded into the first slot rather than dropped.
            _ => &mut self.slots[0],
        };

        if slot.0 == second {
            slot.1 = slot.1.saturating_add(accepted);
        } else {
            // A slot the ring reaches again a full window later is **replaced**,
            // never added to. Accumulating is the classic ring-buffer bug and it
            // reports a rate that only ever climbs.
            *slot = (second, accepted);
        }
    }

    /// The rate at `now`, in acceptances per second.
    ///
    /// Zero before anything has been accepted, and zero again once the window
    /// has emptied — a campaign whose message centre stopped answering reads as
    /// stopped, which is the whole point of a live figure.
    #[must_use]
    pub fn per_second(&self, now: Timestamp) -> f64 {
        let Some(started) = self.started else {
            return 0.0;
        };

        let current = whole_seconds(started, now).max(0);
        let oldest = current - RATE_WINDOW_SECONDS + 1;

        let counted: u64 = self
            .slots
            .iter()
            .filter(|(second, _)| *second >= oldest && *second <= current)
            .map(|(_, accepted)| *accepted)
            .sum();

        if counted == 0 {
            return 0.0;
        }

        // The divisor is how much of the window has actually happened, not its
        // nominal width — the same correction `smpp_session::metrics` makes, for
        // the same reason: dividing a young campaign's first second by ten
        // reports a tenth of its real rate, on the screen an operator watches
        // hardest.
        #[expect(
            clippy::cast_precision_loss,
            reason = "a bounded count of whole seconds, at most ten"
        )]
        let elapsed = (current + 1).min(RATE_WINDOW_SECONDS) as f64;

        #[expect(
            clippy::cast_precision_loss,
            reason = "a rate rendered to one decimal; exactness past 2^53 acceptances is not a thing"
        )]
        let counted = counted as f64;

        counted / elapsed
    }
}

impl Default for AcceptanceRate {
    fn default() -> Self {
        Self::new()
    }
}

/// Whole seconds from `from` to `to`, negative when `to` is the earlier.
fn whole_seconds(from: Timestamp, to: Timestamp) -> i64 {
    (*to.as_offset_date_time() - *from.as_offset_date_time()).whole_seconds()
}

/// The live counters of one running campaign.
///
/// Cheap to clone through an `Arc`, and deliberately **not** `Clone` itself:
/// there is one publisher, the runner, and a second one would mean two campaigns
/// writing the same readings.
#[derive(Debug)]
pub struct CampaignProgress {
    readings: watch::Sender<CampaignReading>,
}

impl CampaignProgress {
    /// A progress handle reading all zeroes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            readings: watch::channel(CampaignReading::default()).0,
        }
    }

    /// Publishes what the campaign has counted so far.
    ///
    /// Called by the runner after every item it takes off the queue. It never
    /// fails and never waits: `watch` keeps only the latest value, so a reader
    /// that is slow misses intermediate readings rather than slowing the
    /// campaign down.
    pub fn publish(&self, reading: CampaignReading) {
        self.readings.send_replace(reading);
    }

    /// The most recent reading.
    #[must_use]
    pub fn snapshot(&self) -> CampaignReading {
        *self.readings.borrow()
    }
}

impl Default for CampaignProgress {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{AcceptanceRate, CampaignProgress, CampaignReading, CampaignTally};
    use smpp_core::time::Timestamp;

    /// An instant `seconds` after the fixture's origin.
    fn at(seconds: i64) -> Timestamp {
        let origin = Timestamp::parse("2026-08-02T12:00:00Z").expect("a valid instant");

        Timestamp::from_offset_date_time(
            *origin.as_offset_date_time() + time::Duration::seconds(seconds),
        )
    }

    /// Two rates are equal to within a tenth of what the screen renders.
    fn close(left: f64, right: f64) -> bool {
        (left - right).abs() < 0.001
    }

    #[test]
    fn a_fresh_handle_reads_all_zeroes() {
        let reading = CampaignProgress::new().snapshot();

        assert_eq!(reading.tally, CampaignTally::default());
        assert!(close(reading.accepted_per_second, 0.0));
    }

    #[test]
    fn a_snapshot_is_the_last_reading_published() {
        let progress = CampaignProgress::new();

        progress.publish(CampaignReading {
            tally: CampaignTally {
                accepted: 3,
                ..CampaignTally::default()
            },
            accepted_per_second: 1.0,
        });
        progress.publish(CampaignReading {
            tally: CampaignTally {
                accepted: 7,
                failed: 1,
                ..CampaignTally::default()
            },
            accepted_per_second: 4.5,
        });

        let snapshot = progress.snapshot();

        assert_eq!(snapshot.tally.accepted, 7);
        assert_eq!(snapshot.tally.failed, 1);
        assert!(close(snapshot.accepted_per_second, 4.5));
    }

    // NO TEST for "a snapshot is never a mixture of two readings", although the
    // header makes it the reason this is a `watch` and not a bag of atomics.
    //
    // One was written and then deleted: it published three readings in sequence
    // and asserted each snapshot was one of them, which a bag of relaxed atomics
    // passes just as happily — a single-threaded publisher never tears. A test
    // that could fail would need a concurrent reader racing a concurrent writer,
    // and it would fail *sometimes*, which is the sort of test that gets
    // disabled rather than fixed (CLAUDE.md §7 asks for deterministic tests).
    //
    // The property is held by the **type** instead: `publish` takes a whole
    // `CampaignReading` and `snapshot` returns one, so there is no API through
    // which half an update could be read.

    // --- the campaign's own throughput ---------------------------------------

    #[test]
    fn a_window_that_saw_nothing_reads_zero() {
        assert!(close(AcceptanceRate::new().per_second(at(0)), 0.0));
    }

    #[test]
    fn a_steady_rate_is_reported_as_it_is() {
        let mut rate = AcceptanceRate::new();

        for second in 0..10 {
            rate.record(at(second), 10);
        }

        assert!(close(rate.per_second(at(9)), 10.0));
    }

    /// **The divisor is the window that has elapsed, not its nominal width.** A
    /// campaign one second old that accepted ten messages is running at ten a
    /// second, not at one — and one a second is what a fixed divisor of ten
    /// would put on the screen an operator watches hardest.
    #[test]
    fn a_young_campaign_is_divided_by_what_has_actually_elapsed() {
        let mut rate = AcceptanceRate::new();

        rate.record(at(0), 10);

        assert!(close(rate.per_second(at(0)), 10.0));
    }

    /// A message centre that goes quiet takes the rate down with it. A window
    /// that kept its last figure would show a campaign sending while nothing
    /// moved.
    #[test]
    fn a_silence_longer_than_the_window_brings_the_rate_back_to_zero() {
        let mut rate = AcceptanceRate::new();

        for second in 0..10 {
            rate.record(at(second), 10);
        }

        assert!(rate.per_second(at(9)) > 9.0);
        assert!(close(rate.per_second(at(30)), 0.0));
    }

    /// The window slides: what left it stops being counted.
    #[test]
    fn what_falls_out_of_the_window_stops_being_counted() {
        let mut rate = AcceptanceRate::new();

        rate.record(at(0), 100);
        rate.record(at(11), 10);

        assert!(
            close(rate.per_second(at(11)), 1.0),
            "the burst at second zero is outside the window: {}",
            rate.per_second(at(11))
        );
    }

    /// A slot the ring reaches again a full window later must **replace** what
    /// it held, never add to it — the classic ring-buffer bug, which reports a
    /// rate that only ever climbs.
    #[test]
    fn a_slot_reused_a_window_later_replaces_rather_than_accumulates() {
        let mut rate = AcceptanceRate::new();

        rate.record(at(0), 100);
        rate.record(at(10), 5);

        assert!(
            close(rate.per_second(at(10)), 0.5),
            "the reused slot kept the older burst: {}",
            rate.per_second(at(10))
        );
    }

    /// An acceptance is not lost when the wall clock steps backwards — an NTP
    /// correction mid-campaign — because the campaign's rate would then
    /// disagree with its own counters.
    #[test]
    fn a_clock_that_steps_backwards_does_not_lose_an_acceptance() {
        let mut rate = AcceptanceRate::new();

        rate.record(at(5), 10);
        rate.record(at(1), 10);

        assert!(rate.per_second(at(5)) > 0.0);
    }

    /// Nothing but an acceptance moves the rate. A campaign whose every message
    /// is being refused and replayed would otherwise look like the fastest one
    /// on the screen.
    #[test]
    fn a_recording_of_nothing_leaves_the_window_untouched() {
        let mut rate = AcceptanceRate::new();

        rate.record(at(0), 0);
        rate.record(at(1), 0);

        assert!(close(rate.per_second(at(1)), 0.0));
    }
}
