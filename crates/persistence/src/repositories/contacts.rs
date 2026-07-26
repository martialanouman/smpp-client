//! Contacts and contact lists.

use futures_core::stream::BoxStream;
use futures_util::StreamExt;

use crate::db::Database;
use crate::ports::ContactRepository;
use crate::records::{Contact, ContactId, ContactList, ListId};
use crate::repositories::convert::{
    read_contact_id, read_list_id, read_msisdn, read_timestamp, store_u32,
};
use crate::{Cursor, Page, PersistenceError};

const TABLE: &str = "contacts";
const LIST_TABLE: &str = "contact_lists";

/// The SQLx implementation of [`ContactRepository`].
#[derive(Debug, Clone)]
pub struct SqliteContactRepository {
    database: Database,
}

impl SqliteContactRepository {
    /// Binds the repository to an open database.
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

/// One row of `contacts`, exactly as SQLite stores it.
struct ContactRow {
    rowid: i64,
    contact_id: String,
    msisdn: String,
    country: Option<String>,
    valid: i64,
    line_type: Option<String>,
    attributes: Option<String>,
    source: Option<String>,
    created_at: String,
}

impl ContactRow {
    /// Turns the stored columns into the domain record.
    fn into_contact(self) -> Result<Contact, PersistenceError> {
        Ok(Contact {
            contact_id: read_contact_id(&self.contact_id)?,
            msisdn: read_msisdn(&self.msisdn, TABLE, "msisdn")?,
            country: self.country,
            // Any non-zero integer is truthy, which is how SQLite itself reads
            // the column; insisting on exactly 1 would reject a file written
            // by a perfectly ordinary `UPDATE contacts SET valid = 2`.
            valid: self.valid != 0,
            line_type: self.line_type,
            attributes: self.attributes,
            source: self.source,
            created_at: read_timestamp(&self.created_at, TABLE, "created_at")?,
        })
    }
}

/// One row of `contact_lists`, exactly as SQLite stores it.
struct ContactListRow {
    list_id: String,
    name: String,
    created_at: String,
}

impl ContactListRow {
    /// Turns the stored columns into the domain record.
    fn into_list(self) -> Result<ContactList, PersistenceError> {
        Ok(ContactList {
            list_id: read_list_id(&self.list_id)?,
            name: self.name,
            created_at: read_timestamp(&self.created_at, LIST_TABLE, "created_at")?,
        })
    }
}

impl ContactRepository for SqliteContactRepository {
    async fn insert_contact(&self, contact: &Contact) -> Result<(), PersistenceError> {
        let mut connection = self.database.pool().acquire().await?;
        insert_one(&mut *connection, contact).await
    }

    async fn insert_contacts(&self, contacts: &[Contact]) -> Result<u64, PersistenceError> {
        // ONE transaction for an import of tens of thousands of rows
        // (spec §11.2): one `fsync` instead of one per contact, and no
        // half-imported list left behind on failure.
        let mut transaction = self.database.pool().begin().await?;

        for contact in contacts {
            insert_one(&mut *transaction, contact).await?;
        }

        transaction.commit().await?;

        Ok(contacts.len().try_into().unwrap_or(u64::MAX))
    }

    async fn find_contact(
        &self,
        contact_id: ContactId,
    ) -> Result<Option<Contact>, PersistenceError> {
        let identifier = contact_id.to_string();

        let row = sqlx::query_as!(
            ContactRow,
            r#"SELECT rowid AS "rowid!: i64",
                      contact_id, msisdn, country, valid, line_type,
                      attributes, source, created_at
               FROM contacts
               WHERE contact_id = ?"#,
            identifier
        )
        .fetch_optional(self.database.pool())
        .await?;

        row.map(ContactRow::into_contact).transpose()
    }

