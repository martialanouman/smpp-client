//! Delivery receipts against a real SQLite file — milestone 008.
//!
//! The in-memory doubles of `messaging::correlation` prove the correlation
//! logic. What they cannot prove is the half that lives **in the statement**:
//! the state machine expanded into the `WHERE` clause, the `COALESCE` that
//! makes a replayed transition a no-op, and the orphan journal's round trip.
//! Those need the engine.

// See the note in `schema.rs`: `tests/` is compiled without `cfg(test)`, so
// the test relaxations of `clippy.toml` do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use messaging::correlation::{OrphanReason, OrphanReceipt, OrphanReceiptStore};
use messaging::ports::MessageRepository;
use persistence::ports::MessageJournal;
use persistence::{
    Cursor, MessageFilter, MessageState, MessageStateUpdate, OrphanJournal,
    SqliteMessageRepository, SqliteOrphanRepository,
};
use smpp_core::types::{ClientMessageId, SessionId};

use support::{a_queued_message, instant, temp_database};

/// The transition a `DELIVRD` receipt produces, as `messaging` builds it.
fn a_delivery(client_message_id: ClientMessageId, at: &str) -> MessageStateUpdate {
    MessageStateUpdate::new(client_message_id, MessageState::Delivered)
        .receipt_at(instant(at))
        .with_delivery_receipt("DELIVRD", Some(String::from("000")))
}

/// **CA-008-01**, on the file rather than on a double: the row really moves to
/// `DELIVERED` and really carries `dlr_at`, `dlr_stat` and `dlr_err`.
#[tokio::test]
async fn a_delivery_receipt_moves_the_row_to_delivered() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let client_message_id = ClientMessageId::new();

    let mut message = a_queued_message(client_message_id, "+2250102030405");
    message.state = MessageState::Accepted;
    message.smsc_message_id = Some(String::from("SMSC-1"));
    repository.insert_message(&message).await.unwrap();

    repository
        .update_state(&a_delivery(client_message_id, "2026-07-26T12:00:00Z"))
        .await
        .unwrap();

    let stored = repository
        .find_message(client_message_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(stored.state, MessageState::Delivered);
    assert_eq!(stored.dlr_stat.as_deref(), Some("DELIVRD"));
    assert_eq!(stored.dlr_err.as_deref(), Some("000"));
    assert_eq!(stored.dlr_at, Some(instant("2026-07-26T12:00:00Z")));
    // The identifier is untouched: nothing in the receipt path may reassign it.
    assert_eq!(stored.smsc_message_id.as_deref(), Some("SMSC-1"));
}

/// step-008 §5 — **the same receipt twice counts once.**
///
/// A message centre that does not get its `deliver_sm_resp` re-sends the
/// receipt, so a duplicate is the ordinary case rather than a corner one.
/// Replaying the identical transition must leave the row byte-for-byte as it
/// was: that is the idempotence CLAUDE.md §4 requires, and it is what
/// `COALESCE` and the `WHERE` clause's self-transition give together.
#[tokio::test]
async fn the_same_receipt_applied_twice_leaves_the_row_identical() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let client_message_id = ClientMessageId::new();

    let mut message = a_queued_message(client_message_id, "+2250102030405");
    message.state = MessageState::Accepted;
    message.smsc_message_id = Some(String::from("SMSC-1"));
    repository.insert_message(&message).await.unwrap();

    let transition = a_delivery(client_message_id, "2026-07-26T12:00:00Z");

    repository.update_state(&transition).await.unwrap();
    let after_first = repository
        .find_message(client_message_id)
        .await
        .unwrap()
        .unwrap();

    repository.update_state(&transition).await.unwrap();
    let after_second = repository
        .find_message(client_message_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        after_first, after_second,
        "replaying a committed transition must change nothing"
    );
    assert_eq!(after_second.dlr_at, Some(instant("2026-07-26T12:00:00Z")));
}

