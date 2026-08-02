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

use messaging::template::{MissingVariablePolicy, Template, Variables};
use messaging::{
    encoding::{preview::preview, Encoding, EncodingChoice, Gsm7BitPacking},
    segmentation::{
        reassemble, segment, ConcatenationReference, Segment, SegmentationMode, SegmentationOptions,
    },
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

/// The ten characters that cost two septets rather than one.
const GSM_EXTENSION_CHARACTERS: &[char] =
    &['\u{000C}', '^', '{', '}', '\\', '[', '~', ']', '|', '€'];

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

/// Texts Latin-1 can write. CA-004-08 names three encodings, and this is the
/// one the other two strategies never reach: `€` is GSM-only, `ç` is
/// Latin-1-only, and neither generator produces a text that is exactly the
/// intersection.
fn latin1_text(max_characters: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(0..=0xFF_u32, 0..=max_characters)
        .prop_map(|code_points| code_points.into_iter().filter_map(char::from_u32).collect())
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

/// Both GSM layouts. The packed one moves where the cuts fall, so every
/// property has to hold under it too.
fn any_packing() -> impl Strategy<Value = Gsm7BitPacking> {
    prop::sample::select(&[Gsm7BitPacking::Unpacked, Gsm7BitPacking::Packed][..])
}

fn options(mode: SegmentationMode) -> SegmentationOptions {
    SegmentationOptions::default().with_mode(mode)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// `decode(encode(text)) == text` over the GSM alphabet, escapes included.
    ///
    /// Bounded to a single segment so this really is the encoder round trip
    /// and not the concatenation one, which the next property covers.
    #[test]
    fn a_short_gsm_text_survives_encoding_and_decoding(
        text in gsm_text(70),
        gsm_packing in any_packing(),
    ) {
        // Same documented limit of the packed layout as below: a genuine
        // trailing carriage return at the padding alignment is
        // indistinguishable from padding.
        prop_assume!(gsm_packing == Gsm7BitPacking::Unpacked || !text.ends_with('\r'));

        let options = SegmentationOptions::default().with_gsm_packing(gsm_packing);
        let message = segment(&text, &options, REFERENCE).unwrap();

        prop_assert_eq!(message.encoding(), Encoding::Gsm7Bit);

        // The septet count is what the budget is expressed in, and it is not
        // the character count — every extension character costs two. Stating
        // it independently is what makes this more than a tautology.
        let expected_septets: usize = text
            .chars()
            .map(|character| usize::from(GSM_EXTENSION_CHARACTERS.contains(&character)) + 1)
            .sum();

        prop_assert_eq!(message.segments()[0].content_units(), expected_septets);
        prop_assert_eq!(reassemble(message.segments()), Ok(text));
    }

    /// The Latin-1 round trip, which neither of the other two generators
    /// reaches (CA-004-08 names three encodings).
    #[test]
    fn a_latin1_text_survives_encoding_and_decoding(
        text in latin1_text(400),
        mode in any_mode(),
    ) {
        let options = options(mode).with_encoding(EncodingChoice::Forced(Encoding::Latin1));
        let message = segment(&text, &options, REFERENCE).unwrap();

        prop_assert_eq!(message.encoding(), Encoding::Latin1);
        // Latin-1 has no escape and no surrogate: one octet per character.
        prop_assert_eq!(
            message.segments().iter().map(Segment::content_units).sum::<usize>(),
            text.chars().count()
        );
        prop_assert_eq!(reassemble(message.segments()), Ok(text));
    }

    /// The same, for UCS2: a text with at least one foreign character.
    #[test]
    fn a_short_ucs2_text_survives_encoding_and_decoding(text in any_text(30)) {
        let options = SegmentationOptions::default()
            .with_encoding(EncodingChoice::Forced(Encoding::Ucs2));
        let message = segment(&text, &options, REFERENCE).unwrap();

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
        let message = segment(&text, &options(mode), REFERENCE).unwrap();

        prop_assert_eq!(reassemble(message.segments()), Ok(text));
    }

    /// The same over GSM texts, where the extension escapes make the cuts
    /// irregular — the case a uniform-width encoding never exercises.
    #[test]
    fn concatenating_the_segments_restores_a_gsm_message(
        text in gsm_text(900),
        mode in any_mode(),
        gsm_packing in any_packing(),
    ) {
        // The one documented limit of the packed layout: a genuine trailing
        // carriage return at the padding alignment is indistinguishable from
        // padding. Unpacked — the default — has no such case, and the
        // acceptance suite pins the packed one by example.
        prop_assume!(
            gsm_packing == Gsm7BitPacking::Unpacked || !text.ends_with('\r')
        );

        let options = options(mode).with_gsm_packing(gsm_packing);
        let message = segment(&text, &options, REFERENCE).unwrap();

        prop_assert_eq!(message.encoding(), Encoding::Gsm7Bit);
        prop_assert_eq!(reassemble(message.segments()), Ok(text));
    }

    /// The oracle of the packed layout, as a property: a receiver that only
    /// has `sm_length` recovers exactly what the encoder wrote — for every
    /// segment but the last, where the padding septet is allowed and dropped.
    #[test]
    fn a_receiver_counting_octets_recovers_every_non_final_packed_segment(
        text in gsm_text(900),
        mode in prop::sample::select(&[SegmentationMode::Udh, SegmentationMode::Sar][..]),
    ) {
        let options = options(mode).with_gsm_packing(Gsm7BitPacking::Packed);
        let message = segment(&text, &options, REFERENCE).unwrap();
        let segments = message.segments();

        for part in &segments[..segments.len() - 1] {
            let body = part.short_message().unwrap();
            let user_data = body.len() - part.header_octets();
            let fill_bits = usize::from(part.header_octets() > 0);

            prop_assert_eq!(
                (user_data * 8 - fill_bits) / 7,
                part.content_units(),
                "segment {} would be over-read",
                part.sequence_number()
            );
        }
    }

    /// CA-004-09: the counter the editor draws and the segments actually sent
    /// agree on the encoding, the number of parts and the units used.
    #[test]
    fn the_preview_agrees_with_the_segmentation(
        text in any_text(500),
        mode in any_mode(),
        gsm_packing in any_packing(),
    ) {
        let options = options(mode).with_gsm_packing(gsm_packing);
        let preview = preview(&text, &options).unwrap();
        let message = segment(&text, &options, REFERENCE).unwrap();

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
        let before = preview(&text, &SegmentationOptions::default())
            .unwrap()
            .segments();

        let mut longer = text;
        longer.push(appended);

        let after = preview(&longer, &SegmentationOptions::default())
            .unwrap()
            .segments();

        prop_assert!(after >= before, "{before} segments became {after}");
    }

    /// No segment ever holds more than its budget, and every segment but the
    /// last is filled to within one character of it — a segmenter that cut too
    /// early would still round-trip, and would still be wrong.
    #[test]
    fn no_segment_exceeds_its_budget(text in any_text(500), mode in any_mode()) {
        let message = segment(&text, &options(mode), REFERENCE).unwrap();
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
        let message = segment(&text, &options(mode), REFERENCE).unwrap();
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

// --- Milestone 008: the delivery-receipt parser -----------------------------

proptest! {
    /// step-008 §5 — **no delivery-receipt body, whatever it is, may panic.**
    ///
    /// The parser walks a body by byte offsets and slices it (`body[at..]`,
    /// `raw.get(0..2)`), which is where an off-by-one stops being a wrong
    /// answer and becomes a crash. The input is arbitrary because that is what
    /// the socket delivers: a message centre can send any octets it likes.
    ///
    /// # Why arbitrary **octets** and not an arbitrary `String`
    ///
    /// It is the faithful domain, not a convenience. `messaging::dlr` decodes a
    /// `deliver_sm` body as ISO-8859-1 — one octet, one code point — so every
    /// string the parser can ever see is built exactly the way this generator
    /// builds one. A `".{0,400}"` regex would instead draw from the whole
    /// Unicode range: a domain the parser never receives, and one proptest
    /// spends minutes sampling.
    ///
    /// The assertion is deliberately weak — *reaching* it is the property.
    /// What it must not become is a check on the parsed fields: an arbitrary
    /// body has no expected parse, and asserting one would only pin whatever
    /// the implementation happens to do.
    #[test]
    fn no_receipt_body_can_panic(octets in prop::collection::vec(any::<u8>(), 0..400)) {
        let body: String = octets.iter().map(|byte| char::from(*byte)).collect();
        let receipt = messaging::dlr::parse_receipt_body(&body);

        prop_assert_eq!(receipt.raw, body);
    }

    /// The same over arbitrary **text**, which is what a body containing a
    /// multi-byte character produces once the interface or an export has been
    /// through it. Kept short: this is about char boundaries, and a body of ten
    /// characters exercises every one of them.
    #[test]
    fn no_multi_byte_body_can_panic(body in "[\\x20-\\x7e\\u{00e9}\\u{20ac}\\u{4e2d}: ]{0,40}") {
        let receipt = messaging::dlr::parse_receipt_body(&body);

        prop_assert_eq!(receipt.raw, body);
    }

    /// The same, over bodies shaped enough like receipts to reach every branch.
    /// `.{0,400}` almost never produces `stat:` or a ten-digit date, so on its
    /// own it exercises the scanner's skip path and little else.
    #[test]
    fn no_receipt_shaped_body_can_panic(
        identifier in "[a-zA-Z0-9:_-]{0,20}",
        date in "[0-9]{0,14}",
        status in "[A-Za-z]{0,12}",
        error in "[A-Za-z0-9_]{0,12}",
        text in "[^\n]{0,60}",
    ) {
        let body = format!(
            "id:{identifier} sub:001 dlvrd:001 submit date:{date} \
             done date:{date} stat:{status} err:{error} text:{text}"
        );

        let receipt = messaging::dlr::parse_receipt_body(&body);

        // The one field that is always determined: `text:` swallows the rest
        // of the body, so whatever was put there comes back.
        prop_assert_eq!(receipt.text.as_deref(), Some(text.trim_start()));
    }
}

// --- Milestone 010 — the template engine ------------------------------------

/// The whole of CA-010-06, as a property rather than as a list of examples.
///
/// The example-based tests in `messaging::template` pin the cases somebody
/// thought of. This one pins the invariant *between* them, over **every**
/// source: a rendered message never holds a `{{` with a `}}` after it. No
/// exception for the escape — a review found that the earlier form of this
/// test, which excused any source containing `{{{{`, excluded precisely the
/// family where the invariant fell over.
///
/// # Why the source is assembled from fragments rather than drawn from an
/// alphabet
///
/// A character-level generator over `[a-c{} ]` looks brace-heavy and is
/// useless: it almost never produces a *well-formed* placeholder naming a
/// variable the recipient has, so nearly every draw is a template that fails to
/// parse or holds no variable at all, and the substitution path — the one the
/// criterion is about — is never reached. Measured, not assumed: with that
/// generator, removing **both** defences of the module header left this test
/// green.
///
/// The fragments below are the pieces a real template is made of, malformed
/// ones included, so a draw of a few of them exercises escapes, nesting,
/// unterminated openings and values holding braces at the same time. Two of
/// them are multi-byte: every offset in the parser is a **byte** offset, and an
/// off-by-one on a `é` is a panic, which CLAUDE.md §4 forbids outright.
const FRAGMENTS: &[&str] = &[
    "{{a}}",
    "{{ b }}",
    "{{c}}",
    "{{a",
    "{{}}",
    "{{{{",
    "}}",
    "{",
    "}",
    " ",
    "x",
    "é",
    "🕴€中",
    "{{a{{b}}}}",
];

/// Whether a text reads as holding a placeholder.
///
/// Transcribed from the criterion, not from the engine: a `{{` with a `}}`
/// anywhere after it.
fn reads_as_a_placeholder(text: &str) -> bool {
    match text.find("{{") {
        Some(opening) => text[opening + 2..].contains("}}"),
        None => false,
    }
}

proptest! {
    #[test]
    fn no_rendered_message_ever_reads_as_holding_a_placeholder(
        pieces in prop::collection::vec(prop::sample::select(FRAGMENTS), 0..6),
        first in "[a-c{}é€ ]{0,6}",
        second in "[a-c{}é€ ]{0,6}",
        substitute in "[a-c{}é€ ]{0,6}",
    ) {
        let source: String = pieces.concat();
        let Ok(template) = Template::parse(&source) else {
            // A template that does not parse never reaches a recipient, which
            // is the first of the three mechanisms.
            return Ok(());
        };

        let variables = Variables::new().with("a", &first).with("b", &second);

        for policy in [
            MissingVariablePolicy::Reject,
            MissingVariablePolicy::Substitute(substitute.clone()),
        ] {
            let Ok(rendered) = template.render(&variables, &policy) else {
                continue;
            };

            prop_assert!(
                !reads_as_a_placeholder(&rendered),
                "{source:?} rendered as {rendered:?}"
            );
        }
    }
}
