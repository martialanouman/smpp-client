//! The PDU log.

use smpp_core::types::SessionId;

use crate::db::Database;
use crate::ports::PduLogRepository;
use crate::records::{PduDirection, PduLogEntry};
use crate::repositories::convert::{read_optional_session_id, read_timestamp, read_u32, store_u32};
use crate::repositories::page::{into_page, PagedRow};
use crate::{Cursor, Page, PersistenceError};

const TABLE: &str = "pdu_log";

/// The SQLx implementation of [`PduLogRepository`].
#[derive(Debug, Clone)]
pub struct SqlitePduLogRepository {
    database: Database,
}

impl SqlitePduLogRepository {
    /// Binds the repository to an open database.
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

/// One row of `pdu_log`, exactly as SQLite stores it.
struct PduLogRow {
    id: i64,
    session_id: Option<String>,
    direction: String,
    command_id: Option<i64>,
    command_status: Option<i64>,
    sequence_number: Option<i64>,
    raw_hex: Option<String>,
    decoded: Option<String>,
    ts: String,
}

impl PagedRow for PduLogRow {
    type Record = PduLogEntry;

    fn cursor(&self) -> i64 {
        self.id
    }

    fn into_record(self) -> Result<PduLogEntry, PersistenceError> {
        Ok(PduLogEntry {
            session_id: read_optional_session_id(self.session_id.as_deref(), TABLE, "session_id")?,
            direction: PduDirection::parse(&self.direction)?,
            command_id: self
                .command_id
                .map(|raw| read_u32(raw, TABLE, "command_id"))
                .transpose()?,
            command_status: self
                .command_status
                .map(|raw| read_u32(raw, TABLE, "command_status"))
                .transpose()?,
            sequence_number: self
                .sequence_number
                .map(|raw| read_u32(raw, TABLE, "sequence_number"))
                .transpose()?,
            raw_hex: self.raw_hex,
            decoded: self.decoded,
            ts: read_timestamp(&self.ts, TABLE, "ts")?,
        })
    }
}

/// Writes one entry on the given executor.
///
/// Generic over the executor so the same statement serves a single insert and
/// a batch inside a transaction — the alternative is the same SQL written
/// twice, which is the same SQL until someone changes one of them.
async fn insert_one<'e, E>(executor: E, entry: &PduLogEntry) -> Result<i64, PersistenceError>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let session_id = entry.session_id.map(|id| id.to_string());
    let direction = entry.direction.as_str();
    let command_id = entry.command_id.map(store_u32);
    let command_status = entry.command_status.map(store_u32);
    let sequence_number = entry.sequence_number.map(store_u32);
    let ts = entry.ts.to_storage();

    let id = sqlx::query!(
        r#"INSERT INTO pdu_log (
               session_id, direction, command_id, command_status,
               sequence_number, raw_hex, decoded, ts
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
        session_id,
        direction,
        command_id,
        command_status,
        sequence_number,
        entry.raw_hex,
        entry.decoded,
        ts
    )
    .execute(executor)
    .await?
    .last_insert_rowid();

    Ok(id)
}

impl PduLogRepository for SqlitePduLogRepository {
    async fn insert_entry(&self, entry: &PduLogEntry) -> Result<i64, PersistenceError> {
        let mut connection = self.database.pool().acquire().await?;

        insert_one(&mut *connection, entry).await
    }

    async fn insert_entries(&self, entries: &[PduLogEntry]) -> Result<u64, PersistenceError> {
        // ONE transaction for the whole batch. The `?` on each insert drops
        // `transaction` without committing, which rolls back: a batch either
        // lands whole or not at all.
        let mut transaction = self.database.pool().begin().await?;

        for entry in entries {
            insert_one(&mut *transaction, entry).await?;
        }

        transaction.commit().await?;

        Ok(entries.len().try_into().unwrap_or(u64::MAX))
    }

    async fn page_entries(
        &self,
        session_id: Option<SessionId>,
        cursor: Cursor,
        limit: u32,
    ) -> Result<Page<PduLogEntry>, PersistenceError> {
        let session = session_id.map(|id| id.to_string());
        let after = cursor.into_raw();
        let window = store_u32(limit);

        let rows = sqlx::query_as!(
            PduLogRow,
            r#"SELECT id AS "id!: i64",
                      session_id, direction, command_id, command_status,
                      sequence_number, raw_hex, decoded, ts
               FROM pdu_log
               WHERE id > ?
                 AND (? IS NULL OR session_id = ?)
               ORDER BY id
               LIMIT ?"#,
            after,
            session,
            session,
            window
        )
        .fetch_all(self.database.pool())
        .await?;

        into_page(rows, limit)
    }
}