/// A **redelivered** receipt — the same delivery, read off the socket a second
/// time, so stamped with a later arrival instant by
/// `messaging::correlation`.
///
/// What must not move is the message's standing: the state, the `stat` and the
/// error code are the same delivery reported twice, and a second `DELIVERED`
/// must not be a second delivery for anything counting them.
///
/// `dlr_at` **does** move, and that is the deliberate reading: it is "when this
/// application last had the receipt in hand", not "when the handset got the
/// message" — the message centre's own `done date` is the second thing, is
/// unverifiable, and is kept on the receipt rather than in this column. Keeping
/// the first arrival instead would leave `dlr_at` describing one copy while
/// `dlr_stat` describes another the moment a centre upgrades `ACCEPTD` to
/// `DELIVRD`.
#[tokio::test]
async fn a_redelivered_receipt_refreshes_its_instant_and_nothing_else() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let client_message_id = ClientMessageId::new();

    let mut message = a_queued_message(client_message_id, "+2250102030405");
    message.state = MessageState::Accepted;
    repository.insert_message(&message).await.unwrap();

    repository
        .update_state(&a_delivery(client_message_id, "2026-07-26T12:00:00Z"))
        .await
        .unwrap();
    let after_first = repository
        .find_message(client_message_id)
        .await
        .unwrap()
        .unwrap();

    repository
        .update_state(&a_delivery(client_message_id, "2026-07-26T13:30:00Z"))
        .await
        .unwrap();
    let after_second = repository
        .find_message(client_message_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(after_second.state, after_first.state);
    assert_eq!(after_second.dlr_stat, after_first.dlr_stat);
    assert_eq!(after_second.dlr_err, after_first.dlr_err);
    assert_eq!(
        after_second.attempts, after_first.attempts,
        "a receipt must never spend a sending attempt"
    );
    assert_eq!(after_second.dlr_at, Some(instant("2026-07-26T13:30:00Z")));
}

/// **The milestone-006 barrier, on the engine.** A late `DELIVRD` for a message
/// that already failed must not resurrect it: the machine is in the `WHERE`
/// clause, and this is what proves it is still there.
#[tokio::test]
async fn a_late_delivery_receipt_cannot_resurrect_a_failed_message() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let client_message_id = ClientMessageId::new();

    let mut message = a_queued_message(client_message_id, "+2250102030405");
    message.state = MessageState::Failed;
    message.smsc_message_id = Some(String::from("SMSC-1"));
    repository.insert_message(&message).await.unwrap();

    // Not an error: the transition is refused, the caller is not at fault.
    repository
        .update_state(&a_delivery(client_message_id, "2026-07-26T12:00:00Z"))
        .await
        .unwrap();

    let stored = repository
        .find_message(client_message_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(stored.state, MessageState::Failed);
    assert_eq!(
        stored.dlr_stat, None,
        "a refused transition must not write its receipt fields either"
    );
    assert_eq!(stored.dlr_at, None);
}

/// A `UNDELIV` receipt closes the message as `FAILED` and keeps the centre's
/// own words, which is what an operator quotes back to their provider.
#[tokio::test]
async fn an_undeliverable_receipt_fails_the_message_and_keeps_its_error_code() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let client_message_id = ClientMessageId::new();

    let mut message = a_queued_message(client_message_id, "+2250102030405");
    message.state = MessageState::Accepted;
    repository.insert_message(&message).await.unwrap();

    repository
        .update_state(
            &MessageStateUpdate::new(client_message_id, MessageState::Failed)
                .receipt_at(instant("2026-07-26T12:00:00Z"))
                .with_delivery_receipt("UNDELIV", Some(String::from("058"))),
        )
        .await
        .unwrap();

    let stored = repository
        .find_message(client_message_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(stored.state, MessageState::Failed);
    assert_eq!(stored.dlr_stat.as_deref(), Some("UNDELIV"));
    assert_eq!(stored.dlr_err.as_deref(), Some("058"));
}

