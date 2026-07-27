//! The port this crate defines and does not implement.
//!
//! Guide §8.1 and CLAUDE.md §3: a port is declared by the layer that
//! **consumes** it and implemented by the layer below. **CA-009-13** makes that
//! real for contacts — ADR 0012 records the reasoning and the shape of the
//! dependency graph it produces, and ADR 0007 is the debt it settles.
//!
//! | Port | Implemented by | What it abstracts |
//! |------|----------------|-------------------|
//! | [`ContactRepository`] | `persistence` | the durable contact store |
//!
//! Without it, an import of fifty thousand rows could only be tested against a
//! real SQLite file, and the two things this milestone must prove about the
//! import — that a cancellation leaves no half-written batch, and that a
//! rejection carries its line and its reason — would each need a database.
//! With it, `contacts` depends on `smpp-core` and on nothing else of ours.
//!
//! # What deliberately did **not** move
//!
//! Paging. `page_contacts` is what the contacts screen scrolls through, and its
//! caller is `src-tauri`, not this crate: nothing in the import or the list
//! algebra reads a page. It also speaks in `persistence::Cursor` and
//! `persistence::Page`, types whose whole reason to exist is the storage they
//! page over. So it stayed behind as `persistence::ports::ContactDirectory`,
//! consumed downward — the same split ADR 0010 made between
//! `messaging::ports::MessageRepository` and
//! `persistence::ports::MessageJournal`, and for the same reason.
//!
//! # Shape of the methods
//!
//! Each returns `impl Future<Output = …> + Send` rather than being an
//! `async fn`. Same thing to write against, one difference that matters: the
//! `Send` bound is part of the trait, so an implementation that is not `Send`
//! fails to compile at its definition instead of at the `tokio::spawn` three
//! layers up.

use core::future::Future;

use futures_core::stream::BoxStream;

use crate::import::ImportProfile;
use crate::lists::ListSelection;
use crate::model::{Contact, ContactId, ContactList, ListId};

/// Why the contact store refused a call.
///
/// # Why not the implementor's own error
///
/// `persistence` reports a `PersistenceError` carrying a `sqlx::Error` and, on
/// one variant, a filesystem path. Threading that type through this port would
/// put SQLx in the signature of a crate that must not know a database exists,
/// and would drag a path into an error this crate renders towards the IPC
/// boundary (CA-001-06).
///
/// So the port names the outcomes a caller can *act* on, and the implementor
/// maps onto them. The cost is stated rather than hidden: the source chain is
/// lost at the boundary, so the implementor logs the full failure — with its
/// chain — before returning [`Self::Unavailable`]. `persistence` does.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ContactStoreError {
    /// A row already carries this identifier.
    #[error("a record with this identifier already exists")]
    Conflict,

    /// The row a write referred to does not exist.
    ///
    /// Adding contacts to a list that was deleted, most often.
    #[error("the record this call refers to does not exist")]
    NotFound,

    /// The store could not be read or written.
    ///
    /// `reason` is the implementor's own rendering, without its source chain
    /// and without a filesystem path.
    #[error("the contact store is unavailable: {reason}")]
    Unavailable {
        /// Short, path-free summary of the underlying failure.
        reason: String,
    },
}

/// Reads and writes contacts, contact lists and import profiles.
///
/// Every write that an import performs is **batched**: a fifty-thousand-row
/// file must not mean fifty thousand transactions, and a batch that fails must
/// leave nothing behind (spec §11.2, CA-009-10).
pub trait ContactRepository: Send + Sync {
    /// Inserts one contact.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Conflict`] if the identifier already exists,
    /// [`ContactStoreError::Unavailable`] otherwise.
    fn insert_contact(
        &self,
        contact: &Contact,
    ) -> impl Future<Output = Result<(), ContactStoreError>> + Send;

    /// Inserts a batch of contacts in **one** transaction.
    ///
    /// An import is tens of thousands of rows (spec §11.2). One transaction
    /// per row would mean one `fsync` per row; one transaction for the batch
    /// is also all-or-nothing, so a failed batch leaves no half-written rows
    /// behind.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Conflict`] if any identifier already exists — and
    /// then **no** contact of the batch is written.
    fn insert_contacts(
        &self,
        contacts: &[Contact],
    ) -> impl Future<Output = Result<u64, ContactStoreError>> + Send;

    /// Reads one contact.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Unavailable`] if the read fails.
    fn find_contact(
        &self,
        contact_id: ContactId,
    ) -> impl Future<Output = Result<Option<Contact>, ContactStoreError>> + Send;

    /// Traverses the contacts a selection picks out, one row at a time.
    ///
    /// Rows arrive one at a time (spec §10.4): a campaign of several million
    /// recipients feeds a bounded queue from this stream, and memory stays flat
    /// whatever the total.
    ///
    /// The **order is unspecified**, and deliberately so: it is whichever order
    /// the index serving the traversal already yields. Imposing one would mean
    /// sorting, and sorting means buffering the whole result set before the
    /// first row — exactly what this method exists to avoid. Every contact is
    /// yielded exactly once; that is the whole contract.
    fn stream_contacts(
        &self,
        selection: &ListSelection,
    ) -> BoxStream<'_, Result<Contact, ContactStoreError>>;

    /// Counts the contacts a selection picks out.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Unavailable`] if the read fails.
    fn count_contacts(
        &self,
        selection: &ListSelection,
    ) -> impl Future<Output = Result<u64, ContactStoreError>> + Send;

    /// Creates a contact list.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Conflict`] if the identifier already exists.
    fn insert_contact_list(
        &self,
        list: &ContactList,
    ) -> impl Future<Output = Result<(), ContactStoreError>> + Send;

    /// Reads one contact list.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Unavailable`] if the read fails.
    fn find_contact_list(
        &self,
        list_id: ListId,
    ) -> impl Future<Output = Result<Option<ContactList>, ContactStoreError>> + Send;

    /// Reads every contact list, oldest first.
    ///
    /// Unpaginated, and deliberately: a list is created by hand or by one
    /// import, so the table holds units to hundreds, not millions. The
    /// contacts *inside* them are what needs paging, and that is
    /// [`Self::stream_contacts`]'s job.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Unavailable`] if the read fails.
    fn list_contact_lists(
        &self,
    ) -> impl Future<Output = Result<Vec<ContactList>, ContactStoreError>> + Send;

    /// Adds contacts to a list, in **one** transaction, ignoring those already
    /// in it.
    ///
    /// Returns how many memberships were created. Re-adding a contact is a
    /// no-op rather than an error: an import that overlaps a previous one is
    /// ordinary, not a fault.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::NotFound`] when the list or a contact does not
    /// exist, which the foreign keys reject.
    fn add_contacts_to_list(
        &self,
        list_id: ListId,
        contacts: &[ContactId],
    ) -> impl Future<Output = Result<u64, ContactStoreError>> + Send;

    /// Saves a column-mapping profile, replacing one of the same identifier.
    ///
    /// Upsert rather than insert-then-update because the caller is an
    /// interface form: it always holds the whole profile (CA-009-09).
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Unavailable`] if the write fails.
    fn upsert_import_profile(
        &self,
        profile: &ImportProfile,
    ) -> impl Future<Output = Result<(), ContactStoreError>> + Send;

    /// Reads every saved mapping profile, oldest first.
    ///
    /// # Errors
    ///
    /// [`ContactStoreError::Unavailable`] if the read fails.
    fn list_import_profiles(
        &self,
    ) -> impl Future<Output = Result<Vec<ImportProfile>, ContactStoreError>> + Send;
}
