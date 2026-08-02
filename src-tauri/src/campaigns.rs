//! What the campaign commands are allowed to reach (deliverable L-010-07).
//!
//! Three repositories, the event emitter, and a table of the campaigns running
//! right now. Everything that decides anything — the lifecycle of spec §10.3,
//! the write-ahead key, the back-pressure, the replay policy, the daily window —
//! lives in `messaging`, and this module calls it (CLAUDE.md §3).
//!
//! What is genuinely here, and could not be anywhere else, is three things:
//!
//! | | Why it cannot live lower |
//! |---|---|
//! | [`ContactRecipients`] | `messaging::ports::RecipientSource` over `contacts` — and `messaging` does not depend on `contacts`, nor the other way round. The port's own table names this layer as its implementor. |
//! | [`Running`] | pause and cancel arrive as **later IPC calls**, so the control of a campaign has to outlive the command that started it. Same shape as the import token of milestone 009. |
//! | [`run_reporting`] | turning counters into Tauri events, at a cadence. `messaging` must not know Tauri exists. |
//!
//! # Why a campaign is not awaited by the command that starts it
//!
//! `contacts_import` blocks its command for the length of the import and returns
//! the report. A campaign of half a million recipients runs for hours, survives
//! a pause of a working day, and has to be controllable from three other
//! commands while it runs. So [`CampaignServices::start`] spawns it and returns
//! at once; the interface follows it on `campaign:progress` and reads the detail
//! out of the journal, page by page.
//!
//! # A campaign is never restarted on its own
//!
//! Spec §10.5 has a resumed application pick up the campaigns that are not
//! terminal, and this application does **not** do it at startup. A bulk SMS
//! client that resumes sending to two hundred thousand people because somebody
//! double-clicked the icon is the failure CLAUDE.md §8 asks for guard rails
//! against. A campaign left `RUNNING` or `PAUSED` by a crash is shown as such and
//! resumed by [`CampaignServices::resume`], which is one click and an explicit
//! one. The resume itself loses nothing: the write-ahead key is derived
//! (ADR 0014), so it picks up exactly where the process died.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use messaging::campaign::runner::{CampaignPlan, CampaignRunner, CampaignTally};
use messaging::ports::{Recipient, RecipientSource, RecipientSourceError};
use messaging::sender::Sender;
use messaging::{
    CampaignControl, CampaignProgress, CampaignReading, CampaignStatus, CampaignSummary,
};
use persistence::ports::CampaignRepository as _;
use persistence::{
    Campaign, CampaignId, Database, ListSelection, SqliteCampaignRepository,
    SqliteContactRepository, SqliteMessageRepository,
};
use smpp_core::time::{SystemClock, Timestamp};
use smpp_core::types::SessionId;
use smpp_session::SessionHandle;
use tauri::{AppHandle, Runtime};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use contacts::ports::ContactRepository as _;

use crate::error::ErrorDto;
use crate::events::{CampaignProgressEvent, EventEmitter, CAMPAIGN_PROGRESS_INTERVAL};

/// The recipients of one campaign, read from the contact store.
///
/// The implementor `messaging::ports::RecipientSource` names in its own table,
/// and the reason it is here rather than in a crate: the port is declared by
/// `messaging`, the rows come from `contacts`, and neither crate depends on the
/// other. This layer is the only one that sees both.
///
/// **A stream, never a page and never a `Vec`.** `stream_contacts` keeps one row
/// in flight over SQLite, so a campaign of half a million recipients holds a
/// bounded footprint here as well as in the feeder (CA-010-01).
///
/// # Invalid numbers are not filtered out here
///
/// The selection carries every contact of the chosen lists, including the ones
/// the importer marked `valid = false`. They are not dropped silently: the
/// feeder's `Destination::parse_with` refuses them and the campaign counts them
/// as *rejected*, which is a figure the operator reads. A filter here would make
/// them vanish from the totals instead.
#[derive(Debug)]
pub(crate) struct ContactRecipients {
    repository: SqliteContactRepository,
    selection: ListSelection,
}

impl ContactRecipients {
    /// The recipients a selection picks out.
    pub(crate) const fn new(repository: SqliteContactRepository, selection: ListSelection) -> Self {
        Self {
            repository,
            selection,
        }
    }
}

impl RecipientSource for ContactRecipients {
    fn stream_recipients(&self) -> BoxStream<'_, Result<Recipient, RecipientSourceError>> {
        Box::pin(self.repository.stream_contacts(&self.selection).map(|row| {
            row.map(|contact| Recipient {
                destination: contact.msisdn,
                attributes: contact.attributes,
            })
            .map_err(|error| RecipientSourceError::Unavailable {
                // The port asks for a short, path-free rendering and the store
                // error is already one — its own documentation says the source
                // chain stays on the implementor's side.
                reason: error.to_string(),
            })
        }))
    }
}

/// One campaign that is running right now.
///
/// Held in a table keyed by campaign, because the four controls of spec §10.3
/// arrive as separate IPC calls: `campaign_pause` has to reach the
/// [`CampaignControl`] that `campaign_start` created, and the progress sampler
/// has to reach the counters the runner is filling.
#[derive(Debug)]
struct Running {
    control: CampaignControl,
    progress: Arc<CampaignProgress>,
    /// The session it sends on, carried so a progress reading can name it.
    session_id: SessionId,
    /// Recipients the campaign was created over — what the bar is drawn
    /// against.
    total: u32,
}

impl Running {
    /// A live campaign under the operator's controls.
    fn new(session_id: SessionId, total: u32) -> Self {
        Self {
            control: CampaignControl::new(),
            progress: Arc::new(CampaignProgress::new()),
            session_id,
            total,
        }
    }

    /// The intermediate reading the sampler publishes.
    ///
    /// # It takes no status, and that is the fix
    ///
    /// This method used to be a call to `publish` with `CampaignStatus::Running`
    /// written out at the call site, and the consequence was a bug an operator
    /// could not get out of: pausing a campaign wrote `PAUSED`, the button
    /// became *Reprendre*, and under 250 ms later the next reading published
    /// `RUNNING` and put *Mettre en pause* back. Clicking it called
    /// `campaign_pause` again — `PAUSED → PAUSED` is a legal no-op — so a paused
    /// campaign of two hundred thousand recipients could only be cancelled.
    ///
    /// The status now comes from [`CampaignControl::state`], through the
    /// projection `messaging` states, and there is no parameter through which a
    /// caller could disagree with it.
    ///
    /// Both halves are read **here**, when the sampler wakes, and never prepared
    /// in advance: a payload assembled before the wait is already stale when it
    /// is emitted — the rule the session forwarder learned at milestone 007.
    fn sampled_reading(&self, campaign_id: &str) -> CampaignProgressEvent {
        CampaignProgressEvent::of(
            campaign_id,
            &self.session_id.to_string(),
            CampaignStatus::from(self.control.state()),
            self.total,
            &self.progress.snapshot(),
            false,
        )
    }

