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
        Encoding, EncodingChoice, EncodingError, Gsm7BitCharset, Gsm7BitPacking,
    },
    segmentation::{
        reassemble, segment, ConcatenationReference, SegmentationMode, SegmentationOptions,
        SegmentedMessage,
    },
};

/// A fixed reference: nothing in this milestone depends on its value, and a
/// varying one would make a failure harder to read.
const REFERENCE: ConcatenationReference = ConcatenationReference::new(0xAB_CD);

fn options(mode: SegmentationMode) -> SegmentationOptions {
    SegmentationOptions::default().with_mode(mode)
}

fn split(text: &str, mode: SegmentationMode) -> SegmentedMessage {
    segment(text, &options(mode), REFERENCE).expect("automatic encoding")
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
    // Unpacked: one octet per septet, and `sm_length` allows 254.
    assert_eq!(message.segments()[0].short_message().unwrap().len(), 160);
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
        &SegmentationOptions::default().with_encoding(EncodingChoice::Forced(Encoding::Gsm7Bit)),
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
        &SegmentationOptions::default().with_encoding(EncodingChoice::Forced(Encoding::Ucs2)),
        REFERENCE,
    )
    .expect("UCS2 represents everything");

    assert_eq!(message.encoding(), Encoding::Ucs2);
    assert_eq!(units_per_segment(&message), vec![67, 33]);

    let message = segment(
        "caf\u{00E9} \u{00E7}a va",
        &SegmentationOptions::default().with_encoding(EncodingChoice::Forced(Encoding::Latin1)),
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
            &SegmentationOptions::default().with_encoding(EncodingChoice::Forced(Encoding::Latin1)),
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
        &SegmentationOptions::default(),
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

/// The six octets of header come out of the body. Unpacked, the rest is one
/// octet per septet, so a full concatenated segment is 6 + 153 octets.
#[test]
fn ca_004_06_a_udh_segment_body_is_six_octets_of_header_then_the_septets() {
    let message = split(&"a".repeat(161), SegmentationMode::Udh);
    let segment = &message.segments()[0];

    assert_eq!(segment.header_octets(), 6);
    assert_eq!(segment.content_units(), 153);
    assert_eq!(segment.short_message().unwrap().len(), 6 + 153);
}

/// Packed, the same segment is 6 octets of header and 134 of body — the
/// figure the specification's 140 octets comes from. The septets start one
/// bit late, because six octets of header are not a whole number of septets.
#[test]
fn ca_004_06_a_packed_udh_segment_body_is_six_octets_of_header_and_one_hundred_and_thirty_four() {
    let message = segment(
        &"a".repeat(161),
        &SegmentationOptions::default().with_gsm_packing(Gsm7BitPacking::Packed),
        REFERENCE,
    )
    .expect("plain ASCII");
    let first = &message.segments()[0];

    assert_eq!(first.header_octets(), 6);
    assert_eq!(first.content_units(), 153);
    assert_eq!(first.short_message().unwrap().len(), 6 + 134);
    assert_eq!(reassemble(message.segments()), Ok("a".repeat(161)));
}

// ---------------------------------------------------------------------------
// CA-004-07 — sar_* mode
// ---------------------------------------------------------------------------

#[test]
fn ca_004_07_every_sar_segment_carries_the_three_tlvs_and_no_udhi_bit() {
    let text = "a".repeat(400);
    let message = segment(
        &text,
        &options(SegmentationMode::Sar),
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

    let sar = segment(&text, &options(SegmentationMode::Sar), reference).unwrap();
    assert_eq!(sar.segments()[0].sar().unwrap().msg_ref_num, 0x12_34);

    let udh = segment(&text, &options(SegmentationMode::Udh), reference).unwrap();
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
        segment(&text, &options(SegmentationMode::MessagePayload), REFERENCE),
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
            let message = segment(
                text,
                &SegmentationOptions::default()
                    .with_mode(mode)
                    .with_encoding(*choice),
                REFERENCE,
            )
            .expect("representable");

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
            let preview = preview(text, &options(mode)).unwrap();
            let message = segment(text, &options(mode), REFERENCE).unwrap();

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

// ---------------------------------------------------------------------------
// Packed GSM 7-bit — the optional legacy layout
// ---------------------------------------------------------------------------

fn packed(mode: SegmentationMode) -> SegmentationOptions {
    options(mode).with_gsm_packing(Gsm7BitPacking::Packed)
}

/// Septets a receiver holding only `sm_length` recovers from a segment.
///
/// The oracle. It reads the **octets**, never `content_units`, because a
/// receiver has no access to what the encoder happened to know. `reassemble`
/// does the same thing internally; this function states the count on its own
/// so that a failure names the wrong number instead of only showing two
/// strings that differ.
fn septets_a_receiver_reads(segment: &messaging::segmentation::Segment) -> usize {
    let body = segment.short_message().expect("short_message mode");
    let user_data = body.len() - segment.header_octets();

    match segment.gsm_packing() {
        Gsm7BitPacking::Unpacked => user_data,
        // Six octets of concatenation UDH are 48 bits, so the septets start
        // one bit late; without a header they start on the first bit.
        Gsm7BitPacking::Packed => {
            let fill_bits = usize::from(segment.header_octets() > 0);

            (user_data * 8 - fill_bits) / 7
        }
    }
}

/// The reported bug, reproduced and pinned.
///
/// `"a"×152 + "€"` is the text the extension-pair rule brings to exactly 152
/// septets — which, packed behind a six-octet UDH, occupies the same 134
/// octets as 153 would. A receiver dividing octets therefore reads 153 and
/// finds a carriage return nobody typed, in the middle of the message.
///
/// The segmenter now closes that segment on 151 septets instead.
#[test]
fn a_packed_non_final_segment_is_never_over_read_by_one_septet() {
    let text = format!("{}€{}", "a".repeat(152), "b".repeat(20));
    let message = segment(&text, &packed(SegmentationMode::Udh), REFERENCE).expect("plain GSM");

    let first = &message.segments()[0];

    // 151, not 152: the last `a` was handed to the next segment.
    assert_eq!(first.content_units(), 151);
    assert_eq!(first.short_message().unwrap().len(), 6 + 133);
    assert_eq!(septets_a_receiver_reads(first), 151);
    assert_eq!(reassemble(message.segments()), Ok(text));
}

/// The same text unpacked, where the count cannot be wrong: one octet per
/// septet, so the segmenter keeps the greedy 152.
#[test]
fn the_unpacked_layout_keeps_the_greedy_count() {
    let text = format!("{}€{}", "a".repeat(152), "b".repeat(20));
    let message = split(&text, SegmentationMode::Udh);

    let first = &message.segments()[0];

    assert_eq!(first.content_units(), 152);
    assert_eq!(septets_a_receiver_reads(first), 152);
    assert_eq!(reassemble(message.segments()), Ok(text));
}

/// Every prefix around the boundary, in both layouts and both concatenation
/// modes: no **non-final** segment may ever be over-read, and the text must
/// come back exactly.
#[test]
fn no_packed_non_final_segment_is_ever_over_read() {
    for mode in [SegmentationMode::Udh, SegmentationMode::Sar] {
        for gsm_packing in [Gsm7BitPacking::Unpacked, Gsm7BitPacking::Packed] {
            let options = options(mode).with_gsm_packing(gsm_packing);

            for prefix in 0..400_usize {
                let text = format!("{}€{}", "a".repeat(prefix), "b".repeat(20));
                let message = segment(&text, &options, REFERENCE).expect("plain GSM");
                let segments = message.segments();

                for part in &segments[..segments.len() - 1] {
                    assert_eq!(
                        septets_a_receiver_reads(part),
                        part.content_units(),
                        "prefix {prefix}, {mode:?}, {gsm_packing:?}, segment {}",
                        part.sequence_number()
                    );
                }

                assert_eq!(
                    reassemble(segments),
                    Ok(text),
                    "prefix {prefix}, {mode:?}, {gsm_packing:?}"
                );
            }
        }
    }
}

/// The last segment is the one case that cannot be repaired — there is no
/// later segment to push a character into. TS 23.038 §6.1.2.3.1 covers it by
/// prescribing `CR` as the pad value, and the reassembler drops it again.
///
/// `"a"×161` splits into 153 + 8, and 8 septets behind a one-bit fill leave
/// exactly seven spare bits.
#[test]
fn a_packed_last_segment_may_be_padded_and_the_padding_is_dropped_again() {
    let text = "a".repeat(161);
    let message = segment(&text, &packed(SegmentationMode::Udh), REFERENCE).expect("plain GSM");

    let last = &message.segments()[1];

    assert_eq!(last.content_units(), 8);
    // The receiver reads one septet more than was written…
    assert_eq!(septets_a_receiver_reads(last), 9);
    // …and it is the padding, which does not reach the text.
    assert_eq!(reassemble(message.segments()), Ok(text));
}

/// The documented residual ambiguity of the packed layout, pinned so that it
/// is a known limit and not a surprise: a genuine trailing carriage return at
/// the padding alignment is indistinguishable from padding and is lost.
/// Unpacked, the same text round-trips exactly.
#[test]
fn a_packed_message_can_lose_a_genuine_trailing_carriage_return() {
    let text = format!("{}\r", "a".repeat(7));

    let packed_message =
        segment(&text, &packed(SegmentationMode::Udh), REFERENCE).expect("plain GSM");
    assert_eq!(reassemble(packed_message.segments()), Ok("a".repeat(7)));

    let unpacked_message = split(&text, SegmentationMode::Udh);
    assert_eq!(reassemble(unpacked_message.segments()), Ok(text));
}

/// A packed body read as unpacked would be silent gibberish. The high bit of
/// a septet is always clear, so the reassembler can refuse instead.
#[test]
fn a_packed_body_is_not_silently_read_as_unpacked() {
    let text = "hellohello";
    let message = segment(text, &packed(SegmentationMode::Udh), REFERENCE).expect("plain GSM");

    // 0xE8 is the first octet of the packed form: not a septet.
    assert_eq!(message.segments()[0].short_message().unwrap()[0], 0xE8);
    assert_eq!(reassemble(message.segments()), Ok(text.to_owned()));
}

/// CA-004-01 is stated in septets, and septets do not change with the layout.
/// Only the octet count does.
#[test]
fn the_segment_budget_is_the_same_in_both_layouts() {
    for gsm_packing in [Gsm7BitPacking::Unpacked, Gsm7BitPacking::Packed] {
        let options = options(SegmentationMode::Udh).with_gsm_packing(gsm_packing);

        let single = segment(&"a".repeat(160), &options, REFERENCE).unwrap();
        assert_eq!(units_per_segment(&single), vec![160], "{gsm_packing:?}");

        let split = segment(&"a".repeat(161), &options, REFERENCE).unwrap();
        assert_eq!(units_per_segment(&split), vec![153, 8], "{gsm_packing:?}");
    }
}

// ---------------------------------------------------------------------------
// The alt-charset — ADR 0009, the debt milestone 004 left open
// ---------------------------------------------------------------------------

fn alt_charset(mode: SegmentationMode) -> SegmentationOptions {
    options(mode).with_gsm_charset(Gsm7BitCharset::Latin1)
}

/// The whole difference, end to end. Written on `@ £ $` and an accented
/// letter on purpose: those are the only characters the two readings disagree
/// on, and a test written on ASCII passes under both while proving nothing.
#[test]
fn a_segment_carries_gsm_positions_or_latin1_code_points_depending_on_the_session() {
    let text = "@£$é";

    let gsm = segment(text, &options(SegmentationMode::Udh), REFERENCE).unwrap();
    assert_eq!(
        gsm.segments()[0].short_message().unwrap(),
        &[0x00, 0x01, 0x02, 0x05]
    );

    let alt = segment(text, &alt_charset(SegmentationMode::Udh), REFERENCE).unwrap();
    assert_eq!(
        alt.segments()[0].short_message().unwrap(),
        &[0x40, 0xA3, 0x24, 0xE9]
    );

    // Both are GSM 7-bit as far as the PDU is concerned: `data_coding` is 0x00
    // either way, which is exactly why nothing on the wire reveals a mistake.
    assert_eq!(gsm.encoding(), Encoding::Gsm7Bit);
    assert_eq!(alt.encoding(), Encoding::Gsm7Bit);
    assert_eq!(
        gsm.segments()[0].data_coding(),
        alt.segments()[0].data_coding()
    );
}

#[test]
fn an_ascii_message_is_byte_identical_under_both_charsets() {
    let text = "Your code is 4821";

    let gsm = segment(text, &options(SegmentationMode::Udh), REFERENCE).unwrap();
    let alt = segment(text, &alt_charset(SegmentationMode::Udh), REFERENCE).unwrap();

    assert_eq!(
        gsm.segments()[0].short_message(),
        alt.segments()[0].short_message(),
        "ASCII is the blind spot: this equality is the trap, stated"
    );
}

/// Each segment remembers the reading it was written under, so reassembly
/// cannot silently apply the other one.
#[test]
fn an_alt_charset_message_reassembles_into_the_text_that_produced_it() {
    let text = "Cafe a 3£ ".repeat(30);

    let message = segment(&text, &alt_charset(SegmentationMode::Udh), REFERENCE).unwrap();

    assert!(message.segments().len() > 1);
    assert_eq!(reassemble(message.segments()), Ok(text));
}

/// `€` has no ISO-8859-1 code point. Under the alt-charset the automatic
/// choice widens to UCS2 rather than writing an octet the message centre would
/// transcode into something else.
#[test]
fn the_euro_sign_widens_an_alt_charset_message_to_ucs2() {
    let text = "Total : 10€";

    assert_eq!(
        segment(text, &options(SegmentationMode::Udh), REFERENCE)
            .unwrap()
            .encoding(),
        Encoding::Gsm7Bit
    );
    assert_eq!(
        segment(text, &alt_charset(SegmentationMode::Udh), REFERENCE)
            .unwrap()
            .encoding(),
        Encoding::Ucs2
    );
}

/// Forcing GSM 7-bit on a text the alt-charset cannot write is an error, not a
/// silent widening — the rule CA-004-04 states for the other encodings.
#[test]
fn a_forced_gsm_encoding_is_refused_when_the_alt_charset_cannot_write_the_text() {
    let options =
        alt_charset(SegmentationMode::Udh).with_encoding(EncodingChoice::Forced(Encoding::Gsm7Bit));

    assert_eq!(
        segment("10€", &options, REFERENCE),
        Err(EncodingError::UnrepresentableCharacter {
            character: '€',
            index: 2,
            encoding: Encoding::Gsm7Bit,
        })
    );
}

/// ADR 0009 §7, enforced where the octets are actually written and not only
/// on the session profile.
///
/// Latin-1 octets use all eight bits — `é` is `0xE9` — and packing masks the
/// top one off every single one of them. The profile refused the pair; a
/// `SegmentationOptions` built by hand did not, and the corruption is silent
/// all the way to the handset.
#[test]
fn the_alt_charset_cannot_be_combined_with_septet_packing() {
    let impossible = options(SegmentationMode::Udh)
        .with_gsm_charset(Gsm7BitCharset::Latin1)
        .with_gsm_packing(Gsm7BitPacking::Packed);

    assert_eq!(
        segment("Café", &impossible, REFERENCE),
        Err(EncodingError::IncompatibleGsm7Layout {
            charset: "latin1",
            packing: "packed",
        })
    );

    // The live counter goes through the same check, so the editor cannot show
    // a length for a message that will never be sent.
    assert!(preview("Café", &impossible).is_err());

    // The three combinations that do hold are untouched.
    for (charset, packing) in [
        (Gsm7BitCharset::Latin1, Gsm7BitPacking::Unpacked),
        (Gsm7BitCharset::Gsm0338, Gsm7BitPacking::Packed),
        (Gsm7BitCharset::Gsm0338, Gsm7BitPacking::Unpacked),
    ] {
        let allowed = options(SegmentationMode::Udh)
            .with_gsm_charset(charset)
            .with_gsm_packing(packing);

        assert!(
            segment("Cafe", &allowed, REFERENCE).is_ok(),
            "{charset:?} with {packing:?} is legitimate"
        );
    }
}

/// The segment budget is stated in septets and does not move: the alt-charset
/// changes what the octets mean, not how much fits.
#[test]
fn the_segment_budget_is_the_same_under_both_charsets() {
    for charset in [Gsm7BitCharset::Gsm0338, Gsm7BitCharset::Latin1] {
        let options = options(SegmentationMode::Udh).with_gsm_charset(charset);

        let single = segment(&"a".repeat(160), &options, REFERENCE).unwrap();
        assert_eq!(units_per_segment(&single), vec![160], "{charset:?}");

        let split = segment(&"a".repeat(161), &options, REFERENCE).unwrap();
        assert_eq!(units_per_segment(&split), vec![153, 8], "{charset:?}");
    }
}
