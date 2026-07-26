//! Codec acceptance tests for milestone 003.
//!
//! Integration tests, so they see exactly what another crate would see: if a
//! type needed here is not public, the test does not compile — which is the
//! cheapest possible check on the crate's public surface.

// `allow-unwrap-in-tests` and `allow-expect-in-tests` in clippy.toml only
// relax the lints under `#[cfg(test)]`. Files under `tests/` are separate
// crates compiled WITHOUT that cfg, so the relaxation does not reach them and
// the ban would apply here as if this were production code.
//
// Re-stating it at the top of the file is the right scope: it stays confined
// to a test target, and it is visible to anyone reading it — unlike widening
// the rule workspace-wide, which would also silence production code.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use proptest::prelude::*;
use smpp_core::{
    codec::{self, Command, Pdu},
    pdus::{
        BindReceiver, BindReceiverResp, BindTransceiver, BindTransceiverResp, BindTransmitter,
        BindTransmitterResp, BroadcastSm, BroadcastSmResp, CancelBroadcastSm, CancelSm, DataSm,
        DataSmResp, DeliverSm, DeliverSmResp, Outbind, QueryBroadcastSm, QueryBroadcastSmResp,
        QuerySm, QuerySmResp, ReplaceSm, SubmitMulti, SubmitMultiResp, SubmitSm, SubmitSmResp,
    },
    types::SequenceNumber,
    values::{CommandId, CommandStatus},
    SmppError,
};

/// Every operation of spec §7.2, with the `command_id` the table gives it.
///
/// The list is written out by hand on purpose: transcribing it from the codec
/// would make the test agree with itself.
fn operations_of_the_specification() -> Vec<(u32, Pdu)> {
    vec![
        (
            0x0000_0002,
            Pdu::BindTransmitter(BindTransmitter::default()),
        ),
        (
            0x8000_0002,
            Pdu::BindTransmitterResp(BindTransmitterResp::default()),
        ),
        (0x0000_0001, Pdu::BindReceiver(BindReceiver::default())),
        (
            0x8000_0001,
            Pdu::BindReceiverResp(BindReceiverResp::default()),
        ),
        (
            0x0000_0009,
            Pdu::BindTransceiver(BindTransceiver::default()),
        ),
        (
            0x8000_0009,
            Pdu::BindTransceiverResp(BindTransceiverResp::default()),
        ),
        (0x0000_000B, Pdu::Outbind(Outbind::default())),
        (0x0000_0006, Pdu::Unbind),
        (0x8000_0006, Pdu::UnbindResp),
        (0x0000_0004, Pdu::SubmitSm(SubmitSm::default())),
        (0x8000_0004, Pdu::SubmitSmResp(SubmitSmResp::default())),
        (0x0000_0021, Pdu::SubmitMulti(SubmitMulti::default())),
        (
            0x8000_0021,
            Pdu::SubmitMultiResp(SubmitMultiResp::default()),
        ),
        (0x0000_0005, Pdu::DeliverSm(DeliverSm::default())),
        (0x8000_0005, Pdu::DeliverSmResp(DeliverSmResp::default())),
        (0x0000_0103, Pdu::DataSm(DataSm::default())),
        (0x8000_0103, Pdu::DataSmResp(DataSmResp::default())),
        (0x0000_0003, Pdu::QuerySm(QuerySm::default())),
        (0x8000_0003, Pdu::QuerySmResp(QuerySmResp::default())),
        (0x0000_0008, Pdu::CancelSm(CancelSm::default())),
        (0x8000_0008, Pdu::CancelSmResp),
        (0x0000_0007, Pdu::ReplaceSm(ReplaceSm::default())),
        (0x8000_0007, Pdu::ReplaceSmResp),
        (0x0000_0015, Pdu::EnquireLink),
        (0x8000_0015, Pdu::EnquireLinkResp),
        (
            0x0000_0102,
            Pdu::AlertNotification(smpp_core::pdus::AlertNotification::default()),
        ),
        (0x8000_0000, Pdu::GenericNack),
        (0x0000_0111, Pdu::BroadcastSm(BroadcastSm::default())),
        (
            0x8000_0111,
            Pdu::BroadcastSmResp(BroadcastSmResp::default()),
        ),
        (
            0x0000_0112,
            Pdu::QueryBroadcastSm(QueryBroadcastSm::default()),
        ),
        (
            0x8000_0112,
            Pdu::QueryBroadcastSmResp(QueryBroadcastSmResp::default()),
        ),
        (
            0x0000_0113,
            Pdu::CancelBroadcastSm(CancelBroadcastSm::default()),
        ),
        (0x8000_0113, Pdu::CancelBroadcastSmResp),
    ]
}

