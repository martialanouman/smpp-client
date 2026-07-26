//! Opening the database: pool, `PRAGMA`s and migrations.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{ConnectOptions as _, SqlitePool};

use crate::PersistenceError;

/// The versioned migrations of `migrations/`, embedded at compile time.
///
/// Embedding rather than reading the directory at runtime is what lets a
/// packaged application (spec §20) create its schema with no files shipped
/// alongside the executable. `build.rs` declares the directory so that adding
/// a migration triggers a rebuild.
static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// How the database is opened.
///
/// Every field has a default suited to a desktop application; a test overrides
/// what it needs through the builder methods.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    path: PathBuf,
    max_connections: u32,
    busy_timeout: Duration,
}

impl DatabaseConfig {
    /// Points at a database file.
    ///
    /// The parent directory is created on [`Database::open`] if it is missing;
    /// spec §14.4 puts it under the platform's application data directory,
    /// which the caller resolves — this crate depends on no Tauri API.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            // Ten readers plus the writer. SQLite serialises writes whatever
            // this value is, so raising it buys concurrency on reads only —
            // and each connection costs a file descriptor and a page cache.
            max_connections: 8,
            // How long a statement waits for the write lock before giving up.
            // WAL keeps readers out of the way, so the only contention left is
            // writer against writer; five seconds absorbs a long batch commit
            // without turning a genuine deadlock into an unbounded hang.
            busy_timeout: Duration::from_secs(5),
        }
    }

    /// Overrides the size of the connection pool.
    #[must_use]
    pub const fn with_max_connections(mut self, max_connections: u32) -> Self {
        self.max_connections = max_connections;
        self
    }

    /// Overrides how long a statement waits for the write lock.
    #[must_use]
    pub const fn with_busy_timeout(mut self, busy_timeout: Duration) -> Self {
        self.busy_timeout = busy_timeout;
        self
    }

    /// The database file this configuration points at.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// One entry of the SQLite catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaObject {
    /// `table`, `index`, `view` or `trigger`.
    pub kind: String,
    /// Name of the object.
    pub name: String,
    /// Table it belongs to; for a table, its own name.
    pub table: String,
}

