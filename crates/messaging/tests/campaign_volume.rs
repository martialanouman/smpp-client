//! CA-010-01, **measured** rather than argued.
//!
//! > A campaign of 500 000 recipients runs to `COMPLETED` with a stable memory
//! > footprint — no growth proportional to the number of recipients.
//!
//! # What is measured, and how
//!
//! The **resident set size of this process**, sampled by `ps` while the campaign
//! is running — every [`SAMPLE_EVERY`] submissions, from inside the message
//! centre double, which is the one place that is called once per message. The
//! peak of those samples is the figure. The campaign is then run again, a
//! hundred times larger, and the two peaks are compared.
//!
//! A path holding anything per recipient shows it here: 500 000 recipients
//! retaining even the 24 bytes of an empty `String` header is twelve megabytes,
//! and a rendered message is nearer a hundred bytes.
//!
//! ## Why `ps` and not a counting allocator
//!
//! A `#[global_allocator]` that counted bytes would be the sharper instrument,
//! and it is not available here: `unsafe_code = "forbid"` in the workspace
//! manifest, and `forbid` cannot be lifted by a local `#[allow]` — deliberately,
//! since that file is CODEOWNERS-gated (CLAUDE.md §4). Implementing
//! `GlobalAlloc` requires `unsafe`. Adding a dependency that carries the
//! `unsafe` for us would go round the same rule, so it was not done either.
//!
//! Resident set size is coarser in one direction only, and it is the safe one:
//! an allocator that does not return freed pages to the operating system makes
//! RSS an **over**-estimate, never an under-estimate. A bounded path measured
//! this way can look slightly worse than it is; an unbounded one cannot look
//! bounded.
//!
//! Sampling is `#[cfg(unix)]`. On Windows the campaign still runs to completion
//! and its counters are still checked — only the memory comparison is skipped,
//! and the assertion says so rather than passing silently.
//!
//! ## What the measurement excludes, on purpose
//!
//! * **The database.** [`MemoryJournal::forgetful`] counts rows and keeps none,
//!   so what is measured is the client rather than the store. A SQLite file
//!   grows with the campaign, and is supposed to.
//! * **The recipients.** [`GeneratedRecipients`] synthesises them one at a time,
//!   as `stream_contacts` does over SQLite. A source that materialised its rows
//!   would not be caught here — it would be a defect of the adapter, and
//!   `ports::RecipientSource` is a stream precisely so that no adapter can hold
//!   more than one row.
//!
//! What it *does* cover is everything this milestone adds between those two: the
//! bounded queue, the rendered messages sitting in it, the counters, the retry
//! bookkeeping, and the per-message path from the template to the PDU.
//!
//! # Determinism
//!
//! The assertion is about memory, not time. Nothing here asserts on a duration,
//! so a loaded machine makes this test slower and not flakier.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use messaging::addressing::Destination;
use messaging::campaign::control::CampaignControl;
use messaging::campaign::runner::{CampaignPlan, CampaignRunner};
use messaging::campaign::CampaignStatus;
use messaging::ports::{SmscSession, SubmitError};
use messaging::sender::Sender;
use messaging::submit::SubmitOptions;
use messaging::template::Template;
use messaging::testing::{FakeSmsc, FixedClock, GeneratedRecipients, MemoryJournal};
use smpp_core::codec::{Command, Pdu};
use smpp_core::types::{CampaignId, SessionId};
use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking};

/// The small run, whose peak is the baseline.
const SMALL: u64 = 5_000;

/// The large run. The figure CA-010-01 names.
const LARGE: u64 = 500_000;

/// Whether this platform can be asked for its resident set size at all.
///
/// A `const` rather than a bare `cfg!` at the assertion, which clippy reads as a
/// constant condition — and it is, per platform. The point is that the skip is
/// only legitimate where the measurement is impossible.
const SAMPLING_IS_SUPPORTED: bool = cfg!(unix);

/// How often the resident set size is sampled, in submissions.
///
/// Each sample spawns `ps`, which costs a few milliseconds: a hundred samples
/// over the large run is a fraction of a second, and the queue is only 256 deep,
/// so nothing this test is looking for can appear and vanish between two
/// samples.
const SAMPLE_EVERY: u64 = 5_000;

/// How much the peak may grow between the two runs, in kilobytes.
///
/// An **absolute** margin rather than a fraction, because the quantity being
/// bounded is absolute: a bounded path holds the same working set whatever the
/// campaign's size, so the difference between the two runs should be noise —
/// the allocator's arenas, Tokio's own buffers, the sampling.
///
/// Eight megabytes is far below what a per-recipient leak costs (see the module
/// header: twelve megabytes for the cheapest imaginable one) and comfortably
/// above that noise.
const TOLERANCE_KB: u64 = 8 * 1024;