/// CA-003-01 — every PDU of the spec §7.2 table encodes and decodes, and lands
/// on the `command_id` the table gives it.
#[test]
fn every_operation_of_the_specification_round_trips() {
    for (expected_id, pdu) in operations_of_the_specification() {
        let command = Command::new(CommandStatus::EsmeRok, 1, pdu);

        assert_eq!(
            u32::from(command.id()),
            expected_id,
            "{:?} does not carry the command_id of spec §7.2",
            command.id()
        );

        let bytes = codec::encode(&command).expect("encoding");
        let decoded = codec::decode(&bytes).expect("decoding");

        assert_eq!(
            decoded,
            command,
            "{:?} did not survive the round trip",
            command.id()
        );
    }
}

/// CA-003-01 — the fixture list is complete: no operation the codec knows is
/// left untested.
#[test]
fn the_fixture_list_covers_every_known_operation() {
    let covered: Vec<u32> = operations_of_the_specification()
        .iter()
        .map(|(id, _)| *id)
        .collect();

    let mut missing = Vec::new();

    for value in 0..=0x0000_0200u32 {
        for candidate in [value, value | 0x8000_0000] {
            if matches!(CommandId::from(candidate), CommandId::Other(_)) {
                continue;
            }

            if !covered.contains(&candidate) {
                missing.push(format!("{candidate:#010X}"));
            }
        }
    }

    assert!(missing.is_empty(), "operations not covered: {missing:?}");
}

/// The header layout of spec §7.1, checked on a PDU whose bytes are known.
#[test]
fn the_header_follows_the_layout_of_the_specification() {
    let command = Command::new(CommandStatus::EsmeRok, 0x0BAD_CAFE, Pdu::EnquireLink);
    let bytes = codec::encode(&command).expect("encoding");

    assert_eq!(bytes.len(), 16, "enquire_link has no body");
    assert_eq!(&bytes[0..4], &[0x00, 0x00, 0x00, 0x10]); // command_length
    assert_eq!(&bytes[4..8], &[0x00, 0x00, 0x00, 0x15]); // command_id
    assert_eq!(&bytes[8..12], &[0x00, 0x00, 0x00, 0x00]); // command_status
    assert_eq!(&bytes[12..16], &[0x0B, 0xAD, 0xCA, 0xFE]); // sequence_number
}

/// Reference vector from the `rusmpp` documentation, itself lifted from the
/// SMPP specification: a `bind_transmitter` in hexadecimal.
#[test]
fn a_reference_vector_from_the_specification_decodes_to_the_expected_structure() {
    let bytes: Vec<u8> = vec![
        0x00, 0x00, 0x00, 0x2F, // command_length = 47
        0x00, 0x00, 0x00, 0x02, // command_id = bind_transmitter
        0x00, 0x00, 0x00, 0x00, // command_status = ESME_ROK
        0x00, 0x00, 0x00, 0x01, // sequence_number = 1
        0x53, 0x4D, 0x50, 0x50, 0x33, 0x54, 0x45, 0x53, 0x54, 0x00, // "SMPP3TEST"
        0x73, 0x65, 0x63, 0x72, 0x65, 0x74, 0x30, 0x38, 0x00, // "secret08"
        0x53, 0x55, 0x42, 0x4D, 0x49, 0x54, 0x31, 0x00, // "SUBMIT1"
        0x50, // interface_version = 0x50
        0x01, // addr_ton
        0x01, // addr_npi
        0x00, // addr_range = NULL
    ];

    let command = codec::decode(&bytes).expect("decoding");

    assert_eq!(command.id(), CommandId::BindTransmitter);
    assert_eq!(command.status(), CommandStatus::EsmeRok);
    assert_eq!(command.sequence_number(), 1);

    let Some(Pdu::BindTransmitter(bind)) = command.pdu() else {
        panic!("expected a bind_transmitter body");
    };
    assert_eq!(bind.system_id.to_string(), "SMPP3TEST");
    assert_eq!(u8::from(bind.interface_version), 0x50);

    // Re-encoding must produce exactly the original bytes.
    assert_eq!(codec::encode(&command).expect("encoding"), bytes);
}

