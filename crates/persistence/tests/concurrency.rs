//! What WAL actually buys: a reader that does not block the writer.

// See the note in `schema.rs`: `tests/` is compiled without `cfg(test)`, so
// the test relaxations of `clippy.toml` do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use std::time::Duration;

use futures_util::StreamExt;
use messaging::ports::MessageRepository;
use persistence::ports::MessageJournal;
use persistence::{Message, MessageFilter, MessageState, SqliteMessageRepository};
use smpp_core::types::ClientMessageId;

use support::{a_queued_message, numbered_msisdn, temp_database};

/// Messages the reader walks through while the writers work.
const EXISTING: usize = 5_000;

/// The whole point of spec §14.1's choice of WAL.
///
/// A traversal holds a read transaction open for as long as it runs. Under the
/// default rollback journal that read transaction blocks every writer, so a
/// campaign would stall the moment the user opened the log screen. Under WAL
/// the writers proceed, and this test fails by timing out if the mode were
/// ever silently downgraded.
#[tokio::test]
async fn writers_make_progress_while_a_long_read_is_in_flight() {
    let harness = temp_database().await;
    let reader_database = harness.database().clone();
    let writer_database = harness.reopen().await;

    let seed = SqliteMessageRepository::new(reader_database.clone());
    let existing: Vec<Message> = (0..EXISTING)
        .map(|index| a_queued_message(ClientMessageId::new(), numbered_msisdn(index).as_str()))
        .collect();
    seed.insert_messages(&existing).await.unwrap();

    // Open the traversal and consume one row, which is what actually starts
    // the read transaction, then keep it alive across the writes below.
    let reader = SqliteMessageRepository::new(reader_database);
    let filter = MessageFilter::all();
    let mut stream = reader.stream_messages(&filter);
    assert!(stream.next().await.is_some());

    let writers = (0..4).map(|writer| {
        let repository = SqliteMessageRepository::new(writer_database.clone());
        tokio::spawn(async move {
            for index in 0..25 {
                let message = a_queued_message(
                    ClientMessageId::new(),
                    numbered_msisdn(EXISTING + writer * 25 + index).as_str(),
                );
                repository.insert_message(&message).await.unwrap();
            }
        })
    });

    let written = tokio::time::timeout(
        Duration::from_secs(20),
        futures_util::future::join_all(writers),
    )
    .await
    .expect("the writers were blocked by the open read transaction");

    for outcome in written {
        outcome.expect("a writer task panicked");
    }

    // The traversal still runs, and still sees the snapshot it started on:
    // WAL gives the reader a consistent view, so the rows written above must
    // NOT appear in it.
    let remaining = stream.count().await;
    assert_eq!(remaining, EXISTING - 1);

    let total = reader.count_messages(&MessageFilter::all()).await.unwrap();
    assert_eq!(total, u64::try_from(EXISTING).unwrap() + 100);
}

/// Two independent handles on the same file both see the committed rows.
#[tokio::test]
async fn a_second_handle_sees_what_the_first_committed() {
    let harness = temp_database().await;
    let first = SqliteMessageRepository::new(harness.database().clone());
    let second = SqliteMessageRepository::new(harness.reopen().await);

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    first.insert_message(&message).await.unwrap();

    let read_back = second
        .find_message(message.client_message_id)
        .await
        .unwrap()
        .expect("a committed row is visible from any handle");

    assert_eq!(read_back.state, MessageState::Queued);
}

/// Concurrent batches from several tasks all land, in whatever order SQLite
/// serialises them: `busy_timeout` absorbs the contention instead of turning
/// it into an error.
#[tokio::test]
async fn concurrent_batches_all_land() {
    let harness = temp_database().await;

    let tasks = (0..4).map(|writer| {
        let database = harness.database().clone();
        tokio::spawn(async move {
            let repository = SqliteMessageRepository::new(database);
            let batch: Vec<Message> = (0..250)
                .map(|index| {
                    a_queued_message(
                        ClientMessageId::new(),
                        numbered_msisdn(writer * 250 + index).as_str(),
                    )
                })
                .collect();

            repository.insert_messages(&batch).await.unwrap()
        })
    });

    for outcome in futures_util::future::join_all(tasks).await {
        assert_eq!(outcome.expect("a writer task panicked"), 250);
    }

    let repository = SqliteMessageRepository::new(harness.database().clone());
    assert_eq!(
        repository
            .count_messages(&MessageFilter::all())
            .await
            .unwrap(),
        1_000
    );
}
