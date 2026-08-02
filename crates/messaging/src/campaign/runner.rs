//! Running one campaign: feed, guard, emit, count.
//!
//! ```text
//!            ┌──────────── CampaignControl ────────────┐
//!            │  start · pause · resume · cancel        │
//!            └────────┬──────────────────────┬─────────┘
//!                     ▼                      ▼
//!   source ──►  Feeder (L-010-02)  ══►  this runner  ──► SmscSession
//!                                   bounded queue          │
//!                                                          ▼
//!                                                   MessageRepository
//! ```
//!
//! Two futures, **one task**. [`tokio::join!`] drives the feeder and the emitter
//! together rather than spawning either, and that is deliberate on three counts:
//!
//! * nothing here is an orphan task (CLAUDE.md §4) — the campaign is one future
//!   the caller owns and drops;
//! * neither half needs `'static`, so the template, the plan and the ports are
//!   borrowed instead of being wrapped in `Arc`s that exist only for `spawn`;
//! * the crate stays free of `tokio/rt`, which its manifest says it does not
//!   need because it spawns nothing.
//!
//! # Draining before joining
//!
//! The emitter reads the queue until it closes, and the queue closes when the
//! feeder finishes. A runner that awaited the feeder *before* draining would
//! deadlock the instant the queue filled up — the mirror image of the milestone
//! 009 bug the feeder's header describes. `join!` polls both, so the drain and
//! the join happen at once, and no ordering can be got wrong.
//!
//! # THE INVARIANT
//!
//! > **At most one accepted message per recipient.**
//!
//! [`super::resume`] states it in full and holds the two mechanisms. This file
//! is the only caller of them, and the rule for anything added here is: nothing
//! goes on the wire that has not passed either the write-ahead insert (which
//! fails on a conflicting key) or [`EmissionGuard::admit`]. There is exactly one
//! call to [`crate::sender::Sender::resend`] in this crate, and it sits directly
//! under an [`Admission::Resume`].
//!
//! # What is bounded, and what is not
//!
//! Everything this loop holds is a scalar or one message: the counters, one item
//! taken from the queue, one report. No collection here grows with the number of
//! recipients — which is CA-010-01, and which `tests/campaign_volume.rs`
//! measures rather than asserts by reading.
//!
//! # Sequential emission, and what that costs
//!
//! One message is submitted at a time. The send window of milestone 007 can hold
//! more, so a campaign's throughput is bounded by the round trip to the message
//! centre rather than by the configured rate — roughly `1/RTT` messages a
//! second.
//!
//! That is a stated limitation of this sub-milestone and not an oversight: what
//! spec §10.6 answers it with is `submit_multi`, up to 254 recipients in one
//! PDU, which is deliverable L-010-06 and not in this file yet. Whoever adds
//! concurrency or batching here inherits the invariant above, and the property
//! test of `tests/campaign_invariant.rs` is what will hold them to it.

use core::time::Duration;
use std::sync::Arc;

use smpp_core::time::Clock;
use smpp_core::types::CampaignId;
use smpp_core::values::{CommandStatus, Npi, Ton};
use tokio::sync::mpsc;

use crate::campaign::control::{CampaignControl, ControlHandle, Resumption};
use crate::campaign::feeder::{Fed, FeedItem, Feeder, RECIPIENT_QUEUE_CAPACITY};
use crate::campaign::progress::{AcceptanceRate, CampaignProgress, CampaignReading};
use crate::campaign::resume::{Admission, EmissionGuard, UnansweredPolicy};
use crate::campaign::schedule::Schedule;
use crate::campaign::CampaignStatus;
use crate::encoding::EncodingChoice;
use crate::error::MessagingError;
use crate::message::{MessageState, MessageStateUpdate};
use crate::ports::{MessageRepository, MessageStoreError, RecipientSource, SmscSession};
use crate::retry::{RetryDecision, RetryPolicy, SendFailure};
use crate::segmentation::SegmentationMode;
use crate::sender::{SegmentOutcome, SendReport, SendRequest, Sender};
use crate::submit::SubmitOptions;
use crate::template::{MissingVariablePolicy, Template};

/// Whether this run starts a campaign or picks one up.
///
/// The difference is **one journal read per recipient**, and that is the whole
/// reason the distinction exists rather than being inferred.
///
/// A fresh campaign has no rows, so looking every recipient up would be half a
/// million reads that all answer "no such message"; the write-ahead insert
/// already refuses a key that exists, so the conflict is the check. A resumed
/// campaign has rows for most of its recipients, and asking first is cheaper
/// than an insert that fails — and much clearer about what is happening.
///
/// Both are equally safe: the fresh path falls back on the guard the moment an
/// insert conflicts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StartMode {
    /// A campaign that has never run.
    #[default]
    Fresh,
    /// A campaign being picked up after a pause, a restart or a crash.
    Resuming,
}

/// Everything one campaign needs to run, minus the session and the recipients.
///
/// # The destination of `submit`
///
/// [`SubmitOptions`] carries the recipient, and a campaign has one per message.
/// The one held here is a **placeholder**: every field of it is used as written
/// except [`SubmitOptions::destination`], which the runner replaces with the
/// recipient the feeder resolved. A separate "options without a destination"
/// type was considered and rejected — it would be [`SubmitOptions`] with one
/// field removed, and two structures to keep in step for the sake of a
/// docstring.
#[derive(Debug, Clone)]
pub struct CampaignPlan {
    /// Which campaign this is. Written on every message row.
    pub campaign_id: CampaignId,
    /// The message template (spec §10.2).
    pub template: Template,
    /// What to do with a recipient a variable is missing for.
    pub on_missing: MissingVariablePolicy,
    /// The fields of spec §7.3, minus the recipient. See above.
    pub submit: SubmitOptions,
    /// Automatic encoding, or the one the operator forced.
    pub encoding: EncodingChoice,
    /// How the parts of a long message announce that they belong together.
    pub mode: SegmentationMode,
    /// The replay policy of spec §10.7.
    pub retry: RetryPolicy,
    /// When the campaign is allowed to send (CA-010-10).
    pub schedule: Schedule,
    /// What to do with a message the last run left in flight.
    pub unanswered: UnansweredPolicy,
    /// Whether this run starts the campaign or picks it up.
    pub start: StartMode,
    /// `dest_addr_ton` of every recipient.
    pub ton: Ton,
    /// `dest_addr_npi` of every recipient.
    pub npi: Npi,
}

impl CampaignPlan {
    /// A campaign with the safe defaults: reject a recipient whose variables are
    /// missing, three attempts, no planning, replay what the last run left in
    /// flight.
    #[must_use]
    pub fn new(campaign_id: CampaignId, template: Template, submit: SubmitOptions) -> Self {
        Self {
            campaign_id,
            template,
            on_missing: MissingVariablePolicy::Reject,
            submit,
            encoding: EncodingChoice::Automatic,
            mode: SegmentationMode::default(),
            retry: RetryPolicy::default(),
            schedule: Schedule::immediate(),
            unanswered: UnansweredPolicy::default(),
            start: StartMode::Fresh,
            ton: Ton::International,
            npi: Npi::Isdn,
        }
    }

    /// The same plan, under another missing-variable policy.
    #[must_use]
    pub fn on_missing_variable(mut self, policy: MissingVariablePolicy) -> Self {
        self.on_missing = policy;
        self
    }

    /// The same plan, under another replay policy.
    #[must_use]
    pub const fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// The same plan, under a planning.
    #[must_use]
    pub const fn scheduled(mut self, schedule: Schedule) -> Self {
        self.schedule = schedule;
        self
    }

    /// The same plan, under another arbitration for the messages a crash left in
    /// flight.
    #[must_use]
    pub const fn on_unanswered(mut self, policy: UnansweredPolicy) -> Self {
        self.unanswered = policy;
        self
    }

    /// The same plan, picking a campaign up rather than starting it.
    #[must_use]
    pub const fn resuming(mut self) -> Self {
        self.start = StartMode::Resuming;
        self
    }

    /// The same plan, announcing recipients under another TON and NPI.
    #[must_use]
    pub const fn addressed_as(mut self, ton: Ton, npi: Npi) -> Self {
        self.ton = ton;
        self.npi = npi;
        self
    }

