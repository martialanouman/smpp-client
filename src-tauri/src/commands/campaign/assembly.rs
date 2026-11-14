//! CA-010-12 — the M3 recipe, driven through the campaign commands.
//!
//! This is the one thing milestone 010 shipped without: every piece was covered
//! separately — the runner against a fake message centre (`messaging`), the
//! recipient source and the lifecycle against a real database
//! (`crate::campaigns`), the commands' validation (`super::tests`) — and the
//! **assembly** was not. No campaign had ever sent a message from an IPC call to
//! a socket.
//!
//! # Where it attaches, and why there
//!
//! At [`super::launch`], which is what `campaign_start` and `campaign_resume`
//! both reduce to — the first with `resuming` clear, the second with it set.
//! `campaign_create` and `campaign_pause` are called as the commands they are,
//! since neither takes an `AppHandle`.
//!
//! Entering there exercises everything the commands do bar the
//! `#[tauri::command]` macro, and in particular the two steps
//! `CampaignServices::start` does not have: reading `campaigns.send_config` back
//! and **re-validating** it through the same `parse` the create command used.
//!
//! The test is in-crate because `CampaignServices` is `pub(crate)`. Nothing of
//! the production code had its visibility raised for it.
//!
//! # The one piece of scaffolding, and why it is unavoidable
//!
//! [`bridge`]. `SessionServices` holds a `SessionRegistry<TcpTransport>`, a
//! concrete type, so the only way into its registry is a real TCP connection —
//! and `smpp_session::testing::Smsc` speaks over `tokio::io::duplex`. The bridge
//! accepts on a loopback port and copies the bytes both ways, so the scripted
//! message centre is reached through the production transport, codec and session
//! actors. Making `SessionServices` generic would have been the alternative, and
//! it would have changed production code to suit a test.
//!
//! # The three campaigns
//!
//! | | What it proves |
//! |---|---|
//! | the nominal assembly | every recipient gets exactly one `submit_sm`, the counters cover the list, and the last `campaign:progress` carries `done` |
//! | pause and resume | the feeding stops without dropping the link, the resume is in place, and nobody is sent to twice |
//! | the cold restart | a campaign a crash left `RUNNING` is picked up, and the submission count across both runs is still one per recipient |
//!
//! # Determinism
//!
//! No assertion is made on a duration, and nothing here sleeps for an interval
//! and then asserts. Every wait is [`until`], which polls a **condition** with a
//! deadline that only ever makes the failure legible. Tokio's virtual clock is
//! deliberately not used: these tests drive a SQLx pool and a loopback socket,
//! neither of which a paused clock can advance — the same reason
//! `crate::campaigns::tests::every_status_writer_waits_for_the_live_table`
//! gives for its own real-time respite.
//!
//! # What these three tests do NOT cover
//!
//! * **The volume.** Twenty recipients, not five hundred thousand: CA-010-01 is
//!   measured in `messaging::campaign_volume`, where the footprint is sampled,
//!   and a slow test here would be a test somebody disables. So "massive,
//!   end to end through the IPC" stays unverified, and `tasks-todo/step-010.md`
//!   §7 says so.
//! * **CA-010-11 as a measurement.** Nothing is rendered; the event stream is
//!   observed, not a frame rate.
//! * **CA-010-03 in detail.** The pause test asserts that the link survives and
//!   that nobody is sent to twice; "the messages already in the window receive
//!   their response" is asserted where the control is, in
//!   `messaging::campaign::control` and `crate::campaigns::tests`.
//! * **The `SENT`-with-no-answer family under `Reemit`.** The cold-restart test
//!   runs under [`UnansweredInput::Abandon`], which is the policy that *promises*
//!   not to duplicate — so the submission count is an exact equality rather than
//!   a bound. The duplicate-risk arbitration of ADR 0014 is asserted over
//!   arbitrary histories in `messaging::campaign_invariant`.

// `cfg(test)`, so `clippy.toml` already reopens `unwrap`/`expect`/`panic`.
// `block_on` behind `#[tokio::test]` is what the deny list reserves for a
// binary entry point, and a test harness is one.
#![allow(clippy::disallowed_methods)]

