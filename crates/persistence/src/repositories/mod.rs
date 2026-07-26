//! The SQLx implementations of the ports.
//!
//! Every statement of the application lives under this module, and nowhere
//! else (CA-002-03). Each repository takes a [`crate::Database`] — a cloned
//! handle on the shared pool — and speaks only in the records of
//! [`crate::records`].
//!
//! # Compile-time checking
//!
//! Every query goes through `sqlx::query!` or `sqlx::query_as!`, so a column
//! that a migration renames stops the build instead of stopping a campaign
//! (ADR 0002). There is **one** exception, in [`crate::Database`] itself: the
//! two `PRAGMA` reads use the unchecked `sqlx::query_scalar`, because a pragma
//! is not a table and `sqlx` has nothing to check it against.

mod campaigns;
mod contacts;
mod convert;
mod messages;
mod pdu_log;
mod session_profiles;

pub use campaigns::SqliteCampaignRepository;
pub use contacts::SqliteContactRepository;
pub use messages::SqliteMessageRepository;
pub use pdu_log::SqlitePduLogRepository;
pub use session_profiles::SqliteSessionProfileRepository;