    /// The same plan, under another encoding choice.
    #[must_use]
    pub fn with_encoding(mut self, encoding: EncodingChoice) -> Self {
        self.encoding = encoding;
        self
    }

    /// The same plan, under another concatenation mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: SegmentationMode) -> Self {
        self.mode = mode;
        self
    }
}

/// What became of every recipient of one campaign.
///
/// # The five buckets partition the recipients
///
/// Every recipient the feeder queued lands in exactly one of them, so
/// [`Self::total`] is the number of recipients the campaign covered — which is
/// what CA-010-02 checks against the journal.
///
/// The three fields **after** them are annotations rather than buckets: a
/// replayed message is counted once in [`Self::accepted`] or [`Self::failed`]
/// *and* once in [`Self::retried`]. Adding them to the total would double-count,
/// which is why [`Self::total`] does not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CampaignTally {
    /// Messages the message centre accepted.
    pub accepted: u64,
    /// Messages that ended `FAILED`.
    pub failed: u64,
    /// Recipients no message could be built for (CA-010-06).
    pub rejected: u64,
    /// Recipients that already had a message and were not sent to again.
    pub skipped: u64,
    /// Recipients queued but never emitted to, because the campaign stopped.
    pub cancelled: u64,

    /// Replays issued, across every message.
    pub retried: u64,
    /// Messages re-emitted although a previous run had already sent them.
    ///
    /// **The duplicate-risk figure.** Under
    /// [`UnansweredPolicy::Reemit`](crate::campaign::resume::UnansweredPolicy::Reemit)
    /// each of these may reach its recipient twice, and it is reported rather
    /// than buried so an operator can see how many are at stake.
    pub reemitted_unanswered: u64,
    /// Messages that were sent and whose outcome could not be written down.
    ///
    /// A resume will send them again — the row is still `QUEUED`. Nothing here
    /// can prevent that; naming it is what lets an operator see it.
    pub not_journalled: u64,
}

impl CampaignTally {
    /// Recipients the campaign covered.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.accepted + self.failed + self.rejected + self.skipped + self.cancelled
    }

    /// The durable counters of the campaign row (spec §14.2).
    #[must_use]
    pub const fn summary(&self) -> CampaignSummary {
        CampaignSummary {
            sent: self.accepted,
            failed: self.failed,
        }
    }
}

/// What of a campaign's tally is written back onto its row.
///
/// # Why this projection is stated here and not at the boundary
///
/// `campaigns.sent_count` and `campaigns.failed_count` are a **summary** of the
/// `messages` table, and which bucket feeds which column is a decision about
/// what those words mean — not a serialisation detail. Written in the IPC layer
/// it would be a business rule in a layer CLAUDE.md §3 keeps free of them, and
/// one nobody could test without a Tauri runtime.
///
/// # What is in it, and what is deliberately not
///
/// * `sent` is [`CampaignTally::accepted`] — messages the **message centre
///   took**, not messages this application attempted. A column fed from
///   attempts would count a recipient the centre refused three times as three
///   sends.
/// * `failed` is [`CampaignTally::failed`]; the recipients no message was built
///   for, the ones already sent to and the ones a cancellation dropped are not
///   failures and are not in it. They are in the campaign's live counters, which
///   is where the five buckets are shown apart.
/// * **`delivered` is absent.** `campaigns.delivered_count` exists in the schema
///   and nothing in this workspace feeds it: a delivery receipt is correlated to
///   one message (milestone 008) and nobody aggregates receipts back onto the
///   campaign. Producing a zero here would put a figure that means "not
///   measured" beside three that are exact. It belongs to the statistics of
///   milestone 014.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CampaignSummary {
    /// Messages the message centre accepted.
    pub sent: u64,
    /// Messages that ended `FAILED`.
    pub failed: u64,
}

/// How one campaign ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignOutcome {
    /// `COMPLETED`, `CANCELLED` or `FAILED` (spec §10.3).
    pub status: CampaignStatus,
    /// What became of every recipient.
    pub tally: CampaignTally,

    /// How many recipients the **feeder** handed over.
    ///
    /// Counted on the other side of the queue, by the half of the runner that
    /// reads the database, and it is the figure [`CampaignTally::total`] is
    /// checked against — `total == queued` is CA-010-02, and it is an equality
    /// between two counters that are incremented by different loops for
    /// different reasons.
    ///
    /// A tally checked against itself would be an identity rather than a test:
    /// this field replaced exactly such an alias, which a mutation showed could
    /// be deleted without turning a single assertion red.
    pub queued: u64,
}

/// Runs one campaign against one session.
///
/// One session, deliberately: spreading a campaign over several is milestone
/// 011's, and the fiche puts it out of scope here.
#[derive(Debug)]
pub struct CampaignRunner<R, C> {
    sender: Sender<R, C>,
    plan: CampaignPlan,
    progress: Option<Arc<CampaignProgress>>,
}

impl<R: MessageRepository, C: Clock> CampaignRunner<R, C> {
    /// A runner over a send orchestrator and a plan.
    #[must_use]
    pub const fn new(sender: Sender<R, C>, plan: CampaignPlan) -> Self {
        Self {
            sender,
            plan,
            progress: None,
        }
    }

