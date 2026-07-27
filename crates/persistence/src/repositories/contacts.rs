//! Contacts, contact lists and import profiles.
//!
//! # Two ports, one implementation
//!
//! [`contacts::ports::ContactRepository`] is declared **above** this crate
//! (ADR 0012, CA-009-13) and reports a `ContactStoreError`, which names the
//! outcomes an importer can act on and carries neither a `sqlx::Error` nor a
//! filesystem path. [`crate::ports::ContactDirectory`] stayed here, reports a
//! [`PersistenceError`], and exists for the contacts screen.
//!
//! [`store_error`] is the projection between the two, and it logs the full
//! failure — with its source chain — before discarding it, exactly as
//! `messages::store_error` does for the send path.
//!
//! # Combining lists without dynamic SQL
//!
//! A [`ListSelection`] holds an arbitrary number of list identifiers, and
//! `sqlx::query_as!` needs a **literal** query — so `IN (?, ?, ?)` built at
//! runtime is not available, and neither is `QueryBuilder`, since that would
//! give up the compile-time checking ADR 0002 chose sqlx for.
//!
//! The identifiers therefore travel as **one** JSON array parameter, expanded
//! by SQLite's own `json_each`. One literal query covers every arity, the
//! checking stands, and the plan is a lookup on the composite primary key of
//! `contact_list_members` rather than a scan.

use contacts::lists::{Combination, ListSelection};
use contacts::model::{Contact, ContactId, ContactList, LineType, ListId};
use contacts::ports::{ContactRepository, ContactStoreError};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;

use crate::db::Database;
use crate::records::ImportProfile;
use crate::repositories::convert::{
    read_contact_id, read_line_type, read_list_id, read_msisdn, read_profile_id, read_timestamp,
    store_u32,
};
use crate::repositories::page::{into_page, PagedRow};
use crate::{Cursor, Page, PersistenceError};

const TABLE: &str = "contacts";
const LIST_TABLE: &str = "contact_lists";
const PROFILE_TABLE: &str = "import_profiles";

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

/// Projects a storage failure onto the port's vocabulary.
///
/// The source chain is **logged before it is dropped**. Without this, a full
/// disk during an import of fifty thousand rows would reach the interface as
/// "the contact store is unavailable" and leave nothing anywhere saying why.
fn store_error(error: PersistenceError) -> ContactStoreError {
    match error {
        PersistenceError::Conflict { .. } => ContactStoreError::Conflict,
        other => {
            tracing::error!(error = ?other, "the contact store refused a call");

            ContactStoreError::Unavailable {
                reason: summarise(&other),
            }
        }
    }
}

/// A short, path-free rendering of a storage failure.
///
/// [`PersistenceError::DataDirectory`] is the one variant whose `Display`
/// carries a filesystem path, and CA-001-06 forbids one crossing towards the
/// interface. Every other variant renders as itself — none of them quotes an
/// identifier, a number or a message body.
fn summarise(error: &PersistenceError) -> String {
    match error {
        PersistenceError::DataDirectory { .. } => {
            String::from("the database file could not be reached")
        }
        other => other.to_string(),
    }
}