    async fn page_contacts(
        &self,
        cursor: Cursor,
        limit: u32,
    ) -> Result<Page<Contact>, PersistenceError> {
        let after = cursor.into_raw();
        let window = store_u32(limit);

        let rows = sqlx::query_as!(
            ContactRow,
            r#"SELECT rowid AS "rowid!: i64",
                      contact_id, msisdn, country, valid, line_type,
                      attributes, source, created_at
               FROM contacts
               WHERE rowid > ?
               ORDER BY rowid
               LIMIT ?"#,
            after,
            window
        )
        .fetch_all(self.database.pool())
        .await?;

        let complete = u64::try_from(rows.len()).unwrap_or(u64::MAX) == u64::from(limit);
        let last = rows.last().map(|row| row.rowid);

        let items = rows
            .into_iter()
            .map(ContactRow::into_contact)
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

    fn stream_contacts(
        &self,
        list: Option<ListId>,
    ) -> BoxStream<'_, Result<Contact, PersistenceError>> {
        let list_id = list.map(|id| id.to_string());

        sqlx::query_as!(
            ContactRow,
            r#"SELECT contacts.rowid AS "rowid!: i64",
                      contacts.contact_id, contacts.msisdn, contacts.country,
                      contacts.valid, contacts.line_type, contacts.attributes,
                      contacts.source, contacts.created_at
               FROM contacts
               WHERE ? IS NULL
                  OR contacts.contact_id IN (
                         SELECT contact_id FROM contact_list_members WHERE list_id = ?
                     )
               ORDER BY contacts.rowid"#,
            list_id,
            list_id
        )
        .fetch(self.database.pool())
        .map(|row| {
            row.map_err(PersistenceError::from)
                .and_then(ContactRow::into_contact)
        })
        .boxed()
    }

    async fn insert_contact_list(&self, list: &ContactList) -> Result<(), PersistenceError> {
        let list_id = list.list_id.to_string();
        let created_at = list.created_at.to_storage();

        sqlx::query!(
            "INSERT INTO contact_lists (list_id, name, created_at) VALUES (?, ?, ?)",
            list_id,
            list.name,
            created_at
        )
        .execute(self.database.pool())
        .await
        .map_err(|source| PersistenceError::from_write(source, LIST_TABLE, list_id.clone()))?;

        Ok(())
    }

    async fn find_contact_list(
        &self,
        list_id: ListId,
    ) -> Result<Option<ContactList>, PersistenceError> {
        let identifier = list_id.to_string();

        let row = sqlx::query_as!(
            ContactListRow,
            "SELECT list_id, name, created_at FROM contact_lists WHERE list_id = ?",
            identifier
        )
        .fetch_optional(self.database.pool())
        .await?;

        row.map(ContactListRow::into_list).transpose()
    }

    async fn add_contacts_to_list(
        &self,
        list_id: ListId,
        contacts: &[ContactId],
    ) -> Result<u64, PersistenceError> {
        let list = list_id.to_string();
        let mut transaction = self.database.pool().begin().await?;
        let mut created = 0_u64;

        for contact_id in contacts {
            let contact = contact_id.to_string();

            // `OR IGNORE` on the composite primary key: re-adding a
            // contact already in the list is ordinary — two overlapping
            // imports — not a fault. It does NOT swallow a foreign key
            // violation, which still fails the whole batch.
            created += sqlx::query!(
                "INSERT OR IGNORE INTO contact_list_members (list_id, contact_id) VALUES (?, ?)",
                list,
                contact
            )
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        }

        transaction.commit().await?;

        Ok(created)
    }
}

/// Writes one contact on the given executor.
async fn insert_one<'e, E>(executor: E, contact: &Contact) -> Result<(), PersistenceError>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let contact_id = contact.contact_id.to_string();
    let msisdn = contact.msisdn.as_str();
    let valid = i64::from(contact.valid);
    let created_at = contact.created_at.to_storage();

    sqlx::query!(
        r#"INSERT INTO contacts (
               contact_id, msisdn, country, valid, line_type, attributes, source, created_at
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
        contact_id,
        msisdn,
        contact.country,
        valid,
        contact.line_type,
        contact.attributes,
        contact.source,
        created_at
    )
    .execute(executor)
    .await
    .map_err(|source| PersistenceError::from_write(source, TABLE, contact_id.clone()))?;

    Ok(())
}
