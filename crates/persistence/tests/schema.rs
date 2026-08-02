//! What a freshly migrated file actually contains — CA-002-01, CA-002-02.

// Integration tests are compiled WITHOUT `cfg(test)`, so the relaxations of
// `clippy.toml` (`allow-unwrap-in-tests`) do not reach them.
//
//   · `unwrap`/`expect`: a panic here IS the failure report. The lints exist
//     to keep them out of production code, which this is not.
//   · `disallowed_methods`: `#[tokio::test]` expands to
//     `Runtime::block_on`, and `clippy.toml` reserves that call for "the
//     binary entry point". A test harness is one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use persistence::SchemaObject;

use support::temp_database;

/// Every table and index spec §14.2 declares.
const EXPECTED_TABLES: &[&str] = &[
    "campaigns",
    "contact_list_members",
    "contact_lists",
    "contacts",
    // Milestone 008, CA-008-04: receipts that correlate to no message.
    "dlr_orphans",
    "import_profiles",
    "messages",
    "pdu_log",
    "session_profiles",
];

const EXPECTED_INDEXES: &[&str] = &[
    "idx_contacts_msisdn",
    "idx_dlr_orphans_session",
    "idx_dlr_orphans_smscid",
    "idx_messages_campaign",
    "idx_messages_smscid",
    "idx_messages_state",
];

fn names_of(objects: &[SchemaObject], kind: &str) -> Vec<String> {
    objects
        .iter()
        .filter(|object| object.kind == kind)
        .map(|object| object.name.clone())
        .collect()
}

#[tokio::test]
async fn a_fresh_database_runs_in_wal_mode() {
    let harness = temp_database().await;

    assert_eq!(harness.database().journal_mode().await.unwrap(), "wal");
}

#[tokio::test]
async fn foreign_keys_are_enforced_on_a_pooled_connection() {
    let harness = temp_database().await;

    assert!(harness.database().foreign_keys_enforced().await.unwrap());
}

/// The pragmas are set on the pool's connect options, so they apply to every
/// connection — not only the one that happened to open the file. Asking eight
/// times over a pool of eight forces sqlx to open more than one.
#[tokio::test]
async fn every_connection_of_the_pool_carries_the_pragmas() {
    let harness = temp_database().await;
    let database = harness.database();

    let checks = (0..8).map(|_| async move {
        (
            database.journal_mode().await.unwrap(),
            database.foreign_keys_enforced().await.unwrap(),
        )
    });

    for (mode, foreign_keys) in futures_util::future::join_all(checks).await {
        assert_eq!(mode, "wal");
        assert!(foreign_keys);
    }
}

#[tokio::test]
async fn every_table_of_the_specification_exists() {
    let harness = temp_database().await;

    let objects = harness.database().schema_objects().await.unwrap();
    let tables = names_of(&objects, "table");

    for expected in EXPECTED_TABLES {
        assert!(
            tables.iter().any(|name| name == expected),
            "missing table `{expected}`; found {tables:?}"
        );
    }
}

#[tokio::test]
async fn every_index_of_the_specification_exists() {
    let harness = temp_database().await;

    let objects = harness.database().schema_objects().await.unwrap();
    let indexes = names_of(&objects, "index");

    for expected in EXPECTED_INDEXES {
        assert!(
            indexes.iter().any(|name| name == expected),
            "missing index `{expected}`; found {indexes:?}"
        );
    }
}

/// The migration table is `sqlx`'s bookkeeping and does not belong to the
/// schema of spec §14.2 — but it must exist, otherwise nothing records which
/// migrations ran.
#[tokio::test]
async fn the_migration_bookkeeping_table_exists() {
    let harness = temp_database().await;

    let objects = harness.database().schema_objects().await.unwrap();
    let tables = names_of(&objects, "table");

    assert!(tables.iter().any(|name| name == "_sqlx_migrations"));
}

/// The schema holds nothing beyond what the migrations declare. A stray table
/// means a migration did something it did not say it was doing.
#[tokio::test]
async fn the_schema_holds_nothing_unexpected() {
    let harness = temp_database().await;

    let objects = harness.database().schema_objects().await.unwrap();

    for table in names_of(&objects, "table") {
        assert!(
            EXPECTED_TABLES.contains(&table.as_str()) || table == "_sqlx_migrations",
            "unexpected table `{table}`"
        );
    }
}