    /// The same runner, publishing its counters as it goes (L-010-07).
    ///
    /// Optional because the runner does not need one: a caller that only wants
    /// the [`CampaignOutcome`] gets it either way. What a handle buys is the
    /// ability to read the counters **before** the campaign ends, which is what
    /// a progress bar over half a million recipients requires.
    #[must_use]
    pub fn reporting_to(mut self, progress: Arc<CampaignProgress>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// The plan this runner executes.
    #[must_use]
    pub const fn plan(&self) -> &CampaignPlan {
        &self.plan
    }

    /// Runs the campaign to its end, or until it is cancelled.
    ///
    /// # Errors
    ///
    /// [`MessagingError::Store`] when the journal fails in a way that is not a
    /// key conflict. That stops the campaign, and it is the only failure that
    /// does: emitting without the write-ahead row is the one thing the ordering
    /// of CLAUDE.md §4 forbids, so a campaign that cannot write cannot send.
    ///
    /// Everything else is an outcome and not an error — a refused message, a
    /// recipient the template could not be rendered for, a source that stopped —
    /// and comes back in the [`CampaignOutcome`].
    #[tracing::instrument(
        skip_all,
        fields(campaign_id = %self.plan.campaign_id, session_id = %session.session_id()),
    )]
    pub async fn run<S: SmscSession, Src: RecipientSource + ?Sized>(
        &self,
        session: &S,
        source: &Src,
        control: &CampaignControl,
    ) -> Result<CampaignOutcome, MessagingError> {
        let (queue, receiver) = mpsc::channel(RECIPIENT_QUEUE_CAPACITY);

        let feeder = Feeder::new(self.plan.campaign_id, &self.plan.template)
            .on_missing_variable(&self.plan.on_missing)
            .addressed_as(self.plan.ton, self.plan.npi);

        // BOTH HALVES AT ONCE, and never one after the other. See the module
        // header: draining and joining are the same act here, so the deadlock
        // that shape can produce is not reachable.
        let (feed, emitted) = tokio::join!(
            feeder.run(source, queue, control.handle()),
            self.emit_all(session, receiver, control.handle()),
        );

        let tally = emitted?;

        let status =
            if feed.cancelled || control.state() == crate::campaign::control::RunState::Cancelled {
                CampaignStatus::Cancelled
            } else if feed.failure.is_some() {
                CampaignStatus::Failed
            } else {
                CampaignStatus::Completed
            };

        tracing::info!(
            status = %status,
            accepted = tally.accepted,
            failed = tally.failed,
            rejected = tally.rejected,
            skipped = tally.skipped,
            cancelled = tally.cancelled,
            retried = tally.retried,
            reemitted_unanswered = tally.reemitted_unanswered,
            "the campaign ended"
        );

        if tally.reemitted_unanswered > 0 {
            tracing::warn!(
                count = tally.reemitted_unanswered,
                "messages left in flight by a previous run were sent again; \
                 their recipients may receive them twice"
            );
        }

        Ok(CampaignOutcome {
            status,
            tally,
            queued: feed.queued,
        })
    }

    /// Drains the queue, emitting what may be emitted.
    async fn emit_all<S: SmscSession>(
        &self,
        session: &S,
        mut queue: mpsc::Receiver<Fed>,
        mut control: ControlHandle,
    ) -> Result<CampaignTally, MessagingError> {
        let mut tally = CampaignTally::default();
        // The campaign's OWN throughput (spec §15.3), measured here rather than
        // read off the session: `metrics:tick` counts every submission on the
        // link, so a unit send made while a campaign runs is inside it, and a
        // rate shown beside a campaign's counters has to be that campaign's.
        //
        // It lives in the loop, not in `CampaignProgress`, and that is what
        // keeps it lock-free: one task owns it, the same one that owns the
        // tally. The clock is the sender's injected one (CLAUDE.md §7).
        let mut rate = AcceptanceRate::new();
        let mut accepted = 0;
        // Once the campaign stops, the rest of the queue is still drained — and
        // counted. A recipient the feeder queued and nobody emitted to is a
        // *cancelled* recipient, and dropping it silently would break the
        // balance CA-010-02 asks for.
        let mut stopped = false;

        while let Some(fed) = queue.recv().await {
            let outcome = self
                .step(session, fed, &mut control, &mut stopped, &mut tally)
                .await;

            // Published AFTER every item, in one place, whatever branch it
            // took. A report written at the emission site would miss the three
            // branches that never reach it — a recipient the template rejected,
            // one the cancellation dropped, one the daily window shut out — and
            // a campaign made of nothing but those would show a progress bar
            // frozen at zero while its counters climbed.
            let now = self.sender.clock().now();

            rate.record(now, tally.accepted - accepted);
            accepted = tally.accepted;

            self.report(CampaignReading {
                tally,
                accepted_per_second: rate.per_second(now),
            });

            outcome?;
        }

        Ok(tally)
    }

    /// Deals with one item off the queue.
    ///
    /// Split out of the loop above so that "count, then publish" holds for
    /// every path without a `report` call per branch — see the call site.
    async fn step<S: SmscSession>(
        &self,
        session: &S,
        fed: Fed,
        control: &mut ControlHandle,
        stopped: &mut bool,
        tally: &mut CampaignTally,
    ) -> Result<(), MessagingError> {
        let item = match fed {
            // Counted HERE rather than taken from the feeder's own summary:
            // every bucket has to be filled by the loop that reads the queue,
            // or the total would count an item the feeder prepared and a
            // cancellation dropped before it was pushed.
            Fed::Rejected(_) => {
                tally.rejected += 1;

                return Ok(());
            }
            Fed::Ready(item) => item,
        };

        if *stopped {
            tally.cancelled += 1;

            return Ok(());
        }

        if control.wait_until_running().await == Resumption::Cancelled {
            *stopped = true;
            tally.cancelled += 1;

            return Ok(());
        }

        // The daily window is checked HERE rather than once at the start: a
        // window that closes mid-campaign has to stop the sending, and a
        // campaign started at 19:59 would otherwise run all night.
        if !self.await_schedule(control).await {
            *stopped = true;
            tally.cancelled += 1;

            return Ok(());
        }

        self.emit(session, &item, control, tally).await
    }

    /// Offers the reading to whoever is watching, if anybody is.
    fn report(&self, reading: CampaignReading) {
        if let Some(progress) = self.progress.as_ref() {
            progress.publish(reading);
        }
    }

    /// Waits until the planning allows sending. `false` when cancelled.
    async fn await_schedule(&self, control: &ControlHandle) -> bool {
        loop {
            let Some(wait) = self.plan.schedule.wait_for(self.sender.clock().now()) else {
                return true;
            };

            tracing::debug!(
                seconds = wait.as_secs(),
                "the campaign waits for its window"
            );

            if !sleep_unless_cancelled(wait, control).await {
                return false;
            }
        }
    }

    /// Sends one message, replaying it as the policy allows.
    ///
    /// # The invariant, at the only place it can be broken
    ///
    /// Nothing reaches the wire here except through one of two doors:
    ///
    /// * [`Sender::send`], which inserts the write-ahead row first and fails
    ///   with [`MessageStoreError::Conflict`] if the recipient already has one;
    /// * [`Sender::resend`], which is reached **only** under an
    ///   [`Admission::Resume`] — that is, only after the journal said the
    ///   existing row has not been accepted.
    async fn emit<S: SmscSession>(
        &self,
        session: &S,
        item: &FeedItem,
        control: &ControlHandle,
        tally: &mut CampaignTally,
    ) -> Result<(), MessagingError> {
        let guard = EmissionGuard::new(self.sender.repository(), self.plan.unanswered);
        let request = self.request_for(item);

        // A resumed campaign asks first; a fresh one lets the insert answer.
        // See `StartMode`.
        let mut admission = if self.plan.start == StartMode::Resuming {
            guard.admit(item.client_message_id).await?
        } else {
            Admission::Fresh
        };

        if let Admission::Fresh = admission {
            match self.sender.send(session, &request).await {
                Ok(report) => {
                    return self
                        .follow_up(session, &request, report, control, tally)
                        .await
                }
                Err(MessagingError::Store(MessageStoreError::Conflict)) => {
                    // The recipient already has a row: a resume the plan did not
                    // announce, or the same number twice in the source. Nothing
                    // was submitted, so the guard decides from here.
                    admission = guard.admit(item.client_message_id).await?;
                }
                Err(error) => return self.absorb(error, tally),
            }
        }

        match admission {
            Admission::Skip(reason) => {
                tracing::debug!(reason = ?reason, "the recipient already has a message");
                tally.skipped += 1;

                Ok(())
            }
            Admission::Resume {
                attempts_made,
                was_unanswered,
            } => {
                if was_unanswered {
                    tally.reemitted_unanswered += 1;
                }

                let attempt = attempts_made.saturating_add(1);
                let request = self.numbered(&request, attempt);

                match self.sender.resend(session, &request).await {
                    Ok(report) => {
                        self.follow_up(session, &request, report, control, tally)
                            .await
                    }
                    Err(error) => self.absorb(error, tally),
                }
            }
            // Reachable, and it took a review to see it: the insert conflicted
            // — so a row exists — and the read that followed found none. With
            // SQLite that is two runs of the same campaign racing, one deleting
            // or rolling back between the other's insert and read.
            //
            // Counted as a skip, which is what it is: another writer holds this
            // recipient. Not silently dropped, which is what the previous
            // `Ok(())` did — the message fell out of every bucket and the
            // `total == queued` balance of CA-010-02 broke with nothing to show
            // for it.
            Admission::Fresh => {
                tracing::warn!(
                    "the write-ahead key conflicted and its row could not be read back; \
                     another run of this campaign is writing to the same journal"
                );
                debug_assert!(false, "a conflicting key with no row behind it");

                tally.skipped += 1;

                Ok(())
            }
        }
    }

    /// Counts one report, and replays the message if the policy says so.
    async fn follow_up<S: SmscSession>(
        &self,
        session: &S,
        request: &SendRequest,
        first: SendReport,
        control: &ControlHandle,
        tally: &mut CampaignTally,
    ) -> Result<(), MessagingError> {
        let mut report = first;
        let mut attempts_made = request.attempt;

        loop {
            if !report.journalled {
                tally.not_journalled += 1;
            }

            if report.is_accepted() {
                tally.accepted += 1;

                return Ok(());
            }

            let failure = failure_of(&report);

            match self.plan.retry.decide(&failure, attempts_made) {
                RetryDecision::GiveUp(reason) => {
                    tracing::debug!(reason = ?reason, "the message is not replayed");
                    tally.failed += 1;

                    return Ok(());
                }
                RetryDecision::RetryAfter { attempt, delay } => {
                    // The wait is the campaign's, not the message's: the codes
                    // that are replayed — `ESME_RTHROTTLED`, `ESME_RMSGQFUL`, a
                    // response that never came — are conditions of the link, and
                    // pressing on with the next recipient while one is waiting
                    // would answer "slow down" with more traffic.
                    if !sleep_unless_cancelled(delay, control).await {
                        // CA-010-09: nothing is left undecided. The campaign is
                        // over, so this message will never be retried — its
                        // last attempt is its verdict, and a row left `SENT`
                        // would stay non-terminal for ever.
                        //
                        // Counted as `failed` and not as `cancelled`: the
                        // recipient WAS emitted to and the message centre
                        // refused. `cancelled` is for the recipients nothing
                        // was ever sent to.
                        self.give_up(&report, tally).await;

                        return Ok(());
                    }

                    tally.retried += 1;
                    attempts_made = attempt;

                    let retried = self.numbered(request, attempt);

                    match self.sender.resend(session, &retried).await {
                        Ok(next) => report = next,
                        Err(error) => return self.absorb(error, tally),
                    }
                }
            }
        }
    }

    /// Writes the verdict of a message the campaign will not retry.
    ///
    /// Reached only on a cancellation, which is why a journal failure here is
    /// logged rather than propagated: the campaign is already stopping, and
    /// turning a cancellation into an error would hide the outcome the caller
    /// asked for. The row then stays `SENT` and a resume — of a *different*
    /// campaign run, since a cancelled one is terminal — would read it under
    /// the arbitration, which is the safe direction.
    async fn give_up(&self, report: &SendReport, tally: &mut CampaignTally) {
        let mut verdict = MessageStateUpdate::new(report.client_message_id, MessageState::Failed)
            .responded_at(self.sender.clock().now());

        if let Some(status) = report.command_status {
            verdict = verdict.with_command_status(status);
        }

        if let Err(error) = self.sender.repository().update_state(&verdict).await {
            tracing::error!(
                error = ?error,
                "the cancelled message could not be given its verdict"
            );

            tally.not_journalled += 1;
        }

        tally.failed += 1;
    }

    /// The write-ahead request for one queued item.
    fn request_for(&self, item: &FeedItem) -> SendRequest {
        let mut submit = self.plan.submit.clone();

        submit.destination = item.destination.clone();

        let request = SendRequest::new(item.text.clone(), submit)
            .keyed(item.client_message_id)
            .in_campaign(self.plan.campaign_id)
            .with_encoding(self.plan.encoding)
            .with_mode(self.plan.mode);

        self.numbered(&request, 1)
    }

    /// The same request, as attempt `attempt`, declaring whether the campaign
    /// still has attempts left.
    ///
    /// The second half is what keeps the journal and the counters in step: a
    /// failure the runner may replay is written `SENT`, and only the attempt the
    /// runner will not replay writes `FAILED`. See
    /// [`SendRequest::last_attempt`].
    fn numbered(&self, request: &SendRequest, attempt: u32) -> SendRequest {
        request
            .clone()
            .as_attempt(attempt)
            .with_more_attempts_allowed(attempt < self.plan.retry.max_attempts())
    }

    /// What to do with a failure the send path raised.
    ///
    /// EXHAUSTIVE ON PURPOSE, with no `_` arm: a variant added to
    /// [`MessagingError`] has to be classified here rather than falling into
    /// whichever branch a wildcard pointed at. The two classes are not
    /// interchangeable — one loses a recipient, the other stops a campaign of
    /// half a million.
    fn absorb(
        &self,
        error: MessagingError,
        tally: &mut CampaignTally,
    ) -> Result<(), MessagingError> {
        match error {
            // The journal is the campaign's floor. Without it there is no
            // write-ahead, so there is no sending.
            MessagingError::Store(failure) => Err(MessagingError::Store(failure)),

            // Everything else is about THIS message: a text the chosen encoding
            // cannot write, a field that does not fit its PDU, an address the
            // builder refused. The recipient is lost, the campaign is not.
            error @ (MessagingError::Encoding(_)
            | MessagingError::Address(_)
            | MessagingError::Submit(_)
            | MessagingError::Template(_)
            | MessagingError::Render(_)
            | MessagingError::RetryPolicy(_)
            | MessagingError::NotImplemented) => {
                tracing::warn!(error = %error, "the message could not be built");
                tally.failed += 1;

                Ok(())
            }
        }
    }
}

