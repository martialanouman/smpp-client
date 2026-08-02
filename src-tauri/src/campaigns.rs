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
use messaging::{CampaignControl, CampaignProgress, CampaignStatus};
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
    running: Mutex<HashMap<CampaignId, Arc<Running>>>,
}

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

        let running = Arc::new(Running {
            control: CampaignControl::new(),
            progress: Arc::new(CampaignProgress::new()),
            session_id,
            total: campaign.total_count,
        });

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
        self.write_status(campaign_id, CampaignStatus::Running)
            .await?;

        table.insert(campaign_id, Arc::clone(&running));
        drop(table);

        let inner = Arc::clone(&self.inner);
        let app = app.clone();

        tauri::async_runtime::spawn(async move {
            inner
                .drive(app, campaign_id, session, plan, selection, running)
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
        let table = self.inner.running.lock().await;

        self.write_status(campaign_id, CampaignStatus::Paused)
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
        let table = self.inner.running.lock().await;

        let Some(running) = table.get(&campaign_id).map(Arc::clone) else {
            return Ok(false);
        };

        self.write_status(campaign_id, CampaignStatus::Running)
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
        let table = self.inner.running.lock().await;

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
                self.write_status(campaign_id, CampaignStatus::Cancelled)
                    .await?;
            }
        }

        Ok(())
    }

    /// Stops every running campaign. Called when the application exits.
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
        campaign_id: CampaignId,
        next: CampaignStatus,
    ) -> Result<(), ErrorDto> {
        let campaign = self.read(campaign_id).await?;

        // THE MACHINE DECIDES, not this layer. `messaging::CampaignStatus`
        // enumerates the transitions of spec §10.3 and refuses the rest; a
        // second reading of the same rules here would be a second reading to
        // keep in step.
        let status = campaign
            .status
            .try_move_to(next)
            .map_err(|rejection| ErrorDto::campaign_invalid_transition(&rejection))?;

        let mut updated = campaign;

        updated.status = status;

        if status == CampaignStatus::Running && updated.started_at.is_none() {
            updated.started_at = Some(Timestamp::now());
        }

        if status.is_terminal() {
            updated.completed_at = Some(Timestamp::now());
        }

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
            || async {
                // The reading is taken HERE, when the sampler wakes, and never
                // prepared in advance — the same rule the session forwarder
                // learned at milestone 007: a payload assembled before the wait
                // is a payload that is already stale when it is emitted.
                let tally = running.progress.snapshot();

                self.publish(
                    &app,
                    &rendered,
                    &running,
                    CampaignStatus::Running,
                    &tally,
                    false,
                )
                .await;
            },
        )
        .await;

        let (status, tally) = match outcome {
            Ok(outcome) => (outcome.status, outcome.tally),
            Err(error) => {
                tracing::error!(error = %error, "the campaign stopped on a journal failure");

                // The counters are still the truth of what happened before the
                // journal gave out; they are simply not the counters of a
                // campaign that covered its recipients.
                (CampaignStatus::Failed, running.progress.snapshot())
            }
        };

        // Leaving the table and writing the terminal status are ONE step, under
        // the lock `CampaignServices::start` also holds while it writes
        // `RUNNING`. Split in two, the two writers interleave: a campaign
        // restarted in the gap reads `RUNNING`, and this task writes
        // `COMPLETED` over a campaign that is sending.
        {
            let mut table = self.running.lock().await;

            table.remove(&campaign_id);
            self.settle(campaign_id, status, &tally).await;
        }

        // THE LAST EVENT, and it is unconditional. The sampler has returned, so
        // nothing can arrive after it; the emitter applies no throttle, so
        // nothing can drop it. Milestone 007 shipped a `sessions:state` whose
        // rate limit swallowed the last transition and left the screen on
        // `CONNECTING` for ever — this is the same defect, on the event that
        // says a campaign of two hundred thousand messages has finished.
        self.publish(&app, &rendered, &running, status, &tally, true)
            .await;
    }

    /// Emits one reading.
    async fn publish<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        campaign_id: &str,
        running: &Running,
        status: CampaignStatus,
        tally: &CampaignTally,
        done: bool,
    ) {
        self.events.emit_campaign_progress(
            app,
            &CampaignProgressEvent::of(
                campaign_id,
                &running.session_id.to_string(),
                status,
                running.total,
                tally,
                done,
            ),
        );
    }

    /// Writes the terminal status and the final counters onto the row.
    ///
    /// A failure here is logged rather than propagated: the campaign is over,
    /// the journal holds every message it sent, and there is nobody left to
    /// return an error to. The row is a summary of the `messages` table, which
    /// is the record (spec §17.6).
    async fn settle(&self, campaign_id: CampaignId, status: CampaignStatus, tally: &CampaignTally) {
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

        let Ok(status) = stored.status.try_move_to(status) else {
            tracing::warn!(
                from = %stored.status,
                to = %status,
                "the campaign ended in a status the lifecycle refuses from where the row stands"
            );

            return;
        };

        let mut updated = stored;

        updated.status = status;
        updated.sent_count = narrow(tally.accepted);
        updated.failed_count = narrow(tally.failed);
        updated.completed_at = Some(Timestamp::now());

        if let Err(error) = self.campaigns.upsert_campaign(&updated).await {
            tracing::error!(error = %error, "the campaign outcome could not be written");
        }
    }
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

    use super::{run_reporting, ContactRecipients, Duration};
    use contacts::model::{Contact, ContactId, ContactList, ListId};
    use futures_util::StreamExt as _;
    use messaging::ports::RecipientSource as _;
    use persistence::{
        Database, DatabaseConfig, ListSelection, SqliteContactRepository, Timestamp,
    };
    use smpp_core::types::Msisdn;
    use std::sync::Arc;

    const INTERVAL: Duration = Duration::from_millis(250);

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

    // NO TEST for "the traversal order is stable across two runs", although
    // `RecipientSource` states it as a requirement on its implementor.
    //
    // One was written and then deleted: it read the same store twice and
    // compared the two lists, which passes whatever the query does — SQLite
    // returns rows in rowid order with or without an `ORDER BY`, so the
    // assertion could not fail even with the ordering removed. What actually
    // holds the property is `ORDER BY contacts.rowid` in
    // `persistence::repositories::contacts::stream_contacts`, and that crate's
    // own tests are where it belongs.

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
