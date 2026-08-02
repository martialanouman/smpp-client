//! Campaigns.

use crate::db::Database;
use crate::ports::CampaignRepository;
use crate::records::{Campaign, CampaignId};
use crate::repositories::convert::{
    read_campaign_id, read_campaign_status, read_optional_timestamp, read_timestamp, read_u32,
    store_u32,
};
use crate::repositories::page::{into_page, PagedRow};
use crate::{Cursor, Page, PersistenceError};

const TABLE: &str = "campaigns";

/// The SQLx implementation of [`CampaignRepository`].
#[derive(Debug, Clone)]
pub struct SqliteCampaignRepository {
    database: Database,
}

impl SqliteCampaignRepository {
    /// Binds the repository to an open database.
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

/// One row of `campaigns`, exactly as SQLite stores it.
struct CampaignRow {
    rowid: i64,
    campaign_id: String,
    name: String,
    status: String,
    template: String,
    send_config: String,
    total_count: i64,
    sent_count: i64,
    delivered_count: i64,
    failed_count: i64,
    created_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
}

impl PagedRow for CampaignRow {
    type Record = Campaign;

    fn cursor(&self) -> i64 {
        self.rowid
    }

    fn into_record(self) -> Result<Campaign, PersistenceError> {
        Ok(Campaign {
            campaign_id: read_campaign_id(&self.campaign_id)?,
            name: self.name,
            status: read_campaign_status(&self.status)?,
            template: self.template,
            send_config: self.send_config,
            total_count: read_u32(self.total_count, TABLE, "total_count")?,
            sent_count: read_u32(self.sent_count, TABLE, "sent_count")?,
            delivered_count: read_u32(self.delivered_count, TABLE, "delivered_count")?,
            failed_count: read_u32(self.failed_count, TABLE, "failed_count")?,
            created_at: read_timestamp(&self.created_at, TABLE, "created_at")?,
            started_at: read_optional_timestamp(self.started_at.as_deref(), TABLE, "started_at")?,
            completed_at: read_optional_timestamp(
                self.completed_at.as_deref(),
                TABLE,
                "completed_at",
            )?,
        })
    }
}

impl CampaignRepository for SqliteCampaignRepository {
    async fn upsert_campaign(&self, campaign: &Campaign) -> Result<(), PersistenceError> {
        let campaign_id = campaign.campaign_id.to_string();
        let status = campaign.status.as_str();
        let total_count = store_u32(campaign.total_count);
        let sent_count = store_u32(campaign.sent_count);
        let delivered_count = store_u32(campaign.delivered_count);
        let failed_count = store_u32(campaign.failed_count);
        let created_at = campaign.created_at.to_storage();
        let started_at = campaign.started_at.map(|instant| instant.to_storage());
        let completed_at = campaign.completed_at.map(|instant| instant.to_storage());

        // `ON CONFLICT DO UPDATE`, not `INSERT OR REPLACE`, for the same
        // reason as on session profiles: a replace would delete the row and
        // trip `ON DELETE SET NULL` on every message of the campaign,
        // detaching a running campaign from its own messages.
        sqlx::query!(
            r#"INSERT INTO campaigns (
                   campaign_id, name, status, template, send_config,
                   total_count, sent_count, delivered_count, failed_count,
                   created_at, started_at, completed_at
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT (campaign_id) DO UPDATE SET
                   name = excluded.name,
                   status = excluded.status,
                   template = excluded.template,
                   send_config = excluded.send_config,
                   total_count = excluded.total_count,
                   sent_count = excluded.sent_count,
                   delivered_count = excluded.delivered_count,
                   failed_count = excluded.failed_count,
                   started_at = excluded.started_at,
                   completed_at = excluded.completed_at"#,
            campaign_id,
            campaign.name,
            status,
            campaign.template,
            campaign.send_config,
            total_count,
            sent_count,
            delivered_count,
            failed_count,
            created_at,
            started_at,
            completed_at
        )
        .execute(self.database.pool())
        .await?;

        Ok(())
    }