/// Sleeps, unless the campaign is cancelled first. `false` when it was.
///
/// The only `sleep` on the campaign path, and it is never bare: a campaign
/// deferred to tomorrow morning, or waiting out an hour-long retry delay, has to
/// stop when it is told to (CA-010-09).
async fn sleep_unless_cancelled(wait: Duration, control: &ControlHandle) -> bool {
    tokio::select! {
        biased;

        () = control.cancelled() => false,
        () = tokio::time::sleep(wait) => true,
    }
}

/// The failure one report describes, in the vocabulary the replay policy reads.
fn failure_of(report: &SendReport) -> SendFailure {
    match report.command_status {
        Some(status) if status != CommandStatus::EsmeRok => SendFailure::Rejected(status),
        _ => report
            .outcomes
            .iter()
            .find_map(|outcome| match outcome {
                SegmentOutcome::Unanswered { failure } => {
                    Some(SendFailure::NoResponse(failure.clone()))
                }
                SegmentOutcome::Answered { .. } | SegmentOutcome::NotAttempted => None,
            })
            // A report that is neither accepted, nor refused, nor unanswered
            // does not exist; if one ever did, "nothing to replay" is the answer
            // that cannot loop.
            .unwrap_or(SendFailure::Rejected(CommandStatus::EsmeRok)),
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::{CampaignPlan, CampaignRunner, CampaignTally, StartMode};
    use crate::addressing::Destination;
    use crate::campaign::control::CampaignControl;
    use crate::campaign::progress::CampaignProgress;
    use crate::campaign::resume::{message_key, UnansweredPolicy};
    use crate::campaign::schedule::{DailyWindow, Schedule};
    use crate::campaign::CampaignStatus;
    use crate::message::MessageState;
    use crate::ports::{MessageStoreError, Recipient};
    use crate::retry::{RetryBackoff, RetryPolicy};
    use crate::sender::{SendRequest, Sender};
    use crate::submit::SubmitOptions;
    use crate::template::{MissingVariablePolicy, Template};
    use crate::testing::{
        journal_row, FakeSmsc, FixedClock, MemoryJournal, Reply, StaticRecipients, VirtualClock,
    };
    use core::time::Duration;
    use smpp_core::time::Timestamp;
    use smpp_core::types::{CampaignId, Msisdn};
    use smpp_core::values::CommandStatus;
    use std::collections::HashSet;
    use std::sync::Arc;
    use time::{Time, UtcOffset};

    const SETTLE: Duration = Duration::from_millis(50);

    fn campaign() -> CampaignId {
        CampaignId::parse("3f8d0a2e-0000-4000-8000-000000000001").expect("a valid UUID")
    }

    fn number(index: u32) -> Msisdn {
        Msisdn::parse(&format!("+225070000000{index}")).expect("a valid number")
    }

    fn plan(source_text: &str) -> CampaignPlan {
        CampaignPlan::new(
            campaign(),
            Template::parse(source_text).expect("the fixture template parses"),
            SubmitOptions::to(Destination::parse("+2250700000000").expect("a valid number")),
        )
    }

    fn recipients(count: u32) -> StaticRecipients {
        StaticRecipients::new(
            (1..=count)
                .map(|index| Recipient {
                    destination: number(index),
                    attributes: None,
                })
                .collect(),
        )
    }

    // --- the nominal path ---------------------------------------------------

    #[tokio::test]
    async fn every_recipient_receives_exactly_one_message() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().recording();
        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        let outcome = runner
            .run(&smsc, &recipients(5), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(outcome.status, CampaignStatus::Completed);
        assert_eq!(outcome.tally.accepted, 5);
        assert_eq!(outcome.tally.total(), 5);
        assert_eq!(outcome.tally.total(), outcome.queued);
        assert_eq!(smsc.submitted(), 5);
        assert_eq!(journal.rows().await.len(), 5);

        let distinct: HashSet<String> = smsc.destinations().await.into_iter().collect();
        assert_eq!(distinct.len(), 5, "one emission per recipient");
    }

    /// CA-010-02, checked against the journal rather than against the runner's
    /// own arithmetic.
    #[tokio::test]
    async fn the_counters_balance_and_agree_with_the_journal() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::scripted([Reply::Rejected(CommandStatus::EsmeRinvdstadr)]);
        let source = StaticRecipients::new(vec![
            Recipient {
                destination: number(1),
                attributes: None,
            },
            Recipient {
                destination: number(2),
                attributes: Some(String::from("{}")),
            },
            Recipient {
                destination: number(3),
                attributes: None,
            },
        ]);

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour {{prenom}}")
                .on_missing_variable(MissingVariablePolicy::Substitute(String::from("client"))),
        );

        // The second recipient has an empty attribute set, so under a
        // substitution policy nothing is rejected: three messages, one of which
        // the message centre refuses.
        let outcome = runner
            .run(&smsc, &source, &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(outcome.tally.total(), 3);
        assert_eq!(outcome.tally.total(), outcome.queued);
        assert_eq!(outcome.tally.accepted, 2);
        assert_eq!(outcome.tally.failed, 1);

        let rows = journal.rows().await;

        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter()
                .filter(|row| row.state == MessageState::Accepted)
                .count(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.state == MessageState::Failed)
                .count(),
            1
        );
    }

    /// A recipient the template cannot be rendered for is counted and never
    /// emitted to (CA-010-06).
    #[tokio::test]
    async fn a_rejected_recipient_is_counted_and_never_sent_to() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();
        let source = StaticRecipients::new(vec![
            Recipient {
                destination: number(1),
                attributes: Some(String::from(r#"{"prenom":"Awa"}"#)),
            },
            Recipient {
                destination: number(2),
                attributes: Some(String::from("{}")),
            },
        ]);

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour {{prenom}}"),
        );

        let outcome = runner
            .run(&smsc, &source, &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(outcome.tally.rejected, 1);
        assert_eq!(outcome.tally.accepted, 1);
        assert_eq!(outcome.tally.total(), 2);
        assert_eq!(outcome.tally.total(), outcome.queued);
        assert_eq!(smsc.submitted(), 1);
        assert_eq!(journal.rows().await.len(), 1);
    }

    /// The same number twice in one source is one recipient: the derived key
    /// conflicts, and the guard reads the row it finds.
    #[tokio::test]
    async fn the_same_recipient_twice_in_the_source_receives_one_message() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();
        let source = StaticRecipients::numbers(&["+2250700000001", "+2250700000001"]);

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        let outcome = runner
            .run(&smsc, &source, &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(smsc.submitted(), 1, "the second occurrence is not emitted");
        assert_eq!(outcome.tally.accepted, 1);
        assert_eq!(outcome.tally.skipped, 1);
        assert_eq!(outcome.tally.total(), 2);
        assert_eq!(journal.rows().await.len(), 1);
    }

    // --- resume (CA-010-04, CA-010-05) --------------------------------------

    /// CA-010-05: the criterion, end to end.
    #[tokio::test]
    async fn a_message_already_accepted_is_never_emitted_again() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().recording();

        journal
            .force_row(journal_row(
                message_key(campaign(), &number(2)),
                MessageState::Accepted,
            ))
            .await;

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").resuming(),
        );

        let outcome = runner
            .run(&smsc, &recipients(3), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        let sent = smsc.destinations().await;

        assert_eq!(sent.len(), 2);
        assert!(!sent.contains(&number(2).as_str().to_owned()));
        assert_eq!(outcome.tally.skipped, 1);
        assert_eq!(outcome.tally.accepted, 2);
        assert_eq!(outcome.tally.total(), 3);
    }

    /// CA-010-04: a `QUEUED` row is what a `kill -9` between the insert and the
    /// socket leaves. It is sent, and it does **not** produce a second
    /// `client_message_id`.
    #[tokio::test]
    async fn a_row_left_queued_by_a_crash_is_sent_without_a_second_key() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().recording();
        let key = message_key(campaign(), &number(1));

        journal
            .force_row(journal_row(key, MessageState::Queued))
            .await;

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").resuming(),
        );

        let outcome = runner
            .run(&smsc, &recipients(2), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(smsc.submitted(), 2);
        assert_eq!(outcome.tally.accepted, 2);
        assert_eq!(
            journal.inserted().await,
            1,
            "only the recipient with no row is written"
        );
        assert_eq!(journal.rows().await.len(), 2);
        assert_eq!(
            journal
                .row(key)
                .await
                .expect("the resumed row is there")
                .state,
            MessageState::Accepted
        );
    }

    /// The arbitration of fiche §6, at the level that acts on it.
    #[tokio::test]
    async fn an_unanswered_row_is_reemitted_by_default() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();

        journal
            .force_row(journal_row(
                message_key(campaign(), &number(1)),
                MessageState::Sent,
            ))
            .await;

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").resuming(),
        );

        let outcome = runner
            .run(&smsc, &recipients(1), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(smsc.submitted(), 1);
        assert_eq!(outcome.tally.accepted, 1);
        assert_eq!(
            outcome.tally.reemitted_unanswered, 1,
            "the duplicate risk is counted and reported, not hidden"
        );
    }

    #[tokio::test]
    async fn an_unanswered_row_may_be_abandoned_instead() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();

        journal
            .force_row(journal_row(
                message_key(campaign(), &number(1)),
                MessageState::Sent,
            ))
            .await;

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour")
                .resuming()
                .on_unanswered(UnansweredPolicy::Abandon),
        );

        let outcome = runner
            .run(&smsc, &recipients(1), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(smsc.submitted(), 0);
        assert_eq!(outcome.tally.skipped, 1);
        assert_eq!(outcome.tally.reemitted_unanswered, 0);
    }

    /// **The window ADR 0014 is about, exercised end to end.**
    ///
    /// A `kill -9` between the `submit_sm` leaving and the outcome being
    /// committed: the message centre took five messages and the journal recorded
    /// none of their verdicts. What the resume must find is five rows saying *an
    /// emission was attempted* — not five rows saying *nothing has left*, which
    /// is what a journal written only after the answer would leave behind.
    ///
    /// Written first, and it failed: the rows came back `QUEUED`, so both
    /// policies re-sent all five and `reemitted_unanswered` reported zero
    /// duplicates at the exact moment five were going out.
    #[tokio::test]
    async fn a_crash_between_the_emission_and_the_verdict_leaves_the_attempt_recorded() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();

        journal.lose_verdicts(true).await;

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        runner
            .run(&smsc, &recipients(5), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(smsc.submitted(), 5);
        assert_eq!(journal.lost_verdicts().await, 5);

        for row in journal.rows().await {
            assert_eq!(
                row.state,
                MessageState::Sent,
                "a message whose submit_sm left must not look untouched"
            );
            assert!(row.sent_at.is_some());
            assert_eq!(
                row.command_status, None,
                "no answer was journalled, and that is what makes it uncertain"
            );
        }
    }

    /// CA-010-05 under the policy that exists to protect against duplicates.
    ///
    /// An operator who chose `Abandon` said, in so many words, "I would rather
    /// under-deliver than send twice". If the crash window leaves rows that read
    /// as untouched, that choice buys nothing: every one of them is sent again.
    #[tokio::test]
    async fn a_campaign_resumed_under_abandon_sends_nothing_the_smsc_may_have_taken() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().recording();

        journal.lose_verdicts(true).await;

        CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        )
        .run(&smsc, &recipients(5), &CampaignControl::new())
        .await
        .expect("the first run");

        journal.lose_verdicts(false).await;

        let outcome = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour")
                .resuming()
                .on_unanswered(UnansweredPolicy::Abandon),
        )
        .run(&smsc, &recipients(5), &CampaignControl::new())
        .await
        .expect("the resumed run");

        assert_eq!(
            smsc.submitted(),
            5,
            "the resume sent again what the message centre may already have taken"
        );
        assert_eq!(outcome.tally.skipped, 5);

        let sent = smsc.accepted_destinations().await;
        let distinct: HashSet<String> = sent.iter().cloned().collect();

        assert_eq!(sent.len(), distinct.len());
    }

    /// The default policy re-sends them — the arbitration of ADR 0014 — and the
    /// point of this test is that the risk is **counted**. A figure that reads
    /// zero while five duplicates go out is worse than no figure at all.
    #[tokio::test]
    async fn a_campaign_resumed_under_reemit_counts_every_message_it_may_duplicate() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();

        journal.lose_verdicts(true).await;

        CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        )
        .run(&smsc, &recipients(5), &CampaignControl::new())
        .await
        .expect("the first run");

        journal.lose_verdicts(false).await;

        let outcome = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").resuming(),
        )
        .run(&smsc, &recipients(5), &CampaignControl::new())
        .await
        .expect("the resumed run");

        assert_eq!(smsc.submitted(), 10);
        assert_eq!(
            outcome.tally.reemitted_unanswered, 5,
            "every message that may reach its recipient twice is reported"
        );
        assert_eq!(outcome.tally.accepted, 5);
    }

    // --- pause and cancel (CA-010-03, CA-010-09) ----------------------------

    /// CA-010-03: pausing stops the emission, and resuming sends the rest —
    /// once each.
    #[tokio::test(start_paused = true)]
    async fn pausing_stops_the_emission_and_resuming_finishes_without_a_duplicate() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().recording().gated(2);
        let control = CampaignControl::new();
        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        let source = recipients(6);
        let (outcome, during_pause) = tokio::join!(runner.run(&smsc, &source, &control), {
            let smsc = smsc.clone();
            let control = &control;

            async move {
                tokio::time::sleep(SETTLE).await;
                control.pause();

                // Everything the gate allows is already through; the pause is
                // what stops the rest.
                smsc.release(10);
                tokio::time::sleep(SETTLE).await;

                let during_pause = smsc.submitted();

                control.resume();
                during_pause
            }
        });

        let outcome = outcome.expect("the campaign runs");

        // Three, not two, and that is the criterion rather than a tolerance:
        // spec §10.3 says the pause suspends the *feeding* and lets "the
        // messages already in the window finish normally". Two had been
        // answered when the pause landed and a third was on the wire; it
        // completes and is journalled. The fourth is where the pause bites.
        assert_eq!(
            during_pause, 3,
            "a paused campaign finishes what is in flight and emits nothing new"
        );
        assert_eq!(outcome.status, CampaignStatus::Completed);
        assert_eq!(outcome.tally.accepted, 6);

        let distinct: HashSet<String> = smsc.destinations().await.into_iter().collect();
        assert_eq!(distinct.len(), 6, "no recipient was sent to twice");
    }

    /// CA-010-09: cancelling stops the emission at once, the campaign ends
    /// `CANCELLED`, and **no row is left in an indeterminate state** — every row
    /// the journal holds carries a final verdict.
    #[tokio::test(start_paused = true)]
    async fn cancelling_stops_the_emission_and_leaves_no_row_undecided() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().gated(2);
        let control = CampaignControl::new();
        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        let source = recipients(20);
        let (outcome, ()) = tokio::join!(runner.run(&smsc, &source, &control), {
            let smsc = smsc.clone();
            let control = &control;

            async move {
                tokio::time::sleep(SETTLE).await;
                control.cancel();
                // Let the in-flight submission finish: the message already on
                // the wire is journalled rather than abandoned.
                smsc.release(10);
            }
        });

        let outcome = outcome.expect("the campaign runs");

        assert_eq!(outcome.status, CampaignStatus::Cancelled);
        assert!(smsc.submitted() < 20);
        assert!(outcome.tally.cancelled > 0);
        // CA-010-02 on the path where it is hardest: every recipient the feeder
        // handed over is in exactly one bucket, cancellation included.
        assert_eq!(outcome.tally.total(), outcome.queued);

        for row in journal.rows().await {
            assert!(
                matches!(row.state, MessageState::Accepted | MessageState::Failed),
                "a cancelled campaign left a row in {}",
                row.state
            );
        }
    }

    // --- replay (CA-010-07) -------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_throttled_message_is_replayed_after_the_delay() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::scripted([Reply::Rejected(CommandStatus::EsmeRthrottled)]);
        let key = message_key(campaign(), &number(1));

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        let outcome = runner
            .run(&smsc, &recipients(1), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(smsc.submitted(), 2, "one refusal, one replay");
        assert_eq!(outcome.tally.accepted, 1);
        assert_eq!(outcome.tally.retried, 1);
        assert_eq!(
            outcome.tally.total(),
            1,
            "a replay is not a second recipient"
        );

        let row = journal.row(key).await.expect("the row is there");

        assert_eq!(row.state, MessageState::Accepted);
        assert_eq!(row.attempts, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_invalid_destination_is_never_replayed() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::scripted([Reply::Rejected(CommandStatus::EsmeRinvdstadr)]);

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        let outcome = runner
            .run(&smsc, &recipients(1), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(smsc.submitted(), 1);
        assert_eq!(outcome.tally.failed, 1);
        assert_eq!(outcome.tally.retried, 0);
    }

    /// A replay budget that runs out ends the message, not the campaign.
    #[tokio::test(start_paused = true)]
    async fn a_message_that_exhausts_its_budget_fails_and_the_campaign_carries_on() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting()
            .then(Reply::Rejected(CommandStatus::EsmeRthrottled))
            .recording();

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").with_retry(
                RetryPolicy::new(
                    2,
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    RetryBackoff::Fixed,
                )
                .expect("the bounds are valid"),
            ),
        );

        let outcome = runner
            .run(&smsc, &recipients(2), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(smsc.submitted(), 4, "two attempts for each of two messages");
        assert_eq!(outcome.tally.failed, 2);
        assert_eq!(outcome.tally.total(), 2);
    }

    /// CA-010-09 again, on the path that would otherwise hold the campaign the
    /// longest: a retry delay is a `sleep` the cancellation has to cut short.
    #[tokio::test(start_paused = true)]
    async fn cancelling_interrupts_a_retry_delay() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().then(Reply::Rejected(CommandStatus::EsmeRthrottled));
        let control = CampaignControl::new();

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").with_retry(
                RetryPolicy::new(
                    5,
                    Duration::from_secs(3_600),
                    Duration::from_secs(3_600),
                    RetryBackoff::Fixed,
                )
                .expect("the bounds are valid"),
            ),
        );

        let started = tokio::time::Instant::now();
        let source = recipients(1);

        let (outcome, ()) = tokio::join!(runner.run(&smsc, &source, &control), {
            let control = &control;

            async move {
                tokio::time::sleep(SETTLE).await;
                control.cancel();
            }
        });

        let outcome = outcome.expect("the campaign runs");

        // CA-010-09 names one second, and `retry.rs` claims it in prose. This
        // is the assertion that holds the claim.
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancelling took {:?}, and CA-010-09 allows one second",
            started.elapsed()
        );
        assert_eq!(outcome.status, CampaignStatus::Cancelled);
        assert_eq!(smsc.submitted(), 1);
    }

    /// CA-010-09: "no message is left in an undetermined state".
    ///
    /// A message waiting out a replay delay when the campaign is cancelled will
    /// never be retried — the campaign is over. Its last attempt is therefore
    /// its verdict, and leaving the row `SENT` would leave it non-terminal for
    /// ever, with a screen showing a message that is neither sent nor failed.
    #[tokio::test(start_paused = true)]
    async fn a_message_whose_replay_was_cancelled_is_still_given_a_verdict() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().then(Reply::Rejected(CommandStatus::EsmeRthrottled));
        let control = CampaignControl::new();
        let key = message_key(campaign(), &number(1));

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").with_retry(
                RetryPolicy::new(
                    5,
                    Duration::from_secs(600),
                    Duration::from_secs(600),
                    RetryBackoff::Fixed,
                )
                .expect("the bounds are valid"),
            ),
        );

        let source = recipients(1);
        let (outcome, ()) = tokio::join!(runner.run(&smsc, &source, &control), {
            let control = &control;

            async move {
                tokio::time::sleep(SETTLE).await;
                control.cancel();
            }
        });

        let outcome = outcome.expect("the campaign runs");
        let row = journal.row(key).await.expect("the row is there");

        assert_eq!(row.state, MessageState::Failed, "left undecided");
        assert!(row.state.is_terminal());
        assert_eq!(row.command_status, Some(CommandStatus::EsmeRthrottled));
        assert_eq!(outcome.tally.failed, 1);
        assert_eq!(outcome.tally.total(), outcome.queued);
    }

    /// The crash counterpart, which no cancellation can tidy up: a process that
    /// dies during a replay delay leaves a `SENT` row **carrying the refusal**.
    /// The message centre answered it, so nothing was accepted and re-emitting
    /// cannot duplicate — both policies send it, including the one whose whole
    /// purpose is to avoid duplicates.
    #[tokio::test]
    async fn a_refused_message_left_by_a_crash_is_resumed_under_both_policies() {
        for policy in [UnansweredPolicy::Reemit, UnansweredPolicy::Abandon] {
            let journal = MemoryJournal::new();
            let smsc = FakeSmsc::accepting();
            let key = message_key(campaign(), &number(1));

            let mut refused = journal_row(key, MessageState::Sent);
            refused.command_status = Some(CommandStatus::EsmeRthrottled);
            refused.attempts = 1;

            journal.force_row(refused).await;

            let outcome = CampaignRunner::new(
                Sender::new(journal.clone(), FixedClock::default()),
                plan("Bonjour").resuming().on_unanswered(policy),
            )
            .run(&smsc, &recipients(1), &CampaignControl::new())
            .await
            .expect("the campaign runs");

            assert_eq!(smsc.submitted(), 1, "under {policy:?}");
            assert_eq!(outcome.tally.accepted, 1, "under {policy:?}");
            assert_eq!(
                outcome.tally.reemitted_unanswered, 0,
                "a refusal is not a duplicate risk, under {policy:?}"
            );
            assert_eq!(
                journal.row(key).await.expect("the row").state,
                MessageState::Accepted
            );
        }
    }

    // --- planning (CA-010-10) ------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_deferred_campaign_sends_nothing_before_its_start() {
        let clock = VirtualClock::at("2026-07-26T12:00:00Z");
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), clock),
            plan("Bonjour").scheduled(
                Schedule::immediate()
                    .starting_at(Timestamp::parse("2026-07-26T13:00:00Z").expect("valid")),
            ),
        );

        let started = tokio::time::Instant::now();
        let outcome = runner
            .run(&smsc, &recipients(2), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(outcome.tally.accepted, 2);
        assert!(
            started.elapsed() >= Duration::from_secs(3_600),
            "the campaign started before its planned instant"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_campaign_outside_its_daily_window_waits_for_the_opening() {
        let clock = VirtualClock::at("2026-07-26T06:00:00Z");
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();

        let window = DailyWindow::new(
            Time::from_hms(8, 0, 0).expect("a valid time"),
            Time::from_hms(20, 0, 0).expect("a valid time"),
            UtcOffset::UTC,
        )
        .expect("the two ends differ");

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), clock),
            plan("Bonjour").scheduled(Schedule::immediate().within(window)),
        );

        let started = tokio::time::Instant::now();
        let outcome = runner
            .run(&smsc, &recipients(1), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(outcome.tally.accepted, 1);
        assert!(
            started.elapsed() >= Duration::from_secs(2 * 3_600),
            "the campaign sent before its window opened"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_campaign_inside_its_window_sends_at_once() {
        let clock = VirtualClock::at("2026-07-26T12:00:00Z");
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();

        let window = DailyWindow::new(
            Time::from_hms(8, 0, 0).expect("a valid time"),
            Time::from_hms(20, 0, 0).expect("a valid time"),
            UtcOffset::UTC,
        )
        .expect("the two ends differ");

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), clock),
            plan("Bonjour").scheduled(Schedule::immediate().within(window)),
        );

        let started = tokio::time::Instant::now();

        runner
            .run(&smsc, &recipients(1), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert!(started.elapsed() < Duration::from_secs(60));
    }

    /// A campaign deferred to tomorrow morning is still cancellable this
    /// afternoon (CA-010-09).
    #[tokio::test(start_paused = true)]
    async fn cancelling_interrupts_a_scheduled_wait() {
        let clock = VirtualClock::at("2026-07-26T12:00:00Z");
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();
        let control = CampaignControl::new();

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), clock),
            plan("Bonjour").scheduled(
                Schedule::immediate()
                    .starting_at(Timestamp::parse("2026-08-26T12:00:00Z").expect("valid")),
            ),
        );

        let source = recipients(1);
        let (outcome, ()) = tokio::join!(runner.run(&smsc, &source, &control), {
            let control = &control;

            async move {
                tokio::time::sleep(SETTLE).await;
                control.cancel();
            }
        });

        let outcome = outcome.expect("the campaign runs");

        assert_eq!(outcome.status, CampaignStatus::Cancelled);
        assert_eq!(smsc.submitted(), 0);
    }

    // --- failures -----------------------------------------------------------

    /// A journal that cannot be read or written stops the campaign: carrying on
    /// would emit without the write-ahead row, which is the one thing the
    /// ordering of CLAUDE.md §4 forbids.
    #[tokio::test]
    async fn a_journal_failure_stops_the_campaign() {
        let journal = MemoryJournal::new().refusing_inserts(MessageStoreError::Unavailable {
            reason: String::from("the disk is full"),
        });
        let smsc = FakeSmsc::accepting();

        let runner =
            CampaignRunner::new(Sender::new(journal, FixedClock::default()), plan("Bonjour"));

        let failure = runner
            .run(&smsc, &recipients(3), &CampaignControl::new())
            .await
            .expect_err("the campaign cannot run without its journal");

        assert!(matches!(failure, crate::MessagingError::Store(_)));
        assert_eq!(smsc.submitted(), 0);
    }

    /// A source that stops half-way has **not** covered the campaign, and
    /// reporting `COMPLETED` would tell the operator the opposite.
    #[tokio::test]
    async fn a_source_that_fails_marks_the_campaign_failed() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();
        let source = recipients(5).failing_after(2);

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        let outcome = runner
            .run(&smsc, &source, &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(outcome.status, CampaignStatus::Failed);
        assert_eq!(outcome.tally.accepted, 2, "what was read is still sent");
        assert_eq!(smsc.submitted(), 2);
    }

    /// A message centre that never answers is a failure of every message, not a
    /// failure of the campaign: the retry policy decides, and the campaign
    /// finishes with its counters intact.
    #[tokio::test(start_paused = true)]
    async fn a_session_that_never_answers_fails_every_message_and_completes() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().then(Reply::Failed(crate::ports::SubmitError::Closed));

        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").with_retry(
                RetryPolicy::new(
                    1,
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    RetryBackoff::Fixed,
                )
                .expect("the bounds are valid"),
            ),
        );

        let outcome = runner
            .run(&smsc, &recipients(3), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(outcome.status, CampaignStatus::Completed);
        assert_eq!(outcome.tally.failed, 3);
        assert_eq!(outcome.tally.total(), 3);
    }

    #[tokio::test]
    async fn a_campaign_with_no_recipients_completes_at_once() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();

        let runner =
            CampaignRunner::new(Sender::new(journal, FixedClock::default()), plan("Bonjour"));

        let outcome = runner
            .run(&smsc, &StaticRecipients::default(), &CampaignControl::new())
            .await
            .expect("the campaign runs");

        assert_eq!(outcome.status, CampaignStatus::Completed);
        assert_eq!(outcome.tally.total(), 0);
        assert_eq!(smsc.submitted(), 0);
    }

    /// The one observable difference between the two modes, and the reason the
    /// distinction exists: a fresh campaign of half a million recipients does
    /// **no** journal reads, because the write-ahead insert is the check.
    #[tokio::test]
    async fn a_fresh_campaign_lets_the_insert_answer_instead_of_reading_first() {
        let journal = MemoryJournal::new();
        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour"),
        );

        runner
            .run(
                &FakeSmsc::accepting(),
                &recipients(4),
                &CampaignControl::new(),
            )
            .await
            .expect("the campaign runs");

        assert_eq!(journal.reads().await, 0);
        assert_eq!(journal.inserted().await, 4);
    }

    #[tokio::test]
    async fn a_resumed_campaign_asks_the_journal_about_every_recipient() {
        let journal = MemoryJournal::new();
        let runner = CampaignRunner::new(
            Sender::new(journal.clone(), FixedClock::default()),
            plan("Bonjour").resuming(),
        );

        runner
            .run(
                &FakeSmsc::accepting(),
                &recipients(4),
                &CampaignControl::new(),
            )
            .await
            .expect("the campaign runs");

        assert_eq!(journal.reads().await, 4);
    }

    #[test]
    fn a_campaign_starts_fresh_unless_it_says_otherwise() {
        assert_eq!(plan("Bonjour").start, StartMode::Fresh);
        assert_eq!(plan("Bonjour").resuming().start, StartMode::Resuming);
    }

    // --- progress (L-010-07) -------------------------------------------------

    /// The whole reason [`CampaignProgress`] exists: a campaign of half a
    /// million recipients is a future that does not resolve for an hour, and
    /// the counters have to be readable **while** it runs.
    ///
    /// The gate holds the message centre at two answers, so at the moment the
    /// snapshot is taken the campaign is provably unfinished.
    #[tokio::test(start_paused = true)]
    async fn a_running_campaign_publishes_its_counters_before_it_ends() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().gated(2);
        let progress = Arc::new(CampaignProgress::new());
        let runner =
            CampaignRunner::new(Sender::new(journal, FixedClock::default()), plan("Bonjour"))
                .reporting_to(Arc::clone(&progress));

        let source = recipients(6);
        let control = CampaignControl::new();
        let (outcome, midway) = tokio::join!(runner.run(&smsc, &source, &control), {
            let smsc = smsc.clone();
            let progress = Arc::clone(&progress);

            async move {
                tokio::time::sleep(SETTLE).await;

                let midway = progress.snapshot().tally;

                smsc.release(10);
                midway
            }
        });

        let outcome = outcome.expect("the campaign runs");

        assert!(
            midway.accepted > 0,
            "nothing was published while the campaign was running"
        );
        assert!(
            midway.total() < 6,
            "the campaign had already finished when the snapshot was taken: {midway:?}"
        );
        assert_eq!(outcome.tally.accepted, 6);
    }

    /// And what is left in the handle at the end is what the outcome says, so a
    /// reader that only ever samples cannot be told a different story from the
    /// one the caller gets.
    #[tokio::test]
    async fn the_last_reading_published_is_the_final_tally() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::scripted([Reply::Rejected(CommandStatus::EsmeRinvdstadr)]);
        let progress = Arc::new(CampaignProgress::new());

        let outcome =
            CampaignRunner::new(Sender::new(journal, FixedClock::default()), plan("Bonjour"))
                .reporting_to(Arc::clone(&progress))
                .run(&smsc, &recipients(4), &CampaignControl::new())
                .await
                .expect("the campaign runs");

        // Not a vacuous equality: the campaign really did produce a mixture.
        assert_eq!(outcome.tally.accepted, 3);
        assert_eq!(outcome.tally.failed, 1);
        assert_eq!(progress.snapshot().tally, outcome.tally);
    }

    /// A recipient the template rejected never reaches [`CampaignRunner::emit`],
    /// so it is the branch a report written at the emission site would miss.
    /// The reading has to move for it too, or a campaign of nothing but
    /// rejections would show a progress bar frozen at zero.
    #[tokio::test]
    async fn a_reading_is_published_for_a_recipient_no_message_was_built_for() {
        let journal = MemoryJournal::new();
        let progress = Arc::new(CampaignProgress::new());
        let source = StaticRecipients::new(vec![Recipient {
            destination: number(1),
            attributes: Some(String::from("{}")),
        }]);

        let outcome = CampaignRunner::new(
            Sender::new(journal, FixedClock::default()),
            plan("Bonjour {{prenom}}"),
        )
        .reporting_to(Arc::clone(&progress))
        .run(&FakeSmsc::accepting(), &source, &CampaignControl::new())
        .await
        .expect("the campaign runs");

        assert_eq!(outcome.tally.rejected, 1);
        assert_eq!(progress.snapshot().tally.rejected, 1);
    }

    /// The other branch that never reaches the emission site: a recipient the
    /// queue held when the campaign was cancelled.
    #[tokio::test(start_paused = true)]
    async fn a_reading_is_published_for_a_recipient_the_cancellation_dropped() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting().gated(2);
        let control = CampaignControl::new();
        let progress = Arc::new(CampaignProgress::new());
        let runner =
            CampaignRunner::new(Sender::new(journal, FixedClock::default()), plan("Bonjour"))
                .reporting_to(Arc::clone(&progress));

        let source = recipients(20);
        let (outcome, ()) = tokio::join!(runner.run(&smsc, &source, &control), {
            let smsc = smsc.clone();
            let control = &control;

            async move {
                tokio::time::sleep(SETTLE).await;
                control.cancel();
                smsc.release(10);
            }
        });

        let outcome = outcome.expect("the campaign runs");

        assert!(outcome.tally.cancelled > 0);
        assert_eq!(progress.snapshot().tally, outcome.tally);
    }

    /// **The reason the rate is the campaign's and not the session's.**
    ///
    /// Spec §15.3 puts a throughput beside a campaign's counters, and the two
    /// have to describe the same thing. `metrics:tick` counts every submission
    /// on the link — a unit send made while a campaign runs is inside it — so a
    /// campaign of five messages sending beside five unit messages would have
    /// read as twice its real rate.
    ///
    /// Here the five unit sends go through the **same** message centre, on the
    /// same clock, and the campaign's figure does not move.
    #[tokio::test]
    async fn the_rate_is_the_campaign_s_own_and_not_the_link_s() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::accepting();
        let progress = Arc::new(CampaignProgress::new());

        // Five unit messages on the same session, before the campaign runs.
        let aside = Sender::new(journal.clone(), FixedClock::default());

        for index in 100..105 {
            let request = SendRequest::new(
                String::from("un envoi unitaire"),
                SubmitOptions::to(
                    Destination::parse(&format!("+225070000{index:04}")).expect("a valid number"),
                ),
            );

            aside.send(&smsc, &request).await.expect("the unit send");
        }

        let outcome =
            CampaignRunner::new(Sender::new(journal, FixedClock::default()), plan("Bonjour"))
                .reporting_to(Arc::clone(&progress))
                .run(&smsc, &recipients(5), &CampaignControl::new())
                .await
                .expect("the campaign runs");

        let reading = progress.snapshot();

        assert_eq!(smsc.submitted(), 10, "the link really carried both");
        assert_eq!(outcome.tally.accepted, 5);
        assert!(
            (reading.accepted_per_second - 5.0).abs() < 0.001,
            "the campaign's rate counted the traffic beside it: {}",
            reading.accepted_per_second
        );
    }

    /// A **replay** is not throughput. Counting attempts rather than
    /// acceptances would put a campaign's rate at its highest exactly when the
    /// message centre is throttling it and nothing is getting through.
    ///
    /// The first message here is refused with `ESME_RTHROTTLED`, waits, and is
    /// replayed into an acceptance: four recipients, five submissions, four
    /// acceptances — and four is the rate.
    #[tokio::test(start_paused = true)]
    async fn a_replay_does_not_raise_the_rate() {
        let journal = MemoryJournal::new();
        let smsc = FakeSmsc::scripted([Reply::Rejected(CommandStatus::EsmeRthrottled)]);
        let progress = Arc::new(CampaignProgress::new());

        let outcome =
            CampaignRunner::new(Sender::new(journal, FixedClock::default()), plan("Bonjour"))
                .reporting_to(Arc::clone(&progress))
                .run(&smsc, &recipients(4), &CampaignControl::new())
                .await
                .expect("the campaign runs");

        let reading = progress.snapshot();

        assert_eq!(smsc.submitted(), 5, "one refusal and its replay");
        assert_eq!(outcome.tally.accepted, 4);
        assert_eq!(outcome.tally.retried, 1);
        assert!(
            (reading.accepted_per_second - 4.0).abs() < 0.001,
            "the replay was counted as throughput: {}",
            reading.accepted_per_second
        );
    }

    // NO TEST for "a runner with no observer still runs": every other test in
    // this module builds one without `reporting_to`, so one more asserting it
    // would be a copy that can only fail when thirty others already have.

    // --- the durable summary -------------------------------------------------

    /// `sent_count` is what the message centre **took**, not what was
    /// attempted, and `failed_count` is the terminal failures alone. The three
    /// other buckets are not failures and must not inflate either column.
    #[test]
    fn the_row_summary_counts_acceptances_and_terminal_failures_only() {
        let summary = CampaignTally {
            accepted: 7,
            failed: 2,
            rejected: 3,
            skipped: 4,
            cancelled: 5,
            retried: 11,
            reemitted_unanswered: 1,
            not_journalled: 1,
        }
        .summary();

        assert_eq!(summary.sent, 7);
        assert_eq!(summary.failed, 2);
    }

    /// A replay is not a second send: a message refused twice and accepted on
    /// the third attempt is **one** acceptance.
    #[test]
    fn a_replayed_message_is_summarised_once() {
        let tally = CampaignTally {
            accepted: 1,
            retried: 2,
            ..CampaignTally::default()
        };

        assert_eq!(tally.summary().sent, 1);
        assert_eq!(tally.summary().failed, 0);
    }
}
