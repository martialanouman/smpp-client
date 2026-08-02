//! What a running campaign lets an observer read (part of L-010-07).
//!
//! [`super::runner`] counts what happens to every recipient in a
//! [`CampaignTally`] it owns and hands back once, at the end. That is exactly
//! what the runner needs and nothing of what a progress bar needs: a campaign
//! of half a million recipients is a future that does not resolve for an hour.
//!
//! This is the window into it. The runner publishes its tally after **every**
//! item; a reader samples the latest one whenever it wants to.
//!
//! # Why publishing on every item is not the throttle problem
//!
//! CA-010-11 asks for `campaign:progress` to be throttled, and this publishes at
//! the full message rate. The two are not in tension, because publishing here
//! costs one `watch::Sender::send_replace` — a lock, a move of a 64-byte `Copy`
//! struct, and no wake-up unless somebody is parked on it. What CA-010-11 is
//! about is the **IPC bridge**, and nothing here crosses it: the reader samples
//! at its own cadence and decides what to emit.
//!
//! That split is deliberate. A rate limit applied where the counters are
//! produced would make the figures depend on the display cadence — the mistake
//! `metrics:tick` avoided at milestone 007 for the same reason.
//!
//! # A whole tally, never a field at a time
//!
//! [`tokio::sync::watch`] over a `Copy` struct rather than eight atomics. The
//! difference is observable: eight relaxed counters can be read mid-update, so a
//! snapshot could hold an `accepted` that had been incremented beside a `failed`
//! that had not, and `CampaignTally::total()` — the figure the progress bar
//! divides by — would be off by one for a sampling period. A snapshot here is
//! always a tally the runner actually held.

use tokio::sync::watch;

use super::runner::CampaignTally;

/// The live counters of one running campaign.
///
/// Cheap to clone through an `Arc`, and deliberately **not** `Clone` itself:
/// there is one publisher, the runner, and a second one would mean two
/// campaigns writing the same readings.
#[derive(Debug)]
pub struct CampaignProgress {
    readings: watch::Sender<CampaignTally>,
}

impl CampaignProgress {
    /// A progress handle reading all zeroes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            readings: watch::channel(CampaignTally::default()).0,
        }
    }

    /// Publishes what the campaign has counted so far.
    ///
    /// Called by the runner after every item it takes off the queue. It never
    /// fails and never waits: `watch` keeps only the latest value, so a reader
    /// that is slow misses intermediate readings rather than slowing the
    /// campaign down.
    pub fn publish(&self, tally: &CampaignTally) {
        self.readings.send_replace(*tally);
    }

    /// The most recent reading.
    #[must_use]
    pub fn snapshot(&self) -> CampaignTally {
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
    use super::{CampaignProgress, CampaignTally};

    #[test]
    fn a_fresh_handle_reads_all_zeroes() {
        assert_eq!(CampaignProgress::new().snapshot(), CampaignTally::default());
    }

    #[test]
    fn a_snapshot_is_the_last_tally_published() {
        let progress = CampaignProgress::new();

        progress.publish(&CampaignTally {
            accepted: 3,
            ..CampaignTally::default()
        });
        progress.publish(&CampaignTally {
            accepted: 7,
            failed: 1,
            ..CampaignTally::default()
        });

        let snapshot = progress.snapshot();

        assert_eq!(snapshot.accepted, 7);
        assert_eq!(snapshot.failed, 1);
    }

    // NO TEST for "a snapshot is never a mixture of two readings", although the
    // header makes it the reason this is a `watch` and not eight atomics.
    //
    // One was written and then deleted: it published three tallies in sequence
    // and asserted each snapshot was one of them, which a bag of relaxed atomics
    // passes just as happily — a single-threaded publisher never tears. A test
    // that could fail would need a concurrent reader racing a concurrent writer,
    // and it would fail *sometimes*, which is the sort of test that gets
    // disabled rather than fixed (CLAUDE.md §7 asks for deterministic tests).
    //
    // The property is held by the **type** instead: `publish` takes a whole
    // `CampaignTally` and `snapshot` returns one, so there is no API through
    // which half an update could be read.
}
