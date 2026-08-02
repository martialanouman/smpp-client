//! The PDU log, from the socket to the store — CA-008-09.
//!
//! `receipts.rs` proves that the observer is *called*. These tests prove what
//! the acceptance criterion actually asks: that with the switch on, the entries
//! reach the store, and that with it off there are none.
//!
//! # What is under test is the chain, not a piece of it
//!
//! ```text
//! socket → reader/writer → PduObserver::saw → bounded queue → task → PduRecorder → store
//! ```
//!
//! Every link is the real one. The session is the real session, driven against
//! the in-memory message centre; the recorder is `logging_export::PduRecorder`,
//! with its switch and its batching; only the store underneath it is a double,
//! because SQLite is `persistence`'s business and not this criterion's.
//!
//! The observer in the middle is assembled here rather than imported: the
//! application's own lives in `src-tauri`, which is a binary and cannot be
//! depended on. It is fifteen lines, and they are the fifteen the contract is
//! about — `try_send`, and a `warn!` instead of an `await`.
//!
//! # The dev-dependency on `logging-export`, and why it is not a layer break
//!
//! The library does not depend on that crate and must not (CLAUDE.md §3): the
//! whole reason `PduObserver` is declared *here* is so the recorder can sit
//! above without this crate ever naming it. An integration test is a separate
//! crate, compiled after the library and unable to affect what it exports, so
//! the edge exists under `cargo test` and nowhere else.

// `tests/` is compiled without `cfg(test)`, so the relaxations of
// `clippy.toml` do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: `#[tokio::test]` expands to `Runtime::block_on`,
//     which `clippy.toml` reserves for "the binary entry point". A test
//     harness is one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use core::time::Duration;

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use logging_export::{LoggingExportError, PduRecorder, PduSink};
use persistence::{PduDirection, PduLogEntry};
use smpp_core::codec::{Command, Pdu};
use smpp_core::pdus::SubmitSm;
use smpp_core::time::{Clock, Timestamp};
use smpp_core::types::SessionId;
use smpp_core::values::CommandId;
use smpp_session::state::{BindMode, SessionState};
use smpp_session::testing::{
    a_password, a_profile, acknowledged, drain, submissions, wait_until_bound, Script, Seen, Smsc,
};
use smpp_session::{PduFlow, PduObserver};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Long enough for every queued PDU to have crossed the socket **and** for the
/// draining task to have handed it to the recorder.
///
/// Under `start_paused` this is virtual time: the runtime advances the clock
/// only once every task is idle, so it costs nothing and is not a race.
const SETTLE: Duration = Duration::from_secs(1);

/// PDUs held between the socket and the recorder in these tests.
///
/// Far above what any of them produces, so the queue is never what a failure
/// is about — except in the one test whose subject it is, which sets its own.
const QUEUE_CAPACITY: usize = 1_024;

/// The `sequence_number` a bind carries.
///
/// The handshake happens before the correlation table exists, so it does not
/// draw from it — `connection` numbers it 1. Stated here rather than read off
/// the message centre because the double reports the credentials of a bind, not
/// its sequence.
const BIND_SEQUENCE: u32 = 1;

/// The recorder these tests drive.
type Recorder = PduRecorder<Store, FrozenClock>;

/// What the observer hands the draining task.
type Recorded = (SessionId, PduDirection, Command);

/// One recorded PDU, reduced to what an assertion is about.
type Line = (PduDirection, u32, u32);

// ---------------------------------------------------------------------------
// The store, the clock, and the observer that stands in for `src-tauri`'s
// ---------------------------------------------------------------------------

/// The `pdu_log` table, in memory.
///
/// `std::sync::Mutex` and not the Tokio one: every critical section here is a
/// `Vec` push or a clone, so no guard can reach an `.await` — the case
/// `clippy.toml` bans the std lock for.
#[derive(Clone, Default)]
#[allow(clippy::disallowed_types)]
struct Store(Arc<std::sync::Mutex<Vec<PduLogEntry>>>);

impl Store {
    /// Everything written so far.
    fn entries(&self) -> Vec<PduLogEntry> {
        self.0.lock().expect("uncontended").clone()
    }

    /// Every entry as `(direction, command_id, sequence_number)`.
    ///
    /// The identity CA-008-09 names: a PDU is told apart by its sequence
    /// number, and the direction is what makes a request distinct from the
    /// response that echoes it.
    fn lines(&self) -> Vec<Line> {
        self.entries()
            .iter()
            .map(|entry| {
                (
                    entry.direction,
                    entry
                        .command_id
                        .expect("a recorded PDU carries its command"),
                    entry
                        .sequence_number
                        .expect("a recorded PDU carries its sequence"),
                )
            })
            .collect()
    }
}

