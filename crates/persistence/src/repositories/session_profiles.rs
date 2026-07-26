//! Connection profiles.

use smpp_core::types::SessionId;

use crate::db::Database;
use crate::ports::SessionProfileRepository;
use crate::records::{BindType, SessionProfile};
use crate::repositories::convert::{
    read_interface_version, read_session_id, read_timestamp, read_u16, read_u32,
    store_interface_version, store_u16, store_u32,
};
use crate::PersistenceError;

const TABLE: &str = "session_profiles";

/// The SQLx implementation of [`SessionProfileRepository`].
#[derive(Debug, Clone)]
pub struct SqliteSessionProfileRepository {
    database: Database,
}

impl SqliteSessionProfileRepository {
    /// Binds the repository to an open database.
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

/// One row of `session_profiles`, exactly as SQLite stores it.
struct SessionProfileRow {
    session_id: String,
    name: String,
    host: String,
    port: i64,
    bind_type: String,
    interface_version: String,
    system_id: String,
    password_enc: Vec<u8>,
    system_type: String,
    tls_config: Option<String>,
    window_size: i64,
    throughput_tps: i64,
    enquire_link_s: i64,
    response_timeout_s: i64,
    reconnect_config: Option<String>,
    bind_count: i64,
    created_at: String,
    updated_at: String,
}

impl SessionProfileRow {
    /// Turns the stored columns into the domain record.
    fn into_profile(self) -> Result<SessionProfile, PersistenceError> {
        Ok(SessionProfile {
            session_id: read_session_id(&self.session_id, TABLE, "session_id")?,
            name: self.name,
            host: self.host,
            port: read_u16(self.port, TABLE, "port")?,
            bind_type: BindType::parse(&self.bind_type)?,
            interface_version: read_interface_version(&self.interface_version)?,
            system_id: self.system_id,
            password_enc: self.password_enc,
            system_type: self.system_type,
            tls_config: self.tls_config,
            window_size: read_u32(self.window_size, TABLE, "window_size")?,
            throughput_tps: read_u32(self.throughput_tps, TABLE, "throughput_tps")?,
            enquire_link_s: read_u32(self.enquire_link_s, TABLE, "enquire_link_s")?,
            response_timeout_s: read_u32(self.response_timeout_s, TABLE, "response_timeout_s")?,
            reconnect_config: self.reconnect_config,
            bind_count: read_u32(self.bind_count, TABLE, "bind_count")?,
            created_at: read_timestamp(&self.created_at, TABLE, "created_at")?,
            updated_at: read_timestamp(&self.updated_at, TABLE, "updated_at")?,
        })
    }
}

impl SessionProfileRepository for SqliteSessionProfileRepository {
    async fn upsert_session_profile(
        &self,
        profile: &SessionProfile,
    ) -> Result<(), PersistenceError> {
        let session_id = profile.session_id.to_string();
        let port = store_u16(profile.port);
        let bind_type = profile.bind_type.as_str();
        let interface_version = store_interface_version(profile.interface_version);
        let window_size = store_u32(profile.window_size);
        let throughput_tps = store_u32(profile.throughput_tps);
        let enquire_link_s = store_u32(profile.enquire_link_s);
        let response_timeout_s = store_u32(profile.response_timeout_s);
        let bind_count = store_u32(profile.bind_count);
        let created_at = profile.created_at.to_storage();
        let updated_at = profile.updated_at.to_storage();

        // `ON CONFLICT DO UPDATE` rather than `INSERT OR REPLACE`: the
        // latter deletes the row and inserts a new one, which fires
        // `ON DELETE SET NULL` on every message referencing this profile.
        // Editing a profile's port would quietly orphan its whole history.
        sqlx::query!(
            r#"INSERT INTO session_profiles (
                   session_id, name, host, port, bind_type, interface_version,
                   system_id, password_enc, system_type, tls_config,
                   window_size, throughput_tps, enquire_link_s, response_timeout_s,
                   reconnect_config, bind_count, created_at, updated_at
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT (session_id) DO UPDATE SET
                   name = excluded.name,
                   host = excluded.host,
                   port = excluded.port,
                   bind_type = excluded.bind_type,
                   interface_version = excluded.interface_version,
                   system_id = excluded.system_id,
                   password_enc = excluded.password_enc,
                   system_type = excluded.system_type,
                   tls_config = excluded.tls_config,
                   window_size = excluded.window_size,
                   throughput_tps = excluded.throughput_tps,
                   enquire_link_s = excluded.enquire_link_s,
                   response_timeout_s = excluded.response_timeout_s,
                   reconnect_config = excluded.reconnect_config,
                   bind_count = excluded.bind_count,
                   updated_at = excluded.updated_at"#,
            session_id,
            profile.name,
            profile.host,
            port,
            bind_type,
            interface_version,
            profile.system_id,
            profile.password_enc,
            profile.system_type,
            profile.tls_config,
            window_size,
            throughput_tps,
            enquire_link_s,
            response_timeout_s,
            profile.reconnect_config,
            bind_count,
            created_at,
            updated_at
        )
        .execute(self.database.pool())
        .await?;

        Ok(())
    }

    async fn find_session_profile(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionProfile>, PersistenceError> {
        let identifier = session_id.to_string();

        let row = sqlx::query_as!(
            SessionProfileRow,
            r#"SELECT session_id, name, host, port, bind_type, interface_version,
                      system_id, password_enc, system_type, tls_config,
                      window_size, throughput_tps, enquire_link_s, response_timeout_s,
                      reconnect_config, bind_count, created_at, updated_at
               FROM session_profiles
               WHERE session_id = ?"#,
            identifier
        )
        .fetch_optional(self.database.pool())
        .await?;

        row.map(SessionProfileRow::into_profile).transpose()
    }

    async fn list_session_profiles(&self) -> Result<Vec<SessionProfile>, PersistenceError> {
        let rows = sqlx::query_as!(
            SessionProfileRow,
            r#"SELECT session_id, name, host, port, bind_type, interface_version,
                      system_id, password_enc, system_type, tls_config,
                      window_size, throughput_tps, enquire_link_s, response_timeout_s,
                      reconnect_config, bind_count, created_at, updated_at
               FROM session_profiles
               ORDER BY rowid"#
        )
        .fetch_all(self.database.pool())
        .await?;

        rows.into_iter()
            .map(SessionProfileRow::into_profile)
            .collect()
    }

    async fn delete_session_profile(
        &self,
        session_id: SessionId,
    ) -> Result<bool, PersistenceError> {
        let identifier = session_id.to_string();

        let affected = sqlx::query!(
            "DELETE FROM session_profiles WHERE session_id = ?",
            identifier
        )
        .execute(self.database.pool())
        .await?
        .rows_affected();

        Ok(affected > 0)
    }
}
