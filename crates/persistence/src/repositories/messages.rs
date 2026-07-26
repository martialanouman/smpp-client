//! The message journal.
//!
//! # Why the filtered queries are written out four times
//!
//! [`MessageFilter`] has two indexed columns — `campaign_id` and `state` — and
//! one that is not, `session_id`. The obvious way to keep a single SQL literal
//! is `(? IS NULL OR campaign_id = ?)`, and it is a trap: SQLite cannot use an
//! index behind an `OR`, so the whole filter degrades to a scan. Measured on
//! this schema:
//!
//! ```text
//! WHERE (? IS NULL OR state = ?)  ->  SCAN messages USING COVERING INDEX idx_messages_state
//! WHERE state = ?                 ->  SEARCH messages USING INDEX idx_messages_state (state=?)
//! ```
//!
//! A scan is O(table); a seek is O(matches). On a 500 000-row log filtered
//! down to a few hundred failures, that is the difference between the cursor
//! pagination this crate promises and a linear re-walk on every page.
//! `idx_messages_campaign` and `idx_messages_state` exist because CA-002-02
//! requires them; the `OR` form made them decorative.
//!
//! So the two indexed columns are matched on at the Rust level, into the four
//! literal queries an index can serve. `session_id` keeps the `OR` form: it has
//! no index, so nothing is lost, and it was measured NOT to prevent the other
//! two from using theirs. `repositories::plans` holds that measurement as a
//! test, driven by the `.sqlx` cache so it cannot drift from the queries.
//!
//! The cost is four literals per query instead of one. The compile-time
//! checking of ADR 0002 is preserved, and there is no cheaper way to keep it:
//! `sqlx::query_as!` demands a string **literal**, and rejects `concat!`.

use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use smpp_core::types::ClientMessageId;

use messaging::ports::{MessageRepository, MessageStoreError};

use crate::db::Database;
use crate::ports::MessageJournal;
use crate::records::{
    Message, MessageFilter, MessageState, MessageStateUpdate, SmscMessageIdUpdate,
};
use crate::repositories::convert::{
    read_client_message_id, read_command_status, read_data_coding, read_message_state, read_msisdn,
    read_npi, read_optional_campaign_id, read_optional_session_id, read_optional_timestamp,
    read_timestamp, read_ton, read_u32, store_command_status, store_u32, store_u8,
};
use crate::repositories::page::{into_page, PagedRow};
use crate::{Cursor, Page, PersistenceError};

const TABLE: &str = "messages";

/// The SQLx implementation of [`MessageRepository`].
///
/// Holds a [`Database`], which is a cloned handle on the shared pool: building
/// one is free, so a caller creates it where it needs it rather than threading
/// a long-lived reference around.
#[derive(Debug, Clone)]
pub struct SqliteMessageRepository {
    database: Database,
}

impl SqliteMessageRepository {
    /// Binds the repository to an open database.
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

/// One row of `messages`, exactly as SQLite stores it.
///
/// The intermediate step between the columns and [`Message`]. It exists so the
/// queries reading this table share one mapping — written once, in
/// [`PagedRow::into_record`] — instead of a copy each.
///
/// `rowid` is selected by every query, including the ones that discard it.
/// Reading it costs nothing (it is the row key the engine already holds), and
/// the alternative is a second row struct carrying a second copy of the
/// twenty-two-column mapping.
struct MessageRow {
    rowid: i64,
    client_message_id: String,
    campaign_id: Option<String>,
    session_id: Option<String>,
    smsc_message_id: Option<String>,
    source_addr: Option<String>,
    source_ton: Option<i64>,
    source_npi: Option<i64>,
    dest_addr: Option<String>,
    dest_ton: Option<i64>,
    dest_npi: Option<i64>,
    data_coding: Option<i64>,
    segments: i64,
    text: Option<String>,
    state: String,
    command_status: Option<i64>,
    dlr_stat: Option<String>,
    dlr_err: Option<String>,
    attempts: i64,
    created_at: String,
    sent_at: Option<String>,
    resp_at: Option<String>,
    dlr_at: Option<String>,
}

impl PagedRow for MessageRow {
    type Record = Message;