impl PduSink for Store {
    async fn record(&self, entries: &[PduLogEntry]) -> Result<u64, LoggingExportError> {
        self.0
            .lock()
            .expect("uncontended")
            .extend_from_slice(entries);

        Ok(entries.len() as u64)
    }
}

/// A clock that never moves: the timestamp is not what any of this is about.
struct FrozenClock;

impl Clock for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::parse("2026-07-27T12:00:00Z").expect("valid instant")
    }
}

/// Hands every PDU to a bounded queue, and never blocks.
///
/// The contract of [`PduObserver`] in full: a `try_send` cannot await, so the
/// reader and the writer are never paced by what happens to the entry
/// afterwards, and a full queue is **dropped** rather than waited on — a lost
/// debug entry must not cost a message.
struct Queueing {
    entries: mpsc::Sender<Recorded>,
    /// Entries the queue had no room for. The subject of one test below.
    dropped: AtomicUsize,
}

impl Queueing {
    /// How many entries were dropped for want of room.
    fn dropped(&self) -> usize {
        self.dropped.load(Ordering::SeqCst)
    }
}

impl PduObserver for Queueing {
    fn saw(&self, session_id: SessionId, flow: PduFlow, command: &Command) {
        let direction = match flow {
            PduFlow::Inbound => PduDirection::Inbound,
            PduFlow::Outbound => PduDirection::Outbound,
        };

        // Cloned rather than borrowed: the recorder runs in another task and
        // the socket's loop moves on.
        if self
            .entries
            .try_send((session_id, direction, command.clone()))
            .is_err()
        {
            self.dropped.fetch_add(1, Ordering::SeqCst);

            tracing::warn!(%session_id, "the PDU log queue is full or closed; an entry was dropped");
        }
    }
}

/// An observer over a queue of `capacity`, and the queue's other end.
fn queueing(capacity: usize) -> (Arc<Queueing>, mpsc::Receiver<Recorded>) {
    let (entries, inbox) = mpsc::channel(capacity);

    (
        Arc::new(Queueing {
            entries,
            dropped: AtomicUsize::new(0),
        }),
        inbox,
    )
}

/// Starts the task that drains the queue into the recorder.
///
/// This is where the database write happens in the application, and it is why
/// `saw` can be synchronous. The returned handle is held by the test so the
/// task is never an orphan.
fn draining(recorder: Arc<Recorder>, mut inbox: mpsc::Receiver<Recorded>) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some((session_id, direction, command)) = inbox.recv().await {
            if let Err(error) = recorder
                .observe(Some(session_id), direction, &command)
                .await
            {
                tracing::warn!(error = %error, "a PDU could not be recorded");
            }
        }
    })
}

/// A recorder over a fresh store, switched **off** as it always starts.
fn a_recorder() -> (Arc<Recorder>, Store) {
    let store = Store::default();

    (
        Arc::new(PduRecorder::new(store.clone(), FrozenClock)),
        store,
    )
}

/// The `submit_sm` these tests send. Its content is irrelevant; its sequence
/// number is the whole point, and the session allocates that.
fn a_submit() -> Pdu {
    Pdu::SubmitSm(SubmitSm::default())
}

/// `(direction, command_id, sequence)`, as [`Store::lines`] records it.
fn line(direction: PduDirection, operation: CommandId, sequence: u32) -> Line {
    (direction, u32::from(operation), sequence)
}

/// The sequence number of the `submit_sm` the centre has just seen.
///
/// Read off the **message centre** rather than guessed: what crossed the socket
/// is what the recorder must have captured, and a number invented here would
/// make the assertion agree with itself.
fn last_submit(seen: &mut mpsc::UnboundedReceiver<Seen>) -> u32 {
    let notes = drain(seen);
    let submitted = submissions(&notes);

    submitted
        .last()
        .expect("the message centre received the submission")
        .0
}

// ---------------------------------------------------------------------------
// CA-008-09 — off by default
// ---------------------------------------------------------------------------

/// **The default.** A whole session — bind, submit, receipt, unbind — writes
/// nothing at all while nobody has asked for it.
///
/// The exchange is asserted first, and that ordering is deliberate: an empty
/// store proves "disabled by default" only once the session is shown to have
/// really talked to the message centre.
#[tokio::test(start_paused = true)]
async fn a_full_exchange_records_nothing_while_the_recorder_is_off() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let (recorder, store) = a_recorder();
    let (observer, inbox) = queueing(QUEUE_CAPACITY);
    let _draining = draining(Arc::clone(&recorder), inbox);

    let session = smpp_session::spawn_observed(
        a_profile(),
        a_password(),
        smsc.clone(),
        Some(Arc::clone(&observer) as Arc<dyn PduObserver>),
    );

    wait_until_bound(&session.handle).await;

    session.handle.request(a_submit()).await.unwrap();
    let pushed = smsc.deliver_receipt("id:SMSC-1 stat:DELIVRD err:000");

    tokio::time::sleep(SETTLE).await;
    session.handle.shutdown().await.unwrap();
    tokio::time::sleep(SETTLE).await;

    recorder.flush().await.unwrap();

    // The exchange really happened, in both directions.
    let notes = drain(&mut seen);
    assert_eq!(smsc.submissions(), 1, "the submission crossed the socket");
    assert!(
        acknowledged(&notes).contains(&pushed),
        "the receipt was acknowledged: {notes:?}"
    );

    // And none of it was written.
    assert!(
        store.entries().is_empty(),
        "a disabled recorder must write nothing: {:?}",
        store.lines()
    );
    assert!(
        !recorder.is_enabled(),
        "a recorder nobody switched on stays off"
    );
}

