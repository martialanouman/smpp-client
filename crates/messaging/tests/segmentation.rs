//! Acceptance tests for milestone 004, one section per criterion.
//!
//! Integration tests, so they see exactly what milestone 006 will see: a type
//! that is not public does not compile here.

// `allow-unwrap-in-tests` and `allow-expect-in-tests` in clippy.toml only
// relax the lints under `#[cfg(test)]`. Files under `tests/` are separate
// crates compiled WITHOUT that cfg, so the relaxation does not reach them and
// the ban would apply here as if this were production code.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use messaging::{
    encoding::{
        preview::{preview, MAX_MESSAGE_PAYLOAD_OCTETS},
        Encoding, EncodingChoice, EncodingError,
    },
    segmentation::{
        reassemble, segment, ConcatenationReference, SegmentationMode, SegmentedMessage,
    },
};

/// A fixed reference: nothing in this milestone depends on its value, and a
/// varying one would make a failure harder to read.
const REFERENCE: ConcatenationReference = ConcatenationReference::new(0xAB_CD);

fn split(text: &str, mode: SegmentationMode) -> SegmentedMessage {
    segment(text, EncodingChoice::Automatic, mode, REFERENCE).expect("automatic encoding")
}

/// Units carried by each segment, in the order they will be sent.
fn units_per_segment(message: &SegmentedMessage) -> Vec<usize> {
    message
        .segments()
        .iter()
        .map(messaging::segmentation::Segment::content_units)
        .collect()
}

// ---------------------------------------------------------------------------
// CA-004-01 — the GSM boundary at 160, then 153 + 8
// ---------------------------------------------------------------------------

#[test]
fn ca_004_01_one_hundred_and_sixty_gsm_characters_are_one_segment() {
    let message = split(&"a".repeat(160), SegmentationMode::Udh);

    assert_eq!(message.encoding(), Encoding::Gsm7Bit);
    assert_eq!(units_per_segment(&message), vec![160]);
    // A lone segment carries no concatenation header at all.
    assert_eq!(message.segments()[0].header_octets(), 0);
    assert_eq!(message.segments()[0].short_message().unwrap().len(), 140);
    assert_eq!(message.reference(), None);
}

#[test]
fn ca_004_01_one_hundred_and_sixty_one_gsm_characters_are_one_hundred_and_fifty_three_plus_eight() {
    let message = split(&"a".repeat(161), SegmentationMode::Udh);

    assert_eq!(units_per_segment(&message), vec![153, 8]);
}

/// The exact boundaries the fiche §5 asks for, in one place.
#[test]
fn ca_004_01_the_gsm_boundaries_are_where_the_specification_puts_them() {
    for (characters, expected) in [
        (0_usize, vec![0_usize]),
        (1, vec![1]),
        (152, vec![152]),
        (153, vec![153]),
        (154, vec![154]),
        (159, vec![159]),
        (160, vec![160]),
        (161, vec![153, 8]),
        (306, vec![153, 153]),
        (307, vec![153, 153, 1]),
    ] {
        let message = split(&"a".repeat(characters), SegmentationMode::Udh);

        assert_eq!(
            units_per_segment(&message),
            expected,
            "{characters} GSM characters"
        );
    }
}

// ---------------------------------------------------------------------------
// CA-004-02 — an extension character costs two septets
// ---------------------------------------------------------------------------

#[test]
fn ca_004_02_the_euro_sign_costs_two_septets_in_the_segment_count() {
    // 159 plain characters plus one euro sign is 161 septets: two segments,
    // where 160 plain characters would still have been one.
    let text = format!("{}€", "a".repeat(159));
    let message = split(&text, SegmentationMode::Udh);

    assert_eq!(message.encoding(), Encoding::Gsm7Bit);
    assert_eq!(units_per_segment(&message), vec![153, 8]);
    assert_eq!(reassemble(message.segments()), Ok(text));
}

#[test]
fn ca_004_02_eighty_euro_signs_still_fit_in_one_segment() {
    let text = "€".repeat(80);
    let message = split(&text, SegmentationMode::Udh);

    assert_eq!(units_per_segment(&message), vec![160]);
    assert_eq!(reassemble(message.segments()), Ok(text));
}