use std::collections::BTreeSet;

use core::time::Duration;

use messaging::message::MessageState;
use messaging::CampaignStatus;
use persistence::ports::MessageJournal as _;
use persistence::{
    Campaign, CampaignId, Cursor, Database, DatabaseConfig, MessageFilter, SqliteContactRepository,
    SqliteMessageRepository,
};
use smpp_core::types::{Msisdn, SessionId};
use smpp_session::profile::SessionProfile;
use smpp_session::testing::{a_password, submissions, Script, Seen, Smsc, SubmitReply};
use tauri::test::MockRuntime;
use tauri::{AppHandle, Listener as _, Manager as _};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex;

use contacts::model::{Contact, ContactList, ListId};
use contacts::ports::ContactRepository as _;

use super::{
    CampaignCreateInput, CampaignSendConfigInput, EncodingDto, MissingVariableInput, NpiDto,
    RegisteredDeliveryDto, RetryInput, ScheduleInput, SegmentationModeDto, TonDto, UnansweredInput,
};
use crate::config::{AppConfig, ConfigStore};
use crate::events::CampaignProgressEvent;
use crate::state::AppState;

/// Recipients each campaign is created over.
///
/// Two dozen, as fiche §7 asks: enough that the feeder, the send window and the
/// resume guard all run more than once, and small enough that the test is
/// bounded by the message centre's answers rather than by its own size.
const RECIPIENTS: usize = 20;

/// Submissions the first run of the cold-restart test gets an answer for.
///
/// Eight of twenty, so the resume finds all three families a crash leaves
/// behind: rows already `ACCEPTED`, rows `SENT` with no answer, and recipients
/// with no row at all.
const ANSWERED_BEFORE_THE_CRASH: usize = 8;

/// Rows the crash leaves `SENT` with no answer — measured, not chosen.
///
/// **One**, because the runner awaits each submission's response before taking
/// the next recipient: the back-pressure of this milestone is in the feeder's
/// bounded queue, not in a window of concurrent emissions. That is what makes
/// the crash point observable *and* stable — once this row exists the first run
/// is provably stuck, waiting on an answer the quiet centre will not send for
/// five minutes, so it can neither emit nor settle a status.
const IN_FLIGHT_AT_THE_CRASH: usize = 1;

/// How long a [`until`] condition may take before the test is declared stuck.
///
/// Not a pause and never awaited in full: it is the bound past which a hang is
/// reported as a failure instead of as a timeout of the test harness. Generous,
/// because a loaded machine must make this test slower rather than flakier.
const DEADLINE: Duration = Duration::from_secs(30);

/// How often a condition is re-read while waiting.
const POLL: Duration = Duration::from_millis(5);

/// An open, migrated database in a directory of its own (CLAUDE.md §7).
///
/// A **file**, never `:memory:` — every connection of a SQLx pool onto
/// `:memory:` gets its own database, so the migrations and the reads would land
/// on different ones. `crate::campaigns::tests::store` learnt this the hard way.
async fn database(directory: &tempfile::TempDir) -> Database {
    let database = Database::open(DatabaseConfig::new(directory.path().join("campaign.db")))
        .await
        .expect("the database opens");

    database.migrate().await.expect("the migrations apply");

    database
}

