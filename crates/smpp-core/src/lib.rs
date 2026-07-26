//! SMPP protocol core: PDU codec and typed protocol values, v3.4 and v5.0.
//!
//! The lowest layer of the architecture (guide §8.1). It depends on **no other
//! internal crate** and knows nothing about persistence, networking or Tauri:
//! it turns bytes into typed PDUs and back.
//!
//! # What lives here, and what does not
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`codec`] | encode/decode a whole PDU, and the `CommandCodec` milestone 005 will mount on a socket |
//! | [`values`] | the constrained protocol enums — TON, NPI, DCS, `command_id`, `command_status` |
//! | [`status_codes`] | the `command_status` table: labels for the UI, classification for the retry policy |
//! | [`types`] | domain newtypes: [`types::Msisdn`], [`types::SessionId`], [`types::ClientMessageId`], [`types::CampaignId`], [`types::SequenceNumber`] |
//! | [`time`] | [`time::Timestamp`], the single instant format, and the [`time::Clock`] port |
//! | [`debug`] | PDU hex dump, gated behind an explicit authorisation |
//!
//! Not here, by design: any I/O (milestone 005), text encoding and
//! segmentation (milestone 004), the session state machine (milestone 005).
//!
//! # Relationship to `rusmpp`
//!
//! ADR
//! [`0001-choix-de-la-pile-smpp`](../../../docs/adr/0001-choix-de-la-pile-smpp.md)
//! settles it: the codec comes from `rusmpp`, taken at its **low level**, and
//! this crate is the only one in the workspace allowed to depend on it. The
//! re-exports below are the surface the rest of the workspace sees. Be honest
//! about what that surface is: `pdus`, `tlvs` and `types` are re-exported as
//! WHOLE MODULES, not symbol by symbol. ADR 0001 says why — a full adapter
//! layer would cost thousands of conversion lines, teach nothing about the
//! protocol, and make every conversion an opportunity for a silent bug.
//!
//! The consequence is worth stating rather than glossing over: an upstream
//! API change propagates to calling crates. The centralised re-export only
//! guarantees it becomes visible in one place.

pub mod codec;
pub mod debug;
mod error;
pub mod status_codes;
pub mod time;
pub mod types;
pub mod values;

pub use error::{FieldRejection, SmppError};

/// PDU bodies: `SubmitSm`, `DeliverSm`, `BindTransceiver`, and their responses.
///
/// The whole set of spec §7.2 is available; milestone 003 makes them
/// representable, milestones 004 and 012 put them to work.
pub use rusmpp::pdus;

/// Optional parameters (TLVs) and their tags.
///
/// Milestone 003 only makes them representable; the v5.0 TLVs of spec §7.7 are
/// exploited at milestone 012.
pub use rusmpp::tlvs;

/// The string types of the protocol: `COctetString`, `OctetString`.
///
/// Named `octets` rather than `types` to leave that name to the domain
/// newtypes, which are what most callers actually want.
pub use rusmpp::types as octets;

/// User Data Headers, and the concatenation UDH of spec §7.5 in particular.
///
/// A UDH is not a PDU field: it lives *inside* `short_message`, ahead of the
/// user data. Milestone 004 builds one per segment, which is why the module
/// surfaces here rather than staying an implementation detail of `rusmpp`.
/// [`udhs::concatenation::ConcatenatedShortMessage8Bit`] validates the
/// invariants the spec leaves implicit — a part number of zero, or one greater
/// than the total — so the segmenter does not have to restate them.
pub use rusmpp::udhs;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!smpp_core::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
