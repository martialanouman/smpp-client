//! The message journal.

use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use smpp_core::types::ClientMessageId;

use crate::db::Database;
use crate::ports::MessageRepository;
use crate::records::{Message, MessageFilter, MessageState, MessageStateUpdate};
use crate::repositories::convert::{
    read_client_message_id, read_command_status, read_data_coding, read_msisdn, read_npi,
    read_optional_campaign_id, read_optional_session_id, read_optional_timestamp, read_timestamp,
    read_ton, read_u32, store_command_status, store_u32, store_u8,
};
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
/// The intermediate step between the columns and [`Message`]. It exists so
/// that the six queries reading this table share one mapping — written once,
/// in [`MessageRow::into_message`] — instead of six copies drifting apart.
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

impl MessageRow {
    /// Turns the stored columns into the domain record.
    fn into_message(self) -> Result<Message, PersistenceError> {
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
            state: MessageState::parse(&self.state)?,
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

/// The three filter columns, flattened into the strings the query binds.
///
/// `None` on any of them means "do not restrict"; the SQL spells that as
/// `? IS NULL OR column = ?`, which keeps the statement a single compile-time
/// checked literal instead of a string assembled at runtime.
struct FilterBinds {
    campaign_id: Option<String>,
    session_id: Option<String>,
    state: Option<&'static str>,
}

impl FilterBinds {
    fn new(filter: &MessageFilter) -> Self {
        Self {
            campaign_id: filter.campaign_id.map(|id| id.to_string()),
            session_id: filter.session_id.map(|id| id.to_string()),
            state: filter.state.map(MessageState::as_str),
        }
    }
}

impl MessageRepository for SqliteMessageRepository {
    async fn insert_message(&self, message: &Message) -> Result<(), PersistenceError> {
        let mut connection = self.database.pool().acquire().await?;
        insert_one(&mut *connection, message).await
    }

    async fn insert_messages(&self, messages: &[Message]) -> Result<u64, PersistenceError> {
        // ONE transaction for the whole batch (CA-002-06, guide §11.2).
        // The `?` on each insert drops `transaction` without committing,
        // which rolls back: a batch either lands whole or not at all.
        let mut transaction = self.database.pool().begin().await?;

        for message in messages {
            insert_one(&mut *transaction, message).await?;
        }

        transaction.commit().await?;

        Ok(messages.len().try_into().unwrap_or(u64::MAX))
    }

    async fn find_message(
        &self,
        client_message_id: ClientMessageId,
    ) -> Result<Option<Message>, PersistenceError> {
        let identifier = client_message_id.to_string();

        let row = sqlx::query_as!(
            MessageRow,
            r#"SELECT rowid AS "rowid!: i64",
                      client_message_id, campaign_id, session_id, smsc_message_id,
                      source_addr, source_ton, source_npi,
                      dest_addr, dest_ton, dest_npi,
                      data_coding, segments, text, state, command_status,
                      dlr_stat, dlr_err, attempts,
                      created_at, sent_at, resp_at, dlr_at
               FROM messages
               WHERE client_message_id = ?"#,
            identifier
        )
        .fetch_optional(self.database.pool())
        .await?;

        row.map(MessageRow::into_message).transpose()
    }

    async fn find_message_by_smsc_id(
        &self,
        smsc_message_id: &str,
    ) -> Result<Option<Message>, PersistenceError> {
        let row = sqlx::query_as!(
            MessageRow,
            r#"SELECT rowid AS "rowid!: i64",
                      client_message_id, campaign_id, session_id, smsc_message_id,
                      source_addr, source_ton, source_npi,
                      dest_addr, dest_ton, dest_npi,
                      data_coding, segments, text, state, command_status,
                      dlr_stat, dlr_err, attempts,
                      created_at, sent_at, resp_at, dlr_at
               FROM messages
               WHERE smsc_message_id = ?
               ORDER BY rowid
               LIMIT 1"#,
            smsc_message_id
        )
        .fetch_optional(self.database.pool())
        .await?;

        row.map(MessageRow::into_message).transpose()
    }

    async fn update_state(&self, update: &MessageStateUpdate) -> Result<(), PersistenceError> {
        let mut connection = self.database.pool().acquire().await?;
        apply_update(&mut *connection, update).await
    }

    async fn update_states(&self, updates: &[MessageStateUpdate]) -> Result<u64, PersistenceError> {
        let mut transaction = self.database.pool().begin().await?;

        for update in updates {
            apply_update(&mut *transaction, update).await?;
        }

        transaction.commit().await?;

        Ok(updates.len().try_into().unwrap_or(u64::MAX))
    }

    async fn page_messages(
        &self,
        filter: &MessageFilter,
        cursor: Cursor,
        limit: u32,
    ) -> Result<Page<Message>, PersistenceError> {
        let binds = FilterBinds::new(filter);
        let after = cursor.into_raw();
        let window = store_u32(limit);

        let rows = sqlx::query_as!(
            MessageRow,
            r#"SELECT rowid AS "rowid!: i64",
                      client_message_id, campaign_id, session_id, smsc_message_id,
                      source_addr, source_ton, source_npi,
                      dest_addr, dest_ton, dest_npi,
                      data_coding, segments, text, state, command_status,
                      dlr_stat, dlr_err, attempts,
                      created_at, sent_at, resp_at, dlr_at
               FROM messages
               WHERE rowid > ?
                 AND (? IS NULL OR campaign_id = ?)
                 AND (? IS NULL OR session_id = ?)
                 AND (? IS NULL OR state = ?)
               ORDER BY rowid
               LIMIT ?"#,
            after,
            binds.campaign_id,
            binds.campaign_id,
            binds.session_id,
            binds.session_id,
            binds.state,
            binds.state,
            window
        )
        .fetch_all(self.database.pool())
        .await?;

        into_page(rows, limit)
    }

    async fn count_messages(&self, filter: &MessageFilter) -> Result<u64, PersistenceError> {
        let binds = FilterBinds::new(filter);

        let total = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "total!: i64"
               FROM messages
               WHERE (? IS NULL OR campaign_id = ?)
                 AND (? IS NULL OR session_id = ?)
                 AND (? IS NULL OR state = ?)"#,
            binds.campaign_id,
            binds.campaign_id,
            binds.session_id,
            binds.session_id,
            binds.state,
            binds.state
        )
        .fetch_one(self.database.pool())
        .await?;

        Ok(u64::try_from(total).unwrap_or(0))
    }

    fn stream_messages(
        &self,
        filter: &MessageFilter,
    ) -> BoxStream<'_, Result<Message, PersistenceError>> {
        let binds = FilterBinds::new(filter);

        // `.fetch()` walks the SQLite statement one step at a time: at most one
        // row is materialised, whatever the size of the result set (CA-002-05).
        // `fetch_all` on the same query would build a `Vec` of the lot.
        sqlx::query_as!(
            MessageRow,
            r#"SELECT rowid AS "rowid!: i64",
                      client_message_id, campaign_id, session_id, smsc_message_id,
                      source_addr, source_ton, source_npi,
                      dest_addr, dest_ton, dest_npi,
                      data_coding, segments, text, state, command_status,
                      dlr_stat, dlr_err, attempts,
                      created_at, sent_at, resp_at, dlr_at
               FROM messages
               WHERE (? IS NULL OR campaign_id = ?)
                 AND (? IS NULL OR session_id = ?)
                 AND (? IS NULL OR state = ?)
               ORDER BY rowid"#,
            binds.campaign_id,
            binds.campaign_id,
            binds.session_id,
            binds.session_id,
            binds.state,
            binds.state
        )
        .fetch(self.database.pool())
        .map(|row| {
            row.map_err(PersistenceError::from)
                .and_then(MessageRow::into_message)
        })
        .boxed()
    }
}