    fn cursor(&self) -> i64 {
        self.rowid
    }

    fn into_record(self) -> Result<Message, PersistenceError> {
        Ok(Message {
            client_message_id: read_client_message_id(
                &self.client_message_id,
                TABLE,
                "client_message_id",
            )?,
            campaign_id: read_optional_campaign_id(self.campaign_id.as_deref())?,
            session_id: read_optional_session_id(self.session_id.as_deref(), TABLE, "session_id")?,
            smsc_message_id: self.smsc_message_id,
            source_addr: self.source_addr,
            source_ton: read_ton(self.source_ton, TABLE, "source_ton")?,
            source_npi: read_npi(self.source_npi, TABLE, "source_npi")?,
            dest_addr: self
                .dest_addr
                .as_deref()
                .map(|raw| read_msisdn(raw, TABLE, "dest_addr"))
                .transpose()?,
            dest_ton: read_ton(self.dest_ton, TABLE, "dest_ton")?,
            dest_npi: read_npi(self.dest_npi, TABLE, "dest_npi")?,
            data_coding: read_data_coding(self.data_coding, TABLE, "data_coding")?,
            segments: read_u32(self.segments, TABLE, "segments")?,
            text: self.text,
            state: read_message_state(&self.state)?,
            command_status: read_command_status(self.command_status, TABLE, "command_status")?,
            dlr_stat: self.dlr_stat,
            dlr_err: self.dlr_err,
            attempts: read_u32(self.attempts, TABLE, "attempts")?,
            created_at: read_timestamp(&self.created_at, TABLE, "created_at")?,
            sent_at: read_optional_timestamp(self.sent_at.as_deref(), TABLE, "sent_at")?,
            resp_at: read_optional_timestamp(self.resp_at.as_deref(), TABLE, "resp_at")?,
            dlr_at: read_optional_timestamp(self.dlr_at.as_deref(), TABLE, "dlr_at")?,
        })
    }
}

/// The SQL half, reporting this crate's own error.
///
/// Private: the only way in from outside is the [`MessageRepository`] impl
/// below, so no caller can pick the richer error and start branching on a
/// `sqlx::Error` three layers up.
impl SqliteMessageRepository {
    async fn stored_insert_message(&self, message: &Message) -> Result<(), PersistenceError> {
        let mut connection = self.database.pool().acquire().await?;
        insert_one(&mut *connection, message).await
    }

    async fn stored_insert_messages(&self, messages: &[Message]) -> Result<u64, PersistenceError> {
        // ONE transaction for the whole batch (CA-002-06, guide §11.2). The
        // `?` on each insert drops `transaction` without committing, which
        // rolls back: a batch either lands whole or not at all.
        let mut transaction = self.database.pool().begin().await?;

        for message in messages {
            insert_one(&mut *transaction, message).await?;
        }

        transaction.commit().await?;

        Ok(messages.len().try_into().unwrap_or(u64::MAX))
    }

    async fn stored_find_message(
        &self,
        client_message_id: ClientMessageId,
    ) -> Result<Option<Message>, PersistenceError> {
        let identifier = client_message_id.to_string();

        let row = sqlx::query_as!(
            MessageRow,
            r#"SELECT rowid AS "rowid!: i64",
                      client_message_id, campaign_id, session_id, smsc_message_id,
                      source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                      data_coding, segments, text, state, command_status,
                      dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
               FROM messages
               WHERE client_message_id = ?"#,
            identifier
        )
        .fetch_optional(self.database.pool())
        .await?;

        row.map(PagedRow::into_record).transpose()
    }

    async fn stored_find_message_by_smsc_id(
        &self,
        smsc_message_id: &str,
    ) -> Result<Option<Message>, PersistenceError> {
        let row = sqlx::query_as!(
            MessageRow,
            r#"SELECT rowid AS "rowid!: i64",
                      client_message_id, campaign_id, session_id, smsc_message_id,
                      source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                      data_coding, segments, text, state, command_status,
                      dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
               FROM messages
               WHERE smsc_message_id = ?
               ORDER BY rowid
               LIMIT 1"#,
            smsc_message_id
        )
        .fetch_optional(self.database.pool())
        .await?;

        row.map(PagedRow::into_record).transpose()
    }

