//! Typed protocol values.
//!
//! Spec §7.3, §7.4 and §7.5 list the constrained fields of an SMPP PDU. Guide
//! §5.6 requires them to be **enums with an explicit conversion to and from
//! the wire octet**, never a bare `u8`. `rusmpp` already models them that way,
//! so this module re-exports them rather than shadowing them behind a facade
//! that would have to be kept in sync for no benefit.
//!
//! The re-export is **curated, not wholesale**: every symbol listed below is a
//! commitment of the crate's public API (milestone 003 §6). `rusmpp` exposes
//! several dozen more value types — TLV payloads, broadcast descriptors — that
//! milestone 012 will surface as it needs them.
//!
//! Each enum carries an `Other(u8)` variant. That is deliberate on `rusmpp`'s
//! part and load-bearing here: an SMSC may send a value no specification lists,
//! and an ESME that refused to decode it could not even read the response
//! status. The unknown value is preserved and re-encoded verbatim — verified by
//! the round-trip tests over the whole byte range at the bottom of this file.
//!
//! # Milestone 004 additions
//!
//! [`MessagePayload`] carries the message body when it does not fit
//! `short_message` (spec §7.5). It was reachable neither here nor through
//! [`crate::octets`]: `rusmpp` places it under `values::owned`, a path the
//! curated list did not cover, which left the 64 KiB alternative
//! unimplementable from outside this crate.
//!
//! [`GsmFeatures`], [`MessagingMode`], [`MessageType`] and [`Ansi41Specific`]
//! are the four fields of [`EsmClass`]. Re-exporting the struct without its
//! field types made it impossible to build an `esm_class` other than the
//! default one, or to assert on the UDHI bit — which milestone 004 has to do
//! on every concatenated segment.

//! # Milestone 005 additions
//!
//! [`Gsm7BitPacking`] moved here from `messaging::encoding`, and
//! [`Gsm7BitCharset`] joined it. Both describe how the octets of a GSM 7-bit
//! body are to be read, both are decided by the **message centre**, and both
//! are therefore fields of the session profile — which lives in
//! `smpp-session`, a layer below `messaging`. A value the profile carries and
//! the encoder applies has to sit under both, and that is here. `messaging`
//! re-exports them, so its milestone-004 API is unchanged.
//!
//! # Milestone 006 additions
//!
//! Spec §7.3 lists sixteen mandatory fields on a `submit_sm` and CA-006-06
//! requires **every one** of them to be settable from the interface and to
//! reach the PDU. Five of their types were not on the curated list, which made
//! four of those fields unreachable from outside this crate:
//!
//! * [`ServiceType`] — the `service_type` field;
//! * [`ReplaceIfPresentFlag`] — the `replace_if_present_flag` field;
//! * [`MCDeliveryReceipt`], [`SmeOriginatedAcknowledgement`] and
//!   [`IntermediateNotification`] — the three sub-fields of
//!   [`RegisteredDelivery`], which was re-exported without them, so the only
//!   values constructible were `default()` and `request_all()`. Spec §23.3
//!   asks for `registered_delivery = 1`, which is neither.
//!
//! # Milestone 008 additions
//!
//! [`MessageState`] is the payload of the `message_state` TLV of spec §7.8 —
//! the machine-readable half of a delivery receipt, and the fallback when a
//! message centre sends no `stat:` in the body. It is **not** this
//! application's `messaging::MessageState`: one is a wire value with ten
//! variants, the other the six-state lifecycle of spec §14.3. `messaging`
//! imports this one under an alias for exactly that reason.

mod gsm7;
mod version;

pub use gsm7::{Gsm7BitCharset, Gsm7BitPacking};
pub use version::{SmppVersion, UnsupportedInterfaceVersion};