    async fn find_campaign(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Option<Campaign>, PersistenceError> {
        let identifier = campaign_id.to_string();

        let row = sqlx::query_as!(
            CampaignRow,
            r#"SELECT rowid AS "rowid!: i64",
                      campaign_id, name, status, template, send_config,
                      total_count, sent_count, delivered_count, failed_count,
                      created_at, started_at, completed_at
               FROM campaigns
               WHERE campaign_id = ?"#,
            identifier
        )
        .fetch_optional(self.database.pool())
        .await?;

        row.map(PagedRow::into_record).transpose()
    }

    async fn page_campaigns(
        &self,
        cursor: Cursor,
        limit: u32,
    ) -> Result<Page<Campaign>, PersistenceError> {
        let after = cursor.into_raw();
        let window = store_u32(limit);

        let rows = sqlx::query_as!(
            CampaignRow,
            r#"SELECT rowid AS "rowid!: i64",
                      campaign_id, name, status, template, send_config,
                      total_count, sent_count, delivered_count, failed_count,
                      created_at, started_at, completed_at
               FROM campaigns
               WHERE rowid > ?
               ORDER BY rowid
               LIMIT ?"#,
            after,
            window
        )
        .fetch_all(self.database.pool())
        .await?;

        into_page(rows, limit)
    }

    async fn delete_campaign(&self, campaign_id: CampaignId) -> Result<bool, PersistenceError> {
        let identifier = campaign_id.to_string();

        let affected = sqlx::query!("DELETE FROM campaigns WHERE campaign_id = ?", identifier)
            .execute(self.database.pool())
            .await?
            .rows_affected();

        Ok(affected > 0)
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::SqliteCampaignRepository;
    use crate::db::{Database, DatabaseConfig};
    use crate::ports::CampaignRepository;
    use crate::records::{Campaign, CampaignId, CampaignStatus};
    use crate::{PersistenceError, Timestamp};

    fn a_campaign(campaign_id: CampaignId) -> Campaign {
        Campaign {
            campaign_id,
            name: String::from("juillet"),
            status: CampaignStatus::Cancelled,
            template: String::from("Bonjour"),
            send_config: String::from("{}"),
            total_count: 0,
            sent_count: 0,
            delivered_count: 0,
            failed_count: 0,
            created_at: Timestamp::parse("2026-08-02T10:00:00Z").expect("valid RFC 3339"),
            started_at: None,
            completed_at: None,
        }
    }

    /// `campaigns.status` carries **no** `CHECK` constraint — spec §14.2 leaves
    /// the set open — so `read_campaign_status` is the only thing between a
    /// value this version does not know and a campaign read back as something
    /// it never was. Written inside the crate rather than in `tests/` because
    /// the injection needs the pool, which stays `pub(crate)` (CA-002-03).
    ///
    /// Three failures this pins, all of them live once the milestone-010 move
    /// retired the `stored_enum!` round-trip test: reading an unknown status as
    /// a default (a cancelled campaign coming back `CREATED`, and startable),
    /// naming the wrong table or column, and echoing the offending value into
    /// an error that crosses the IPC boundary (CA-001-06).
    #[tokio::test]
    async fn a_status_this_version_does_not_know_is_a_malformed_row() {
        let directory = tempfile::TempDir::new().expect("creating a temporary directory");
        let database = Database::open(DatabaseConfig::new(directory.path().join("campaigns.db")))
            .await
            .expect("opening a fresh database");
        let repository = SqliteCampaignRepository::new(database.clone());

        let campaign = a_campaign(CampaignId::new());
        repository
            .upsert_campaign(&campaign)
            .await
            .expect("seeding the campaign");

        // Runtime SQL rather than `sqlx::query!`: the macro would put this
        // deliberately invalid write in the `.sqlx` cache, next to the queries
        // the application actually runs.
        let identifier = campaign.campaign_id.to_string();
        let injected = sqlx::query("UPDATE campaigns SET status = ? WHERE campaign_id = ?")
            .bind("ARCHIVED")
            .bind(&identifier)
            .execute(database.pool())
            .await
            .expect("the column takes any text, having no CHECK");

        assert_eq!(injected.rows_affected(), 1, "nothing was injected");

        let rejection = repository
            .find_campaign(campaign.campaign_id)
            .await
            .expect_err("an unknown status must not be read as a campaign");

        assert!(
            matches!(
                rejection,
                PersistenceError::MalformedRow {
                    table: "campaigns",
                    column: "status",
                    ..
                }
            ),
            "{rejection:?}"
        );

        let rendered = rejection.to_string();
        assert!(rendered.contains("campaigns.status"), "{rendered}");
        assert!(!rendered.contains("ARCHIVED"), "{rendered}");
    }
}