    async fn stored_update_state(
        &self,
        update: &MessageStateUpdate,
    ) -> Result<(), PersistenceError> {
        let mut connection = self.database.pool().acquire().await?;
        apply_update(&mut *connection, update).await
    }

    async fn stored_update_states(
        &self,
        updates: &[MessageStateUpdate],
    ) -> Result<u64, PersistenceError> {
        let mut transaction = self.database.pool().begin().await?;

        for update in updates {
            apply_update(&mut *transaction, update).await?;
        }

        transaction.commit().await?;

        Ok(updates.len().try_into().unwrap_or(u64::MAX))
    }
}

/// The write-and-lookup half of the journal, as `messaging` declares it.
///
/// # Why the two layers keep different error types
///
/// The methods above report a [`PersistenceError`], which names the failing
/// table and carries a `sqlx::Error` — the detail a log needs. The port speaks
/// in [`MessageStoreError`], which names only what a caller can act on. The
/// translation happens here, and it **logs the full failure first**: the source
/// chain the port cannot carry is written to the trace rather than dropped,
/// which is the price ADR 0010 records for the boundary.
impl MessageRepository for SqliteMessageRepository {
    async fn insert_message(&self, message: &Message) -> Result<(), MessageStoreError> {
        self.stored_insert_message(message)
            .await
            .map_err(store_error)
    }

    async fn insert_messages(&self, messages: &[Message]) -> Result<u64, MessageStoreError> {
        self.stored_insert_messages(messages)
            .await
            .map_err(store_error)
    }

    async fn find_message(
        &self,
        client_message_id: ClientMessageId,
    ) -> Result<Option<Message>, MessageStoreError> {
        self.stored_find_message(client_message_id)
            .await
            .map_err(store_error)
    }

    async fn find_message_by_smsc_id(
        &self,
        smsc_message_id: &str,
    ) -> Result<Option<Message>, MessageStoreError> {
        self.stored_find_message_by_smsc_id(smsc_message_id)
            .await
            .map_err(store_error)
    }

    async fn update_state(&self, update: &MessageStateUpdate) -> Result<(), MessageStoreError> {
        self.stored_update_state(update).await.map_err(store_error)
    }

