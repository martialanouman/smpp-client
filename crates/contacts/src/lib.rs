//! Contact import, E.164 validation and list management (milestone 009).
//!
//! Reads CSV and XLSX files, normalises numbers to E.164 through the
//! numbering-plan database of `phonenumber`, deduplicates on the **normalised**
//! number, reports every rejection with its line and its reason, and organises
//! contacts into named lists that combine by union and intersection.
//!
//! *Parse, don't validate*: a number crossing this crate's boundary is an
//! [`smpp_core::types::Msisdn`] carried by a [`model::Contact`], not a
//! `String` — invalid state becomes unrepresentable downstream.
//!
//! # Layout
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`model`] | the contact aggregate and its identifiers |
//! | [`ports`] | [`ports::ContactRepository`], implemented by `persistence` |
//! | [`validation`] | E.164 normalisation and the reasons a number is refused |
//! | [`import`] | the CSV and XLSX readers, the column mapping, the report |
//! | [`lists`] | named lists and the union/intersection algebra |
//!
//! # This crate depends on `smpp-core` and on nothing else of ours
//!
//! It declares [`ports::ContactRepository`] and `persistence` implements it —
//! the dependency inversion of guide §8.1, whose deadline was **CA-009-13** and
//! whose reasoning is ADR 0012. What that buys is the reason it was done: an
//! import of fifty thousand rows, its cancellation, and the exactness of its
//! report are all tested here against an in-memory double, with no database in
//! reach.
//!
//! # Volumetry
//!
//! CA-009-01: a one-million-row file must not make the process grow with it.
//! Every reader is a streaming one and nothing here ever holds the file; what
//! *does* grow is the deduplication index, by design and by distinct number
//! rather than by row — [`import::Deduplication`] states exactly how much, and
//! which of the two strategies trades that guarantee away.

mod error;
pub mod import;
pub mod lists;
pub mod model;
pub mod ports;
pub mod validation;

pub use error::ContactsError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!contacts::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
