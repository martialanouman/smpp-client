//! What milestone 004 needs to reach from outside `smpp-core`.
//!
//! An integration test sees the crate exactly as another crate does. A symbol
//! that `rusmpp` exposes but the facade forgot to re-export makes this file
//! fail to compile — which is the point. Milestone 003 shipped with two such
//! gaps, and neither showed up until the segmenter tried to use them.

// `allow-unwrap-in-tests` in clippy.toml only relaxes the lint under
// `#[cfg(test)]`. Files under `tests/` are separate crates compiled WITHOUT
// that cfg, so the ban would otherwise apply here as if this were production
// code.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use smpp_core::{
    octets::AnyOctetString,
    udhs::concatenation::ConcatenatedShortMessage8Bit,
    values::{EsmClass, GsmFeatures, MessagePayload},
};

/// Spec §7.5: past 254 octets the body moves to the `message_payload` TLV.
/// Without this type the alternative cannot be built at all.
#[test]
fn message_payload_is_reachable_from_the_facade() {
    let payload = MessagePayload::new(AnyOctetString::from_slice(b"hello"));

    assert_eq!(AnyOctetString::from(payload).into_vec(), b"hello");
}

/// The UDHI bit has to be both set and asserted on, which needs the field
/// type, not only the struct that contains it.
#[test]
fn the_udhi_bit_is_observable_on_an_esm_class() {
    let plain = EsmClass::default();
    let with_udhi = plain.with_udhi_indicator();

    assert_eq!(plain.gsm_features, GsmFeatures::NotSelected);
    assert_eq!(with_udhi.gsm_features, GsmFeatures::UdhiIndicator);
    assert_eq!(u8::from(with_udhi) & 0b0100_0000, 0b0100_0000);
}

/// The six octets of the concatenation UDH, spelled out by spec §7.5.
#[test]
fn the_concatenation_udh_is_reachable_from_the_facade() {
    let udh = ConcatenatedShortMessage8Bit::new(0x2A, 3, 2).unwrap();

    assert_eq!(udh.udh_bytes(), [0x05, 0x00, 0x03, 0x2A, 0x03, 0x02]);
}
