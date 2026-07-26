//! Property tests for milestone 004 (fiche §5).
//!
//! Example-based tests pin the boundaries the specification names. These pin
//! the invariants that must hold *between* them — which is where a segmenter
//! actually breaks, because the interesting inputs are the ones nobody thought
//! to write down.

// `allow-unwrap-in-tests` and `allow-expect-in-tests` in clippy.toml only
// relax the lints under `#[cfg(test)]`. Files under `tests/` are separate
// crates compiled WITHOUT that cfg, so the relaxation does not reach them and
// the ban would apply here as if this were production code.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use messaging::{
    encoding::{preview::preview, Encoding, EncodingChoice},
    segmentation::{reassemble, segment, ConcatenationReference, Segment, SegmentationMode},
};
use proptest::prelude::*;

const REFERENCE: ConcatenationReference = ConcatenationReference::new(0x1234);

/// The GSM 03.38 alphabet, transcribed independently of the implementation.
///
/// Copying it out of `gsm0338.rs` would make the test agree with itself; this
/// list comes from the standard. The extension characters are at the end, and
/// they are the ones that cost two septets.
const GSM_ALPHABET: &[char] = &[
    '@', '£', '$', '¥', 'è', 'é', 'ù', 'ì', 'ò', 'Ç', '\n', 'Ø', 'ø', '\r', 'Å', 'å', 'Δ', '_',
    'Φ', 'Γ', 'Λ', 'Ω', 'Π', 'Ψ', 'Σ', 'Θ', 'Ξ', 'Æ', 'æ', 'ß', 'É', ' ', '!', '"', '#', '¤', '%',
    '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/', '0', '1', '2', '3', '4', '5', '6', '7', '8',
    '9', ':', ';', '<', '=', '>', '?', '¡', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K',
    'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'Ä', 'Ö', 'Ñ', 'Ü',
    '§', '¿', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q',
    'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'ä', 'ö', 'ñ', 'ü', 'à', '\u{000C}', '^', '{',
    '}', '\\', '[', '~', ']', '|', '€',
];

/// Texts written entirely in the GSM alphabet, extension characters included.
fn gsm_text(max_characters: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(GSM_ALPHABET), 0..=max_characters)
        .prop_map(|characters| characters.into_iter().collect())
}

/// Texts drawn from the whole of Unicode.
fn any_text(max_characters: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..=max_characters)
        .prop_map(|characters| characters.into_iter().collect())
}

