//! Behaviour at scale — CA-002-05 and CA-002-06.

// See the note in `schema.rs`: `tests/` is compiled without `cfg(test)`, so
// the test relaxations of `clippy.toml` do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use std::alloc::System;
use std::future::Future;
use std::time::Instant;

use futures_util::StreamExt;
use persistence::ports::MessageRepository;
use persistence::{
    Message, MessageFilter, MessageState, MessageStateUpdate, PersistenceError,
    SqliteMessageRepository,
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

/// Live bytes an operation leaves behind: what it allocated minus what it
/// gave back.
///
/// Measured across the `await` points of the operation, which is why the test
/// using it runs on a `current_thread` runtime: no other task may allocate
/// inside the region.
async fn live_bytes<T>(operation: impl Future<Output = T>) -> (T, isize) {
    let region = Region::new(GLOBAL);
    let outcome = operation.await;
    let change = region.change();

    let allocated = isize::try_from(change.bytes_allocated).unwrap_or(isize::MAX);
    let freed = isize::try_from(change.bytes_deallocated).unwrap_or(isize::MAX);

    (outcome, allocated - freed)
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

/// CA-002-05, first half: the memory a full traversal holds does not grow with
/// the number of rows.
///
/// The assertion is deliberately **relative**. An absolute byte budget would
/// encode the allocator's behaviour on the machine that wrote it; comparing
/// the same traversal discarding rows against the same traversal keeping them
/// measures the only thing that matters — whether the result set is
/// materialised.
#[tokio::test(flavor = "current_thread")]
async fn streaming_a_hundred_thousand_messages_does_not_materialise_them() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    fill(&repository, ROWS).await;

    let filter = MessageFilter::all();

    let (streamed, streaming_bytes) = live_bytes(async {
        let mut stream = repository.stream_messages(&filter);
        let mut seen = 0_usize;
        while let Some(message) = stream.next().await {
            message.unwrap();
            seen += 1;
        }
        seen
    })
    .await;

    let (collected, collecting_bytes) = live_bytes(
        repository
            .stream_messages(&filter)
            .map(|message| message.unwrap())
            .collect::<Vec<_>>(),
    )
    .await;

    assert_eq!(streamed, ROWS);
    assert_eq!(collected.len(), ROWS);

    // The instrument works: keeping a hundred thousand rows costs megabytes.
    // Without this the comparison below would also pass on a measurement stuck
    // at zero, which is the way a memory test usually rots.
    assert!(
        collecting_bytes > 1_000_000,
        "the allocator measurement looks broken: collecting {ROWS} rows reported \
         {collecting_bytes} live bytes"
    );

    // Holding the rows costs at least a hundred bytes each; holding none costs
    // a bounded working set. Two orders of magnitude between them is a wide
    // margin around a difference that is really four.
    assert!(
        streaming_bytes * 100 < collecting_bytes,
        "streaming kept {streaming_bytes} live bytes against {collecting_bytes} when collecting"
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

/// CA-002-06: a batch of transitions is **one** transaction, not N.
///
/// There is no counter in SQLite that reports "transactions committed", so the
/// property is asserted where it is observable: atomicity. If the batch were N
/// transactions, the two transitions preceding the failing one would have
/// committed and would be visible afterwards. They are not.
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
    assert!(matches!(rejection, PersistenceError::NotFound { .. }));

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
    let mut cursor = persistence::Cursor::start();
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