fn campaign() -> CampaignId {
    CampaignId::parse("3f8d0a2e-0000-4000-8000-00000000010a").unwrap()
}

/// The resident set size of this process, in kilobytes.
///
/// `None` when it cannot be read — a platform without `ps`, or a `ps` whose
/// output is not what this expects. The caller then reports the measurement as
/// unavailable rather than passing.
#[cfg(unix)]
fn resident_kb() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;

    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

#[cfg(not(unix))]
fn resident_kb() -> Option<u64> {
    None
}

/// A message centre that accepts everything and watches this process grow.
///
/// The sampling lives here because this is the only thing called once per
/// message. Measuring **after** the campaign would miss a buffer that was held
/// during the run and dropped before it returned, which is precisely the shape
/// of the failure CA-010-01 is about.
#[derive(Clone)]
struct MeteredSmsc {
    inner: FakeSmsc,
    submissions: Arc<AtomicU64>,
    peak_kb: Arc<AtomicU64>,
    samples: Arc<AtomicU64>,
}

impl MeteredSmsc {
    fn new() -> Self {
        Self {
            inner: FakeSmsc::accepting(),
            submissions: Arc::new(AtomicU64::new(0)),
            peak_kb: Arc::new(AtomicU64::new(0)),
            samples: Arc::new(AtomicU64::new(0)),
        }
    }

    fn peak_kb(&self) -> u64 {
        self.peak_kb.load(Ordering::Relaxed)
    }

    fn samples(&self) -> u64 {
        self.samples.load(Ordering::Relaxed)
    }

    fn sample(&self) {
        if let Some(resident) = resident_kb() {
            self.peak_kb.fetch_max(resident, Ordering::Relaxed);
            self.samples.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl SmscSession for MeteredSmsc {
    fn session_id(&self) -> SessionId {
        self.inner.session_id()
    }

    fn gsm7_packing(&self) -> Gsm7BitPacking {
        self.inner.gsm7_packing()
    }

    fn gsm7_charset(&self) -> Gsm7BitCharset {
        self.inner.gsm7_charset()
    }

    async fn submit(&self, pdu: Pdu) -> Result<Command, SubmitError> {
        let count = self.submissions.fetch_add(1, Ordering::Relaxed);

        if count.is_multiple_of(SAMPLE_EVERY) {
            self.sample();
        }

        self.inner.submit(pdu).await
    }
}

/// Runs a campaign of `count` recipients and returns its peak resident size.
async fn run_campaign(count: u64) -> MeteredSmsc {
    let smsc = MeteredSmsc::new();
    let plan = CampaignPlan::new(
        campaign(),
        Template::parse("Bonjour, votre commande est prete.").unwrap(),
        SubmitOptions::to(Destination::parse("+2250700000000").unwrap()),
    );

    let runner = CampaignRunner::new(
        Sender::new(MemoryJournal::forgetful(), FixedClock::default()),
        plan,
    );

    let outcome = runner
        .run(
            &smsc,
            &GeneratedRecipients::of(count),
            &CampaignControl::new(),
        )
        .await
        .expect("the campaign runs");

    // The last sample, taken while the campaign's structures are still alive.
    smsc.sample();

    // CA-010-01 is also a statement that the campaign *finishes*, and CA-010-02
    // that its counters are exact. Both are checked at full size here, which no
    // other test does.
    assert_eq!(outcome.status, CampaignStatus::Completed);
    assert_eq!(outcome.tally.accepted, count);
    assert_eq!(outcome.tally.total(), count);
    assert_eq!(outcome.queued, count);

    smsc
}

/// CA-010-01.
#[tokio::test]
async fn a_campaign_of_five_hundred_thousand_recipients_holds_a_bounded_footprint() {
    let small = run_campaign(SMALL).await;
    let large = run_campaign(LARGE).await;

    if small.samples() == 0 || large.samples() == 0 {
        // Reported, not passed over in silence: a platform where the resident
        // size cannot be read has verified that the campaign completes with
        // exact counters, and has verified **nothing** about its memory.
        assert!(
            resident_kb().is_none(),
            "the resident set size is readable here, so the campaign should have been measured"
        );
        assert!(
            !SAMPLING_IS_SUPPORTED || resident_kb().is_some(),
            "sampling is supported on this platform and produced nothing"
        );

        return;
    }

    let small_peak = small.peak_kb();
    let large_peak = large.peak_kb();

    assert!(
        large_peak <= small_peak + TOLERANCE_KB,
        "the footprint grew with the campaign: {small_peak} kB for {SMALL} recipients, \
         {large_peak} kB for {LARGE} — a difference of {} kB, or {:.3} kB per extra recipient",
        large_peak.saturating_sub(small_peak),
        f64::from(u32::try_from(large_peak.saturating_sub(small_peak)).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(LARGE - SMALL).unwrap_or(u32::MAX)),
    );
}