/// **Two providers both minting `1234`.**
///
/// `smsc_message_id` is unique per message centre and nothing more, and
/// CLAUDE.md §1 has this application talk to several. Without the session in
/// the predicate the lookup returned the oldest row — deterministically the
/// wrong one — and a receipt failed a message belonging to another provider.
#[tokio::test]
async fn a_lookup_by_smsc_identifier_stays_inside_its_session() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let profiles = persistence::SqliteSessionProfileRepository::new(harness.database().clone());

    let first = SessionId::new();
    let second = SessionId::new();

    {
        use persistence::ports::SessionProfileRepository as _;

        for (session_id, name) in [(first, "provider A"), (second, "provider B")] {
            profiles
                .upsert_session_profile(&support::a_session_profile(session_id, name))
                .await
                .unwrap();
        }
    }

    // The older row is the one a session-blind query would return.
    let on_first = ClientMessageId::new();
    let on_second = ClientMessageId::new();

    for (client_message_id, session_id) in [(on_first, first), (on_second, second)] {
        let mut message = a_queued_message(client_message_id, "+2250102030405");
        message.state = MessageState::Accepted;
        message.session_id = Some(session_id);
        message.smsc_message_id = Some(String::from("1234"));
        repository.insert_message(&message).await.unwrap();
    }

    let found = repository
        .find_message_by_smsc_id("1234", Some(second))
        .await
        .unwrap()
        .expect("the second session's message exists");

    assert_eq!(
        found.client_message_id, on_second,
        "a receipt on the second session must not reach the first session's message"
    );

    assert_eq!(
        repository
            .find_message_by_smsc_id("1234", Some(first))
            .await
            .unwrap()
            .map(|message| message.client_message_id),
        Some(on_first)
    );

    // `None` still means "any session": a message whose profile was deleted
    // carries a NULL `session_id` and must remain reachable.
    assert!(repository
        .find_message_by_smsc_id("1234", None)
        .await
        .unwrap()
        .is_some());
}

/// A session that never sent this identifier finds nothing, rather than the
/// nearest row.
#[tokio::test]
async fn a_lookup_on_a_session_that_never_sent_the_identifier_finds_nothing() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let profiles = persistence::SqliteSessionProfileRepository::new(harness.database().clone());

    let owner = SessionId::new();
    let stranger = SessionId::new();

    {
        use persistence::ports::SessionProfileRepository as _;

        for (session_id, name) in [(owner, "owner"), (stranger, "stranger")] {
            profiles
                .upsert_session_profile(&support::a_session_profile(session_id, name))
                .await
                .unwrap();
        }
    }

    let mut message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    message.session_id = Some(owner);
    message.smsc_message_id = Some(String::from("9999"));
    repository.insert_message(&message).await.unwrap();

    assert!(repository
        .find_message_by_smsc_id("9999", Some(stranger))
        .await
        .unwrap()
        .is_none());
}

// --- The orphan journal (CA-008-04) -----------------------------------------

fn an_orphan(identifier: Option<&str>, reason: OrphanReason) -> OrphanReceipt {
    OrphanReceipt {
        session_id: None,
        smsc_message_id: identifier.map(ToOwned::to_owned),
        reason,
        dlr_stat: Some(String::from("DELIVRD")),
        dlr_err: Some(String::from("000")),
        submit_date: Some(instant("2026-07-26T12:00:00Z")),
        done_date: Some(instant("2026-07-26T12:05:00Z")),
        raw: String::from("id:STRANGER stat:DELIVRD err:000 text:hello"),
        received_at: instant("2026-07-26T12:06:00Z"),
    }
}

#[tokio::test]
async fn an_orphaned_receipt_survives_a_round_trip() {
    let harness = temp_database().await;
    let repository = SqliteOrphanRepository::new(harness.database().clone());
    let written = an_orphan(Some("STRANGER"), OrphanReason::UnknownIdentifier);

    assert_eq!(
        repository
            .insert_orphans(core::slice::from_ref(&written))
            .await
            .unwrap(),
        1
    );

    let page = repository
        .page_orphans(None, Cursor::start(), 10)
        .await
        .unwrap();

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].receipt, written);
    assert_eq!(repository.count_orphans(None).await.unwrap(), 1);
}

/// A receipt with no readable identifier is stored too, with the reason that
/// says so — otherwise the one diagnostic for "the centre is sending us
/// rubbish" would be a log line nobody keeps.
#[tokio::test]
async fn an_orphan_with_no_identifier_is_stored_with_its_reason() {
    let harness = temp_database().await;
    let repository = SqliteOrphanRepository::new(harness.database().clone());

    repository
        .insert_orphans(&[an_orphan(None, OrphanReason::NoIdentifier)])
        .await
        .unwrap();

    let page = repository
        .page_orphans(None, Cursor::start(), 10)
        .await
        .unwrap();

    assert_eq!(page.items[0].receipt.smsc_message_id, None);
    assert_eq!(page.items[0].receipt.reason, OrphanReason::NoIdentifier);
}