// ---------------------------------------------------------------------------
// CA-008-09 — once enabled, every PDU, both directions, exactly once
// ---------------------------------------------------------------------------

/// **The half CA-008-09 is stated in.** With the switch on, each PDU of the
/// exchange reaches the store once — identified by its own sequence number,
/// not by its position.
///
/// The bind is in the list on purpose. It carries the password, and it is
/// recorded under the same switch as everything else: that is the recorder's
/// contract (spec §17.7), and an exception for it would leave an operator
/// debugging a rejected bind with nothing to look at.
#[tokio::test(start_paused = true)]
async fn every_pdu_of_both_directions_is_recorded_exactly_once() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let (recorder, store) = a_recorder();
    let (observer, inbox) = queueing(QUEUE_CAPACITY);
    let _draining = draining(Arc::clone(&recorder), inbox);

    // On before the session exists, so the bind exchange is caught too.
    recorder.set_enabled(true);

    let session = smpp_session::spawn_observed(
        a_profile(),
        a_password(),
        smsc.clone(),
        Some(Arc::clone(&observer) as Arc<dyn PduObserver>),
    );

    wait_until_bound(&session.handle).await;

    session.handle.request(a_submit()).await.unwrap();
    let submitted = last_submit(&mut seen);

    let pushed = smsc.deliver_receipt("id:SMSC-1 stat:DELIVRD err:000");

    tokio::time::sleep(SETTLE).await;
    session.handle.shutdown().await.unwrap();
    tokio::time::sleep(SETTLE).await;

    recorder.flush().await.unwrap();

    let lines = store.lines();
    let expected = [
        // The handshake, both halves.
        line(
            PduDirection::Outbound,
            CommandId::BindTransceiver,
            BIND_SEQUENCE,
        ),
        line(
            PduDirection::Inbound,
            CommandId::BindTransceiverResp,
            BIND_SEQUENCE,
        ),
        // The message, and the answer that echoes its sequence number.
        line(PduDirection::Outbound, CommandId::SubmitSm, submitted),
        line(PduDirection::Inbound, CommandId::SubmitSmResp, submitted),
        // The receipt the centre pushed, and the acknowledgement the reader
        // sent back on its own — an outbound PDU no caller ever asked for.
        line(PduDirection::Inbound, CommandId::DeliverSm, pushed),
        line(PduDirection::Outbound, CommandId::DeliverSmResp, pushed),
    ];

    for wanted in expected {
        assert_eq!(
            lines.iter().filter(|entry| **entry == wanted).count(),
            1,
            "{wanted:?} must be recorded exactly once, in {lines:?}"
        );
    }

    // Nothing is recorded twice — the assertion above proves it for six PDUs,
    // this one for the keep-alive and the unbind as well, whatever the run
    // happened to contain.
    let distinct: HashSet<Line> = lines.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        lines.len(),
        "no PDU may be recorded twice: {lines:?}"
    );

    // And what the detail panel shows is there, on the PDU that carries a body.
    let submit = store
        .entries()
        .into_iter()
        .find(|entry| {
            entry.direction == PduDirection::Outbound
                && entry.sequence_number == Some(submitted)
                && entry.command_id == Some(u32::from(CommandId::SubmitSm))
        })
        .expect("the submission was recorded");

    assert_eq!(submit.session_id, Some(session.handle.session_id()));
    assert!(
        submit.raw_hex.is_some_and(|dump| !dump.is_empty()),
        "the raw hexadecimal is what an operator turned this on for"
    );
    assert!(
        submit
            .decoded
            .is_some_and(|decoded| decoded.contains("SubmitSm")),
        "the decoded rendering names the PDU"
    );
}

// ---------------------------------------------------------------------------
// CA-008-09 — the switch is read per PDU
// ---------------------------------------------------------------------------