/// Turns a foreign-key violation into the port's `NotFound`.
///
/// Adding a contact to a list that no longer exists is the case: the row the
/// caller referred to is gone, which is a different thing from the store being
/// broken and leads to a different message.
fn membership_error(error: PersistenceError) -> ContactStoreError {
    if matches!(
        &error,
        PersistenceError::Database { source: sqlx::Error::Database(inner) }
            if inner.is_foreign_key_violation()
    ) {
        return ContactStoreError::NotFound;
    }

    store_error(error)
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

impl PagedRow for ContactRow {
    type Record = Contact;

    fn cursor(&self) -> i64 {
        self.rowid
    }

    fn into_record(self) -> Result<Contact, PersistenceError> {
        Ok(Contact {
            contact_id: read_contact_id(&self.contact_id)?,
            msisdn: read_msisdn(&self.msisdn, TABLE, "msisdn")?,
            country: self.country,
            // Any non-zero integer is truthy, which is how SQLite itself reads
            // the column; insisting on exactly 1 would reject a file written
            // by a perfectly ordinary `UPDATE contacts SET valid = 2`.
            valid: self.valid != 0,
            line_type: read_line_type(self.line_type.as_deref())?,
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

/// One row of `import_profiles`, exactly as SQLite stores it.
struct ImportProfileRow {
    profile_id: String,
    name: String,
    mapping: String,
    created_at: String,
}

impl ImportProfileRow {
    /// Turns the stored columns into the domain record.
    fn into_profile(self) -> Result<ImportProfile, PersistenceError> {
        let created_at = read_timestamp(&self.created_at, PROFILE_TABLE, "created_at")?;

        ImportProfile::from_stored(
            read_profile_id(&self.profile_id)?,
            self.name,
            &self.mapping,
            created_at,
        )
        .map_err(|_| PersistenceError::MalformedRow {
            table: PROFILE_TABLE,
            column: "mapping",
            expected: "a column mapping this version understands",
        })
    }
}

/// Drops a leading `+` when the needle is a phone number, and only then.
///
/// # Why this exists
///
/// `Msisdn` stores digits only — `2250102030405`, no `+`. An operator searching
/// the contacts screen types `+225…`, because that is how E.164 numbers are
/// written everywhere else in the interface, and a literal `LIKE '%+225%'`
/// against that column matches nothing at all. Not an error, not an empty state
/// anyone would question: a screen that says there are no contacts.
///
/// The same mismatch bit the log screen at milestone 008, and
/// `MessageFilter::matching` fixes it the same way and for the same reason. The
/// stripping is **conditional**, because the needle is also matched against the
/// attributes, where a `+` is an ordinary character somebody may be looking
/// for: it is removed only when the needle is `+` followed by digits and
/// nothing else.
fn strip_plus(needle: &str) -> &str {
    match needle.strip_prefix('+') {
        Some(digits) if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) => {
            digits
        }
        _ => needle,
    }
}

/// The list identifiers of a selection, as the JSON array `json_each` expands.
///
/// Hand-built rather than through `serde_json`: the elements are UUIDs, whose
/// text form holds no character JSON escapes, so this is a join — and it keeps
/// `persistence` free of a serialisation dependency it needs for nothing else.
fn json_array(lists: &[ListId]) -> String {
    let mut rendered = String::from("[");

    for (index, list) in lists.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }

        rendered.push('"');
        rendered.push_str(&list.to_string());
        rendered.push('"');
    }

    rendered.push(']');
    rendered
}

/// The three parameters every selection-aware query binds.
///
/// `wanted` is how many distinct lists an intersection must match; it is zero
/// for the other two combinations, and the query compares against it only in
/// the intersection branch.
struct SelectionBinding {
    lists: String,
    excluded: String,
    wanted: i64,
}

impl SelectionBinding {
    /// Renders a selection for binding.
    fn of(selection: &ListSelection) -> Self {
        Self {
            lists: json_array(selection.lists()),
            excluded: json_array(selection.excluded()),
            // `Combination` is `#[non_exhaustive]`, so a wildcard is required
            // here whatever the arms. Zero is the safe default: it is only
            // compared against in the intersection branch, which the flags
            // bound beside it select explicitly.
            wanted: match selection.combination() {
                Combination::All => i64::try_from(selection.lists().len()).unwrap_or(i64::MAX),
                _ => 0,
            },
        }
    }
}

impl ContactRepository for SqliteContactRepository {
    async fn insert_contact(&self, contact: &Contact) -> Result<(), ContactStoreError> {
        let mut connection = self
            .database
            .pool()
            .acquire()
            .await
            .map_err(|error| store_error(PersistenceError::from(error)))?;

        insert_one(&mut *connection, contact)
            .await
            .map_err(store_error)
    }

    async fn insert_contacts(&self, contacts: &[Contact]) -> Result<u64, ContactStoreError> {
        // ONE transaction for a batch of an import (spec §11.2): one `fsync`
        // instead of one per contact, and — the point of CA-009-10 — a batch
        // that fails or is cancelled leaves no half-written rows behind.
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|error| store_error(PersistenceError::from(error)))?;

        for contact in contacts {
            insert_one(&mut *transaction, contact)
                .await
                .map_err(store_error)?;
        }

        transaction
            .commit()
            .await
            .map_err(|error| store_error(PersistenceError::from(error)))?;

        Ok(contacts.len().try_into().unwrap_or(u64::MAX))
    }

    async fn find_contact(
        &self,
        contact_id: ContactId,
    ) -> Result<Option<Contact>, ContactStoreError> {
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
        .await
        .map_err(|error| store_error(PersistenceError::from(error)))?;

        row.map(PagedRow::into_record)
            .transpose()
            .map_err(store_error)
    }

    fn stream_contacts(
        &self,
        selection: &ListSelection,
    ) -> BoxStream<'_, Result<Contact, ContactStoreError>> {
        // An empty union or intersection selects nothing, and the SQL below
        // cannot express that: `COUNT(…) = 0` over no list is true of every
        // contact, so letting the empty case through would return the whole
        // table for a selection that means the opposite.
        if selection.is_empty() {
            return futures_util::stream::empty().boxed();
        }

        let binding = SelectionBinding::of(selection);
        let everything = matches!(selection.combination(), Combination::Everything);
        let intersect = matches!(selection.combination(), Combination::All);

        sqlx::query_as!(
            ContactRow,
            r#"SELECT contacts.rowid AS "rowid!: i64",
                      contacts.contact_id, contacts.msisdn, contacts.country,
                      contacts.valid, contacts.line_type, contacts.attributes,
                      contacts.source, contacts.created_at
               FROM contacts
               WHERE (
                       ?1
                       OR (
                           NOT ?2
                           AND EXISTS (
                               SELECT 1 FROM contact_list_members AS m
                               WHERE m.contact_id = contacts.contact_id
                                 AND m.list_id IN (SELECT value FROM json_each(?3))
                           )
                       )
                       OR (
                           ?2
                           AND (
                               SELECT COUNT(DISTINCT m.list_id)
                               FROM contact_list_members AS m
                               WHERE m.contact_id = contacts.contact_id
                                 AND m.list_id IN (SELECT value FROM json_each(?3))
                           ) = ?4
                       )
                     )
                 AND NOT EXISTS (
                       SELECT 1 FROM contact_list_members AS x
                       WHERE x.contact_id = contacts.contact_id
                         AND x.list_id IN (SELECT value FROM json_each(?5))
                     )
               ORDER BY contacts.rowid"#,
            everything,
            intersect,
            binding.lists,
            binding.wanted,
            binding.excluded
        )
        .fetch(self.database.pool())
        .map(|row| {
            row.map_err(|error| store_error(PersistenceError::from(error)))
                .and_then(|row| PagedRow::into_record(row).map_err(store_error))
        })
        .boxed()
    }

    async fn count_contacts(&self, selection: &ListSelection) -> Result<u64, ContactStoreError> {
        if selection.is_empty() {
            return Ok(0);
        }

        let binding = SelectionBinding::of(selection);
        let everything = matches!(selection.combination(), Combination::Everything);
        let intersect = matches!(selection.combination(), Combination::All);

        let total = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!: i64"
               FROM contacts
               WHERE (
                       ?1
                       OR (
                           NOT ?2
                           AND EXISTS (
                               SELECT 1 FROM contact_list_members AS m
                               WHERE m.contact_id = contacts.contact_id
                                 AND m.list_id IN (SELECT value FROM json_each(?3))
                           )
                       )
                       OR (
                           ?2
                           AND (
                               SELECT COUNT(DISTINCT m.list_id)
                               FROM contact_list_members AS m
                               WHERE m.contact_id = contacts.contact_id
                                 AND m.list_id IN (SELECT value FROM json_each(?3))
                           ) = ?4
                       )
                     )
                 AND NOT EXISTS (
                       SELECT 1 FROM contact_list_members AS x
                       WHERE x.contact_id = contacts.contact_id
                         AND x.list_id IN (SELECT value FROM json_each(?5))
                     )"#,
            everything,
            intersect,
            binding.lists,
            binding.wanted,
            binding.excluded
        )
        .fetch_one(self.database.pool())
        .await
        .map_err(|error| store_error(PersistenceError::from(error)))?;

        Ok(u64::try_from(total).unwrap_or(0))
    }

    async fn insert_contact_list(&self, list: &ContactList) -> Result<(), ContactStoreError> {
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
        .map_err(|source| PersistenceError::from_write(source, LIST_TABLE, list_id.clone()))
        .map_err(store_error)?;

        Ok(())
    }

    async fn find_contact_list(
        &self,
        list_id: ListId,
    ) -> Result<Option<ContactList>, ContactStoreError> {
        let identifier = list_id.to_string();

        let row = sqlx::query_as!(
            ContactListRow,
            "SELECT list_id, name, created_at FROM contact_lists WHERE list_id = ?",
            identifier
        )
        .fetch_optional(self.database.pool())
        .await
        .map_err(|error| store_error(PersistenceError::from(error)))?;

        row.map(ContactListRow::into_list)
            .transpose()
            .map_err(store_error)
    }

    async fn list_contact_lists(&self) -> Result<Vec<ContactList>, ContactStoreError> {
        let rows = sqlx::query_as!(
            ContactListRow,
            "SELECT list_id, name, created_at FROM contact_lists ORDER BY rowid"
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| store_error(PersistenceError::from(error)))?;

        rows.into_iter()
            .map(ContactListRow::into_list)
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_error)
    }

    async fn add_contacts_to_list(
        &self,
        list_id: ListId,
        contacts: &[ContactId],
    ) -> Result<u64, ContactStoreError> {
        let list = list_id.to_string();
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|error| store_error(PersistenceError::from(error)))?;
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
            .await
            .map_err(|error| membership_error(PersistenceError::from(error)))?
            .rows_affected();
        }

        transaction
            .commit()
            .await
            .map_err(|error| store_error(PersistenceError::from(error)))?;

        Ok(created)
    }

    async fn upsert_import_profile(
        &self,
        profile: &ImportProfile,
    ) -> Result<(), ContactStoreError> {
        let profile_id = profile.profile_id.to_string();
        let created_at = profile.created_at.to_storage();
        let mapping = profile.mapping_json().map_err(|error| {
            tracing::error!(error = ?error, "an import profile could not be serialised");

            ContactStoreError::Unavailable {
                reason: String::from("the column mapping could not be stored"),
            }
        })?;

        sqlx::query!(
            r#"INSERT INTO import_profiles (profile_id, name, mapping, created_at)
               VALUES (?, ?, ?, ?)
               ON CONFLICT (profile_id) DO UPDATE
                 SET name = excluded.name, mapping = excluded.mapping"#,
            profile_id,
            profile.name,
            mapping,
            created_at
        )
        .execute(self.database.pool())
        .await
        .map_err(|source| PersistenceError::from_write(source, PROFILE_TABLE, profile_id.clone()))
        .map_err(store_error)?;

        Ok(())
    }

    async fn list_import_profiles(&self) -> Result<Vec<ImportProfile>, ContactStoreError> {
        let rows = sqlx::query_as!(
            ImportProfileRow,
            "SELECT profile_id, name, mapping, created_at FROM import_profiles ORDER BY rowid"
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| store_error(PersistenceError::from(error)))?;

        rows.into_iter()
            .map(ImportProfileRow::into_profile)
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_error)
    }
}