/// The log screen pages the orphan journal the same way it pages the messages:
/// a bounded window and a cursor, never the whole table.
#[tokio::test]
async fn the_orphan_journal_pages_rather_than_returning_everything() {
    let harness = temp_database().await;
    let repository = SqliteOrphanRepository::new(harness.database().clone());

    let batch: Vec<OrphanReceipt> = (0..25)
        .map(|index| an_orphan(Some(&format!("X-{index}")), OrphanReason::UnknownIdentifier))
        .collect();
    repository.insert_orphans(&batch).await.unwrap();

    let first = repository
        .page_orphans(None, Cursor::start(), 10)
        .await
        .unwrap();

    assert_eq!(first.items.len(), 10);
    let cursor = first.next.expect("a full page announces a next one");

    let second = repository.page_orphans(None, cursor, 10).await.unwrap();

    assert_eq!(second.items.len(), 10);
    assert_ne!(first.items[0].id, second.items[0].id, "no page is replayed");
    assert_eq!(repository.count_orphans(None).await.unwrap(), 25);
}

/// Orphans are filtered by session, which is how an operator running two
/// message centres tells whose receipts do not correlate.
#[tokio::test]
async fn orphans_are_filtered_by_session() {
    let harness = temp_database().await;
    let session_id = SessionId::new();
    let profiles = persistence::SqliteSessionProfileRepository::new(harness.database().clone());

    {
        use persistence::ports::SessionProfileRepository as _;

        profiles
            .upsert_session_profile(&support::a_session_profile(session_id, "centre"))
            .await
            .unwrap();
    }

    let repository = SqliteOrphanRepository::new(harness.database().clone());
    let mut attached = an_orphan(Some("A"), OrphanReason::UnknownIdentifier);
    attached.session_id = Some(session_id);

    repository
        .insert_orphans(&[
            attached,
            an_orphan(Some("B"), OrphanReason::UnknownIdentifier),
        ])
        .await
        .unwrap();

    assert_eq!(
        repository.count_orphans(Some(session_id)).await.unwrap(),
        1,
        "one of the two belongs to this session"
    );
    assert_eq!(repository.count_orphans(None).await.unwrap(), 2);
}

// --- The log-screen filters (CA-008-07) -------------------------------------

/// Seeds four messages that differ on every filterable column.
async fn a_journal_to_filter(repository: &SqliteMessageRepository) -> Vec<ClientMessageId> {
    let ids: Vec<ClientMessageId> = (0..4).map(|_| ClientMessageId::new()).collect();

    let mut early = a_queued_message(ids[0], "+22501020304");
    early.created_at = instant("2026-07-01T10:00:00Z");
    early.text = Some(String::from("promotion du mois"));

    let mut late = a_queued_message(ids[1], "+33612345678");
    late.created_at = instant("2026-07-30T10:00:00Z");
    late.text = Some(String::from("rappel de rendez-vous"));

    let mut failed = a_queued_message(ids[2], "+22505060708");
    failed.created_at = instant("2026-07-15T10:00:00Z");
    failed.state = MessageState::Failed;
    failed.dlr_err = Some(String::from("058"));
    failed.text = Some(String::from("promotion du mois"));

    let mut other_error = a_queued_message(ids[3], "+22509080706");
    other_error.created_at = instant("2026-07-16T10:00:00Z");
    other_error.state = MessageState::Failed;
    other_error.dlr_err = Some(String::from("001"));
    other_error.smsc_message_id = Some(String::from("SMSC-NEEDLE"));

    repository
        .insert_messages(&[early, late, failed, other_error])
        .await
        .unwrap();

    ids
}

async fn matching(repository: &SqliteMessageRepository, filter: &MessageFilter) -> Vec<String> {
    let page = repository
        .page_messages(filter, Cursor::start(), 100)
        .await
        .unwrap();

    let mut found: Vec<String> = page
        .items
        .iter()
        .map(|message| message.client_message_id.to_string())
        .collect();
    found.sort();

    // The count has to agree with the page: they are two different SQL
    // literals, and a predicate added to one and forgotten in the other is
    // exactly the drift this asserts against.
    assert_eq!(
        repository.count_messages(filter).await.unwrap(),
        found.len() as u64,
        "count and page disagree for {filter:?}"
    );

    found
}