/// The three modes, so every property is checked against all of them.
fn any_mode() -> impl Strategy<Value = SegmentationMode> {
    prop::sample::select(
        &[
            SegmentationMode::Udh,
            SegmentationMode::Sar,
            SegmentationMode::MessagePayload,
        ][..],
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// `decode(encode(text)) == text` over the GSM alphabet, escapes included.
    ///
    /// Bounded to a single segment so this really is the encoder round trip
    /// and not the concatenation one, which the next property covers.
    #[test]
    fn a_short_gsm_text_survives_encoding_and_decoding(text in gsm_text(70)) {
        let message = segment(&text, EncodingChoice::Automatic, SegmentationMode::Udh, REFERENCE)
            .unwrap();

        prop_assert_eq!(message.encoding(), Encoding::Gsm7Bit);
        prop_assert_eq!(message.segments().len(), 1);
        prop_assert_eq!(reassemble(message.segments()), Ok(text));
    }

    /// The same, for UCS2: a text with at least one foreign character.
    #[test]
    fn a_short_ucs2_text_survives_encoding_and_decoding(text in any_text(30)) {
        let message = segment(&text, EncodingChoice::Forced(Encoding::Ucs2), SegmentationMode::Udh, REFERENCE)
            .unwrap();

        prop_assert_eq!(message.encoding(), Encoding::Ucs2);
        prop_assert_eq!(reassemble(message.segments()), Ok(text));
    }

    /// Concatenating the segments restores the message, whatever its length
    /// and whichever mode carried it. This is the property the whole milestone
    /// exists to satisfy.
    #[test]
    fn concatenating_the_segments_restores_the_message(
        text in any_text(600),
        mode in any_mode(),
    ) {
        let message = segment(&text, EncodingChoice::Automatic, mode, REFERENCE).unwrap();

        prop_assert_eq!(reassemble(message.segments()), Ok(text));
    }

    /// The same over GSM texts, where the extension escapes make the cuts
    /// irregular — the case a uniform-width encoding never exercises.
    #[test]
    fn concatenating_the_segments_restores_a_gsm_message(
        text in gsm_text(900),
        mode in any_mode(),
    ) {
        let message = segment(&text, EncodingChoice::Automatic, mode, REFERENCE).unwrap();

        prop_assert_eq!(message.encoding(), Encoding::Gsm7Bit);
        prop_assert_eq!(reassemble(message.segments()), Ok(text));
    }

    /// CA-004-09: the counter the editor draws and the segments actually sent
    /// agree on the encoding, the number of parts and the units used.
    #[test]
    fn the_preview_agrees_with_the_segmentation(
        text in any_text(500),
        mode in any_mode(),
    ) {
        let preview = preview(&text, EncodingChoice::Automatic, mode).unwrap();
        let message = segment(&text, EncodingChoice::Automatic, mode, REFERENCE).unwrap();

        prop_assert_eq!(preview.encoding(), message.encoding());
        prop_assert_eq!(preview.segments(), message.segments().len());
        prop_assert_eq!(
            preview.units_used(),
            message.segments().iter().map(Segment::content_units).sum::<usize>()
        );
        prop_assert_eq!(preview.characters(), text.chars().count());
    }

    /// Fiche §5: the segment count never falls as the text grows.
    ///
    /// Not obvious, because appending one foreign character switches the whole
    /// message from GSM 7-bit to UCS2 and re-encodes everything. It still
    /// holds: a UCS2 segment carries 67 units where a GSM one carries at most
    /// 153 septets for the same characters.
    #[test]
    fn the_segment_count_never_decreases_as_the_text_grows(
        text in any_text(400),
        appended in any::<char>(),
    ) {
        let before = preview(&text, EncodingChoice::Automatic, SegmentationMode::Udh)
            .unwrap()
            .segments();

        let mut longer = text;
        longer.push(appended);

        let after = preview(&longer, EncodingChoice::Automatic, SegmentationMode::Udh)
            .unwrap()
            .segments();

        prop_assert!(after >= before, "{before} segments became {after}");
    }

    /// No segment ever holds more than its budget, and every segment but the
    /// last is filled to within one character of it — a segmenter that cut too
    /// early would still round-trip, and would still be wrong.
    #[test]
    fn no_segment_exceeds_its_budget(text in any_text(500), mode in any_mode()) {
        let message = segment(&text, EncodingChoice::Automatic, mode, REFERENCE).unwrap();
        let segments = message.segments();

        if mode == SegmentationMode::MessagePayload {
            prop_assert_eq!(segments.len(), 1);

            return Ok(());
        }

        let budget = message.encoding().budget().for_segment_count(segments.len());

        for segment in segments {
            prop_assert!(
                segment.content_units() <= budget,
                "segment {} holds {} units for a budget of {budget}",
                segment.sequence_number(),
                segment.content_units()
            );
        }

        // Every segment but the last is full to within one character. The
        // largest character costs two units, so at most one unit may be left.
        for segment in &segments[..segments.len().saturating_sub(1)] {
            prop_assert!(
                segment.content_units() + 1 >= budget,
                "segment {} was cut {} units early",
                segment.sequence_number(),
                budget - segment.content_units()
            );
        }
    }

    /// CA-004-06 and CA-004-07, as invariants rather than examples: the
    /// concatenation information is coherent across every segment of a split
    /// message, and absent from a message that did not split.
    #[test]
    fn the_concatenation_information_is_coherent(text in any_text(600), mode in any_mode()) {
        let message = segment(&text, EncodingChoice::Automatic, mode, REFERENCE).unwrap();
        let segments = message.segments();
        let total = u8::try_from(segments.len()).unwrap();
        let split = segments.len() > 1;

        for (index, segment) in segments.iter().enumerate() {
            let sequence_number = u8::try_from(index + 1).unwrap();

            prop_assert_eq!(segment.sequence_number(), sequence_number);
            prop_assert_eq!(segment.total_segments(), total);
            prop_assert_eq!(segment.encoding(), message.encoding());

            let udh = split && mode == SegmentationMode::Udh;
            let sar = split && mode == SegmentationMode::Sar;

            prop_assert_eq!(segment.header_octets(), usize::from(udh) * 6);
            prop_assert_eq!(u8::from(segment.esm_class()) & 0b0100_0000, u8::from(udh) << 6);
            prop_assert_eq!(segment.sar().is_some(), sar);

            if let Some(header) = segment.short_message().filter(|_| udh) {
                prop_assert_eq!(
                    &header[..6],
                    &[0x05, 0x00, 0x03, REFERENCE.as_u8(), total, sequence_number]
                );
            }

            if let Some(parameters) = segment.sar() {
                prop_assert_eq!(parameters.msg_ref_num, REFERENCE.as_u16());
                prop_assert_eq!(parameters.total_segments, total);
                prop_assert_eq!(parameters.segment_seqnum, sequence_number);
            }
        }
    }
}