    /// The last reading of a run, carrying the status it actually ended in.
    ///
    /// Separate from [`Self::sampled_reading`] because the terminal status is
    /// the **runner's** verdict and not the control's: a campaign nobody
    /// cancelled ends `COMPLETED`, and one whose source failed ends `FAILED`.
    fn final_reading(
        &self,
        campaign_id: &str,
        status: CampaignStatus,
        reading: &CampaignReading,
    ) -> CampaignProgressEvent {
        CampaignProgressEvent::of(
            campaign_id,
            &self.session_id.to_string(),
            status,
            self.total,
            reading,
            true,
        )
    }
}

/// Everything a spawned campaign needs, behind one `Arc`.
///
/// Split out of [`CampaignServices`] so the task that runs a campaign can hold a
/// handle to the table it must remove itself from when it ends. Without it a
/// finished campaign would stay marked as running and could never be started
/// again.
struct CampaignInner {
    campaigns: SqliteCampaignRepository,
    contacts: SqliteContactRepository,
    messages: SqliteMessageRepository,
    events: Arc<EventEmitter>,
    running: Mutex<LiveTable>,
}

/// The campaigns running right now, keyed by campaign.
type LiveTable = HashMap<CampaignId, Arc<Running>>;

/// Proof that the caller holds the live table.
///
/// # A parameter that is never read
///
/// Every writer of a campaign's status has to hold [`CampaignInner::running`]
/// across the **whole** read-modify-write, not merely take it once: `start`
/// writes `RUNNING` and the finishing task writes the terminal status, and a
/// lock released between the read and the write lets a campaign restarted in
/// the gap be written `COMPLETED` while it is sending.
///
/// A comment saying so is what this replaced, and a test could not hold it: an
/// implementation that took the lock, removed its entry and released it before
/// writing still passed a test that only observed the lock being taken. Threading
/// the guard through makes the same mistake a **compile error** — there is no
/// way to release it and still call the writer.
type Held<'a> = tokio::sync::MutexGuard<'a, LiveTable>;

/// The campaign half of the application state.
pub(crate) struct CampaignServices {
    inner: Arc<CampaignInner>,
}

impl core::fmt::Debug for CampaignServices {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CampaignServices")
            .finish_non_exhaustive()
    }
}

