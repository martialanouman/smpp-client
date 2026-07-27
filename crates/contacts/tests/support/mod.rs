//! An in-memory contact store, and a clock that does not move.
//!
//! # Why a double rather than a temporary SQLite file
//!
//! This is what CA-009-13 bought. `ContactRepository` is declared by this crate
//! (ADR 0012), so an import of fifty thousand rows, its cancellation and the
//! exactness of its report are all assertable with no database in reach — and
//! the tests run in milliseconds instead of in `fsync`s.
//!
//! There is deliberately **no** dev-dependency on `persistence` either: it
//! would form `contacts (dev) → persistence → contacts`, a cycle Cargo tolerates
//! and CLAUDE.md §3 does not distinguish. The SQLx side of the same port is
//! tested in `crates/persistence/tests/repositories.rs`.

#![allow(dead_code)]

use std::sync::Arc;

use contacts::lists::ListSelection;
use contacts::model::{Contact, ContactId, ContactList, ListId};
use contacts::ports::{ContactRepository, ContactStoreError};
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use smpp_core::time::{Clock, Timestamp};
use tokio::sync::Mutex;

/// A clock stopped at one instant.
///
/// CLAUDE.md §7: an import stamps every contact it writes, and a test that read
/// the wall clock could not assert on the value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrozenClock(Timestamp);

impl FrozenClock {
    /// A clock stopped at `2026-07-27T09:00:00Z`.
    pub(crate) fn new() -> Self {
        Self(Timestamp::parse("2026-07-27T09:00:00Z").expect("a valid RFC 3339 instant"))
    }

    /// The instant it is stopped at.
    pub(crate) const fn instant(self) -> Timestamp {
        self.0
    }
}

impl Clock for FrozenClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// What a store did, in the order it did it.
#[derive(Debug, Default)]
pub(crate) struct StoreState {
    pub(crate) contacts: Vec<Contact>,
    pub(crate) lists: Vec<ContactList>,
    pub(crate) memberships: Vec<(ListId, ContactId)>,
    /// One entry per `insert_contacts` call, holding its size.
    ///
    /// This is what makes "the batches already committed survive a
    /// cancellation" (CA-009-10) observable: the count of transactions, not
    /// just the count of rows.
    pub(crate) batches: Vec<usize>,
    /// When set, the *n*-th `insert_contacts` call fails.
    pub(crate) fail_batch: Option<usize>,
}

/// An in-memory [`ContactRepository`].
#[derive(Debug, Clone, Default)]
pub(crate) struct MemoryStore {
    state: Arc<Mutex<StoreState>>,
}

impl MemoryStore {
    /// An empty store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A store whose `index`-th batch (0-based) fails.
    ///
    /// The state is built rather than locked into shape: `blocking_lock` inside
    /// a `#[tokio::test]` is a panic, and this constructor is called from one.
    pub(crate) fn failing_at_batch(index: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(StoreState {
                fail_batch: Some(index),
                ..StoreState::default()
            })),
        }
    }

    /// The contacts written so far.
    pub(crate) async fn contacts(&self) -> Vec<Contact> {
        self.state.lock().await.contacts.clone()
    }

    /// The size of each committed batch, in order.
    pub(crate) async fn batches(&self) -> Vec<usize> {
        self.state.lock().await.batches.clone()
    }

    /// The memberships created so far.
    pub(crate) async fn memberships(&self) -> Vec<(ListId, ContactId)> {
        self.state.lock().await.memberships.clone()
    }
}

impl ContactRepository for MemoryStore {
    async fn insert_contact(&self, contact: &Contact) -> Result<(), ContactStoreError> {
        let mut state = self.state.lock().await;

        if state
            .contacts
            .iter()
            .any(|held| held.contact_id == contact.contact_id)
        {
            return Err(ContactStoreError::Conflict);
        }

        state.contacts.push(contact.clone());

        Ok(())
    }

    async fn insert_contacts(&self, contacts: &[Contact]) -> Result<u64, ContactStoreError> {
        let mut state = self.state.lock().await;

        if state.fail_batch == Some(state.batches.len()) {
            // All-or-nothing, like the transaction it stands for: nothing of
            // this batch is recorded, not even its size.
            return Err(ContactStoreError::Unavailable {
                reason: String::from("the double was asked to fail this batch"),
            });
        }

        state.batches.push(contacts.len());
        state.contacts.extend_from_slice(contacts);

        Ok(contacts.len().try_into().unwrap_or(u64::MAX))
    }

    async fn find_contact(
        &self,
        contact_id: ContactId,
    ) -> Result<Option<Contact>, ContactStoreError> {
        Ok(self
            .state
            .lock()
            .await
            .contacts
            .iter()
            .find(|held| held.contact_id == contact_id)
            .cloned())
    }

    fn stream_contacts(
        &self,
        _selection: &ListSelection,
    ) -> BoxStream<'_, Result<Contact, ContactStoreError>> {
        let state = Arc::clone(&self.state);

        futures_util::stream::once(async move { state.lock().await.contacts.clone() })
            .flat_map(|contacts| futures_util::stream::iter(contacts.into_iter().map(Ok)))
            .boxed()
    }

    async fn count_contacts(&self, _selection: &ListSelection) -> Result<u64, ContactStoreError> {
        Ok(self
            .state
            .lock()
            .await
            .contacts
            .len()
            .try_into()
            .unwrap_or(u64::MAX))
    }

    async fn insert_contact_list(&self, list: &ContactList) -> Result<(), ContactStoreError> {
        self.state.lock().await.lists.push(list.clone());

        Ok(())
    }

    async fn find_contact_list(
        &self,
        list_id: ListId,
    ) -> Result<Option<ContactList>, ContactStoreError> {
        Ok(self
            .state
            .lock()
            .await
            .lists
            .iter()
            .find(|held| held.list_id == list_id)
            .cloned())
    }

    async fn list_contact_lists(&self) -> Result<Vec<ContactList>, ContactStoreError> {
        Ok(self.state.lock().await.lists.clone())
    }

    async fn add_contacts_to_list(
        &self,
        list_id: ListId,
        contacts: &[ContactId],
    ) -> Result<u64, ContactStoreError> {
        let mut state = self.state.lock().await;
        let mut created = 0;

        for contact_id in contacts {
            if !state.memberships.contains(&(list_id, *contact_id)) {
                state.memberships.push((list_id, *contact_id));
                created += 1;
            }
        }

        Ok(created)
    }

    async fn upsert_import_profile(
        &self,
        _profile: &contacts::import::ImportProfile,
    ) -> Result<(), ContactStoreError> {
        Ok(())
    }

    async fn list_import_profiles(
        &self,
    ) -> Result<Vec<contacts::import::ImportProfile>, ContactStoreError> {
        Ok(Vec::new())
    }
}
