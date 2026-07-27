//! Delivery receipts that correlate to no message (CA-008-04).
//!
//! Two halves, and they belong to different consumers, so they are two traits:
//!
//! * the **write** half is `messaging::correlation::OrphanReceiptStore` — the
//!   receipt pipeline declares it, this crate implements it (CLAUDE.md §3);
//! * the **read** half is [`OrphanJournal`], declared here for the same reason
//!   [`crate::ports::MessageJournal`] is: its consumer is the log screen, it
//!   speaks in [`Cursor`] and [`Page`], and those types exist because of the
//!   storage they page over.

use messaging::correlation::{OrphanReason, OrphanReceipt, OrphanReceiptStore};
use messaging::ports::MessageStoreError;
use smpp_core::types::SessionId;

use crate::db::Database;
use crate::repositories::convert::{
    read_optional_session_id, read_optional_timestamp, read_timestamp,
};
use crate::repositories::page::{into_page, PagedRow};
use crate::{Cursor, Page, PersistenceError};

const TABLE: &str = "dlr_orphans";

/// The SQLx implementation of the two orphan halves.
#[derive(Debug, Clone)]
pub struct SqliteOrphanRepository {
    database: Database,
}

impl SqliteOrphanRepository {
    /// Binds the repository to an open database.
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

/// One row of `dlr_orphans`, exactly as SQLite stores it.
struct OrphanRow {
    id: i64,
    session_id: Option<String>,
    smsc_message_id: Option<String>,
    reason: String,
    dlr_stat: Option<String>,
    dlr_err: Option<String>,
    submit_date: Option<String>,
    done_date: Option<String>,
    raw: String,
    received_at: String,
}

impl PagedRow for OrphanRow {
    type Record = StoredOrphan;

    fn cursor(&self) -> i64 {
        self.id
    }

    fn into_record(self) -> Result<StoredOrphan, PersistenceError> {
        // The `CHECK` constraint of the migration makes this unreachable
        // through this application; it is reachable through a hand-written
        // `UPDATE` on the file, which is what the schema's second line of
        // defence is for.
        let reason = OrphanReason::parse(&self.reason).ok_or(PersistenceError::MalformedRow {
            table: TABLE,
            column: "reason",
            expected: "UNKNOWN_ID or NO_IDENTIFIER",
        })?;

        Ok(StoredOrphan {
            id: self.id,
            receipt: OrphanReceipt {
                session_id: read_optional_session_id(
                    self.session_id.as_deref(),
                    TABLE,
                    "session_id",
                )?,
                smsc_message_id: self.smsc_message_id,
                reason,
                dlr_stat: self.dlr_stat,
                dlr_err: self.dlr_err,
                submit_date: read_optional_timestamp(
                    self.submit_date.as_deref(),
                    TABLE,
                    "submit_date",
                )?,
                done_date: read_optional_timestamp(self.done_date.as_deref(), TABLE, "done_date")?,
                raw: self.raw,
                received_at: read_timestamp(&self.received_at, TABLE, "received_at")?,
            },
        })
    }
}

/// An orphan as it comes back out of storage.
///
/// Carries the auto-incremented identifier the log screen uses as a stable
/// React key; the receipt itself has none — nothing about a receipt that
/// matched nothing is unique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOrphan {
    /// Row identifier, and the arrival order.
    pub id: i64,
    /// What arrived.
    pub receipt: OrphanReceipt,
}

/// Reads the orphan journal in bulk.
///
/// Consumed by the log screen; see the module note for why it is declared here
/// rather than in `logging-export`.
pub trait OrphanJournal {
    /// Reads one page of orphans, oldest first, optionally for one session.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails, or
    /// [`PersistenceError::MalformedRow`] if a stored value no longer fits its
    /// type.
    fn page_orphans(
        &self,
        session_id: Option<SessionId>,
        cursor: Cursor,
        limit: u32,
    ) -> impl core::future::Future<Output = Result<Page<StoredOrphan>, PersistenceError>> + Send;

    /// Counts the orphans recorded, optionally for one session.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn count_orphans(
        &self,
        session_id: Option<SessionId>,
    ) -> impl core::future::Future<Output = Result<u64, PersistenceError>> + Send;
}

impl SqliteOrphanRepository {
    /// Writes a batch of orphans in **one** transaction.
    ///
    /// One transaction for the batch, for the reason CA-008-10 gives about
    /// transitions: a message centre replaying a backlog sends orphans as fast
    /// as it sends receipts, and one `fsync` each would make the pipeline the
    /// bottleneck.
    async fn stored_insert_orphans(
        &self,
        orphans: &[OrphanReceipt],
    ) -> Result<u64, PersistenceError> {
        let mut transaction = self.database.pool().begin().await?;

        for orphan in orphans {
            let session_id = orphan.session_id.map(|id| id.to_string());
            let reason = orphan.reason.as_str();
            let submit_date = orphan.submit_date.map(|instant| instant.to_storage());
            let done_date = orphan.done_date.map(|instant| instant.to_storage());
            let received_at = orphan.received_at.to_storage();

            sqlx::query!(
                r#"INSERT INTO dlr_orphans (
                       session_id, smsc_message_id, reason,
                       dlr_stat, dlr_err, submit_date, done_date, raw, received_at
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
                session_id,
                orphan.smsc_message_id,
                reason,
                orphan.dlr_stat,
                orphan.dlr_err,
                submit_date,
                done_date,
                orphan.raw,
                received_at
            )
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(orphans.len().try_into().unwrap_or(u64::MAX))
    }
}

/// The write half, as `messaging` declares it.
///
/// The error is projected onto the port's vocabulary and the full chain — the
/// `sqlx::Error`, and on one variant a filesystem path — is logged here rather
/// than travelling, exactly as [`crate::SqliteMessageRepository`] does.
impl OrphanReceiptStore for SqliteOrphanRepository {
    async fn insert_orphans(&self, orphans: &[OrphanReceipt]) -> Result<u64, MessageStoreError> {
        self.stored_insert_orphans(orphans).await.map_err(|error| {
            tracing::error!(error = ?error, "the orphan journal refused a write");

            MessageStoreError::Unavailable {
                reason: error.to_string(),
            }
        })
    }
}

impl OrphanJournal for SqliteOrphanRepository {
    async fn page_orphans(
        &self,
        session_id: Option<SessionId>,
        cursor: Cursor,
        limit: u32,
    ) -> Result<Page<StoredOrphan>, PersistenceError> {
        let session = session_id.map(|id| id.to_string());
        let after = cursor.into_raw();
        let window = crate::repositories::convert::store_u32(limit);

        let rows = sqlx::query_as!(
            OrphanRow,
            r#"SELECT id AS "id!: i64", session_id, smsc_message_id, reason,
                      dlr_stat, dlr_err, submit_date, done_date, raw, received_at
               FROM dlr_orphans
               WHERE id > ? AND (? IS NULL OR session_id = ?)
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

    async fn count_orphans(&self, session_id: Option<SessionId>) -> Result<u64, PersistenceError> {
        let session = session_id.map(|id| id.to_string());

        let total = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "total!: i64" FROM dlr_orphans
               WHERE (? IS NULL OR session_id = ?)"#,
            session,
            session
        )
        .fetch_one(self.database.pool())
        .await?;

        Ok(u64::try_from(total).unwrap_or(0))
    }
}
