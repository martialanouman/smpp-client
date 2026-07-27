//! Migrations: application, idempotence and immutability — CA-002-08.

// See the note in `schema.rs`: `tests/` is compiled without `cfg(test)`, so
// the test relaxations of `clippy.toml` do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use support::temp_database;

/// The fingerprint of every migration shipped so far.
///
/// # Why this table is written out by hand
///
/// Guide §11.2: a shipped migration is never edited. `sqlx` enforces that
/// against *databases it has already migrated* — it stores each migration's
/// checksum and refuses a file that no longer matches. That check is silent on
/// a fresh clone, which is precisely where the edit happens: a developer fixes
/// a typo in a migration, their own database rejects it, they delete their
/// database, and the change ships. Every user who already ran the old version
/// then has a schema nobody can reproduce.
///
/// Pinning the hashes here moves the detection to the build. Adding a
/// migration means adding a line; changing one means the test fails, and the
/// only correct fix is a new migration.
///
/// Regenerate a line with:
/// `shasum -a 256 migrations/<file>`
const SHIPPED_MIGRATIONS: &[(&str, &str)] = &[
    (
        "20260726120000_initial_schema.down.sql",
        "ec818bdaf1c2cdbbb65ba131da086db78b86842b8caa1e6ab927b273721c0b51",
    ),
    (
        "20260726120000_initial_schema.up.sql",
        "1a7aa1dc5a997eb11a414689c9ce4f04e767004700e244d8b8a7b667d285ce75",
    ),
    // Milestone 005, ADR 0009: the two GSM 7-bit layout characteristics.
    (
        "20260726180000_session_gsm7_layout.down.sql",
        "a13e8b4c2c1364b7991ac4f19f983f806611402ab9050d045a21729e5cd17db1",
    ),
    (
        "20260726180000_session_gsm7_layout.up.sql",
        "7d75d2de3b026e5a44738ebd97e56250bba3579caa19e89ac63807d6cd0756e0",
    ),
    // Milestone 007, spec §9.4: the floor of the adaptive throughput band.
    (
        "20260726210000_session_min_tps.down.sql",
        "6442c8c0b4ad347a0b1696b433e6b59db78b4a1f2753c01be65ad0f4068fad87",
    ),
    (
        "20260726210000_session_min_tps.up.sql",
        "9caad6209ffdc305a12528c73e90bc613447b6ae173e5829339bc15cd603bf67",
    ),
];

/// The `migrations/` directory, resolved from this crate's manifest.
fn migrations_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

fn sha256_of(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("reading a migration file");
    let digest = Sha256::digest(&bytes);

    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn migration_files() -> BTreeMap<String, PathBuf> {
    std::fs::read_dir(migrations_directory())
        .expect("reading the migrations directory")
        .map(|entry| entry.expect("reading a directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .map(|path| {
            (
                path.file_name()
                    .expect("a file has a name")
                    .to_string_lossy()
                    .into_owned(),
                path,
            )
        })
        .collect()
}

#[test]
fn no_shipped_migration_has_been_edited() {
    let files = migration_files();

    for (name, expected) in SHIPPED_MIGRATIONS {
        let path = files
            .get(*name)
            .unwrap_or_else(|| panic!("shipped migration `{name}` has disappeared"));

        assert_eq!(
            &sha256_of(path),
            expected,
            "migration `{name}` was edited after being shipped; add a new one instead"
        );
    }
}

#[test]
fn every_migration_on_disk_is_pinned() {
    let files = migration_files();

    for name in files.keys() {
        assert!(
            SHIPPED_MIGRATIONS.iter().any(|(pinned, _)| pinned == name),
            "migration `{name}` is not pinned in SHIPPED_MIGRATIONS"
        );
    }
}

/// A reversible migration ships both halves. Guide §11.2 accepts a
/// non-reversible migration only when it says so; a missing `.down.sql` is
/// silence, not a statement.
#[test]
fn every_migration_ships_both_directions() {
    let files = migration_files();

    for name in files.keys() {
        let Some(version) = name.strip_suffix(".up.sql") else {
            continue;
        };

        assert!(
            files.contains_key(&format!("{version}.down.sql")),
            "migration `{version}` has no down script"
        );
    }
}

#[tokio::test]
async fn migrations_apply_to_a_pristine_database() {
    let harness = temp_database().await;

    // `temp_database` already migrated; reaching the assertion at all means
    // the run succeeded on an empty file.
    assert_eq!(harness.database().journal_mode().await.unwrap(), "wal");
}

/// Re-running the migrator must be a no-op, not an error: it runs on every
/// application start.
#[tokio::test]
async fn re_running_the_migrator_changes_nothing() {
    let harness = temp_database().await;

    let before = harness.database().schema_objects().await.unwrap();
    harness.database().migrate().await.unwrap();
    let after = harness.database().schema_objects().await.unwrap();

    assert_eq!(before, after);
}

/// Opening the same file a second time replays the migrator against a database
/// that already has the schema — and `sqlx` validates the stored checksums
/// while doing so. An edited migration fails right here.
#[tokio::test]
async fn re_opening_a_migrated_file_validates_the_stored_checksums() {
    let harness = temp_database().await;

    let reopened = harness.reopen().await;

    assert_eq!(reopened.journal_mode().await.unwrap(), "wal");
}