/// CA-003-03 — an inconsistent `command_length` is an error, never a panic.
#[test]
fn an_inconsistent_command_length_is_reported() {
    let mut bytes = codec::encode(&Command::new(CommandStatus::EsmeRok, 1, Pdu::EnquireLink))
        .expect("encoding");

    // Announce 32 bytes while supplying 16.
    bytes[3] = 0x20;
    assert!(matches!(
        codec::decode(&bytes),
        Err(SmppError::Incomplete { .. })
    ));

    // Announce fewer than the 16 bytes of the header.
    bytes[3] = 0x08;
    assert!(codec::decode(&bytes).is_err());
}

#[test]
fn trailing_bytes_after_a_complete_pdu_are_reported() {
    let mut bytes = codec::encode(&Command::new(CommandStatus::EsmeRok, 1, Pdu::EnquireLink))
        .expect("encoding");
    bytes.extend_from_slice(&[0xAA, 0xBB]);

    assert!(matches!(
        codec::decode(&bytes),
        Err(SmppError::TrailingBytes { count: 2 })
    ));
}

#[test]
fn an_unterminated_c_octet_string_is_reported() {
    // bind_transmitter whose system_id is never NUL-terminated.
    let bytes: Vec<u8> = vec![
        0x00, 0x00, 0x00, 0x14, // command_length = 20
        0x00, 0x00, 0x00, 0x02, // bind_transmitter
        0x00, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x01, //
        0x41, 0x42, 0x43, 0x44, // "ABCD", no NUL
    ];

    assert!(matches!(codec::decode(&bytes), Err(SmppError::Decode(_))));
}

#[test]
fn a_truncated_tlv_is_reported() {
    // submit_sm_resp announcing a 4-octet TLV but carrying only 1.
    let bytes: Vec<u8> = vec![
        0x00, 0x00, 0x00, 0x16, // command_length = 22
        0x80, 0x00, 0x00, 0x04, // submit_sm_resp
        0x00, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x01, //
        0x00, // message_id = NULL
        0x00, 0x1E, // tag
        0x00, 0x04, // length = 4
        0x01, // only one octet supplied
    ];

    assert!(codec::decode(&bytes).is_err());
}

/// Octets *inside* `command_length` belong to the PDU, whatever the body
/// parser makes of them.
///
/// `CommandCodec::decode` never advances the buffer to `command_length`, and
/// several PDUs — the three `bind_*`, `outbind`, `query_sm`, `query_sm_resp`,
/// `cancel_sm` — are parsed without a length. A vendor TLV appended to a bind
/// therefore stayed in the buffer and was reported as `TrailingBytes`, even
/// though the sender had counted it in `command_length`.
///
/// The same codec goes on a `Framed` at milestone 005. There the leftover
/// raises nothing: it is read as the start of the next PDU, silently
/// desynchronising the framing. This test pins the boundary decision to the
/// header.
#[test]
fn a_vendor_tlv_inside_command_length_is_not_reported_as_trailing() {
    let bind = Command::new(
        CommandStatus::EsmeRok,
        1,
        Pdu::BindTransceiver(BindTransceiver::default()),
    );
    let encoded = codec::encode(&bind).expect("encoding");

    // A five-octet TLV the body parser will not consume: tag 0x1400 (vendor
    // range), length 1, value 0x2A.
    let extra: [u8; 5] = [0x14, 0x00, 0x00, 0x01, 0x2A];

    let mut framed = encoded.clone();
    framed.extend_from_slice(&extra);

    // The sender counts it in `command_length`, as the specification requires.
    let total = u32::try_from(framed.len()).expect("small");
    framed[0..4].copy_from_slice(&total.to_be_bytes());

    let decoded =
        codec::decode(&framed).expect("a PDU carrying an unconsumed vendor TLV must still decode");
    assert_eq!(decoded.id(), CommandId::BindTransceiver);

    // Contrast: the same octets left OUTSIDE `command_length` are genuine
    // trailing bytes and must be refused.
    let mut with_real_trailing = encoded;
    with_real_trailing.extend_from_slice(&extra);

    assert!(
        matches!(
            codec::decode(&with_real_trailing),
            Err(SmppError::TrailingBytes { count: 5 })
        ),
        "octets beyond command_length must be rejected"
    );
}