/// **Off stops the next PDU, not the next session.** The switch is read on
/// every PDU, so an operator who notices the log filling up can stop it — and
/// start it again — without unbinding.
///
/// The `SETTLE` between each submission and each flip is not padding: the flag
/// is read by the draining task, so a PDU still in the queue when the switch
/// moves is decided by the *new* position. For a debug facility that is the
/// right trade — the alternative is stamping every entry at the call site,
/// which is work done on the hot path for a switch that is off almost always.
#[tokio::test(start_paused = true)]
async fn switching_the_recorder_off_stops_the_next_pdu_of_the_same_session() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let (recorder, store) = a_recorder();
    let (observer, inbox) = queueing(QUEUE_CAPACITY);
    let _draining = draining(Arc::clone(&recorder), inbox);

    recorder.set_enabled(true);

    let session = smpp_session::spawn_observed(
        a_profile(),
        a_password(),
        smsc.clone(),
        Some(Arc::clone(&observer) as Arc<dyn PduObserver>),
    );

    wait_until_bound(&session.handle).await;
    tokio::time::sleep(SETTLE).await;

    // Recorded.
    session.handle.request(a_submit()).await.unwrap();
    let while_on = last_submit(&mut seen);
    tokio::time::sleep(SETTLE).await;

    // Not recorded.
    recorder.set_enabled(false);
    session.handle.request(a_submit()).await.unwrap();
    let while_off = last_submit(&mut seen);
    tokio::time::sleep(SETTLE).await;

    // Recorded again: the same session, still bound throughout.
    recorder.set_enabled(true);
    session.handle.request(a_submit()).await.unwrap();
    let after_on = last_submit(&mut seen);
    tokio::time::sleep(SETTLE).await;

    smpp_session::testing::assert_state(
        &session.handle,
        SessionState::Bound(BindMode::Transceiver),
    );

    session.handle.shutdown().await.unwrap();
    tokio::time::sleep(SETTLE).await;
    recorder.flush().await.unwrap();

    let lines = store.lines();
    let recorded =
        |sequence| lines.contains(&line(PduDirection::Outbound, CommandId::SubmitSm, sequence));

    assert!(recorded(while_on), "recorded while on: {lines:?}");
    assert!(!recorded(while_off), "not recorded while off: {lines:?}");
    assert!(recorded(after_on), "recorded once on again: {lines:?}");

    // The three submissions all reached the message centre: the switch changes
    // what is logged and nothing else.
    assert_eq!(smsc.submissions(), 3);
    assert_ne!(while_on, while_off);
    assert_ne!(while_off, after_on);
}

// ---------------------------------------------------------------------------
// CA-008-09 — a lost debug entry must never cost a message
// ---------------------------------------------------------------------------

/// **The queue is bounded, and full means dropped.** A recorder that cannot
/// keep up loses entries; it does not slow the session down and it does not
/// lose a message.
///
/// Nothing drains the queue here — `inbox` is held open and never read, which
/// is a recorder infinitely behind. That is the worst case of a slow disk,
/// expressed deterministically: with a capacity of one, every PDU after the
/// first has nowhere to go.
#[tokio::test(start_paused = true)]
async fn a_saturated_queue_drops_entries_and_leaves_the_session_untouched() {
    const SUBMISSIONS: u32 = 20;

    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let (recorder, store) = a_recorder();
    let (observer, inbox) = queueing(1);

    // Held, not dropped: a closed queue would fail `try_send` for the wrong
    // reason, and this test is about a **full** one.
    let _inbox = inbox;

    recorder.set_enabled(true);

    let session = smpp_session::spawn_observed(
        a_profile(),
        a_password(),
        smsc.clone(),
        Some(Arc::clone(&observer) as Arc<dyn PduObserver>),
    );

    wait_until_bound(&session.handle).await;

    for _ in 0..SUBMISSIONS {
        let response = session.handle.request(a_submit()).await.unwrap();

        assert_eq!(
            response.status(),
            smpp_core::values::CommandStatus::EsmeRok,
            "a full PDU queue must not fail a submission"
        );
    }

    let pushed = smsc.deliver_receipt("id:SMSC-1 stat:DELIVRD err:000");
    tokio::time::sleep(SETTLE).await;

    // The session is untouched: still bound, still answering, and every
    // message went out.
    smpp_session::testing::assert_state(
        &session.handle,
        SessionState::Bound(BindMode::Transceiver),
    );
    assert_eq!(smsc.submissions(), SUBMISSIONS);

    let notes = drain(&mut seen);
    assert!(
        acknowledged(&notes).contains(&pushed),
        "the reader kept answering receipts: {notes:?}"
    );

    // And the loss is real, on the debug side alone.
    assert!(
        observer.dropped() > 0,
        "a queue of one cannot have held {SUBMISSIONS} submissions and their responses"
    );

    session.handle.shutdown().await.unwrap();
    recorder.flush().await.unwrap();

    assert!(
        store.entries().is_empty(),
        "nothing drained the queue, so nothing reached the store: {:?}",
        store.lines()
    );
}
