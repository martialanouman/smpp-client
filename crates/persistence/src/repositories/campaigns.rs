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