/// An application over `database`, with its handle and the progress it emits.
///
/// `mock_app` is a real Tauri application with no window: `launch` takes an
/// `AppHandle`, and the final `campaign:progress` is emitted through it, so a
/// hand-written double would be observing something other than what ships.
fn app_over(database: Database, directory: &tempfile::TempDir) -> Harness {
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();

    // The same typed event channels the application mounts (`crate::ipc`).
    // Without them `tauri_specta`'s `emit` panics, which would take the
    // campaign's task down with it rather than fail an assertion.
    crate::ipc::events::<MockRuntime>().mount_events(&app);

    // A channel and not a shared `Vec`: a Tauri listener is a **synchronous**
    // closure, so it cannot take a `tokio::sync::Mutex`, and `clippy.toml`
    // forbids the `std` one outright (CLAUDE.md §4). `UnboundedSender::send` is
    // sync and non-blocking, which is all a listener may do.
    let (emitted, inbox): (UnboundedSender<CampaignProgressEvent>, _) = unbounded_channel();

    handle.listen("campaign:progress", move |event| {
        let payload: CampaignProgressEvent =
            serde_json::from_str(event.payload()).expect("the event payload is its own DTO");

        // The receiver outlives the application, so this cannot fail — and if it
        // did there would be nobody left to assert anything.
        let _ignored = emitted.send(payload);
    });

    handle.manage(AppState::new(
        ConfigStore::new(directory.path()),
        AppConfig::default(),
        database,
    ));

    Harness {
        _app: app,
        handle,
        progress: Mutex::new(Progress {
            inbox,
            seen: Vec::new(),
        }),
    }
}

/// Every `campaign:progress` the listener handed over, and the backlog.
struct Progress {
    inbox: UnboundedReceiver<CampaignProgressEvent>,
    seen: Vec<CampaignProgressEvent>,
}

/// One application under test, and what it emitted.
struct Harness {
    /// Kept so the application — and the listener on it — outlives the test.
    ///
    /// Never read: holding it IS its purpose. Dropping a `tauri::App` takes its
    /// event listeners and its managed state with it, so a harness that only
    /// kept the handle would stop receiving `campaign:progress` the moment the
    /// builder's return value went out of scope.
    _app: tauri::App<MockRuntime>,
    handle: AppHandle<MockRuntime>,
    progress: Mutex<Progress>,
}

impl Harness {
    /// Every `campaign:progress` emitted so far, oldest first.
    ///
    /// Drains the listener's channel into a backlog, so repeated calls — which
    /// is what polling a condition does — keep returning the whole history
    /// rather than only what has arrived since the last read.
    async fn progress(&self) -> Vec<CampaignProgressEvent> {
        let mut progress = self.progress.lock().await;

        while let Ok(event) = progress.inbox.try_recv() {
            progress.seen.push(event);
        }

        progress.seen.clone()
    }

    /// Binds a session onto `smsc` and waits for it to be `BOUND`.
    ///
    /// Through the production registry and the production `TcpTransport`, over
    /// [`bridge`]. The returned `Session` is kept by the caller: dropping its
    /// last handle cancels the session (`HandleInner::drop`).
    async fn bind_to(&self, session_id: SessionId, smsc: Smsc) -> smpp_session::Session {
        let port = bridge(smsc).await;
        let profile = SessionProfile::builder(session_id, "double", "127.0.0.1", port)
            .system_id("esme01")
            // No `enquire_link`, and a response timeout at its ceiling. Both are
            // for the cold-restart test: the first run is left blocked on
            // answers that never come, and this is what keeps it blocked for
            // five minutes rather than ten seconds — long enough that it cannot
            // wake up and write a status while the second run is working. The
            // residue is stated in `cold_restart`.
            .enquire_link_s(0)
            .response_timeout_s(300)
            .build()
            .expect("the fixture is a valid profile");

        // The profile row first: `messages.session_id` is a foreign key onto
        // `session_profiles`, and the write-ahead insert of every campaign
        // message fails against an unknown session. The application cannot bind
        // a profile it has not stored either (`session_bind` reads the row), so
        // storing it is part of the assembly rather than a fixture detail.
        persistence::ports::SessionProfileRepository::upsert_session_profile(
            self.state().sessions().profiles(),
            &profile.to_record(),
        )
        .await
        .expect("the profile row is written");

        let session = self
            .state()
            .sessions()
            .registry()
            .bind(profile, a_password())
            .await
            .expect("the registry has room");

        smpp_session::testing::wait_until_bound(&session.handle).await;

        session
    }