pub use rusmpp::{
    values::{
        Ansi41Specific, DataCoding, EsmClass, GsmFeatures, InterfaceVersion,
        IntermediateNotification, MCDeliveryReceipt, MessagePayload, MessageState, MessageType,
        MessagingMode, Npi, PriorityFlag, RegisteredDelivery, ReplaceIfPresentFlag, ServiceType,
        SmeOriginatedAcknowledgement, Ton, UserMessageReference,
    },
    CommandId, CommandStatus,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value of the whole `u8` range must survive a byte -> enum -> byte
    /// trip. A missing variant would silently rewrite a wire field.
    fn assert_u8_round_trip<T>()
    where
        T: From<u8> + Copy,
        u8: From<T>,
    {
        for byte in u8::MIN..=u8::MAX {
            let value = T::from(byte);
            assert_eq!(u8::from(value), byte, "byte {byte:#04X} was not preserved");
        }
    }

    #[test]
    fn ton_round_trips_over_the_whole_byte_range() {
        assert_u8_round_trip::<Ton>();
    }

    #[test]
    fn npi_round_trips_over_the_whole_byte_range() {
        assert_u8_round_trip::<Npi>();
    }

    #[test]
    fn data_coding_round_trips_over_the_whole_byte_range() {
        assert_u8_round_trip::<DataCoding>();
    }

    #[test]
    fn esm_class_round_trips_over_the_whole_byte_range() {
        assert_u8_round_trip::<EsmClass>();
    }

    #[test]
    fn registered_delivery_round_trips_over_the_whole_byte_range() {
        assert_u8_round_trip::<RegisteredDelivery>();
    }

    #[test]
    fn priority_flag_round_trips_over_the_whole_byte_range() {
        assert_u8_round_trip::<PriorityFlag>();
    }

    #[test]
    fn interface_version_round_trips_over_the_whole_byte_range() {
        assert_u8_round_trip::<InterfaceVersion>();
    }

    /// Spec §7.4: the drop-downs are documented from these values.
    #[test]
    fn ton_and_npi_match_the_spec_table() {
        assert_eq!(u8::from(Ton::Unknown), 0);
        assert_eq!(u8::from(Ton::International), 1);
        assert_eq!(u8::from(Ton::National), 2);
        assert_eq!(u8::from(Ton::NetworkSpecific), 3);
        assert_eq!(u8::from(Ton::SubscriberNumber), 4);
        assert_eq!(u8::from(Ton::Alphanumeric), 5);
        assert_eq!(u8::from(Ton::Abbreviated), 6);

        assert_eq!(u8::from(Npi::Unknown), 0);
        assert_eq!(u8::from(Npi::Isdn), 1);
        assert_eq!(u8::from(Npi::Data), 3);
        assert_eq!(u8::from(Npi::Telex), 4);
        assert_eq!(u8::from(Npi::LandMobile), 6);
        assert_eq!(u8::from(Npi::National), 8);
        assert_eq!(u8::from(Npi::Private), 9);
    }

    /// Spec §7.5: the encoding table the segmentation of milestone 004 reads.
    #[test]
    fn data_coding_matches_the_spec_table() {
        assert_eq!(u8::from(DataCoding::McSpecific), 0x00);
        assert_eq!(u8::from(DataCoding::Ia5), 0x01);
        assert_eq!(u8::from(DataCoding::Latin1), 0x03);
        assert_eq!(u8::from(DataCoding::Ucs2), 0x08);
    }

    #[test]
    fn smpp_version_maps_to_the_interface_version_octet() {
        assert_eq!(SmppVersion::V3_4.as_octet(), 0x34);
        assert_eq!(SmppVersion::V5_0.as_octet(), 0x50);
        assert_eq!(
            InterfaceVersion::from(SmppVersion::V3_4),
            InterfaceVersion::Smpp3_4
        );
        assert_eq!(
            InterfaceVersion::from(SmppVersion::V5_0),
            InterfaceVersion::Smpp5_0
        );
    }

    #[test]
    fn smpp_version_rejects_an_unsupported_interface_version() {
        assert_eq!(
            SmppVersion::try_from(InterfaceVersion::Smpp3_4),
            Ok(SmppVersion::V3_4)
        );
        assert_eq!(
            SmppVersion::try_from(InterfaceVersion::Smpp5_0),
            Ok(SmppVersion::V5_0)
        );
        assert!(SmppVersion::try_from(InterfaceVersion::Smpp3_3OrEarlier(0x33)).is_err());
        assert!(SmppVersion::try_from(InterfaceVersion::Other(0x99)).is_err());
    }

    #[test]
    fn smpp_version_defaults_to_the_most_recent() {
        assert_eq!(SmppVersion::default(), SmppVersion::V5_0);
    }

    #[test]
    fn broadcast_operations_are_rejected_before_v5() {
        assert!(!SmppVersion::V3_4.supports(CommandId::BroadcastSm));
        assert!(SmppVersion::V5_0.supports(CommandId::BroadcastSm));
        assert!(SmppVersion::V3_4.supports(CommandId::SubmitSm));
    }
}