    async fn update_states(
        &self,
        updates: &[MessageStateUpdate],
    ) -> Result<u64, MessageStoreError> {
        self.stored_update_states(updates)
            .await
            .map_err(store_error)
    }
}

/// Projects a storage failure onto the port's vocabulary.
///
/// The `#[source]` chain — the sqlx error, and on one variant a filesystem
/// path — is logged here and does not travel: the port is consumed by
/// `messaging`, whose failures reach the interface.
fn store_error(error: PersistenceError) -> MessageStoreError {
    tracing::error!(error = ?error, "the message journal refused an operation");

    match error {
        PersistenceError::Conflict { .. } => MessageStoreError::Conflict,
        PersistenceError::NotFound { .. } => MessageStoreError::NotFound,
        other => MessageStoreError::Unavailable {
            reason: other.to_string(),
        },
    }
}

impl MessageJournal for SqliteMessageRepository {
    async fn page_messages(
        &self,
        filter: &MessageFilter,
        cursor: Cursor,
        limit: u32,
    ) -> Result<Page<Message>, PersistenceError> {
        let session = filter.session_id.map(|id| id.to_string());
        let campaign = filter.campaign_id.map(|id| id.to_string());
        let state = filter.state.map(MessageState::as_str);
        let after = cursor.into_raw();
        let window = store_u32(limit);

        let rows = match (campaign.as_deref(), state) {
            (Some(campaign), Some(state)) => {
                sqlx::query_as!(
                    MessageRow,
                    r#"SELECT rowid AS "rowid!: i64",
                              client_message_id, campaign_id, session_id, smsc_message_id,
                              source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                              data_coding, segments, text, state, command_status,
                              dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
                       FROM messages
                       WHERE campaign_id = ? AND state = ? AND rowid > ?
                         AND (? IS NULL OR session_id = ?)
                       ORDER BY rowid
                       LIMIT ?"#,
                    campaign,
                    state,
                    after,
                    session,
                    session,
                    window
                )
                .fetch_all(self.database.pool())
                .await?
            }
            (Some(campaign), None) => {
                sqlx::query_as!(
                    MessageRow,
                    r#"SELECT rowid AS "rowid!: i64",
                              client_message_id, campaign_id, session_id, smsc_message_id,
                              source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                              data_coding, segments, text, state, command_status,
                              dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
                       FROM messages
                       WHERE campaign_id = ? AND rowid > ?
                         AND (? IS NULL OR session_id = ?)
                       ORDER BY rowid
                       LIMIT ?"#,
                    campaign,
                    after,
                    session,
                    session,
                    window
                )
                .fetch_all(self.database.pool())
                .await?
            }
            (None, Some(state)) => {
                sqlx::query_as!(
                    MessageRow,
                    r#"SELECT rowid AS "rowid!: i64",
                              client_message_id, campaign_id, session_id, smsc_message_id,
                              source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                              data_coding, segments, text, state, command_status,
                              dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
                       FROM messages
                       WHERE state = ? AND rowid > ?
                         AND (? IS NULL OR session_id = ?)
                       ORDER BY rowid
                       LIMIT ?"#,
                    state,
                    after,
                    session,
                    session,
                    window
                )
                .fetch_all(self.database.pool())
                .await?
            }
            (None, None) => {
                sqlx::query_as!(
                    MessageRow,
                    r#"SELECT rowid AS "rowid!: i64",
                              client_message_id, campaign_id, session_id, smsc_message_id,
                              source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                              data_coding, segments, text, state, command_status,
                              dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
                       FROM messages
                       WHERE rowid > ?
                         AND (? IS NULL OR session_id = ?)
                       ORDER BY rowid
                       LIMIT ?"#,
                    after,
                    session,
                    session,
                    window
                )
                .fetch_all(self.database.pool())
                .await?
            }
        };

        into_page(rows, limit)
    }

    async fn count_messages(&self, filter: &MessageFilter) -> Result<u64, PersistenceError> {
        let session = filter.session_id.map(|id| id.to_string());
        let campaign = filter.campaign_id.map(|id| id.to_string());
        let state = filter.state.map(MessageState::as_str);

        let total = match (campaign.as_deref(), state) {
            (Some(campaign), Some(state)) => {
                sqlx::query_scalar!(
                    r#"SELECT COUNT(*) AS "total!: i64" FROM messages
                       WHERE campaign_id = ? AND state = ?
                         AND (? IS NULL OR session_id = ?)"#,
                    campaign,
                    state,
                    session,
                    session
                )
                .fetch_one(self.database.pool())
                .await?
            }
            (Some(campaign), None) => {
                sqlx::query_scalar!(
                    r#"SELECT COUNT(*) AS "total!: i64" FROM messages
                       WHERE campaign_id = ?
                         AND (? IS NULL OR session_id = ?)"#,
                    campaign,
                    session,
                    session
                )
                .fetch_one(self.database.pool())
                .await?
            }
            (None, Some(state)) => {
                sqlx::query_scalar!(
                    r#"SELECT COUNT(*) AS "total!: i64" FROM messages
                       WHERE state = ?
                         AND (? IS NULL OR session_id = ?)"#,
                    state,
                    session,
                    session
                )
                .fetch_one(self.database.pool())
                .await?
            }
            (None, None) => {
                sqlx::query_scalar!(
                    r#"SELECT COUNT(*) AS "total!: i64" FROM messages
                       WHERE (? IS NULL OR session_id = ?)"#,
                    session,
                    session
                )
                .fetch_one(self.database.pool())
                .await?
            }
        };

        Ok(u64::try_from(total).unwrap_or(0))
    }

    fn stream_messages(
        &self,
        filter: &MessageFilter,
    ) -> BoxStream<'_, Result<Message, PersistenceError>> {
        let session = filter.session_id.map(|id| id.to_string());
        let campaign = filter.campaign_id.map(|id| id.to_string());
        let state = filter.state.map(MessageState::as_str);