    /// The managed state, as a command receives it.
    fn state(&self) -> tauri::State<'_, AppState> {
        self.handle.state::<AppState>()
    }

    /// Creates a campaign through the real command, and returns its identifier.
    async fn create(&self, config: CampaignSendConfigInput) -> String {
        super::campaign_create(
            self.state(),
            CampaignCreateInput {
                name: String::from("recette M3"),
                template: String::from("Bonjour {{prenom}}"),
                config,
            },
        )
        .await
        .expect("the configuration is valid and the list is not empty")
    }

    /// Reads the campaign row back.
    async fn row(&self, campaign_id: &str) -> Campaign {
        self.state()
            .campaigns()
            .find(super::parse_id(campaign_id).expect("a valid identifier"))
            .await
            .expect("the row is there")
    }
}

/// Stands a loopback listener in front of `smsc`, and returns its port.
///
/// One task per connection, copying bytes in both directions: the session under
/// test connects with the production `TcpTransport`, and the scripted centre
/// answers over its duplex pipe. Both tasks end when either side closes.
async fn bridge(smsc: Smsc) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available");
    let port = listener
        .local_addr()
        .expect("a bound listener has an address")
        .port();

    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let Ok(mut centre) = smpp_session::Transport::connect(&smsc, "in-memory").await else {
                continue;
            };

            tokio::spawn(async move {
                let _ignored = tokio::io::copy_bidirectional(&mut client, &mut centre).await;
            });
        }
    });

    port
}

/// The numbers the campaigns are sent to, in the order they are inserted.
fn numbers() -> Vec<Msisdn> {
    (0..RECIPIENTS)
        .map(|index| {
            Msisdn::parse(&format!("+22507{:08}", index + 1)).expect("the fixture is valid")
        })
        .collect()
}