/// **The trap of this milestone.** The 153rd septet of the first segment is
/// free and the next character needs two, so the escape pair moves whole to
/// the second segment and one septet is left unused.
///
/// Getting this wrong produces a first segment ending on a dangling escape and
/// a second starting with an orphan `0x65`: the handset shows one wrong
/// character and loses the euro sign. Nothing above the septet level notices.
#[test]
fn ca_004_02_an_extension_pair_is_never_split_across_a_segment_boundary() {
    // 152 plain characters, then a euro sign: the pair would straddle 153/154.
    let text = format!("{}€{}", "a".repeat(152), "b".repeat(20));
    let message = split(&text, SegmentationMode::Udh);

    // 152 septets in the first segment, not 153: the last one is unusable.
    assert_eq!(units_per_segment(&message), vec![152, 22]);
    assert_eq!(reassemble(message.segments()), Ok(text));
}

/// The same boundary, walked one character at a time. Every offset of the euro
/// sign around the cut has to reassemble exactly.
#[test]
fn ca_004_02_the_extension_pair_survives_every_offset_around_the_boundary() {
    for prefix in 145..=160_usize {
        let text = format!("{}€{}", "a".repeat(prefix), "b".repeat(30));
        let message = split(&text, SegmentationMode::Udh);

        let units = units_per_segment(&message);

        assert!(
            units.iter().all(|&count| count <= 153),
            "prefix {prefix} produced an oversized segment: {units:?}"
        );
        assert_eq!(
            reassemble(message.segments()),
            Ok(text),
            "prefix {prefix}, segments {units:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// CA-004-03 — automatic switch to UCS2, boundary at 70 then 67 + 4
// ---------------------------------------------------------------------------

#[test]
fn ca_004_03_a_character_outside_gsm_switches_the_whole_message_to_ucs2() {
    for foreign in ['你', 'ł'] {
        let message = split(&foreign.to_string(), SegmentationMode::Udh);

        assert_eq!(message.encoding(), Encoding::Ucs2, "{foreign}");
        assert_eq!(u8::from(message.data_coding()), 0x08);
    }
}

#[test]
fn ca_004_03_the_ucs2_boundaries_are_where_the_specification_puts_them() {
    for (characters, expected) in [
        (66_usize, vec![66_usize]),
        (67, vec![67]),
        (68, vec![68]),
        (69, vec![69]),
        (70, vec![70]),
        (71, vec![67, 4]),
    ] {
        let text = "你".repeat(characters);
        let message = split(&text, SegmentationMode::Udh);

        assert_eq!(message.encoding(), Encoding::Ucs2);
        assert_eq!(
            units_per_segment(&message),
            expected,
            "{characters} UCS2 characters"
        );
        assert_eq!(reassemble(message.segments()), Ok(text));
    }
}

// ---------------------------------------------------------------------------
// CA-004-04 — the manual override is honoured, and refuses rather than corrupts
// ---------------------------------------------------------------------------

#[test]
fn ca_004_04_forcing_gsm_on_an_unrepresentable_text_is_an_error() {
    let error = segment(
        "prix: 10 zł",
        EncodingChoice::Forced(Encoding::Gsm7Bit),
        SegmentationMode::Udh,
        REFERENCE,
    )
    .unwrap_err();

    assert_eq!(
        error,
        EncodingError::UnrepresentableCharacter {
            character: 'ł',
            index: 10,
            encoding: Encoding::Gsm7Bit,
        }
    );
    // The message names the character and the encoding: the interface can
    // point at the problem without guessing.
    assert!(error.to_string().contains("GSM 7-bit"));
}

#[test]
fn ca_004_04_forcing_an_encoding_overrides_detection() {
    // Plain ASCII would be detected as GSM 7-bit and fit in one segment.
    let message = segment(
        &"a".repeat(100),
        EncodingChoice::Forced(Encoding::Ucs2),
        SegmentationMode::Udh,
        REFERENCE,
    )
    .expect("UCS2 represents everything");

    assert_eq!(message.encoding(), Encoding::Ucs2);
    assert_eq!(units_per_segment(&message), vec![67, 33]);

    let message = segment(
        "caf\u{00E9} \u{00E7}a va",
        EncodingChoice::Forced(Encoding::Latin1),
        SegmentationMode::Udh,
        REFERENCE,
    )
    .expect("Latin-1 covers it");

    assert_eq!(message.encoding(), Encoding::Latin1);
    assert_eq!(u8::from(message.data_coding()), 0x03);
}

#[test]
fn ca_004_04_forcing_latin1_on_a_gsm_only_character_is_an_error() {
    // The euro sign is in the GSM extension table but not in Latin-1.
    assert!(matches!(
        segment(
            "10 €",
            EncodingChoice::Forced(Encoding::Latin1),
            SegmentationMode::Udh,
            REFERENCE
        ),
        Err(EncodingError::UnrepresentableCharacter {
            character: '€', ..
        })
    ));
}

// ---------------------------------------------------------------------------
// CA-004-05 — no character is ever cut in two
// ---------------------------------------------------------------------------

#[test]
fn ca_004_05_a_surrogate_pair_is_never_split_across_a_segment_boundary() {
    // 66 basic-plane characters then an emoji: the pair would straddle 67/68.
    let text = format!("{}\u{1F600}{}", "你".repeat(66), "好".repeat(10));
    let message = split(&text, SegmentationMode::Udh);

    assert_eq!(units_per_segment(&message), vec![66, 12]);
    assert_eq!(reassemble(message.segments()), Ok(text));
}

#[test]
fn ca_004_05_the_surrogate_pair_survives_every_offset_around_the_boundary() {
    for prefix in 60..=70_usize {
        let text = format!("{}\u{1F600}{}", "你".repeat(prefix), "好".repeat(10));
        let message = split(&text, SegmentationMode::Udh);

        assert_eq!(
            reassemble(message.segments()),
            Ok(text),
            "prefix {prefix}, units {:?}",
            units_per_segment(&message)
        );
    }
}

/// A message made only of two-unit characters, split at every length: neither
/// alphabet may ever leave half a pair in a segment.
#[test]
fn ca_004_05_a_message_of_only_two_unit_characters_never_splits_one() {
    for characters in 1..=90_usize {
        for text in ["€".repeat(characters), "\u{1F600}".repeat(characters)] {
            let message = split(&text, SegmentationMode::Udh);

            assert_eq!(
                reassemble(message.segments()),
                Ok(text.clone()),
                "{characters} characters of {:?}",
                text.chars().next()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CA-004-06 — UDH mode
// ---------------------------------------------------------------------------

#[test]
fn ca_004_06_every_udh_segment_carries_six_octets_a_shared_reference_and_its_index() {
    let text = "a".repeat(400);
    let message = segment(
        &text,
        EncodingChoice::Automatic,
        SegmentationMode::Udh,
        ConcatenationReference::new(0x00_2A),
    )
    .expect("plain ASCII");

    let segments = message.segments();
    assert_eq!(segments.len(), 3);

    for (index, segment) in segments.iter().enumerate() {
        let sequence_number = u8::try_from(index + 1).unwrap();
        let body = segment
            .short_message()
            .expect("UDH mode uses short_message");

        assert_eq!(segment.header_octets(), 6);
        assert_eq!(
            &body[..6],
            // UDH length 5, IEI 0x00, IE length 3, reference, total, index.
            &[0x05, 0x00, 0x03, 0x2A, 0x03, sequence_number]
        );
        assert_eq!(segment.sequence_number(), sequence_number);
        assert_eq!(segment.total_segments(), 3);

        // UDHI bit set, and no `sar_*` alongside it.
        assert_eq!(u8::from(segment.esm_class()) & 0b0100_0000, 0b0100_0000);
        assert_eq!(segment.sar(), None);
    }

    assert_eq!(reassemble(segments), Ok(text));
}

#[test]
fn ca_004_06_a_single_segment_carries_no_udh_and_no_udhi_bit() {
    let message = split("court", SegmentationMode::Udh);
    let segment = &message.segments()[0];

    assert_eq!(segment.header_octets(), 0);
    assert_eq!(u8::from(segment.esm_class()) & 0b0100_0000, 0);
    assert_eq!(segment.total_segments(), 1);
}

/// The six octets of header come out of the body, and the septets that follow
/// them start one bit late. If the fill bit were forgotten the body would
/// still be 134 octets and every character would be wrong.
#[test]
fn ca_004_06_a_udh_segment_body_is_six_octets_of_header_and_one_hundred_and_thirty_four_of_data() {
    let message = split(&"a".repeat(161), SegmentationMode::Udh);
    let body = message.segments()[0].short_message().unwrap();

    assert_eq!(body.len(), 140);
    assert_eq!(message.segments()[0].content_units(), 153);
}

// ---------------------------------------------------------------------------
// CA-004-07 — sar_* mode
// ---------------------------------------------------------------------------

#[test]
fn ca_004_07_every_sar_segment_carries_the_three_tlvs_and_no_udhi_bit() {
    let text = "a".repeat(400);
    let message = segment(
        &text,
        EncodingChoice::Automatic,
        SegmentationMode::Sar,
        ConcatenationReference::new(0xBE_EF),
    )
    .expect("plain ASCII");

    let segments = message.segments();
    assert_eq!(segments.len(), 3);

    for (index, segment) in segments.iter().enumerate() {
        let sequence_number = u8::try_from(index + 1).unwrap();
        let sar = segment.sar().expect("sar mode sets the TLVs");

        assert_eq!(sar.msg_ref_num, 0xBE_EF);
        assert_eq!(sar.total_segments, 3);
        assert_eq!(sar.segment_seqnum, sequence_number);

        // The body carries no header, and the UDHI bit stays clear.
        assert_eq!(segment.header_octets(), 0);
        assert_eq!(u8::from(segment.esm_class()) & 0b0100_0000, 0);
    }

    assert_eq!(reassemble(segments), Ok(text));
}

#[test]
fn ca_004_07_a_single_segment_carries_no_sar_tlvs() {
    let message = split("court", SegmentationMode::Sar);

    assert_eq!(message.segments()[0].sar(), None);
    assert_eq!(message.reference(), None);
}

/// The `sar_*` reference is the full 16 bits, where the UDH keeps the low
/// octet only.
#[test]
fn ca_004_07_the_sar_reference_is_sixteen_bits_wide() {
    let text = "a".repeat(400);
    let reference = ConcatenationReference::new(0x12_34);

    let sar = segment(
        &text,
        EncodingChoice::Automatic,
        SegmentationMode::Sar,
        reference,
    )
    .unwrap();
    assert_eq!(sar.segments()[0].sar().unwrap().msg_ref_num, 0x12_34);

    let udh = segment(
        &text,
        EncodingChoice::Automatic,
        SegmentationMode::Udh,
        reference,
    )
    .unwrap();
    assert_eq!(udh.segments()[0].short_message().unwrap()[3], 0x34);
}

// ---------------------------------------------------------------------------
// message_payload — the third mode of spec §7.5
// ---------------------------------------------------------------------------

#[test]
fn the_message_payload_mode_produces_one_segment_with_an_empty_short_message() {
    let text = "a".repeat(1_000);
    let message = split(&text, SegmentationMode::MessagePayload);

    let segment = &message.segments()[0];
    assert_eq!(message.segments().len(), 1);
    assert_eq!(segment.short_message(), None);
    assert!(segment.message_payload().is_some());
    assert_eq!(segment.sar(), None);
    assert_eq!(u8::from(segment.esm_class()) & 0b0100_0000, 0);
    assert_eq!(reassemble(message.segments()), Ok(text));
}

#[test]
fn a_message_payload_beyond_sixty_four_kibibytes_is_refused() {
    let text = "你".repeat(MAX_MESSAGE_PAYLOAD_OCTETS / 2 + 1);

    assert!(matches!(
        segment(
            &text,
            EncodingChoice::Automatic,
            SegmentationMode::MessagePayload,
            REFERENCE
        ),
        Err(EncodingError::PayloadTooLarge { .. })
    ));
}

// ---------------------------------------------------------------------------
// CA-004-08 — reverse concatenation, both modes, three encodings
// ---------------------------------------------------------------------------

#[test]
fn ca_004_08_reassembly_restores_the_text_for_every_mode_and_encoding() {
    let cases = [
        (
            EncodingChoice::Forced(Encoding::Gsm7Bit),
            "GSM {avec} des [extensions] et 10€ ".repeat(20),
        ),
        (
            EncodingChoice::Forced(Encoding::Latin1),
            "Latin-1 : caf\u{00E9}, na\u{00EF}ve, \u{00E7}a ira ".repeat(20),
        ),
        (
            EncodingChoice::Forced(Encoding::Ucs2),
            "UCS2 你好 \u{1F600} ".repeat(30),
        ),
        (EncodingChoice::Automatic, "Auto 你好 \u{1F600} ".repeat(30)),
        (
            EncodingChoice::Automatic,
            "Auto GSM {} [] ~ | \\ ^ 10€ ".repeat(20),
        ),
    ];

    for mode in [
        SegmentationMode::Udh,
        SegmentationMode::Sar,
        SegmentationMode::MessagePayload,
    ] {
        for (choice, text) in &cases {
            let message = segment(text, *choice, mode, REFERENCE).expect("representable");

            assert_eq!(
                reassemble(message.segments()),
                Ok(text.clone()),
                "{mode:?} / {choice:?}"
            );
        }
    }
}

#[test]
fn ca_004_08_reassembly_accepts_the_segments_in_any_order() {
    let text = "a".repeat(400);
    let message = split(&text, SegmentationMode::Udh);

    let mut shuffled = message.into_segments();
    shuffled.reverse();

    assert_eq!(reassemble(&shuffled), Ok(text));
}

#[test]
fn ca_004_08_reassembly_refuses_an_incomplete_set() {
    let message = split(&"a".repeat(400), SegmentationMode::Udh);
    let segments = message.segments();

    assert!(matches!(
        reassemble(&segments[..2]),
        Err(EncodingError::IncompleteConcatenation { .. })
    ));
    assert!(matches!(
        reassemble(&[]),
        Err(EncodingError::IncompleteConcatenation { .. })
    ));
}

#[test]
fn ca_004_08_an_empty_text_round_trips_as_one_empty_segment() {
    for mode in [
        SegmentationMode::Udh,
        SegmentationMode::Sar,
        SegmentationMode::MessagePayload,
    ] {
        let message = split("", mode);

        assert_eq!(message.segments().len(), 1);
        assert_eq!(reassemble(message.segments()), Ok(String::new()));
    }
}

/// A single extension character is the smallest message that exercises the
/// escape mechanism end to end (fiche §5).
#[test]
fn ca_004_08_a_single_extension_character_round_trips() {
    for character in ['^', '{', '}', '[', ']', '~', '\\', '|', '€'] {
        let text = character.to_string();
        let message = split(&text, SegmentationMode::Udh);

        assert_eq!(message.encoding(), Encoding::Gsm7Bit);
        assert_eq!(message.segments()[0].content_units(), 2);
        assert_eq!(reassemble(message.segments()), Ok(text));
    }
}

// ---------------------------------------------------------------------------
// CA-004-09 — the preview agrees with the segmentation
// ---------------------------------------------------------------------------

#[test]
fn ca_004_09_the_preview_matches_the_segmentation_on_the_documented_boundaries() {
    let texts = [
        String::new(),
        "a".repeat(159),
        "a".repeat(160),
        "a".repeat(161),
        "€".repeat(80),
        "€".repeat(81),
        format!("{}€", "a".repeat(152)),
        "你".repeat(70),
        "你".repeat(71),
        "\u{1F600}".repeat(40),
    ];

    for mode in [SegmentationMode::Udh, SegmentationMode::Sar] {
        for text in &texts {
            let preview = preview(text, EncodingChoice::Automatic, mode).unwrap();
            let message = segment(text, EncodingChoice::Automatic, mode, REFERENCE).unwrap();

            assert_eq!(preview.encoding(), message.encoding());
            assert_eq!(preview.segments(), message.segments().len());
            assert_eq!(
                preview.units_used(),
                message
                    .segments()
                    .iter()
                    .map(messaging::segmentation::Segment::content_units)
                    .sum::<usize>(),
                "text of {} characters",
                text.chars().count()
            );
        }
    }
}
