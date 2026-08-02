//! What an error says, and above all what it never says — CA-002-09.

// See the note in `schema.rs`: `tests/` is compiled without `cfg(test)`, so
// the test relaxations of `clippy.toml` do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use std::error::Error;

use contacts::ports::{ContactRepository, ContactStoreError};
use messaging::ports::{MessageRepository, MessageStoreError};
use persistence::ports::SessionProfileRepository;
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

/// SQLite's own messages name the constraint, never the bound parameter — and
/// the contact port drops the driver chain anyway (ADR 0012). Both renderings
/// are checked: `Display`, which reaches the interface, and `Debug`, which is
/// what a `tracing` field would print.
#[tokio::test]
async fn a_rejected_contact_insert_never_echoes_the_offending_value() {
    let harness = temp_database().await;
    let repository = SqliteContactRepository::new(harness.database().clone());

    let contact = a_contact(ContactId::new(), "+2250102030405");
    repository.insert_contact(&contact).await.unwrap();

    let rejection = repository.insert_contact(&contact).await.unwrap_err();
    let rendered = format!("{rejection} | {rejection:?}");

    assert_eq!(rejection, ContactStoreError::Conflict);
    assert!(
        !rendered.contains("2250102030405"),
        "the number reached the error message: {rendered}"
    );
    assert!(
        !rendered.contains("Awa"),
        "a contact attribute reached the error message: {rendered}"
    );
    assert!(
        !rendered.contains(&contact.contact_id.to_string()),
        "the identifier reached the port's error: {rendered}"
    );
}

/// A conflict names the aggregate and the identifier, and that is the point: a
/// UUID says which row without saying anything about it.
///
/// # Why this is built rather than provoked
///
/// It used to be provoked through `SqliteContactRepository::insert_contact`.
/// That method now reports a `ContactStoreError`, which deliberately carries
/// neither the identifier nor the driver chain (ADR 0012), and every other
/// public write of this crate is an upsert or an autoincrement — so no port
/// still hands a `PersistenceError::Conflict` back.
///
/// The *classification* — a SQLite uniqueness violation becoming a `Conflict`
/// rather than a `Database` — is what the test above proves, since the port
/// variant it asserts can only be reached through it. What is left to check
/// here is the rendering, and that is what this does.
#[test]
fn a_conflict_names_the_row_by_identifier_only() {
    let contact_id = ContactId::new();

    let rejection = PersistenceError::Conflict {
        entity: "contacts",
        id: contact_id.to_string(),
    };
    let rendered = rendered_chain(&rejection);

    assert!(rendered.contains("contacts"), "{rendered}");
    assert!(rendered.contains(&contact_id.to_string()), "{rendered}");
    assert!(
        !rendered.contains("2250102030405"),
        "the number reached the error message: {rendered}"
    );
}

/// The message port says **less** than the storage error behind it, and that
/// is deliberate rather than an oversight (ADR 0010).
///
/// `MessageStoreError` names what a caller can act on and nothing else. It is
/// what reaches `messaging`, and through it the IPC boundary, so a UUID it
/// carried would be a UUID in a toast. The identifier and the source chain
/// stay in the log line `persistence` writes before mapping.
#[tokio::test]
async fn a_message_store_conflict_carries_neither_the_identifier_nor_the_body() {
    let harness = temp_database().await;
    let repository = SqliteMessageRepository::new(harness.database().clone());

    let message = a_queued_message(ClientMessageId::new(), "+2250102030405");
    repository.insert_message(&message).await.unwrap();

    let rejection = repository.insert_message(&message).await.unwrap_err();

    assert_eq!(rejection, MessageStoreError::Conflict);

    let rendered = rejection.to_string();
    assert!(
        !rendered.contains(&message.client_message_id.to_string()),
        "{rendered}"
    );
    assert!(!rendered.contains("2250102030405"), "{rendered}");
    assert!(!rendered.contains("Bonjour"), "{rendered}");
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