/// Assembles a page and the cursor that continues it.
///
/// A page shorter than the limit is the last one, which is how the caller
/// learns there is nothing after it without a second round trip.
fn into_page(rows: Vec<MessageRow>, limit: u32) -> Result<Page<Message>, PersistenceError> {
    let complete = u64::try_from(rows.len()).unwrap_or(u64::MAX) == u64::from(limit);
    let last = rows.last().map(|row| row.rowid);

    let items = rows
        .into_iter()
        .map(MessageRow::into_message)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Page {
        items,
        next: if complete {
            last.map(Cursor::from_raw)
        } else {
            None
        },
    })
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
/// `COALESCE(?, column)` on every optional field is what makes the update a
/// **merge**: a delivery receipt that carries no `smsc_message_id` leaves the
/// one the response wrote. It is also what makes replaying the same transition
/// harmless, which CLAUDE.md §4 requires.
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
    let attempt = i64::from(update.counts_as_attempt);

    let affected = sqlx::query!(
        r#"UPDATE messages
           SET state = ?,
               smsc_message_id = COALESCE(?, smsc_message_id),
               command_status = COALESCE(?, command_status),
               dlr_stat = COALESCE(?, dlr_stat),
               dlr_err = COALESCE(?, dlr_err),
               sent_at = COALESCE(?, sent_at),
               resp_at = COALESCE(?, resp_at),
               dlr_at = COALESCE(?, dlr_at),
               attempts = attempts + ?
           WHERE client_message_id = ?"#,
        state,
        update.smsc_message_id,
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