/// A `command_length` below the header size is self-contradictory: no extra
/// byte would ever make it valid, so it is not an `Incomplete`.
#[test]
fn a_command_length_below_the_header_is_malformed_not_incomplete() {
    let bytes: Vec<u8> = vec![
        0x00, 0x00, 0x00, 0x08, // command_length = 8, below the 16-byte header
        0x00, 0x00, 0x00, 0x15, // enquire_link
        0x00, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x01, //
    ];

    assert!(matches!(
        codec::decode(&bytes),
        Err(SmppError::Malformed {
            announced: 8,
            minimum: 16
        })
    ));
}

/// CLAUDE.md §8 — the sanctioned log line must not leak the body.
///
/// Uses a real password so the assertion has something to catch. The contrast
/// with `{:?}` is asserted too: if a future version of the re-exported types
/// stopped leaking, this test would say so rather than silently guarding
/// nothing.
#[test]
fn the_redacted_form_never_leaks_the_bind_password() {
    // `password` is a COctetString<1, 9>: eight characters plus the NUL.
    const PASSWORD: &str = "s3cr3t08";

    let bind = BindTransmitter::builder()
        .system_id(
            smpp_core::octets::COctetString::from_string("SMPP3TEST".to_owned()).expect("bounded"),
        )
        .password(
            smpp_core::octets::COctetString::from_string(PASSWORD.to_owned()).expect("bounded"),
        )
        .build();

    let command = Command::new(CommandStatus::EsmeRok, 7, Pdu::BindTransmitter(bind));

    let line = smpp_core::debug::redacted(&command);
    assert!(
        !line.contains(PASSWORD),
        "the redacted form leaked the password: {line}"
    );
    assert!(
        line.contains("BindTransmitter"),
        "the operation must stay visible"
    );
    assert!(line.contains('7'), "the sequence_number must stay visible");

    // The very leak this function exists to avoid.
    assert!(
        format!("{command:?}").contains(PASSWORD),
        "the derived Debug no longer leaks — `redacted` may have lost its purpose"
    );
}

/// CA-003-03 — light fuzzing: 10 000 pseudo-random inputs, no panic.
///
/// # Why the header is built rather than drawn
///
/// A first version drew all four header bytes at random too. Measured over
/// its own 10 000 inputs, the outcome was:
///
/// ```text
/// too_large=8720  incomplete=1280  decode_err=0  trailing=0  ok=0
/// ```
///
/// Not a single input reached `Pdu::decode`: a uniformly drawn
/// `command_length` averages two billion, so `MAX_COMMAND_LENGTH` rejected
/// everything at the door. The test proved the length guard does not panic
/// and nothing else — while the property that matters, "the body decoder
/// survives arbitrary bytes", went untested.
///
/// So three quarters of the inputs now carry a coherent header — real
/// `command_id`, `command_length` equal to the actual buffer size — and only
/// the body is random. The remaining quarter stays fully random to keep
/// covering the guard itself. The distribution is asserted below: a future
/// change that silently stops reaching the decoder fails the test instead of
/// passing quietly.
#[test]
fn random_bytes_never_panic() {
    // Deterministic generator (guide §13: no uncontrolled randomness).
    let mut state: u64 = 0x2026_0725_5EED_1234;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    // Real command ids, so the header steers the decoder towards an actual
    // body parser rather than being rejected as unknown.
    let known_ids: Vec<u32> = operations_of_the_specification()
        .into_iter()
        .map(|(id, _)| id)
        .collect();

    let mut reached_the_body_decoder = 0_u32;

    for round in 0..10_000_u32 {
        let body_length = usize::try_from(next() % 96).expect("fits");

        let mut body = Vec::with_capacity(body_length);
        for _ in 0..body_length {
            body.push(u8::try_from(next() & 0xFF).expect("masked to a byte"));
        }

        let bytes = if round % 4 == 0 {
            // A quarter fully random: keeps the length guard under test.
            let mut raw = Vec::with_capacity(body_length + 4);
            for _ in 0..4 {
                raw.push(u8::try_from(next() & 0xFF).expect("masked to a byte"));
            }
            raw.extend_from_slice(&body);
            raw
        } else {
            // Three quarters with a coherent header: the body decoder is the
            // thing actually being fuzzed.
            let index = usize::try_from(next() % 64).expect("fits") % known_ids.len();
            let command_id = known_ids[index];
            let status = next() % 0x0000_0100;
            let sequence = 1 + u32::try_from(next() % 0x7FFF_FFFE).expect("fits");

            let total = u32::try_from(16 + body.len()).expect("bounded by the loop");

            let mut framed = Vec::with_capacity(body.len() + 16);
            framed.extend_from_slice(&total.to_be_bytes());
            framed.extend_from_slice(&command_id.to_be_bytes());
            framed.extend_from_slice(&status.to_be_bytes());
            framed.extend_from_slice(&sequence.to_be_bytes());
            framed.extend_from_slice(&body);

            reached_the_body_decoder += 1;
            framed
        };

        // The contract is "returns", not "succeeds".
        let _ = codec::decode(&bytes);
    }

    // Guards the property this test exists for. Without it, a change to the
    // generator could quietly bring us back to fuzzing nothing but the length
    // check — which is precisely the bug this version fixes.
    assert!(
        reached_the_body_decoder > 7_000,
        "only {reached_the_body_decoder} inputs carried a decodable header; \
         the body decoder is barely being fuzzed"
    );
}