impl crate::ports::ContactDirectory for SqliteContactRepository {
    async fn page_contacts(
        &self,
        selection: &ListSelection,
        search: Option<&str>,
        cursor: Cursor,
        limit: u32,
    ) -> Result<Page<Contact>, PersistenceError> {
        if selection.is_empty() {
            return Ok(Page {
                items: Vec::new(),
                next: None,
            });
        }

        let after = cursor.into_raw();
        let window = store_u32(limit);
        let binding = SelectionBinding::of(selection);
        let everything = matches!(selection.combination(), Combination::Everything);
        let intersect = matches!(selection.combination(), Combination::All);

        // The search deliberately covers the number and the attributes and not
        // `source`: an operator looking for a contact types digits or a name,
        // never `import_csv`.
        let needle = search.map(|value| format!("%{}%", strip_plus(value)));

        let rows = sqlx::query_as!(
            ContactRow,
            r#"SELECT contacts.rowid AS "rowid!: i64",
                      contacts.contact_id, contacts.msisdn, contacts.country,
                      contacts.valid, contacts.line_type, contacts.attributes,
                      contacts.source, contacts.created_at
               FROM contacts
               WHERE contacts.rowid > ?1
                 AND (
                       ?2
                       OR (
                           NOT ?3
                           AND EXISTS (
                               SELECT 1 FROM contact_list_members AS m
                               WHERE m.contact_id = contacts.contact_id
                                 AND m.list_id IN (SELECT value FROM json_each(?4))
                           )
                       )
                       OR (
                           ?3
                           AND (
                               SELECT COUNT(DISTINCT m.list_id)
                               FROM contact_list_members AS m
                               WHERE m.contact_id = contacts.contact_id
                                 AND m.list_id IN (SELECT value FROM json_each(?4))
                           ) = ?5
                       )
                     )
                 AND NOT EXISTS (
                       SELECT 1 FROM contact_list_members AS x
                       WHERE x.contact_id = contacts.contact_id
                         AND x.list_id IN (SELECT value FROM json_each(?6))
                     )
                 AND (?7 IS NULL
                      OR contacts.msisdn LIKE ?7
                      OR contacts.attributes LIKE ?7)
               ORDER BY contacts.rowid
               LIMIT ?8"#,
            after,
            everything,
            intersect,
            binding.lists,
            binding.wanted,
            binding.excluded,
            needle,
            window
        )
        .fetch_all(self.database.pool())
        .await?;

        into_page(rows, limit)
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
    let line_type = contact.line_type.map(LineType::code);
    let created_at = contact.created_at.to_storage();

    sqlx::query!(
        r#"INSERT INTO contacts (
               contact_id, msisdn, country, valid, line_type, attributes, source, created_at
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
        contact_id,
        msisdn,
        contact.country,
        valid,
        line_type,
        contact.attributes,
        contact.source,
        created_at
    )
    .execute(executor)
    .await
    .map_err(|source| PersistenceError::from_write(source, TABLE, contact_id.clone()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{json_array, strip_plus};
    use contacts::model::ListId;

    /// The mismatch that makes a search over `Msisdn` silently return nothing:
    /// the column holds digits, the operator types the `+` the rest of the
    /// interface shows them.
    #[test]
    fn a_search_for_a_number_drops_the_plus_the_column_does_not_hold() {
        assert_eq!(strip_plus("+2250102030405"), "2250102030405");
    }

    /// …and only for a number. The needle is matched against the attributes
    /// too, where a `+` is an ordinary character.
    #[test]
    fn a_search_for_text_keeps_its_plus() {
        assert_eq!(strip_plus("+225 01"), "+225 01");
        assert_eq!(strip_plus("+"), "+");
        assert_eq!(strip_plus("C++"), "C++");
    }

    #[test]
    fn an_empty_selection_renders_as_an_empty_json_array() {
        assert_eq!(json_array(&[]), "[]");
    }

    /// `json_each` is what expands this, so the rendering has to be JSON a
    /// SQLite build will accept — quoted elements, comma-separated.
    #[test]
    fn identifiers_render_as_a_json_array_of_strings() {
        let first = ListId::new();
        let second = ListId::new();

        assert_eq!(
            json_array(&[first, second]),
            format!("[\"{first}\",\"{second}\"]")
        );
    }
}