impl CampaignServices {
    /// Binds the services to an open database.
    pub(crate) fn new(database: Database, events: Arc<EventEmitter>) -> Self {
        Self {
            inner: Arc::new(CampaignInner {
                campaigns: SqliteCampaignRepository::new(database.clone()),
                contacts: SqliteContactRepository::new(database.clone()),
                messages: SqliteMessageRepository::new(database),
                events,
                running: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Counts the recipients a selection picks out.
    ///
    /// What the create command needs to refuse an empty campaign and to store
    /// the total the progress bar is drawn against.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_STORAGE` if the store will not answer.
    pub(crate) async fn count_recipients(
        &self,
        selection: &ListSelection,
    ) -> Result<u64, ErrorDto> {
        self.inner
            .contacts
            .count_contacts(selection)
            .await
            .map_err(|error| ErrorDto::campaign_storage(&error))
    }

    /// Writes a campaign row, replacing one of the same identifier.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_STORAGE` if the write fails.
    pub(crate) async fn save(&self, campaign: &Campaign) -> Result<(), ErrorDto> {
        self.inner
            .campaigns
            .upsert_campaign(campaign)
            .await
            .map_err(|error| ErrorDto::campaign_storage(&error))
    }

    /// Reads one campaign.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_NOT_FOUND` when there is none, or
    /// `CAMPAIGN_STORAGE` if the read fails.
    pub(crate) async fn find(&self, campaign_id: CampaignId) -> Result<Campaign, ErrorDto> {
        self.inner
            .campaigns
            .find_campaign(campaign_id)
            .await
            .map_err(|error| ErrorDto::campaign_storage(&error))?
            .ok_or_else(ErrorDto::campaign_not_found)
    }

    /// One page of campaigns, oldest first.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_STORAGE` if the read fails.
    pub(crate) async fn page(
        &self,
        cursor: persistence::Cursor,
        limit: u32,
    ) -> Result<persistence::Page<Campaign>, ErrorDto> {
        self.inner
            .campaigns
            .page_campaigns(cursor, limit)
            .await
            .map_err(|error| ErrorDto::campaign_storage(&error))
    }

    /// Whether this campaign is running right now.
    ///
    /// The row alone cannot answer it: a process killed mid-campaign leaves a
    /// row reading `RUNNING` with nothing behind it, and that is precisely the
    /// campaign the interface must offer to resume.
    pub(crate) async fn is_running(&self, campaign_id: CampaignId) -> bool {
        self.inner.running.lock().await.contains_key(&campaign_id)
    }

    /// Starts or picks up one campaign, and returns at once.
    ///
    /// `resuming` is what tells the runner to ask the journal about every
    /// recipient before emitting rather than letting the write-ahead insert
    /// answer — see `messaging::StartMode`. It is set for a resume and for a
    /// campaign a crash left behind, and clear for a campaign that has never
    /// run.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_BUSY` if it is already running,
    /// `CAMPAIGN_INVALID_TRANSITION` if the lifecycle refuses the move, or
    /// `CAMPAIGN_STORAGE` if the status cannot be written.
    pub(crate) async fn start<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        campaign: &Campaign,
        session: SessionHandle,
        plan: CampaignPlan,
        selection: ListSelection,
    ) -> Result<(), ErrorDto> {
        let campaign_id = campaign.campaign_id;
        let session_id = session.session_id();

        let running = Arc::new(Running::new(session_id, campaign.total_count));

        // The table lock is held ACROSS the status write, and that is not a
        // convenience. `CampaignInner::settle` takes the same lock to remove a
        // finished campaign and write its terminal status; releasing here would
        // leave a window in which a campaign that has just been started reads
        // `RUNNING`, the finishing task reads it back and writes `COMPLETED`
        // over it, and a campaign that is sending shows as finished.
        //
        // Holding a `tokio::sync::Mutex` across an `.await` is the sanctioned
        // shape (CLAUDE.md §4 bans the `std` one, precisely so this is
        // available); the write is one SQLite statement.
        let mut table = self.inner.running.lock().await;

        // `contains_key`, and NOT "contains a key whose control is not
        // cancelled". A cancelled campaign is still running: it drains its
        // queue and journals what is in flight before its task returns. Letting
        // a second run start in that window would put two runners on the same
        // campaign — and although the derived write-ahead key would stop them
        // duplicating a message, they would fight over the status and report
        // half a campaign each.
        if table.contains_key(&campaign_id) {
            return Err(ErrorDto::campaign_busy());
        }

        // The status is written BEFORE the campaign is registered and before
        // the task is spawned: a campaign whose row could not be moved to
        // `RUNNING` must not be sending, and must not be left in the table.
        self.write_status(&mut table, campaign_id, CampaignStatus::Running)
            .await?;

        table.insert(campaign_id, Arc::clone(&running));
        drop(table);

        let inner = Arc::clone(&self.inner);
        let app = app.clone();

        let driver = {
            let inner = Arc::clone(&inner);
            let running = Arc::clone(&running);

            tauri::async_runtime::spawn(async move {
                inner
                    .drive(app, campaign_id, session, plan, selection, running)
                    .await;
            })
        };

        // The campaign task is JOINED, by a second task that does nothing else.
        //
        // `spawn` hands back a `JoinHandle` and the first cut dropped it, which
        // is the shape `crate::sessions` uses — and justifies there, because a
        // forwarder that dies has nothing to clean up. This one does: a panic
        // anywhere under `drive` unwinds past the removal from the live table
        // and past the terminal status, so the campaign stays marked as running
        // for the life of the process. `CAMPAIGN_BUSY` then becomes permanent
        // and the campaign can be neither started, nor resumed, nor cancelled.
        //
        // Not an orphan (CLAUDE.md §4): it waits on one handle and returns when
        // that handle resolves, which the campaign guarantees it eventually
        // does.
        tauri::async_runtime::spawn(async move {
            let Err(error) = driver.await else {
                return;
            };

            tracing::error!(
                campaign_id = %campaign_id,
                error = %error,
                "the campaign task ended abnormally; recording it as failed"
            );

            // The counters as they stood: they are what the campaign really did
            // before it fell over, and zeroing them would report a campaign
            // that sent nothing.
            inner
                .finish(
                    campaign_id,
                    CampaignStatus::Failed,
                    &running.progress.snapshot().tally,
                )
                .await;
        });

        Ok(())
    }

    /// Suspends the feeding of a running campaign (spec §10.3).
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_INVALID_TRANSITION` if the lifecycle refuses
    /// it, or `CAMPAIGN_STORAGE` if the status cannot be written.
    pub(crate) async fn pause(&self, campaign_id: CampaignId) -> Result<(), ErrorDto> {
        let mut table = self.inner.running.lock().await;

        self.write_status(&mut table, campaign_id, CampaignStatus::Paused)
            .await?;

        if let Some(running) = table.get(&campaign_id) {
            running.control.pause();
        }

        Ok(())
    }

    /// Resumes a campaign that is paused **and still running in this process**.
    ///
    /// Returns `false` when nothing was running, which is not a failure: it is
    /// the campaign a restart left behind, and the caller then starts a fresh
    /// run in resuming mode.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_INVALID_TRANSITION` or `CAMPAIGN_STORAGE`.
    pub(crate) async fn resume_in_place(&self, campaign_id: CampaignId) -> Result<bool, ErrorDto> {
        let mut table = self.inner.running.lock().await;

        let Some(running) = table.get(&campaign_id).map(Arc::clone) else {
            return Ok(false);
        };

        self.write_status(&mut table, campaign_id, CampaignStatus::Running)
            .await?;
        running.control.resume();

        Ok(true)
    }

    /// Stops a campaign for good (CA-010-09).
    ///
    /// A campaign that is running is cancelled through its control and writes
    /// its own terminal status when its task returns; one that is not gets the
    /// status written here, because nothing else will.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_INVALID_TRANSITION` or `CAMPAIGN_STORAGE`.
    pub(crate) async fn cancel(&self, campaign_id: CampaignId) -> Result<(), ErrorDto> {
        let mut table = self.inner.running.lock().await;

        match table.get(&campaign_id).map(Arc::clone) {
            Some(running) => {
                // The status is NOT written here: the task is about to end and
                // will write CANCELLED with the final counters. Writing it now
                // would be overwritten a moment later by the same value. The
                // transition is still checked — against the row as it stands —
                // so cancelling a campaign that has just completed is refused
                // rather than silently ignored.
                self.read(campaign_id)
                    .await?
                    .status
                    .try_move_to(CampaignStatus::Cancelled)
                    .map_err(|rejection| ErrorDto::campaign_invalid_transition(&rejection))?;

                running.control.cancel();
            }
            None => {
                self.write_status(&mut table, campaign_id, CampaignStatus::Cancelled)
                    .await?;
            }
        }

        Ok(())
    }

    /// Signals every running campaign to stop. Called when the application
    /// exits.
    ///
    /// **Signals, and does not wait.** Each campaign's own task sees the
    /// cancellation at its next check and drains what it holds; this returns as
    /// soon as the tokens are set. So a message already on its way to `submit`
    /// may still leave after this call, and `crate::run` says so where it
    /// orders the two shutdowns.
    ///
    /// The rows keep whatever status they had, deliberately: a campaign the
    /// operator did not stop is one they will want to resume, and rewriting it
    /// to `CANCELLED` on the way out would take that away.
    pub(crate) async fn shutdown(&self) {
        for running in self.inner.running.lock().await.values() {
            running.control.cancel();
        }
    }

    /// Reads one campaign, without the table lock.
    ///
    /// Private twin of [`Self::find`], which takes no lock either — the
    /// distinction is that this one is only ever called by a caller that
    /// **already holds** it.
    async fn read(&self, campaign_id: CampaignId) -> Result<Campaign, ErrorDto> {
        self.inner
            .campaigns
            .find_campaign(campaign_id)
            .await
            .map_err(|error| ErrorDto::campaign_storage(&error))?
            .ok_or_else(ErrorDto::campaign_not_found)
    }

    /// Applies one lifecycle transition and writes the row.
    ///
    /// # Read-modify-write, and why the caller's copy is not used
    ///
    /// The row is **re-read here**, although every caller has just fetched one.
    /// The copy a command holds was read before the table lock was taken, and a
    /// campaign can finish in that interval: the stale copy would say `RUNNING`,
    /// `RUNNING → PAUSED` is a legal move, and a campaign that had completed
    /// would be written back to `PAUSED`.
    ///
    /// Every caller holds the table lock, so the read, the transition and the
    /// write are one step with respect to the finishing task, which takes the
    /// same lock.
    async fn write_status(
        &self,
        _held: &mut Held<'_>,
        campaign_id: CampaignId,
        next: CampaignStatus,
    ) -> Result<(), ErrorDto> {
        // THE MACHINE DECIDES, not this layer — see [`advance`], which is the
        // one place a row's status and its instants move, shared with the task
        // that ends a campaign.
        let updated = advance(self.read(campaign_id).await?, next, None)
            .map_err(|rejection| ErrorDto::campaign_invalid_transition(&rejection))?;

        self.save(&updated).await
    }
}

impl CampaignInner {
    /// Runs one campaign to its end, reporting as it goes.
    ///
    /// # The shape that makes CA-010-11 hold
    ///
    /// [`run_reporting`] samples the counters every
    /// [`CAMPAIGN_PROGRESS_INTERVAL`] and publishes the **final** reading once,
    /// after the sampling has stopped. Nothing on the send path can raise the
    /// rate of the intermediate events, and nothing can suppress the last one.
    #[tracing::instrument(skip_all, fields(campaign_id = %campaign_id))]
    async fn drive<R: Runtime>(
        self: Arc<Self>,
        app: AppHandle<R>,
        campaign_id: CampaignId,
        session: SessionHandle,
        plan: CampaignPlan,
        selection: ListSelection,
        running: Arc<Running>,
    ) {
        let runner = CampaignRunner::new(Sender::new(self.messages.clone(), SystemClock), plan)
            .reporting_to(Arc::clone(&running.progress));

        let source = ContactRecipients::new(self.contacts.clone(), selection);
        let rendered = campaign_id.to_string();

        let outcome = run_reporting(
            CAMPAIGN_PROGRESS_INTERVAL,
            runner.run(&session, &source, &running.control),
            // No status is passed: `Running::sampled_reading` reads the control.
            // A `CampaignStatus::Running` written out here is exactly the bug
            // that made a paused campaign unresumable — see that method.
            || async {
                self.events
                    .emit_campaign_progress(&app, &running.sampled_reading(&rendered));
            },
        )
        .await;

        // The last reading the runner published: its counters, and the rate
        // over the window that ended with the last message. The outcome's tally
        // is the authoritative one, so it replaces the reading's — the rate has
        // no second source and is carried through as measured.
        let mut reading = running.progress.snapshot();

        let status = match outcome {
            Ok(outcome) => {
                reading.tally = outcome.tally;

                outcome.status
            }
            Err(error) => {
                tracing::error!(error = %error, "the campaign stopped on a journal failure");

                // The counters are still the truth of what happened before the
                // journal gave out; they are simply not the counters of a
                // campaign that covered its recipients.
                CampaignStatus::Failed
            }
        };

        self.finish(campaign_id, status, &reading.tally).await;

        // THE LAST EVENT, and it is unconditional. The sampler has returned, so
        // nothing can arrive after it; the emitter applies no throttle, so
        // nothing can drop it. Milestone 007 shipped a `sessions:state` whose
        // rate limit swallowed the last transition and left the screen on
        // `CONNECTING` for ever — this is the same defect, on the event that
        // says a campaign of two hundred thousand messages has finished.
        self.events
            .emit_campaign_progress(&app, &running.final_reading(&rendered, status, &reading));
    }

    /// Takes a campaign out of the live table and writes its terminal row.
    ///
    /// # One step, under one lock
    ///
    /// Leaving the table and writing the status are **not** separable.
    /// [`CampaignServices::start`] holds the same lock while it writes
    /// `RUNNING`; split in two, the two writers interleave — a campaign
    /// restarted in the gap reads `RUNNING`, and this writes `COMPLETED` over a
    /// campaign that is sending.
    ///
    /// A storage failure here is logged rather than propagated: the campaign is
    /// over and there is nobody to return an error to. The row is a summary of
    /// the `messages` table, which is the record (spec §17.6).
    #[tracing::instrument(skip_all, fields(campaign_id = %campaign_id, status = %status))]
    async fn finish(&self, campaign_id: CampaignId, status: CampaignStatus, tally: &CampaignTally) {
        let mut table = self.running.lock().await;

        table.remove(&campaign_id);
        self.settle(&mut table, campaign_id, status, tally).await;
    }

    /// Writes the terminal row of a campaign whose task has ended.
    ///
    /// Takes the guard rather than the lock, so it cannot be called except by a
    /// caller that already holds it — see [`Held`].
    async fn settle(
        &self,
        _held: &mut Held<'_>,
        campaign_id: CampaignId,
        status: CampaignStatus,
        tally: &CampaignTally,
    ) {
        let stored = match self.campaigns.find_campaign(campaign_id).await {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                tracing::warn!("the campaign row vanished while it was running");

                return;
            }
            Err(error) => {
                tracing::error!(error = %error, "the campaign row could not be read back");

                return;
            }
        };

        let Ok(updated) = advance(stored, status, Some(tally.summary())) else {
            tracing::warn!(
                "the campaign ended in a status the lifecycle refuses from where the row stands"
            );

            return;
        };

        if let Err(error) = self.campaigns.upsert_campaign(&updated).await {
            tracing::error!(error = %error, "the campaign outcome could not be written");
        }
    }
}

/// Applies one lifecycle transition to a stored campaign.
///
/// The **one** place a campaign row's status, its two instants and its durable
/// counters move together — it was written twice, in `write_status` and in what
/// is now [`CampaignInner::finish`], and the two copies were already one
/// `completed_at` apart.
///
/// Every rule it applies belongs to `messaging` and is read from it rather than
/// restated: [`CampaignStatus::try_move_to`] decides whether the move is legal,
/// [`CampaignStatus::is_terminal`] decides whether the campaign is over, and
/// [`messaging::CampaignTally::summary`] decides which bucket feeds which
/// column. What is left here is assignment.
///
/// # Errors
///
/// The rejection the lifecycle produced, for the caller to render or log.
fn advance(
    campaign: Campaign,
    next: CampaignStatus,
    summary: Option<CampaignSummary>,
) -> Result<Campaign, messaging::InvalidCampaignTransition> {
    let status = campaign.status.try_move_to(next)?;
    let mut updated = campaign;

    updated.status = status;

    // `is_none()`, not "always": a campaign resumed after a restart is moved to
    // `RUNNING` again, and stamping it would lose the instant it first started
    // sending — which is the one an operator reads to know how long a campaign
    // of two hundred thousand messages has been going.
    if status == CampaignStatus::Running && updated.started_at.is_none() {
        updated.started_at = Some(Timestamp::now());
    }

    if status.is_terminal() {
        updated.completed_at = Some(Timestamp::now());
    }

    if let Some(summary) = summary {
        updated.sent_count = narrow(summary.sent);
        updated.failed_count = narrow(summary.failed);
    }

    Ok(updated)
}

/// A counter narrowed for the row, saturating rather than wrapping.
fn narrow(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Runs `campaign`, sampling `publish` at `interval` until it ends.
///
/// Split out of [`CampaignInner::drive`] so the two properties CA-010-11 rests
/// on can be stated without a Tauri runtime, a database or a message centre:
///
/// * **the sampling rate is the interval**, and there is no argument, no channel
///   and no code path by which the campaign's throughput could raise it;
/// * **the sampling stops before the caller's last emission**, so the event
///   carrying `done` is the last one the interface sees. That ordering is the
///   whole reason this is one function rather than a `spawn` and an `abort`:
///   an aborted sampler can have one emission already in flight.
///
/// The sleep comes first, so a campaign that finishes inside one interval emits
/// only the final reading — which is right: two events for a campaign of three
/// recipients is one more repaint than the screen needs.
async fn run_reporting<T, Run, Sample, Fut>(interval: Duration, run: Run, sample: Sample) -> T
where
    Run: core::future::Future<Output = T>,
    Sample: Fn() -> Fut,
    Fut: core::future::Future<Output = ()>,
{
    let finished = CancellationToken::new();

    let running = async {
        let outcome = run.await;

        finished.cancel();

        outcome
    };

    let sampling = async {
        loop {
            tokio::select! {
                biased;

                () = finished.cancelled() => return,
                () = tokio::time::sleep(interval) => sample().await,
            }
        }
    };

    let (outcome, ()) = tokio::join!(running, sampling);

    outcome
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::{
        advance, run_reporting, Campaign, CampaignId, CampaignReading, CampaignServices,
        CampaignStatus, CampaignTally, ContactRecipients, Duration, EventEmitter, Running,
        SessionId,
    };
    use contacts::model::{Contact, ContactId, ContactList, ListId};
    use futures_util::StreamExt as _;
    use messaging::ports::RecipientSource as _;
    use persistence::{
        Database, DatabaseConfig, ListSelection, SqliteContactRepository, Timestamp,
    };
    use smpp_core::types::Msisdn;
    use std::sync::Arc;

    const INTERVAL: Duration = Duration::from_millis(250);

    /// **Real** time given to a task that must make no progress.
    ///
    /// Only for `every_status_writer_waits_for_the_live_table`, which is the one
    /// test here that cannot use a virtual clock because it drives a SQLx pool —
    /// its header says why at length. Two hundred milliseconds against writers
    /// that would otherwise complete in microseconds: four orders of magnitude,
    /// paid once.
    const RESPITE: Duration = Duration::from_millis(200);

    // --- the recipient source ------------------------------------------------

    /// An open, migrated database of its own — CLAUDE.md §7 asks each test for
    /// one.
    ///
    /// A **file** in a temporary directory and not `:memory:`, which is what
    /// this was written with first: SQLx opens a pool, and every connection of
    /// a pool onto `:memory:` gets a *separate* database. The migrations and the
    /// inserts landed on one connection and the stream read another, so the
    /// source came back empty with nothing failing anywhere. The directory is
    /// returned so it outlives the repository.
    async fn store() -> (tempfile::TempDir, SqliteContactRepository) {
        let directory = tempfile::TempDir::new().expect("creating a temporary directory");
        let database = Database::open(DatabaseConfig::new(directory.path().join("contacts.db")))
            .await
            .expect("the database opens");

        database.migrate().await.expect("the migrations apply");

        (directory, SqliteContactRepository::new(database))
    }

    fn a_contact(number: &str, attributes: Option<&str>) -> Contact {
        Contact {
            contact_id: ContactId::new(),
            msisdn: Msisdn::parse(number).expect("the fixture is a valid number"),
            country: Some(String::from("CI")),
            valid: true,
            line_type: None,
            attributes: attributes.map(str::to_owned),
            source: Some(String::from("test")),
            created_at: Timestamp::now(),
        }
    }

    /// The whole point of the adapter: a contact row becomes a recipient with
    /// its number **and its attributes**, because the attributes are what the
    /// template engine resolves `{{prenom}}` out of (CA-010-06). An adapter that
    /// dropped them would send "Bonjour" to everybody under a substitution
    /// policy, and reject every recipient under the default one.
    #[tokio::test]
    async fn a_contact_becomes_a_recipient_carrying_its_attributes() {
        use contacts::ports::ContactRepository as _;

        let (_directory, repository) = store().await;

        repository
            .insert_contacts(&[
                a_contact("+2250700000001", Some(r#"{"prenom":"Awa"}"#)),
                a_contact("+2250700000002", None),
            ])
            .await
            .expect("the contacts are written");

        let source = ContactRecipients::new(repository, ListSelection::everything());
        let recipients: Vec<_> = source
            .stream_recipients()
            .map(|row| row.expect("the store answers"))
            .collect()
            .await;

        assert_eq!(recipients.len(), 2);
        assert_eq!(recipients[0].destination.as_str(), "2250700000001");
        assert_eq!(
            recipients[0].attributes.as_deref(),
            Some(r#"{"prenom":"Awa"}"#),
            "the attributes are what the template resolves its variables from"
        );
        assert_eq!(recipients[1].attributes, None);
    }

    /// The selection is honoured: a campaign over a list sends to that list and
    /// not to the whole store. Written because the failure is silent and
    /// enormous — a campaign that ignored its list would text every contact the
    /// application holds.
    #[tokio::test]
    async fn only_the_selected_list_is_streamed() {
        use contacts::ports::ContactRepository as _;

        let (_directory, repository) = store().await;
        let list = ListId::new();
        let member = a_contact("+2250700000001", None);
        let outsider = a_contact("+2250700000002", None);
        let member_id = member.contact_id;

        repository
            .insert_contact_list(&ContactList {
                list_id: list,
                name: String::from("juillet"),
                created_at: Timestamp::now(),
            })
            .await
            .expect("the list is written");

        repository
            .insert_contacts(&[member, outsider])
            .await
            .expect("the contacts are written");

        repository
            .add_contacts_to_list(list, &[member_id])
            .await
            .expect("the member joins the list");

        let source = ContactRecipients::new(repository, ListSelection::union(vec![list]));
        let recipients: Vec<_> = source
            .stream_recipients()
            .map(|row| row.expect("the store answers"))
            .collect()
            .await;

        assert_eq!(recipients.len(), 1);
        assert_eq!(recipients[0].destination.as_str(), "2250700000001");
    }

    // NO TEST here for "the traversal order is stable across two runs", although
    // `RecipientSource` states it as a requirement on its implementor.
    //
    // One was written and then deleted: it read the **same store twice** and
    // compared the two lists, which is a tautology whatever the query does —
    // two identical reads of an unchanged table agree in any order. The
    // property has to be asserted where the order is produced, against a
    // traversal whose insertion order and natural key differ, and that is
    // `persistence::tests::repositories::stream_contacts_traverses_in_insertion_order`.

    // --- the lifecycle, against a real store ---------------------------------

    /// Services over a temporary database of their own.
    ///
    /// None of `pause`, `resume_in_place`, `cancel`, `write_status` or `finish`
    /// needs an `AppHandle` — only `start` does, because only `start` spawns a
    /// task that emits. That is what makes the whole lifecycle testable here.
    async fn services() -> (tempfile::TempDir, CampaignServices) {
        let directory = tempfile::TempDir::new().expect("creating a temporary directory");
        let database = Database::open(DatabaseConfig::new(directory.path().join("campaigns.db")))
            .await
            .expect("the database opens");

        database.migrate().await.expect("the migrations apply");

        (
            directory,
            CampaignServices::new(database, Arc::new(EventEmitter::default())),
        )
    }

    fn a_campaign(status: CampaignStatus) -> Campaign {
        Campaign {
            campaign_id: CampaignId::new(),
            name: String::from("juillet"),
            status,
            template: String::from("Bonjour"),
            send_config: String::from("{}"),
            total_count: 200,
            sent_count: 0,
            delivered_count: 0,
            failed_count: 0,
            created_at: Timestamp::now(),
            started_at: None,
            completed_at: None,
        }
    }

    /// Registers a live run, the way `start` does once the task is spawned.
    async fn register(services: &CampaignServices, campaign_id: CampaignId) -> Arc<Running> {
        let running = Arc::new(Running::new(SessionId::new(), 200));

        services
            .inner
            .running
            .lock()
            .await
            .insert(campaign_id, Arc::clone(&running));

        running
    }

    #[tokio::test]
    async fn pausing_a_campaign_writes_the_status_and_suspends_its_feeding() {
        let (_directory, services) = services().await;
        let campaign = a_campaign(CampaignStatus::Running);

        services.save(&campaign).await.expect("the row is written");

        let running = register(&services, campaign.campaign_id).await;

        services
            .pause(campaign.campaign_id)
            .await
            .expect("a running campaign pauses");

        assert_eq!(
            services
                .find(campaign.campaign_id)
                .await
                .expect("the row is there")
                .status,
            CampaignStatus::Paused
        );
        assert_eq!(running.control.state(), messaging::RunState::Paused);
    }

    /// **The non-regression test of the read-modify-write.**
    ///
    /// The command reads the row, then takes the table lock; a campaign can
    /// finish in between. When `pause` applied the transition to the copy the
    /// *caller* held, that stale copy said `RUNNING`, `RUNNING → PAUSED` is a
    /// legal move, and a campaign that had completed was written back to
    /// `PAUSED` — non-terminal for ever, with a resume button on a campaign
    /// that had finished.
    ///
    /// The stale copy is what `campaign` stands for here: the row underneath has
    /// already moved on.
    #[tokio::test]
    async fn a_control_is_refused_against_the_stored_status_not_a_stale_copy() {
        let (_directory, services) = services().await;
        let campaign = a_campaign(CampaignStatus::Running);

        services.save(&campaign).await.expect("the row is written");

        // The campaign finishes under the caller's feet.
        let mut finished = campaign.clone();
        finished.status = CampaignStatus::Completed;
        services.save(&finished).await.expect("the row moves on");

        for control in ["pause", "resume", "cancel"] {
            let refusal = match control {
                "pause" => services.pause(campaign.campaign_id).await,
                "resume" => services
                    .resume_in_place(campaign.campaign_id)
                    .await
                    .map(|_| ()),
                _ => services.cancel(campaign.campaign_id).await,
            };

            // `resume` on a campaign nothing is running is `Ok(false)`, not a
            // refusal — the caller then launches a fresh run, which reads the
            // row and is refused there.
            if control != "resume" {
                assert_eq!(
                    refusal.expect_err(control).code,
                    crate::error::ErrorCode::CampaignInvalidTransition,
                    "{control}"
                );
            }

            assert_eq!(
                services
                    .find(campaign.campaign_id)
                    .await
                    .expect("the row is there")
                    .status,
                CampaignStatus::Completed,
                "{control} moved a campaign that had finished"
            );
        }
    }

    /// Cancelling a campaign that is **running** does not write the status: the
    /// task is about to end and writes it with the final counters. What it must
    /// do is reach the control (CA-010-09).
    #[tokio::test]
    async fn cancelling_a_running_campaign_stops_it_and_leaves_the_row_to_its_task() {
        let (_directory, services) = services().await;
        let campaign = a_campaign(CampaignStatus::Running);

        services.save(&campaign).await.expect("the row is written");

        let running = register(&services, campaign.campaign_id).await;

        services
            .cancel(campaign.campaign_id)
            .await
            .expect("a running campaign cancels");

        assert_eq!(running.control.state(), messaging::RunState::Cancelled);
        assert_eq!(
            services
                .find(campaign.campaign_id)
                .await
                .expect("the row is there")
                .status,
            CampaignStatus::Running,
            "the row is the finishing task's to write"
        );
    }

    /// One that is **not** running has nobody to write it, so the command does.
    #[tokio::test]
    async fn cancelling_a_campaign_nothing_is_running_writes_its_terminal_row() {
        let (_directory, services) = services().await;
        let campaign = a_campaign(CampaignStatus::Paused);

        services.save(&campaign).await.expect("the row is written");

        services
            .cancel(campaign.campaign_id)
            .await
            .expect("a paused campaign cancels");

        let stored = services
            .find(campaign.campaign_id)
            .await
            .expect("the row is there");

        assert_eq!(stored.status, CampaignStatus::Cancelled);
        assert!(stored.completed_at.is_some());
    }

    #[tokio::test]
    async fn resuming_a_campaign_nothing_is_running_reports_that_there_was_nothing() {
        let (_directory, services) = services().await;
        let campaign = a_campaign(CampaignStatus::Paused);

        services.save(&campaign).await.expect("the row is written");

        assert!(!services
            .resume_in_place(campaign.campaign_id)
            .await
            .expect("no failure"));
        assert_eq!(
            services
                .find(campaign.campaign_id)
                .await
                .expect("the row is there")
                .status,
            CampaignStatus::Paused,
            "a resume that found nothing must not move the row"
        );
    }

    #[tokio::test]
    async fn resuming_a_paused_campaign_in_place_restarts_its_feeding() {
        let (_directory, services) = services().await;
        let campaign = a_campaign(CampaignStatus::Paused);

        services.save(&campaign).await.expect("the row is written");

        let running = register(&services, campaign.campaign_id).await;
        running.control.pause();

        assert!(services
            .resume_in_place(campaign.campaign_id)
            .await
            .expect("a paused campaign resumes"));

        assert_eq!(running.control.state(), messaging::RunState::Running);
        assert_eq!(
            services
                .find(campaign.campaign_id)
                .await
                .expect("the row is there")
                .status,
            CampaignStatus::Running
        );
    }

    /// `finish` takes a campaign out of the live table **and** writes its row,
    /// so a completed campaign can be started again.
    #[tokio::test]
    async fn finishing_a_campaign_frees_it_and_writes_its_counters() {
        let (_directory, services) = services().await;
        let campaign = a_campaign(CampaignStatus::Running);

        services.save(&campaign).await.expect("the row is written");
        register(&services, campaign.campaign_id).await;

        services
            .inner
            .finish(
                campaign.campaign_id,
                CampaignStatus::Completed,
                &CampaignTally {
                    accepted: 190,
                    failed: 10,
                    ..CampaignTally::default()
                },
            )
            .await;

        let stored = services
            .find(campaign.campaign_id)
            .await
            .expect("the row is there");

        assert_eq!(stored.status, CampaignStatus::Completed);
        assert_eq!(stored.sent_count, 190);
        assert_eq!(stored.failed_count, 10);
        assert!(stored.completed_at.is_some());
        assert!(
            !services.is_running(campaign.campaign_id).await,
            "a finished campaign that stays in the table is CAMPAIGN_BUSY for ever"
        );
    }

    /// **Every status writer waits for the live table's lock.**
    ///
    /// # What it asserts, and what it leaves to the compiler
    ///
    /// That a writer takes the lock, and that its write lands only once the lock
    /// is free. It does **not** assert that the lock is held across the whole
    /// read-modify-write: an implementation that took it, removed its entry and
    /// released it before writing passed this test unchanged, which is how that
    /// gap was found. That half is a compile-time guarantee instead —
    /// `write_status` and `settle` take a [`Held`] and cannot be reached without
    /// one, so the shape no longer compiles.
    ///
    /// # NO VIRTUAL CLOCK HERE, and that is the whole point
    ///
    /// This test used `tokio::time::pause()` and inferred the wait from a
    /// `timeout` expiring. It passed on macOS and Windows and failed on Ubuntu,
    /// on its **last** line — the write issued *after* the lock was released.
    ///
    /// Virtual time and a real connection pool do not mix. SQLx measures its
    /// acquire timeout on the same clock, so the moment the pool has to open a
    /// connection the runtime goes idle waiting on the blocking connect, tokio's
    /// auto-advance jumps to the next armed timer, and the acquire deadline
    /// fires instantly: `PoolTimedOut`, surfaced as `CAMPAIGN_STORAGE`.
    ///
    /// Measured rather than assumed. Under `tokio::time::pause()`, fourteen of
    /// sixteen concurrent reads on a *warm* pool failed exactly that way, and the
    /// two that reused an already-open connection succeeded; the old shape, made
    /// to race the pool for a connection, failed on precisely the CI line with
    /// precisely the CI error. The size of the clock jump is irrelevant — whether
    /// a connection has to be opened is everything, and that depends on the
    /// runner's disk.
    ///
    /// So the clock is left alone, and the two directions are stated
    /// differently:
    ///
    /// * **while the table is held**, the assertion is on the campaign row —
    ///   *no status was written* — which is the property itself, observed,
    ///   rather than deduced from a future that made no progress;
    /// * **after the release**, the writer is awaited with **no timeout at all**,
    ///   and the ordering is read off a log both sides append to. "It completed
    ///   after the release" is then a constatation.
    ///
    /// The one inference left is [`RESPITE`], and it is worth naming: a writer
    /// that did not take the lock would finish in microseconds — this suite runs
    /// a hundred tests in a tenth of a second — so the margin is four orders of
    /// magnitude, and it no longer depends on the disk, because nothing here
    /// touches the pool while blocked.
    ///
    /// # The two writers are driven one after the other
    ///
    /// Deliberately, and it is not tidiness: run together they race for the lock
    /// once it is free, and `finish` winning would take the row to `COMPLETED` —
    /// after which `pause`'s move to `PAUSED` is refused by the lifecycle and the
    /// test fails for a reason that has nothing to do with what it checks. One
    /// phase each, with the row moving `RUNNING -> PAUSED -> COMPLETED` along the
    /// legal path.
    #[tokio::test]
    async fn every_status_writer_waits_for_the_live_table() {
        let (_directory, services) = services().await;
        let services = Arc::new(services);
        let campaign = a_campaign(CampaignStatus::Running);

        services.save(&campaign).await.expect("the row is written");

        let log = Arc::new(tokio::sync::Mutex::new(Vec::<&'static str>::new()));

        // --- phase one: `pause`, which reports its own refusal ---------------
        {
            let held = services.inner.running.lock().await;

            let writer = {
                let services = Arc::clone(&services);
                let log = Arc::clone(&log);
                let campaign_id = campaign.campaign_id;

                tokio::spawn(async move {
                    let outcome = services.pause(campaign_id).await;

                    log.lock().await.push("pause wrote");

                    outcome
                })
            };

            tokio::time::sleep(RESPITE).await;

            // THE PROPERTY, observed on the row rather than deduced from a
            // future that has not finished.
            assert_eq!(
                services
                    .find(campaign.campaign_id)
                    .await
                    .expect("the row is there")
                    .status,
                CampaignStatus::Running,
                "pause wrote a status while the live table was held"
            );

            log.lock().await.push("released");
            drop(held);

            // No timeout: this is the line that broke on Ubuntu, and there is
            // nothing left here that a slow machine can turn into a failure.
            writer
                .await
                .expect("the writer task ran")
                .expect("the pause goes through once the table is free");
        }

        assert_eq!(
            services
                .find(campaign.campaign_id)
                .await
                .expect("the row is there")
                .status,
            CampaignStatus::Paused
        );

        // --- phase two: `finish`, which reports nothing and writes the row ---
        {
            let held = services.inner.running.lock().await;

            let writer = {
                let services = Arc::clone(&services);
                let log = Arc::clone(&log);
                let campaign_id = campaign.campaign_id;

                tokio::spawn(async move {
                    services
                        .inner
                        .finish(
                            campaign_id,
                            CampaignStatus::Completed,
                            &CampaignTally::default(),
                        )
                        .await;

                    log.lock().await.push("finish wrote");
                })
            };

            tokio::time::sleep(RESPITE).await;

            assert_eq!(
                services
                    .find(campaign.campaign_id)
                    .await
                    .expect("the row is there")
                    .status,
                CampaignStatus::Paused,
                "the finishing task wrote a status while the live table was held"
            );

            log.lock().await.push("released");
            drop(held);

            writer.await.expect("the writer task ran");
        }

        assert_eq!(
            services
                .find(campaign.campaign_id)
                .await
                .expect("the row is there")
                .status,
            CampaignStatus::Completed
        );

        // Each write landed AFTER its release, which is the ordering the whole
        // test is about — read off the log rather than inferred from a clock.
        assert_eq!(
            log.lock().await.as_slice(),
            ["released", "pause wrote", "released", "finish wrote"]
        );
    }

    // --- what a reading carries ----------------------------------------------

    /// **The blocker.** The sampler used to publish `RUNNING` outright, so a
    /// paused campaign was shown as running again within 250 ms and its resume
    /// button vanished — leaving cancellation as the only way out.
    ///
    /// The reading takes no status argument any more; it reads the control.
    #[tokio::test]
    async fn a_sampled_reading_carries_the_command_actually_in_force() {
        let running = Running::new(SessionId::new(), 200);

        assert_eq!(running.sampled_reading("c").status, "RUNNING");

        running.control.pause();
        assert_eq!(
            running.sampled_reading("c").status,
            "PAUSED",
            "a paused campaign published as RUNNING loses its resume button"
        );

        running.control.resume();
        assert_eq!(running.sampled_reading("c").status, "RUNNING");

        running.control.cancel();
        assert_eq!(running.sampled_reading("c").status, "CANCELLED");
    }

    /// The rate a reading carries is the **campaign's**, taken from the runner's
    /// own measurement — nothing here reads `metrics:tick`, which counts the
    /// whole link and would put a unit send made beside the campaign into the
    /// campaign's figure.
    #[tokio::test]
    async fn a_sampled_reading_carries_the_campaign_s_own_throughput() {
        let running = Running::new(SessionId::new(), 200);

        running.progress.publish(CampaignReading {
            tally: CampaignTally {
                accepted: 40,
                ..CampaignTally::default()
            },
            accepted_per_second: 12.5,
        });

        let reading = running.sampled_reading("c");

        assert!((reading.accepted_per_second - 12.5).abs() < f64::EPSILON);
        assert_eq!(reading.accepted, 40);
    }

    /// A sampled reading is never the last one: only the run's own verdict is.
    #[tokio::test]
    async fn a_sampled_reading_never_claims_to_be_the_last() {
        let running = Running::new(SessionId::new(), 200);

        running.control.cancel();

        assert!(
            !running.sampled_reading("c").done,
            "a cancelled campaign is still draining its queue"
        );
        assert!(
            running
                .final_reading("c", CampaignStatus::Cancelled, &CampaignReading::default())
                .done
        );
    }

    // --- the row transition --------------------------------------------------

    /// A campaign picked up after a restart is moved to `RUNNING` again, and the
    /// instant it *first* started sending must survive it — it is what an
    /// operator reads to know how long a campaign has been going.
    #[tokio::test]
    async fn a_second_start_does_not_move_the_instant_the_campaign_began() {
        let first = advance(
            a_campaign(CampaignStatus::Validated),
            CampaignStatus::Running,
            None,
        )
        .expect("a validated campaign starts");
        let began = first.started_at.expect("the first start is stamped");

        let paused =
            advance(first, CampaignStatus::Paused, None).expect("a running campaign pauses");
        let resumed =
            advance(paused, CampaignStatus::Running, None).expect("a paused campaign resumes");

        assert_eq!(resumed.started_at, Some(began));
    }

    #[tokio::test]
    async fn a_transition_the_lifecycle_refuses_changes_nothing() {
        let rejection = advance(
            a_campaign(CampaignStatus::Completed),
            CampaignStatus::Running,
            None,
        )
        .expect_err("a completed campaign does not restart");

        assert_eq!(rejection.from, CampaignStatus::Completed);
        assert_eq!(rejection.to, CampaignStatus::Running);
    }

    /// The durable counters are written only when the campaign ends, and they
    /// come from `messaging`'s own summary — not from a rule restated here.
    #[tokio::test]
    async fn only_a_terminal_transition_carries_the_counters() {
        let tally = CampaignTally {
            accepted: 7,
            failed: 2,
            rejected: 90,
            ..CampaignTally::default()
        };

        let without = advance(
            a_campaign(CampaignStatus::Running),
            CampaignStatus::Paused,
            None,
        )
        .expect("a running campaign pauses");

        assert_eq!(without.sent_count, 0);

        let with = advance(
            a_campaign(CampaignStatus::Running),
            CampaignStatus::Completed,
            Some(tally.summary()),
        )
        .expect("a running campaign completes");

        assert_eq!(with.sent_count, 7);
        assert_eq!(
            with.failed_count, 2,
            "a rejected recipient is not a failure"
        );
    }

    // --- the progress sampler ------------------------------------------------

    /// What the supervisor emitted, in order.
    type Log = Arc<tokio::sync::Mutex<Vec<&'static str>>>;

    fn recorder(
        log: &Log,
        label: &'static str,
    ) -> impl Fn() -> futures_util::future::BoxFuture<'static, ()> {
        let log = Arc::clone(log);

        move || {
            let log = Arc::clone(&log);

            Box::pin(async move {
                log.lock().await.push(label);
            })
        }
    }

    /// **CA-010-11.** The campaign submits ten thousand messages while this
    /// runs; the sampling rate is the interval and nothing else.
    ///
    /// The load is what makes the test say something: a sampler wired to the
    /// send path would produce ten thousand readings here.
    #[tokio::test(start_paused = true)]
    async fn the_sampling_rate_is_the_interval_whatever_the_throughput() {
        let log: Log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let submitted = Arc::new(tokio::sync::Mutex::new(0_u32));

        let campaign = {
            let submitted = Arc::clone(&submitted);

            async move {
                // Ten seconds of traffic at a thousand messages a second.
                for _ in 0..10_000_u32 {
                    *submitted.lock().await += 1;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }

                "completed"
            }
        };

        let outcome = run_reporting(INTERVAL, campaign, recorder(&log, "sample")).await;

        assert_eq!(outcome, "completed");
        assert_eq!(*submitted.lock().await, 10_000, "the load really ran");

        let emitted = log.lock().await.len();

        assert!(
            emitted <= 41,
            "ten seconds at 4 Hz is at most forty readings, not {emitted}"
        );
        assert!(
            emitted >= 39,
            "and it must actually sample: {emitted} readings in ten seconds"
        );
    }

    /// **The last event cannot be lost, and no stale one follows it.**
    ///
    /// The campaign ends *mid-interval*, which is the case that separates a
    /// sampler watching the end from one that only looks between two sleeps: the
    /// obvious `while !finished.is_cancelled() { sleep; sample }` would wake half
    /// an interval later and publish a `RUNNING` reading for a campaign that had
    /// already completed — after the terminal reading in wall-clock terms, and
    /// in front of it on the screen.
    ///
    /// The run marks its own end in the same log, so the assertion is about
    /// order and not about counting.
    ///
    /// Written against the failure milestone 007 shipped on `sessions:state`,
    /// in the shape it would take here.
    #[tokio::test(start_paused = true)]
    async fn no_reading_is_published_after_the_campaign_has_ended() {
        let log: Log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let started = tokio::time::Instant::now();

        let outcome = run_reporting(
            INTERVAL,
            {
                let log = Arc::clone(&log);

                async move {
                    // Deliberately not a whole number of intervals: the sampler
                    // is asleep when the campaign ends.
                    tokio::time::sleep(INTERVAL * 10 + INTERVAL / 2).await;
                    log.lock().await.push("ended");

                    7_u32
                }
            },
            recorder(&log, "sample"),
        )
        .await;

        let elapsed = started.elapsed();

        // The caller's final emission, exactly where `drive` makes it.
        log.lock().await.push("done");

        // Give any sampler that outlived the run every chance to emit again.
        tokio::time::sleep(INTERVAL * 10).await;

        let seen = log.lock().await.clone();
        let ended = seen
            .iter()
            .position(|entry| *entry == "ended")
            .expect("the campaign ended");

        assert_eq!(outcome, 7);
        assert!(ended > 0, "the sampler never ran at all: {seen:?}");
        assert_eq!(
            &seen[ended..],
            &["ended", "done"],
            "a stale reading was published after the campaign ended: {seen:?}"
        );
        assert!(
            elapsed < INTERVAL * 11,
            "the sampler held the campaign open for {elapsed:?}"
        );
    }

    /// A campaign shorter than one interval emits nothing but its final
    /// reading. Two events for three recipients is one repaint too many.
    #[tokio::test(start_paused = true)]
    async fn a_campaign_shorter_than_one_interval_samples_nothing() {
        let log: Log = Arc::new(tokio::sync::Mutex::new(Vec::new()));

        run_reporting(
            INTERVAL,
            async {
                tokio::time::sleep(INTERVAL / 5).await;
            },
            recorder(&log, "sample"),
        )
        .await;

        assert!(log.lock().await.is_empty());
    }
}