/// CA-003-03 — every truncation of a valid PDU must be rejected cleanly.
#[test]
fn every_truncation_of_a_valid_pdu_is_rejected() {
    for (_, pdu) in operations_of_the_specification() {
        let bytes = codec::encode(&Command::new(CommandStatus::EsmeRok, 7, pdu)).expect("encoding");

        for cut in 0..bytes.len() {
            // Every truncation is strictly shorter than `command_length`, so a
            // rejection is guaranteed — asserting it is what makes the test
            // match its own name. Without this it only proved "no panic".
            assert!(
                codec::decode(&bytes[..cut]).is_err(),
                "a PDU truncated to {cut} byte(s) was accepted"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CA-003-02 — round trip by property
// ---------------------------------------------------------------------------

/// ASCII payload, from empty to `max` characters: covers "empty body" and
/// "maximal body" without writing two separate tests.
fn ascii(max: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(0x21u8..0x7Fu8, 0..=max)
        .prop_map(|bytes| String::from_utf8(bytes).unwrap_or_default())
}

fn bytes(max: usize) -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..=max)
}

/// Builds a `COctetString` from a possibly empty string.
///
/// `COctetString::from_string` requires at least the terminating NUL, so the
/// empty case needs `empty()`. Keeping both paths here is deliberate: the
/// strategies above generate empty values on purpose, and an empty
/// `system_type` or `source_addr` is perfectly legal on the wire.
fn c_octet<const MIN: usize, const MAX: usize>(
    value: String,
) -> smpp_core::octets::COctetString<MIN, MAX> {
    if value.is_empty() {
        smpp_core::octets::COctetString::empty()
    } else {
        smpp_core::octets::COctetString::from_string(value).expect("bounded by the strategy")
    }
}

/// Same idea for `OctetString`, which has no NUL and therefore no empty case.
fn octet<const MIN: usize, const MAX: usize>(
    value: Vec<u8>,
) -> smpp_core::octets::OctetString<MIN, MAX> {
    smpp_core::octets::OctetString::from_vec(value).expect("bounded by the strategy")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn bind_transceiver_round_trips(
        system_id in ascii(15),
        password in ascii(8),
        system_type in ascii(12),
        sequence_number in 1u32..=0x7FFF_FFFF,
    ) {
        let pdu = smpp_core::pdus::BindTransceiver::builder()
            .system_id(c_octet(system_id))
            .password(c_octet(password))
            .system_type(c_octet(system_type))
            .build();

        let command = Command::new(CommandStatus::EsmeRok, sequence_number, pdu);
        let bytes = codec::encode(&command).expect("encoding");

        prop_assert_eq!(codec::decode(&bytes).expect("decoding"), command);
    }

    #[test]
    fn submit_sm_round_trips(
        source in ascii(20),
        destination in ascii(20),
        message in bytes(255),
        with_tlv in any::<bool>(),
        sequence_number in 1u32..=0x7FFF_FFFF,
    ) {
        let mut builder = smpp_core::pdus::SubmitSm::builder()
            .source_addr(c_octet(source))
            .destination_addr(c_octet(destination))
            .short_message(octet(message));

        if with_tlv {
            builder = builder.push_tlv(smpp_core::tlvs::MessageSubmissionRequestTlvValue::UserMessageReference(
                smpp_core::values::UserMessageReference::new(0x1234),
            ));
        }

        let command = Command::new(CommandStatus::EsmeRok, sequence_number, builder.build());
        let bytes = codec::encode(&command).expect("encoding");

        prop_assert_eq!(codec::decode(&bytes).expect("decoding"), command);
    }

    #[test]
    fn submit_sm_resp_round_trips(
        message_id in ascii(64),
        sequence_number in 1u32..=0x7FFF_FFFF,
    ) {
        let pdu = smpp_core::pdus::SubmitSmResp::builder()
            .message_id(c_octet(message_id))
            .build();

        let command = Command::new(CommandStatus::EsmeRok, sequence_number, pdu);
        let bytes = codec::encode(&command).expect("encoding");

        prop_assert_eq!(codec::decode(&bytes).expect("decoding"), command);
    }

    #[test]
    fn deliver_sm_round_trips(
        source in ascii(20),
        destination in ascii(20),
        message in bytes(255),
        sequence_number in 1u32..=0x7FFF_FFFF,
    ) {
        let pdu = smpp_core::pdus::DeliverSm::builder()
            .source_addr(c_octet(source))
            .destination_addr(c_octet(destination))
            .short_message(octet(message))
            .build();

        let command = Command::new(CommandStatus::EsmeRok, sequence_number, pdu);
        let bytes = codec::encode(&command).expect("encoding");

        prop_assert_eq!(codec::decode(&bytes).expect("decoding"), command);
    }

    #[test]
    fn enquire_link_round_trips(sequence_number in 1u32..=0x7FFF_FFFF) {
        let command = Command::new(CommandStatus::EsmeRok, sequence_number, Pdu::EnquireLink);
        let bytes = codec::encode(&command).expect("encoding");

        prop_assert_eq!(codec::decode(&bytes).expect("decoding"), command);
    }

    /// Spec §7.1: `command_length` is the total length of the PDU. An encoder
    /// that lied about it would produce a stream nothing could resynchronise.
    #[test]
    fn command_length_always_matches_the_encoded_size(
        message in bytes(255),
        sequence_number in 1u32..=0x7FFF_FFFF,
    ) {
        let pdu = smpp_core::pdus::SubmitSm::builder()
            .short_message(octet(message))
            .build();

        let bytes = codec::encode(&Command::new(CommandStatus::EsmeRok, sequence_number, pdu))
            .expect("encoding");

        let announced = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        prop_assert_eq!(usize::try_from(announced).expect("fits"), bytes.len());
    }

    /// Every status must survive the header, including the vendor range the
    /// table does not describe.
    #[test]
    fn any_command_status_round_trips(raw_status in any::<u32>()) {
        let status = CommandStatus::from(raw_status);
        let command = Command::new(status, 1, Pdu::EnquireLinkResp);
        let bytes = codec::encode(&command).expect("encoding");

        prop_assert_eq!(codec::decode(&bytes).expect("decoding").status(), status);
    }

    /// Decoding must never panic, whatever the input.
    #[test]
    fn arbitrary_bytes_never_panic(raw in bytes(512)) {
        let _ = codec::decode(&raw);
    }

    /// A sequence number valid per spec §7.1 survives the header untouched.
    #[test]
    fn sequence_numbers_survive_the_header(raw in 1u32..=0x7FFF_FFFF) {
        let sequence_number = SequenceNumber::new(raw).expect("in range");
        let command = Command::new(CommandStatus::EsmeRok, sequence_number.get(), Pdu::EnquireLink);
        let bytes = codec::encode(&command).expect("encoding");

        prop_assert_eq!(
            codec::decode(&bytes).expect("decoding").sequence_number(),
            sequence_number.get()
        );
    }
}
