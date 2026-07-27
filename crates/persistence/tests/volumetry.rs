//! Behaviour at scale — CA-002-05 and CA-002-06.

// See the note in `schema.rs`: `tests/` is compiled without `cfg(test)`, so
// the test relaxations of `clippy.toml` do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use std::alloc::System;
use std::time::Instant;

use futures_util::StreamExt;
use messaging::ports::{MessageRepository, MessageStoreError};
use persistence::ports::MessageJournal;
use persistence::{
    Cursor, Message, MessageFilter, MessageState, MessageStateUpdate, SqliteMessageRepository,
};
use smpp_core::types::ClientMessageId;
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

use support::{a_queued_message, numbered_msisdn, temp_database};

/// Counting allocator, so a test can state what a traversal actually holds.
///
/// No `unsafe` here: `stats_alloc` implements `GlobalAlloc`, this file only
/// names it. `cargo nextest` gives each test its own process, so the
/// measurements below are not polluted by a neighbouring test.
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Enough rows that "loads everything" and "loads one at a time" cannot be
/// confused, and few enough that the test stays a test.
const ROWS: usize = 100_000;

/// Bytes alive right now, counted since the region opened: what has been
/// allocated inside it minus what has been given back.
///
/// The counters `stats_alloc` exposes are cumulative, so this is a **net live
/// footprint at the moment of the call**, never a peak. Which is why callers
/// read it while the thing they are measuring is still alive — reading it
/// afterwards would report the net of a completed operation, and a
/// materialising implementation nets out to nothing.
///
/// The tests using it run on a `current_thread` runtime: no other task may
/// allocate inside the region. `cargo nextest` gives each test its own process
/// on top of that.
fn net_bytes(region: &Region<'_, System>) -> isize {
    let change = region.change();

    let allocated = isize::try_from(change.bytes_allocated).unwrap_or(isize::MAX);
    let freed = isize::try_from(change.bytes_deallocated).unwrap_or(isize::MAX);

    allocated - freed
}

async fn fill(repository: &SqliteMessageRepository, rows: usize) {
    // Ten transactions rather than one, so the write-ahead log does not grow
    // to the size of the whole batch before a single commit.
    for chunk in 0..10 {
        let batch: Vec<Message> = (0..rows / 10)
            .map(|index| {
                a_queued_message(
                    ClientMessageId::new(),
                    numbered_msisdn(chunk * (rows / 10) + index).as_str(),
                )
            })
            .collect();

        repository.insert_messages(&batch).await.unwrap();
    }
}

/// CA-002-05, first half: the memory a live traversal holds does not grow with
/// the number of rows.
///
/// # What is measured, and why the first version measured the wrong thing
///
/// `Region::change()` reports **cumulative** counters, so the difference
/// between allocated and deallocated is the net live footprint *at the end of
/// the region*, never the peak. Taking that difference around a completed
/// traversal therefore answers "did the result escape?", not "was it
/// materialised?" — an implementation reading `fetch_all` into a `Vec` and
/// replaying it as a stream allocates and frees inside the region, nets out to
/// roughly zero, and passes.
///
/// So the measurement is taken with the stream **still alive**: create it,
/// pull one row, read the counters before dropping it. Whatever the stream is
/// holding is live at that instant and shows up. The baseline is the same
/// query materialised through `page_messages`, measured the same way, with the
/// page still alive.
#[tokio::test(flavor = "current_thread")]
async fn streaming_a_hundred_thousand_messages_does_not_materialise_them() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    fill(&repository, ROWS).await;

    let filter = MessageFilter::all();

    let region = Region::new(GLOBAL);
    let mut stream = repository.stream_messages(&filter);
    assert!(stream.next().await.is_some());
    // Read the counters HERE, with the stream still holding whatever it holds.
    // Reading them after the drop would report the net of a finished
    // operation, and a materialising implementation nets out to nothing.
    let streaming_bytes = net_bytes(&region);
    drop(stream);

    // The baseline: the same rows genuinely materialised. `page_messages` with
    // a window as wide as the table builds exactly the `Vec` a streaming
    // implementation must not build, and it is measured while it is alive.
    let region = Region::new(GLOBAL);
    let page = repository
        .page_messages(&filter, Cursor::start(), u32::MAX)
        .await
        .unwrap();
    let materialised_bytes = net_bytes(&region);
    assert_eq!(page.len(), ROWS);
    drop(page);

    // The instrument works: keeping a hundred thousand rows costs megabytes.
    // Without this the comparison below would also pass on a measurement stuck
    // at zero, which is the way a memory test usually rots.
    assert!(
        materialised_bytes > 1_000_000,
        "the allocator measurement looks broken: materialising {ROWS} rows reported \
         {materialised_bytes} live bytes"
    );

    // A live stream holds one row and a statement handle; a materialised page
    // holds every row. Two orders of magnitude is a wide margin around a
    // difference that is really four.
    assert!(
        streaming_bytes * 100 < materialised_bytes,
        "a live stream held {streaming_bytes} bytes against {materialised_bytes} \
         for the materialised page"
    );
}

/// CA-002-05, second half: the first row is available long before the
/// traversal ends.
#[tokio::test]
async fn the_first_streamed_message_arrives_before_the_traversal_ends() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    fill(&repository, ROWS).await;

    let filter = MessageFilter::all();

    let started = Instant::now();
    let first = repository.stream_messages(&filter).next().await;
    let to_first = started.elapsed();
    assert!(first.is_some());

    let started = Instant::now();
    let total = repository
        .stream_messages(&filter)
        .fold(0_usize, |count, message| async move {
            message.unwrap();
            count + 1
        })
        .await;
    let to_last = started.elapsed();

    assert_eq!(total, ROWS);
    assert!(
        to_first * 10 < to_last,
        "first row after {to_first:?}, full traversal in {to_last:?}"
    );
}