/// Writes `RECIPIENTS` contacts into a list, and returns the list.
///
/// Through the real contact repository, so the campaign reads them with the
/// production `stream_contacts` and the adapter under
/// `crate::campaigns::ContactRecipients`. Each one carries a `prenom`, which is
/// what the template resolves (CA-010-06).
async fn a_list(database: &Database) -> ListId {
    let repository = SqliteContactRepository::new(database.clone());
    let list = ListId::new();

    repository
        .insert_contact_list(&ContactList {
            list_id: list,
            name: String::from("recette"),
            created_at: persistence::Timestamp::now(),
        })
        .await
        .expect("the list is written");

    let contacts: Vec<Contact> = numbers()
        .into_iter()
        .enumerate()
        .map(|(index, msisdn)| Contact {
            contact_id: contacts::model::ContactId::new(),
            msisdn,
            country: Some(String::from("CI")),
            valid: true,
            line_type: None,
            attributes: Some(format!(r#"{{"prenom":"client{index}"}}"#)),
            source: Some(String::from("test")),
            created_at: persistence::Timestamp::now(),
        })
        .collect();

    let ids: Vec<_> = contacts.iter().map(|contact| contact.contact_id).collect();

    repository
        .insert_contacts(&contacts)
        .await
        .expect("the contacts are written");
    repository
        .add_contacts_to_list(list, &ids)
        .await
        .expect("they join the list");

    list
}

/// The send configuration the tests create their campaign with.
fn a_config(
    session_id: SessionId,
    list: ListId,
    unanswered: UnansweredInput,
) -> CampaignSendConfigInput {
    CampaignSendConfigInput {
        session_id: session_id.to_string(),
        list_id: Some(list.to_string()),
        excluded_list_ids: Vec::new(),
        source: Some(String::from("ShinobiSMS")),
        dest_ton: TonDto::International,
        dest_npi: NpiDto::Isdn,
        encoding: EncodingDto::Automatic,
        segmentation_mode: SegmentationModeDto::Udh,
        // No receipt is asked for: the double does not send any, and a campaign
        // waiting for one would be asserting on milestone 008 rather than on
        // this assembly.
        registered_delivery: RegisteredDeliveryDto::None,
        on_missing_variable: MissingVariableInput::Reject,
        on_unanswered: unanswered,
        retry: RetryInput::default(),
        schedule: ScheduleInput::default(),
    }
}

/// Waits until `condition` holds, or fails saying what it was waiting for.
///
/// A condition and not a duration: nothing in these tests knows how long a
/// SQLite write or a loopback round trip takes, and a `sleep` long enough to be
/// safe on a loaded machine would be the slowest part of the suite.
async fn until<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: core::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + DEADLINE;

    while std::time::Instant::now() < deadline {
        if condition().await {
            return;
        }

        tokio::time::sleep(POLL).await;
    }

    panic!("timed out waiting for {what}");
}

/// One of the progress DTO's `u32` counters, as a `usize` to compare with.
fn counted(value: u32) -> usize {
    usize::try_from(value).expect("a campaign of twenty messages fits in a usize")
}

/// One of the journal's `u64` counts, likewise.
fn counted64(value: u64) -> usize {
    usize::try_from(value).expect("a campaign of twenty messages fits in a usize")
}

/// Messages of one campaign in one state.
async fn in_state(database: &Database, campaign_id: CampaignId, state: MessageState) -> usize {
    let count = SqliteMessageRepository::new(database.clone())
        .count_messages(
            &MessageFilter::all()
                .for_campaign(campaign_id)
                .in_state(state),
        )
        .await
        .expect("the journal answers");

    counted64(count)
}

/// Every `client_message_id` the campaign journalled, as a set.
///
/// A `BTreeSet`, so "all distinct" and "as many as there are recipients" are the
/// same assertion — which is how CA-010-04 is worded.
async fn journalled_keys(database: &Database, campaign_id: CampaignId) -> BTreeSet<String> {
    let page = SqliteMessageRepository::new(database.clone())
        .page_messages(
            &MessageFilter::all().for_campaign(campaign_id),
            Cursor::start(),
            // Above `RECIPIENTS`, so one page is the whole campaign.
            500,
        )
        .await
        .expect("the journal answers");

    assert_eq!(page.next, None, "the whole campaign fits in one page");

    let rows = page.items.len();
    let keys: BTreeSet<String> = page
        .items
        .into_iter()
        .map(|message| message.client_message_id.to_string())
        .collect();

    // Stated rather than relied upon: `client_message_id` is the primary key of
    // `messages`, so the two can only differ if the set were built wrongly.
    assert_eq!(keys.len(), rows, "every journalled identifier is distinct");

    keys
}

/// The recipients of every `submit_sm` the centre saw, in order.
fn submitted_to(seen: &mut UnboundedReceiver<Seen>) -> Vec<String> {
    submissions(&smpp_session::testing::drain(seen))
        .into_iter()
        .map(|(_, pdu)| pdu.destination_addr.as_str().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// 1 — the nominal assembly
// ---------------------------------------------------------------------------

/// A campaign created and started through the commands reaches every recipient
/// exactly once, and says so.
///
/// Five things, and each one was unverified through the assembly until now:
///
/// * the campaign ends `COMPLETED`;
/// * `total == sent + failed + cancelled` against the stored row (CA-010-02);
/// * the message centre saw **exactly one** `submit_sm` per recipient;
/// * the journal holds one distinct `client_message_id` per recipient
///   (CA-010-04, as that criterion words it);
/// * `campaign:progress` was emitted, and the **last** one carries `done`.
///   ADR 0015 claims that event cannot be lost; nothing had checked the claim
///   through a real `AppHandle`.
#[tokio::test]
async fn a_campaign_started_through_the_command_reaches_every_recipient_once() {
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let database = database(&directory).await;
    let list = a_list(&database).await;
    let harness = app_over(database.clone(), &directory);

    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session_id = SessionId::new();
    let _session = harness.bind_to(session_id, smsc).await;

    let campaign_id = harness
        .create(a_config(session_id, list, UnansweredInput::Reemit))
        .await;
    let parsed = super::parse_id(&campaign_id).expect("a valid identifier");

    super::launch(&harness.handle, &harness.state(), &campaign_id, false)
        .await
        .expect("the campaign starts");

    // The final event is emitted after the terminal row is written, so waiting
    // for it also means the row has settled. Waiting on the row instead would
    // race the emission this test asserts on.
    until("the campaign to report that it is done", || async {
        harness
            .progress()
            .await
            .last()
            .is_some_and(|last| last.done)
    })
    .await;

    let row = harness.row(&campaign_id).await;

    assert_eq!(row.status, CampaignStatus::Completed);
    assert_eq!(
        u64::from(row.total_count),
        u64::from(row.sent_count) + u64::from(row.failed_count),
        "CA-010-02: the row's counters must cover every recipient"
    );
    assert_eq!(counted(row.sent_count), RECIPIENTS);

    let recipients = submitted_to(&mut seen);

    assert_eq!(
        recipients.len(),
        RECIPIENTS,
        "one submit_sm per recipient, no more and no fewer"
    );
    assert_eq!(
        recipients.iter().cloned().collect::<BTreeSet<_>>(),
        numbers()
            .iter()
            .map(|number| number.as_str().to_owned())
            .collect::<BTreeSet<_>>(),
        "every recipient of the list, and nobody else"
    );

    assert_eq!(
        journalled_keys(&database, parsed).await.len(),
        RECIPIENTS,
        "CA-010-04: as many distinct client_message_id as recipients"
    );
    assert_eq!(
        in_state(&database, parsed, MessageState::Accepted).await,
        RECIPIENTS
    );

    let progress = harness.progress().await;
    let last = progress.last().expect("at least one reading was emitted");

    assert_eq!(last.status, CampaignStatus::Completed.as_str());
    assert_eq!(counted(last.processed), RECIPIENTS);
    assert_eq!(counted(last.accepted), RECIPIENTS);
    assert!(
        progress.iter().rev().skip(1).all(|event| !event.done),
        "only the last reading may carry `done`"
    );
}

// ---------------------------------------------------------------------------
// 2 — the cold restart
// ---------------------------------------------------------------------------

/// A campaign a crash left behind is resumed, and no recipient is sent to twice.
///
/// # How the crash is staged
///
/// The message centre answers [`ANSWERED_BEFORE_THE_CRASH`] submissions and then
/// goes quiet. The first run is then provably stuck — see
/// [`IN_FLIGHT_AT_THE_CRASH`] — and the application state is dropped at that
/// point, with no `cancel` and no `shutdown`. What is left is what a `kill -9`
/// leaves: a row reading `RUNNING`, eight rows `ACCEPTED`, one row `SENT` with
/// no answer, and eleven recipients with no row at all. All three families the
/// resume guard arbitrates, in one state.
///
/// A second application is then built **over the same database**, binds a
/// healthy centre under the same session identifier, and resumes.
///
/// ## The residue, stated rather than hidden
///
/// The first run's task is not killed — a test in one process cannot do that,
/// and the `JoinHandle` belongs to `CampaignServices`. It is *blocked*, and the
/// five-minute response timeout is what keeps it so while the second run works.
/// The property being asserted does not depend on that: the write-ahead key is
/// derived (ADR 0014), so even an awakened first run could not emit to a
/// recipient the second one has already journalled — the primary key refuses the
/// insert. The timeout only keeps it from writing a terminal status underneath
/// the assertions.
///
/// # What is asserted
///
/// * the resume reaches `COMPLETED`;
/// * **exactly one `submit_sm` per recipient across both phases** — CA-010-04
///   and CA-010-05 together, counted on the two message centres' own records;
/// * one distinct `client_message_id` per recipient;
/// * the rows the first run had `ACCEPTED` are still `ACCEPTED`, so the resume
///   skipped them rather than sending again (CA-010-05).
#[tokio::test]
async fn a_campaign_a_crash_left_behind_is_resumed_without_sending_twice() {
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let database = database(&directory).await;
    let list = a_list(&database).await;
    let session_id = SessionId::new();

    // --- the run that dies -------------------------------------------------
    let (dying_centre, mut dying_seen) = Smsc::always(Script::Accept);
    let dying_centre = dying_centre.answering_submits_with(
        vec![SubmitReply::Accept; ANSWERED_BEFORE_THE_CRASH],
        SubmitReply::Silent,
    );

    let campaign_id = {
        let harness = app_over(database.clone(), &directory);
        let _session = harness.bind_to(session_id, dying_centre).await;

        // `Abandon`, the policy that promises not to duplicate: a `SENT` row
        // whose answer never came is left alone rather than replayed, which is
        // what makes the submission count below an exact equality. The default
        // `Reemit` deliberately risks a duplicate (ADR 0014), and
        // `messaging::campaign_invariant` is where that arbitration is asserted.
        let campaign_id = harness
            .create(a_config(session_id, list, UnansweredInput::Abandon))
            .await;

        super::launch(&harness.handle, &harness.state(), &campaign_id, false)
            .await
            .expect("the campaign starts");

        let parsed = super::parse_id(&campaign_id).expect("a valid identifier");

        until(
            "the run to block on an answer that will not come",
            || async {
                in_state(&database, parsed, MessageState::Accepted).await
                    == ANSWERED_BEFORE_THE_CRASH
                    && in_state(&database, parsed, MessageState::Sent).await
                        == IN_FLIGHT_AT_THE_CRASH
            },
        )
        .await;

        assert_eq!(
            harness.row(&campaign_id).await.status,
            CampaignStatus::Running,
            "the row a crash leaves behind reads RUNNING"
        );

        campaign_id
        // The state, the live table and the session handle go here. Nothing was
        // cancelled and nothing was shut down.
    };

    let parsed = super::parse_id(&campaign_id).expect("a valid identifier");

    assert_eq!(
        harness_free_row(&database, parsed).await,
        CampaignStatus::Running,
        "and it still reads RUNNING once the process that wrote it is gone"
    );

    // --- the restart -------------------------------------------------------
    let (healthy_centre, mut healthy_seen) = Smsc::always(Script::Accept);
    let restarted = app_over(database.clone(), &directory);
    let _session = restarted.bind_to(session_id, healthy_centre).await;

    super::launch(&restarted.handle, &restarted.state(), &campaign_id, true)
        .await
        .expect("a campaign left RUNNING is resumed");

    until("the resumed campaign to report that it is done", || async {
        restarted
            .progress()
            .await
            .last()
            .is_some_and(|last| last.done)
    })
    .await;

    assert_eq!(
        restarted.row(&campaign_id).await.status,
        CampaignStatus::Completed
    );

    // THE ASSERTION OF THE RECIPE: every recipient saw exactly one submit_sm,
    // counted on the two centres rather than on the client's own figures.
    let mut delivered = submitted_to(&mut dying_seen);
    delivered.extend(submitted_to(&mut healthy_seen));

    let distinct: BTreeSet<_> = delivered.iter().cloned().collect();

    assert_eq!(
        delivered.len(),
        RECIPIENTS,
        "a recipient was submitted to twice across the restart: {delivered:?}"
    );
    assert_eq!(distinct.len(), RECIPIENTS, "and each one exactly once");
    assert_eq!(
        distinct,
        numbers()
            .iter()
            .map(|number| number.as_str().to_owned())
            .collect::<BTreeSet<_>>()
    );

    assert_eq!(
        journalled_keys(&database, parsed).await.len(),
        RECIPIENTS,
        "CA-010-04: the derived key keeps one row per recipient across restarts"
    );

    // CA-010-05, read off the rows: what the first run had accepted is
    // untouched, and the resume reported it as a skip rather than an emission.
    assert_eq!(
        in_state(&database, parsed, MessageState::Accepted).await,
        RECIPIENTS - IN_FLIGHT_AT_THE_CRASH,
        "every recipient but the abandoned one ended accepted"
    );
    assert_eq!(
        in_state(&database, parsed, MessageState::Sent).await,
        IN_FLIGHT_AT_THE_CRASH,
        "`Abandon` left the unanswered row alone rather than sending it again"
    );

    let last = restarted
        .progress()
        .await
        .last()
        .cloned()
        .expect("a reading was emitted");

    assert_eq!(counted(last.processed), RECIPIENTS);
    assert_eq!(
        counted(last.skipped),
        ANSWERED_BEFORE_THE_CRASH + IN_FLIGHT_AT_THE_CRASH,
        "the resume skipped exactly the rows the crash left behind: {last:?}"
    );
}

/// The campaign's status, read without an application.
///
/// Proof that the row survives the process that wrote it: it is read from a
/// repository built on the spot, not from a state the test is still holding.
async fn harness_free_row(database: &Database, campaign_id: CampaignId) -> CampaignStatus {
    use persistence::ports::CampaignRepository as _;

    persistence::SqliteCampaignRepository::new(database.clone())
        .find_campaign(campaign_id)
        .await
        .expect("the store answers")
        .expect("the row outlives the application that wrote it")
        .status
}

// ---------------------------------------------------------------------------
// 3 — pause and resume, mid-flight
// ---------------------------------------------------------------------------

/// Answering delay of the message centre in the pause test.
///
/// The only place these tests inject a duration, and it is injected into the
/// **double** rather than waited on: it stretches a campaign of [`RECIPIENTS`]
/// sequential submissions over about a second, so the pause lands while the
/// campaign is still feeding instead of racing its completion. Nothing is
/// asserted about it, and a loaded machine only makes the window wider.
const ANSWERING_DELAY: Duration = Duration::from_millis(50);

/// A campaign paused mid-flight resumes in place and still reaches everybody
/// once.
///
/// The third of the three things CA-010-12 names — "with pause/resume […] along
/// the way" — and the one the two tests above do not touch. It goes through the
/// real `campaign_pause`, and through `resume_in_place`, which is the whole of
/// `campaign_resume` for a campaign this process is still running: the other
/// branch of that command, the one that re-launches a run, is what
/// `a_campaign_a_crash_left_behind_is_resumed_without_sending_twice` exercises.
///
/// What is asserted beyond the counters is that the **session stays bound**
/// across the pause (CA-010-03): a pause that dropped the link would be a
/// reconnection and a fresh bind on the message centre, which is what the
/// criterion forbids in so many words.
#[tokio::test]
async fn a_campaign_paused_mid_flight_resumes_and_still_reaches_everybody_once() {
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let database = database(&directory).await;
    let list = a_list(&database).await;
    let harness = app_over(database.clone(), &directory);

    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session_id = SessionId::new();
    let session = harness
        .bind_to(session_id, smsc.with_latency(ANSWERING_DELAY))
        .await;

    let campaign_id = harness
        .create(a_config(session_id, list, UnansweredInput::Reemit))
        .await;
    let parsed = super::parse_id(&campaign_id).expect("a valid identifier");

    super::launch(&harness.handle, &harness.state(), &campaign_id, false)
        .await
        .expect("the campaign starts");

    until("the campaign to accept its first message", || async {
        in_state(&database, parsed, MessageState::Accepted).await >= 1
    })
    .await;

    super::campaign_pause(harness.state(), campaign_id.clone())
        .await
        .expect("a running campaign pauses");

    let paused = harness.row(&campaign_id).await;

    assert_eq!(paused.status, CampaignStatus::Paused);
    assert!(
        session.handle.snapshot().state.is_bound(),
        "CA-010-03: pausing suspends the feeding, it does not drop the link"
    );
    assert!(
        harness.state().campaigns().is_running(parsed).await,
        "the run is still live, which is why the resume is in place"
    );

    // The whole of `campaign_resume` for a campaign this process is running.
    assert!(
        harness
            .state()
            .campaigns()
            .resume_in_place(parsed)
            .await
            .expect("a paused campaign resumes"),
        "a live campaign resumes in place rather than starting a second run"
    );

    until("the resumed campaign to report that it is done", || async {
        harness
            .progress()
            .await
            .last()
            .is_some_and(|last| last.done)
    })
    .await;

    assert_eq!(
        harness.row(&campaign_id).await.status,
        CampaignStatus::Completed
    );

    let recipients = submitted_to(&mut seen);
    let distinct: BTreeSet<_> = recipients.iter().cloned().collect();

    assert_eq!(
        recipients.len(),
        RECIPIENTS,
        "CA-010-03: the campaign picked up where it stopped, with no duplicate"
    );
    assert_eq!(distinct.len(), RECIPIENTS);
    assert_eq!(journalled_keys(&database, parsed).await.len(), RECIPIENTS);
}
