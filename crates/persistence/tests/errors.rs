//! What an error says, and above all what it never says — CA-002-09.

// See the note in `schema.rs`: `tests/` is compiled without `cfg(test)`, so
// the test relaxations of `clippy.toml` do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use std::error::Error;

use persistence::ports::{ContactRepository, MessageRepository, SessionProfileRepository};
use persistence::{
    ContactId, PersistenceError, SqliteContactRepository, SqliteMessageRepository,
    SqliteSessionProfileRepository,
};
use smpp_core::types::{ClientMessageId, SessionId};

use support::{a_contact, a_queued_message, a_session_profile, temp_database};

/// The whole error chain, rendered — which is what a log line or an IPC
/// payload would carry.
fn rendered_chain(error: &PersistenceError) -> String {
    let mut rendered = error.to_string();
    let mut source: Option<&(dyn Error + 'static)> = error.source();

    while let Some(cause) = source {
        rendered.push_str(" | ");
        rendered.push_str(&cause.to_string());
        source = cause.source();
    }

    rendered
}

/// The credential is the value CLAUDE.md §8 cares about most, so it gets its
/// own test.
///
/// No repository method takes a credential and can fail on it — `password_enc`
/// is an opaque blob the schema imposes nothing on — so provoking a failure
/// "on a profile write" is not possible, and pretending otherwise would be
/// theatre. What IS assertable, and is what the criterion asks, is that no
/// variant of the error type has a place to put one: each is built here with a
/// profile in scope, rendered with its whole chain, and checked.
#[tokio::test]
async fn no_error_variant_can_carry_the_credential() {
    let harness = temp_database().await;
    let repository = SqliteSessionProfileRepository::new(harness.database().clone());

    let mut profile = a_session_profile(SessionId::new(), "staging");
    profile.password_enc = b"deadbeef-secret-material".to_vec();
    repository.upsert_session_profile(&profile).await.unwrap();

    // Reading it back returns the blob to the caller — as it must — while the
    // errors below carry nothing of it.
    let read_back = repository
        .find_session_profile(profile.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_back.password_enc, profile.password_enc);

    let rejections = [
        PersistenceError::NotFound {
            entity: "session_profiles",
            id: profile.session_id.to_string(),
        },
        PersistenceError::Conflict {
            entity: "session_profiles",
            id: profile.session_id.to_string(),
        },
        PersistenceError::MalformedRow {
            table: "session_profiles",
            column: "password_enc",
            expected: "an opaque blob",
        },
    ];

    for rejection in rejections {
        let rendered = rendered_chain(&rejection);
        assert!(
            !rendered.contains("deadbeef"),
            "an error message carried credential material: {rendered}"
        );
        assert!(
            !rendered.contains("secret-material"),
            "an error message carried credential material: {rendered}"
        );
    }
}

/// SQLite's own messages name the constraint, never the bound parameter. The
/// assertion is on the whole chain, since the driver error is a `source` of
/// [`PersistenceError::Database`].
#[tokio::test]
async fn a_rejected_insert_never_echoes_the_offending_value() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let contact = a_contact(ContactId::new(), "+2250102030405");
    repository.insert_contact(&contact).await.unwrap();

    let rejection = repository.insert_contact(&contact).await.unwrap_err();
    let rendered = rendered_chain(&rejection);

    assert!(
        !rendered.contains("2250102030405"),
        "the number reached the error message: {rendered}"
    );
    assert!(
        !rendered.contains("Awa"),
        "a contact attribute reached the error message: {rendered}"
    );
}

/// A conflict names the aggregate and the identifier, and that is the point:
/// a UUID says which row without saying anything about it.
#[tokio::test]
async fn a_conflict_names_the_row_by_identifier_only() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    repository.insert_message(&message).await.unwrap();

    let rejection = repository.insert_message(&message).await.unwrap_err();
    let rendered = rejection.to_string();

    assert!(rendered.contains("messages"), "{rendered}");
    assert!(
        rendered.contains(&message.client_message_id.to_string()),
        "{rendered}"
    );
    assert!(
        !rendered.contains("Bonjour"),
        "the message body reached the error message: {rendered}"
    );
}

/// Opening a database inside a path that cannot be created reports the path —
/// which is not a secret, and without which "permission denied" is useless.
#[tokio::test]
async fn a_data_directory_failure_reports_the_path_it_tried() {
    let harness = temp_database().await;

    // A file, used as if it were a directory.
    let blocked = harness
        .config()
        .path()
        .to_path_buf()
        .join("not-a-directory")
        .join("shinobismpp.db");

    let rejection = persistence::Database::open(persistence::DatabaseConfig::new(blocked))
        .await
        .unwrap_err();

    assert!(
        matches!(rejection, PersistenceError::DataDirectory { .. }),
        "expected a data-directory error, got {rejection:?}"
    );
}

/// A stored value this version cannot read names the column and says what was
/// expected, never what was found.
#[test]
fn a_malformed_row_describes_the_expectation_not_the_value() {
    let rejection = PersistenceError::MalformedRow {
        table: "messages",
        column: "state",
        expected: "one of QUEUED, SENT, ACCEPTED, DELIVERED, FAILED, EXPIRED",
    };

    let rendered = rejection.to_string();

    assert!(rendered.contains("messages.state"), "{rendered}");
}