#[tokio::test]
async fn a_date_range_selects_only_the_messages_inside_it() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let ids = a_journal_to_filter(&repository).await;

    let inside = matching(
        &repository,
        &MessageFilter::all().created_between(
            Some(instant("2026-07-10T00:00:00Z")),
            Some(instant("2026-07-20T00:00:00Z")),
        ),
    )
    .await;

    let mut expected = vec![ids[2].to_string(), ids[3].to_string()];
    expected.sort();

    assert_eq!(inside, expected);
}

/// An open-ended range is the ordinary case in the interface: "since Monday",
/// with no upper bound.
#[tokio::test]
async fn an_open_ended_date_range_restricts_only_the_end_it_names() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    a_journal_to_filter(&repository).await;

    let since = matching(
        &repository,
        &MessageFilter::all().created_between(Some(instant("2026-07-15T00:00:00Z")), None),
    )
    .await;

    assert_eq!(since.len(), 3);
}

/// **The bug this caught.** `Msisdn` stores digits only, so a literal
/// `LIKE '+225%'` matched nothing — a log screen that silently reported no
/// messages for a prefix the operator typed the way the rest of the interface
/// writes numbers. Both spellings must select the same rows.
#[tokio::test]
async fn a_destination_prefix_selects_a_country_range_with_or_without_the_plus() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    a_journal_to_filter(&repository).await;

    assert_eq!(
        matching(&repository, &MessageFilter::all().with_dest_prefix("+225"))
            .await
            .len(),
        3
    );
    assert_eq!(
        matching(&repository, &MessageFilter::all().with_dest_prefix("225"))
            .await
            .len(),
        3,
        "the two spellings must agree"
    );
    assert_eq!(
        matching(&repository, &MessageFilter::all().with_dest_prefix("+33"))
            .await
            .len(),
        1
    );
    assert!(
        matching(&repository, &MessageFilter::all().with_dest_prefix("+999"))
            .await
            .is_empty(),
        "and a prefix nobody used still selects nothing"
    );
}

#[tokio::test]
async fn an_error_code_selects_the_messages_that_carried_it() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let ids = a_journal_to_filter(&repository).await;

    assert_eq!(
        matching(&repository, &MessageFilter::all().with_dlr_err("058")).await,
        vec![ids[2].to_string()]
    );
}

/// The search runs over the recipient, the body **and** the SMSC identifier:
/// an operator pastes whichever of the three their provider quoted at them.
#[tokio::test]
async fn the_full_text_search_covers_the_recipient_the_body_and_the_identifier() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let ids = a_journal_to_filter(&repository).await;

    assert_eq!(
        matching(&repository, &MessageFilter::all().matching("NEEDLE")).await,
        vec![ids[3].to_string()],
        "the SMSC identifier is searched"
    );
    assert_eq!(
        matching(&repository, &MessageFilter::all().matching("33612"))
            .await
            .len(),
        1,
        "the recipient is searched"
    );
    assert_eq!(
        matching(&repository, &MessageFilter::all().matching("promotion"))
            .await
            .len(),
        2,
        "the body is searched"
    );
}

/// Filters combine as a conjunction. The interface offers all of them at once,
/// and a filter that quietly ignored one of its criteria would show rows the
/// operator excluded.
#[tokio::test]
async fn combined_filters_narrow_rather_than_widen() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    let ids = a_journal_to_filter(&repository).await;

    let combined = MessageFilter::all()
        .in_state(MessageState::Failed)
        .with_dest_prefix("+225")
        .created_between(Some(instant("2026-07-10T00:00:00Z")), None)
        .matching("promotion");

    assert_eq!(
        matching(&repository, &combined).await,
        vec![ids[2].to_string()]
    );
}

/// An all-`None` filter still selects everything: the new predicates must be
/// inert when nobody set them.
#[tokio::test]
async fn an_empty_filter_still_selects_the_whole_table() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());
    a_journal_to_filter(&repository).await;

    assert_eq!(matching(&repository, &MessageFilter::all()).await.len(), 4);
}