        // `.fetch()` walks the SQLite statement one step at a time: at most one
        // row is materialised, whatever the size of the result set (CA-002-05).
        // `fetch_all` on the same query would build a `Vec` of the lot.
        let rows = match (campaign, state) {
            (Some(campaign), Some(state)) => sqlx::query_as!(
                MessageRow,
                r#"SELECT rowid AS "rowid!: i64",
                          client_message_id, campaign_id, session_id, smsc_message_id,
                          source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                          data_coding, segments, text, state, command_status,
                          dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
                   FROM messages
                   WHERE campaign_id = ? AND state = ?
                     AND (? IS NULL OR session_id = ?)
                   ORDER BY rowid"#,
                campaign,
                state,
                session,
                session
            )
            .fetch(self.database.pool()),
            (Some(campaign), None) => sqlx::query_as!(
                MessageRow,
                r#"SELECT rowid AS "rowid!: i64",
                          client_message_id, campaign_id, session_id, smsc_message_id,
                          source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                          data_coding, segments, text, state, command_status,
                          dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
                   FROM messages
                   WHERE campaign_id = ?
                     AND (? IS NULL OR session_id = ?)
                   ORDER BY rowid"#,
                campaign,
                session,
                session
            )
            .fetch(self.database.pool()),
            (None, Some(state)) => sqlx::query_as!(
                MessageRow,
                r#"SELECT rowid AS "rowid!: i64",
                          client_message_id, campaign_id, session_id, smsc_message_id,
                          source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                          data_coding, segments, text, state, command_status,
                          dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
                   FROM messages
                   WHERE state = ?
                     AND (? IS NULL OR session_id = ?)
                   ORDER BY rowid"#,
                state,
                session,
                session
            )
            .fetch(self.database.pool()),
            (None, None) => sqlx::query_as!(
                MessageRow,
                r#"SELECT rowid AS "rowid!: i64",
                          client_message_id, campaign_id, session_id, smsc_message_id,
                          source_addr, source_ton, source_npi, dest_addr, dest_ton, dest_npi,
                          data_coding, segments, text, state, command_status,
                          dlr_stat, dlr_err, attempts, created_at, sent_at, resp_at, dlr_at
                   FROM messages
                   WHERE (? IS NULL OR session_id = ?)
                   ORDER BY rowid"#,
                session,
                session
            )
            .fetch(self.database.pool()),
        };

        rows.map(|row| {
            row.map_err(PersistenceError::from)
                .and_then(PagedRow::into_record)
        })
        .boxed()
    }
}

/// Writes one message on the given executor.
///
/// Generic over the executor so the same statement serves a single insert and
/// a batch inside a transaction — the alternative is the same SQL written
/// twice, which is the same SQL until someone changes one of them.
async fn insert_one<'e, E>(executor: E, message: &Message) -> Result<(), PersistenceError>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let client_message_id = message.client_message_id.to_string();
    let campaign_id = message.campaign_id.map(|id| id.to_string());
    let session_id = message.session_id.map(|id| id.to_string());
    let dest_addr = message
        .dest_addr
        .as_ref()
        .map(|number| number.as_str().to_owned());
    let source_ton = message.source_ton.map(|value| store_u8(u8::from(value)));
    let source_npi = message.source_npi.map(|value| store_u8(u8::from(value)));
    let dest_ton = message.dest_ton.map(|value| store_u8(u8::from(value)));
    let dest_npi = message.dest_npi.map(|value| store_u8(u8::from(value)));
    let data_coding = message.data_coding.map(|value| store_u8(u8::from(value)));
    let segments = store_u32(message.segments);
    let state = message.state.as_str();
    let command_status = message.command_status.map(store_command_status);
    let attempts = store_u32(message.attempts);
    let created_at = message.created_at.to_storage();
    let sent_at = message.sent_at.map(|instant| instant.to_storage());
    let resp_at = message.resp_at.map(|instant| instant.to_storage());
    let dlr_at = message.dlr_at.map(|instant| instant.to_storage());

    sqlx::query!(
        r#"INSERT INTO messages (
               client_message_id, campaign_id, session_id, smsc_message_id,
               source_addr, source_ton, source_npi,
               dest_addr, dest_ton, dest_npi,
               data_coding, segments, text, state, command_status,
               dlr_stat, dlr_err, attempts,
               created_at, sent_at, resp_at, dlr_at
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        client_message_id,
        campaign_id,
        session_id,
        message.smsc_message_id,
        message.source_addr,
        source_ton,
        source_npi,
        dest_addr,
        dest_ton,
        dest_npi,
        data_coding,
        segments,
        message.text,
        state,
        command_status,
        message.dlr_stat,
        message.dlr_err,
        attempts,
        created_at,
        sent_at,
        resp_at,
        dlr_at
    )
    .execute(executor)
    .await
    .map_err(|source| PersistenceError::from_write(source, TABLE, client_message_id.clone()))?;

    Ok(())
}