/// CA-002-06, the atomicity half: a failed batch leaves nothing behind.
///
/// If the batch were N transactions, the transitions preceding the failing one
/// would have committed and would be visible afterwards. They are not.
///
/// This does **not**, on its own, establish that there is exactly one commit —
/// an implementation validating everything first and then committing row by
/// row would also pass. The commit count itself is asserted in
/// `persistence::repositories::messages::tests`, which measures what each
/// commit appends to the write-ahead log. That one is a unit test because it
/// needs the pool; this one covers the property callers actually depend on.
#[tokio::test]
async fn a_batch_of_transitions_that_fails_rolls_back_the_whole_batch() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let messages: Vec<Message> = (0..3)
        .map(|index| a_queued_message(ClientMessageId::new(), numbered_msisdn(index).as_str()))
        .collect();
    repository.insert_messages(&messages).await.unwrap();

    let mut updates: Vec<MessageStateUpdate> = messages
        .iter()
        .map(|message| MessageStateUpdate::new(message.client_message_id, MessageState::Sent))
        .collect();
    // A fourth transition on a message that does not exist, at the end.
    updates.push(MessageStateUpdate::new(
        ClientMessageId::new(),
        MessageState::Sent,
    ));

    let rejection = repository.update_states(&updates).await.unwrap_err();
    assert_eq!(rejection, MessageStoreError::NotFound);

    for message in &messages {
        let read_back = repository
            .find_message(message.client_message_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            read_back.state,
            MessageState::Queued,
            "a transition committed before the batch failed"
        );
    }
}

#[tokio::test]
async fn a_batch_of_transitions_that_succeeds_applies_every_one() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let messages: Vec<Message> = (0..50)
        .map(|index| a_queued_message(ClientMessageId::new(), numbered_msisdn(index).as_str()))
        .collect();
    repository.insert_messages(&messages).await.unwrap();

    let updates: Vec<MessageStateUpdate> = messages
        .iter()
        .map(|message| MessageStateUpdate::new(message.client_message_id, MessageState::Sent))
        .collect();

    assert_eq!(repository.update_states(&updates).await.unwrap(), 50);
    assert_eq!(
        repository
            .count_messages(&MessageFilter::all().in_state(MessageState::Sent))
            .await
            .unwrap(),
        50
    );
}

/// Pagination over a large table stays a constant-cost query: a cursor never
/// re-walks the rows it skipped, which `OFFSET` would.
#[tokio::test]
async fn paging_to_the_end_of_a_large_table_visits_every_row_once() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    fill(&repository, 10_000).await;

    let filter = MessageFilter::all();
    let mut cursor = Cursor::start();
    let mut seen = 0_usize;

    loop {
        let page = repository
            .page_messages(&filter, cursor, 500)
            .await
            .unwrap();
        seen += page.len();

        match page.next {
            Some(next) => cursor = next,
            None => break,
        }
    }

    assert_eq!(seen, 10_000);
}

/// **CA-008-07** — the log screen's filters over 200 000 rows.
///
/// The criterion is "a filter applied in under a second"; the measurement here
/// is the **backend** half of it, which is the half that can degrade silently.
/// The frontend half — a virtualised table that renders a window rather than
/// 200 000 rows — is a `vitest` case in `ui/src/views/Logs/`.
///
/// Every predicate of the screen is exercised at once, because that is what the
/// screen sends: a state, a date range, a destination prefix and a full-text
/// search. The last three ride no index (see [`MessageFilter`]), so this is the
/// pessimistic shape and not a favourable one.
///
/// # Why an elapsed time, in a suite that otherwise refuses wall clocks
///
/// CLAUDE.md §7 bans a *dependency* on real time — an assertion whose outcome
/// changes with the scheduler. This is a performance ceiling: the query is
/// deterministic, only its duration is measured, and the bound is the
/// criterion's own. A machine slow enough to fail it is a machine on which the
/// screen really is unusable.
#[tokio::test]
async fn ca_008_07_filtering_two_hundred_thousand_messages_takes_under_a_second() {
    const LARGE: usize = 200_000;

    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    fill(&repository, LARGE).await;

    let filter = MessageFilter::all()
        .in_state(MessageState::Queued)
        .created_between(
            Some(persistence::Timestamp::parse("2026-07-01T00:00:00Z").unwrap()),
            None,
        )
        .with_dest_prefix("+225")
        .matching("Bonjour");

    let started = Instant::now();
    let page = repository
        .page_messages(&filter, Cursor::start(), 100)
        .await
        .unwrap();
    let elapsed = started.elapsed();

    // The instrument works: a filter matching nothing would make the timing
    // meaningless, and a page of a hundred is what the screen asks for.
    assert_eq!(
        page.len(),
        100,
        "the filter must actually select rows, or the timing means nothing"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "filtering {LARGE} rows took {elapsed:?}; CA-008-07 allows under a second"
    );

    // The count behind the pager runs the same predicates without the `LIMIT`,
    // so it is the slower of the two and the one that decides whether the
    // screen feels responsive.
    let started = Instant::now();
    let total = repository.count_messages(&filter).await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(total, LARGE as u64);
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "counting {LARGE} filtered rows took {elapsed:?}"
    );
}