/// An open database: a connection pool with the schema already migrated.
///
/// Cloning shares the same pool — `SqlitePool` is an `Arc` internally — so a
/// [`Database`] is passed by value to every repository without a second file
/// handle being opened.
#[derive(Debug, Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Opens the database, applying every pending migration.
    ///
    /// The file and its parent directory are created if missing. The call
    /// returns once the schema is at the version this build ships.
    ///
    /// # Errors
    ///
    /// * [`PersistenceError::DataDirectory`] if the parent directory cannot be
    ///   created;
    /// * [`PersistenceError::Open`] if SQLite refuses the file or the options;
    /// * [`PersistenceError::Migrate`] if a migration fails — including when a
    ///   migration already applied to this file no longer matches the one
    ///   shipped, which `sqlx` detects by checksum (guide §11.2).
    pub async fn open(config: DatabaseConfig) -> Result<Self, PersistenceError> {
        if let Some(parent) = config.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(|source| {
                    PersistenceError::DataDirectory {
                        path: parent.to_path_buf(),
                        source,
                    }
                })?;
            }
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(config.max_connections)
            .connect_with(connect_options(&config))
            .await
            .map_err(|source| PersistenceError::Open { source })?;

        let database = Self { pool };
        database.migrate().await?;

        tracing::info!(
            path = %config.path.display(),
            max_connections = config.max_connections,
            "database opened"
        );

        Ok(database)
    }

    /// Applies every pending migration.
    ///
    /// Called by [`Self::open`]; exposed so a caller can re-run it after
    /// restoring a file.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Migrate`] if a migration fails or a shipped
    /// migration was edited after being applied.
    pub async fn migrate(&self) -> Result<(), PersistenceError> {
        MIGRATIONS
            .run(&self.pool)
            .await
            .map_err(|source| PersistenceError::Migrate { source })
    }

    /// The journal mode SQLite reports for this file.
    ///
    /// Expected to be `wal` (spec §14.1). Reading it back is the only honest
    /// way to assert it: `journal_mode` is a *request* SQLite may decline —
    /// on a filesystem without shared memory, for instance — and it declines
    /// silently.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the pragma cannot be read.
    pub async fn journal_mode(&self) -> Result<String, PersistenceError> {
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await?;

        Ok(mode)
    }

    /// Reports whether foreign keys are enforced on a fresh connection.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the pragma cannot be read.
    pub async fn foreign_keys_enforced(&self) -> Result<bool, PersistenceError> {
        let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&self.pool)
            .await?;

        Ok(enabled != 0)
    }

    /// Lists the tables and indexes the file actually holds.
    ///
    /// Part of the integrated diagnostics of spec §18.3, and the only way an
    /// integration test can check the schema without reaching for SQL of its
    /// own — which CA-002-03 rules out. SQLite's own bookkeeping objects
    /// (`sqlite_sequence`, the automatic indexes behind a primary key) are
    /// filtered out: they are not part of what the migrations declare.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the catalogue cannot be read.
    pub async fn schema_objects(&self) -> Result<Vec<SchemaObject>, PersistenceError> {
        let rows = sqlx::query!(
            r#"SELECT type AS "kind!: String", name AS "name!: String",
                      tbl_name AS "table_name!: String"
               FROM sqlite_master
               WHERE name NOT LIKE 'sqlite_%'
               ORDER BY type, name"#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| SchemaObject {
                kind: row.kind,
                name: row.name,
                table: row.table_name,
            })
            .collect())
    }

    /// Closes the pool, waiting for in-flight statements to finish.
    ///
    /// Dropping a [`Database`] also closes it, but without waiting; a caller
    /// shutting the application down wants the wait.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// The underlying pool.
    ///
    /// `pub(crate)` on purpose: it is what keeps CA-002-03 true. If callers
    /// could reach the pool they could run their own SQL, and "no SQL outside
    /// `persistence`" would be a convention instead of a compile error.
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Builds the per-connection options.
///
/// step-002 §6 insists on the point and it is worth restating: these settings
/// are attached to the pool's *connect options*, so `sqlx` replays them on
/// EVERY connection it opens, not only the first. `foreign_keys` and
/// `busy_timeout` are per-connection state in SQLite; setting them once, on
/// connection one, leaves connections two through eight silently running
/// without referential integrity — a failure mode that only shows up under
/// load, which is when it hurts most.
fn connect_options(config: &DatabaseConfig) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(&config.path)
        .create_if_missing(true)
        // Spec §14.1. Readers no longer block the writer and the writer no
        // longer blocks readers: the UI can page through the message log while
        // a campaign is writing to it.
        .journal_mode(SqliteJournalMode::Wal)
        // `Full` fsyncs on every commit; in WAL that costs a disk flush per
        // transaction, and a campaign commits constantly. `Normal` in WAL is
        // the documented pairing: a crash of the *application* loses nothing,
        // only a power loss can lose the last transactions.
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(config.busy_timeout)
        // Off by default in SQLite, for backwards compatibility with files
        // written before 3.6.19. The schema declares foreign keys, so leaving
        // this off would make them decorative.
        .foreign_keys(true)
        // sqlx logs any statement outstanding for more than a second at WARN,
        // with its full text, when the `QueryLogger` is dropped. That default
        // is written for a request/response service; here the longest-lived
        // statement by design is `stream_messages`, and an export of a large
        // campaign would emit a WARN carrying the whole SELECT every time.
        // Noise, not a leak — the bound parameters are not part of it — but
        // noise that would teach the reader to ignore WARN.
        //
        // `Debug` rather than `Off`: a slow statement is still worth seeing
        // when someone goes looking, and the threshold is raised to thirty
        // seconds so it means "something is wrong" rather than "a traversal is
        // running".
        .log_slow_statements(log::LevelFilter::Debug, Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::{DatabaseConfig, MIGRATIONS};

    #[test]
    fn the_embedded_migrator_ships_at_least_one_migration() {
        assert!(
            MIGRATIONS.iter().next().is_some(),
            "migrations/ is embedded but empty"
        );
    }

    #[test]
    fn the_configuration_keeps_the_path_it_was_given() {
        let config = DatabaseConfig::new("/tmp/shinobismpp.db");

        assert_eq!(config.path().to_string_lossy(), "/tmp/shinobismpp.db");
    }
}