/// Applies one transition on the given executor.
///
/// # How this statement stays replayable
///
/// CLAUDE.md §4 requires a transition to be idempotent, and this statement is
/// where that is either true or a slogan. Three rules, one per kind of column:
///
/// * `COALESCE(?, column)` on the fields that only ever arrive once — a
///   delivery receipt carrying no `resp_at` leaves the one the response wrote,
///   and reapplying it writes the same value again.
/// * `IIF(?, ?, smsc_message_id)` on the one column whose value can
///   legitimately change. See [`SmscMessageIdUpdate`] for why merging is wrong
///   there.
/// * `MAX(attempts, ?)` rather than `attempts + ?`. An increment is the one
///   shape that is NOT replayable: a committed batch reapplied after a crash
///   would count every message of it one attempt too high, quietly eating the
///   retry budget of spec §10.7. Taking the maximum of a 1-based attempt
///   number converges instead.
async fn apply_update<'e, E>(
    executor: E,
    update: &MessageStateUpdate,
) -> Result<(), PersistenceError>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let client_message_id = update.client_message_id.to_string();
    let state = update.state.as_str();
    let command_status = update.command_status.map(store_command_status);
    let sent_at = update.sent_at.map(|instant| instant.to_storage());
    let resp_at = update.resp_at.map(|instant| instant.to_storage());
    let dlr_at = update.dlr_at.map(|instant| instant.to_storage());
    let attempt = update.attempt.map_or(0, store_u32);

    let (replace_smsc_id, smsc_message_id) = match &update.smsc_message_id {
        SmscMessageIdUpdate::Keep => (false, None),
        SmscMessageIdUpdate::Set(identifier) => (true, Some(identifier.as_str())),
    };

    let affected = sqlx::query!(
        r#"UPDATE messages
           SET state = ?,
               smsc_message_id = IIF(?, ?, smsc_message_id),
               command_status = COALESCE(?, command_status),
               dlr_stat = COALESCE(?, dlr_stat),
               dlr_err = COALESCE(?, dlr_err),
               sent_at = COALESCE(?, sent_at),
               resp_at = COALESCE(?, resp_at),
               dlr_at = COALESCE(?, dlr_at),
               attempts = MAX(attempts, ?)
           WHERE client_message_id = ?"#,
        state,
        replace_smsc_id,
        smsc_message_id,
        command_status,
        update.dlr_stat,
        update.dlr_err,
        sent_at,
        resp_at,
        dlr_at,
        attempt,
        client_message_id
    )
    .execute(executor)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(PersistenceError::NotFound {
            entity: TABLE,
            id: client_message_id,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use smpp_core::types::{ClientMessageId, Msisdn};

    use super::{SqliteMessageRepository, TABLE};
    use crate::db::{Database, DatabaseConfig};
    use crate::records::{Message, MessageState, MessageStateUpdate};
    use crate::Timestamp;
    use messaging::ports::MessageRepository;

    /// Counts commits by measuring what they append to the write-ahead log.
    ///
    /// # Why not `PRAGMA data_version`
    ///
    /// It looks like the counter for this job and is not. SQLite only promises
    /// that two readings **differ** when another connection committed in
    /// between, not that it advances once per commit — measured here, five
    /// separate commits moved it by exactly 1. It answers "did anything
    /// change", which the batch and the loop answer identically.
    ///
    /// The WAL does count. Every commit appends its modified pages plus a
    /// commit frame; five commits touching the same page append five copies of
    /// that page, one commit appends one. With the log truncated to zero
    /// beforehand, the file size is a direct reading of how many times the
    /// writer committed.
    ///
    /// This matters beyond the atomicity test in `tests/volumetry.rs`:
    /// atomicity refutes the naive "one transaction per row" loop, but not a
    /// "validate everything, then commit each row separately" implementation.
    /// This does.
    struct WalFrames {
        path: std::path::PathBuf,
    }

    impl WalFrames {
        fn beside(database_path: &std::path::Path) -> Self {
            let mut path = database_path.as_os_str().to_os_string();
            path.push("-wal");

            Self {
                path: std::path::PathBuf::from(path),
            }
        }

        /// Empties the log so the next reading counts only what follows.
        async fn reset(&self, database: &Database) {
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(database.pool())
                .await
                .expect("truncating the write-ahead log");
        }

        fn bytes(&self) -> u64 {
            std::fs::metadata(&self.path).map_or(0, |metadata| metadata.len())
        }
    }

    fn a_message() -> Message {
        Message {
            client_message_id: ClientMessageId::new(),
            campaign_id: None,
            session_id: None,
            smsc_message_id: None,
            source_addr: None,
            source_ton: None,
            source_npi: None,
            dest_addr: Some(Msisdn::parse("+2250102030405").expect("valid number")),
            dest_ton: None,
            dest_npi: None,
            data_coding: None,
            segments: 1,
            text: None,
            state: MessageState::Queued,
            command_status: None,
            dlr_stat: None,
            dlr_err: None,
            attempts: 0,
            created_at: Timestamp::parse("2026-07-26T10:00:00Z").expect("valid instant"),
            sent_at: None,
            resp_at: None,
            dlr_at: None,
        }
    }

    /// CA-002-06, counted rather than inferred.
    ///
    /// Five transitions applied one at a time append five commits' worth of
    /// write-ahead log; the same five applied as a batch append one.
    #[tokio::test]
    async fn a_batch_of_transitions_commits_exactly_once() {
        const ROWS: usize = 5;

        let directory = tempfile::TempDir::new().expect("creating a temporary directory");
        let path = directory.path().join("commits.db");
        let database = Database::open(DatabaseConfig::new(&path))
            .await
            .expect("opening a fresh database");

        let repository = SqliteMessageRepository::new(database.clone());
        let one_at_a_time: Vec<Message> = (0..ROWS).map(|_| a_message()).collect();
        let batched: Vec<Message> = (0..ROWS).map(|_| a_message()).collect();

        repository
            .insert_messages(&one_at_a_time)
            .await
            .expect("seeding");
        repository.insert_messages(&batched).await.expect("seeding");

        let log = WalFrames::beside(&path);

        log.reset(&database).await;
        for message in &one_at_a_time {
            repository
                .update_state(&MessageStateUpdate::new(
                    message.client_message_id,
                    MessageState::Sent,
                ))
                .await
                .expect("individual transition");
        }
        let individually = log.bytes();

        log.reset(&database).await;
        let batch: Vec<MessageStateUpdate> = batched
            .iter()
            .map(|message| MessageStateUpdate::new(message.client_message_id, MessageState::Sent))
            .collect();
        repository
            .update_states(&batch)
            .await
            .expect("batched transitions");
        let in_one_batch = log.bytes();

        // The instrument works: the loop of five commits really does write
        // more log than one commit would. Without this the comparison below
        // would pass on a measurement stuck at zero.
        assert!(
            individually > 0,
            "the write-ahead log measurement reports nothing"
        );

        // Five commits rewrite the same page five times; one commit writes it
        // once. The exact ratio depends on how the rows fall across pages, so
        // the assertion is "strictly less", not "one fifth".
        assert!(
            in_one_batch < individually,
            "a batch of {ROWS} transitions wrote {in_one_batch} bytes of log against \
             {individually} for {ROWS} separate commits — it is not committing once"
        );
    }

    #[test]
    fn the_entity_name_matches_the_table() {
        assert_eq!(TABLE, "messages");
    }
}
