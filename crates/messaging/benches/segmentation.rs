//! Reference micro-benchmark for the encoding and segmentation hot path.
//!
//! CA-004-10. Its job is not to be fast today but to be *comparable* at
//! milestone 017: the numbers it prints are the baseline any later change to
//! this crate is measured against. Run it with `cargo bench -p messaging`.
//!
//! # What the numbers mean
//!
//! The allocation budget the criterion states is one buffer per segment plus
//! one for the whole encoded text, and nothing per character:
//!
//! * `preview/*` allocates **nothing at all** — it walks the characters twice
//!   and returns five numbers. This is the case that runs on every keystroke,
//!   so it is the one that matters for the interface;
//! * `segment/*` allocates the encoded text once, a small vector of cut
//!   offsets, and one exactly-sized body per segment. It never re-encodes a
//!   slice, and it never grows a body by reallocation.
//!
//! A regression in the ratio between `preview` and `segment` on the same text
//! is the signal to look for: it means work moved from the planner into the
//! per-segment loop.

// `criterion_group!` expands to an undocumented public function, and an
// attribute cannot be attached to a macro expansion, so the allow has to sit
// at file scope. Everything written by hand below carries its own `///`.
#![allow(missing_docs)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use messaging::{
    encoding::{preview::preview, Encoding, EncodingChoice},
    segmentation::{reassemble, segment, ConcatenationReference, SegmentationMode},
};

const REFERENCE: ConcatenationReference = ConcatenationReference::new(0x1234);

/// The bodies the benchmark measures, from a single segment to a bulk-sized
/// message.
///
/// `gsm-extended` is the interesting one: its escape pairs make every segment
/// boundary land somewhere the plain cases never reach.
fn corpus() -> Vec<(String, String)> {
    vec![
        ("gsm-160".to_owned(), "a".repeat(160)),
        ("gsm-1600".to_owned(), "a".repeat(1_600)),
        (
            "gsm-extended-1600".to_owned(),
            "prix {10}€ [TTC] ~ remise | ".repeat(60),
        ),
        ("ucs2-70".to_owned(), "你".repeat(70)),
        ("ucs2-700".to_owned(), "你".repeat(700)),
        ("ucs2-emoji-700".to_owned(), "\u{1F600}bonjour ".repeat(70)),
    ]
}

/// Characters in `text`, for the throughput figure.
fn characters(text: &str) -> u64 {
    u64::try_from(text.chars().count()).unwrap_or(u64::MAX)
}

/// The live counter: what the message editor calls on every keystroke.
fn bench_preview(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("preview");

    for (name, text) in corpus() {
        group.throughput(Throughput::Elements(characters(&text)));
        group.bench_with_input(
            BenchmarkId::from_parameter(&name),
            &text,
            |bencher, text| {
                bencher.iter(|| {
                    preview(
                        black_box(text),
                        EncodingChoice::Automatic,
                        SegmentationMode::Udh,
                    )
                });
            },
        );
    }

    group.finish();
}

/// The full segmentation, for each of the three modes.
fn bench_segment(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("segment");

    for mode in [
        SegmentationMode::Udh,
        SegmentationMode::Sar,
        SegmentationMode::MessagePayload,
    ] {
        for (name, text) in corpus() {
            let id = BenchmarkId::new(format!("{mode:?}"), &name);

            group.throughput(Throughput::Elements(characters(&text)));
            group.bench_with_input(id, &text, |bencher, text| {
                bencher
                    .iter(|| segment(black_box(text), EncodingChoice::Automatic, mode, REFERENCE));
            });
        }
    }

    group.finish();
}

/// Reassembly, which milestone 012 will run on every long incoming message.
fn bench_reassemble(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("reassemble");

    for (name, text) in corpus() {
        let Ok(message) = segment(
            &text,
            EncodingChoice::Automatic,
            SegmentationMode::Udh,
            REFERENCE,
        ) else {
            continue;
        };

        group.throughput(Throughput::Elements(characters(&text)));
        group.bench_with_input(
            BenchmarkId::from_parameter(&name),
            message.segments(),
            |bencher, segments| {
                bencher.iter(|| reassemble(black_box(segments)));
            },
        );
    }

    group.finish();
}

/// Forcing UCS2 on a text GSM could have carried: the worst realistic case,
/// twice the octets for the same characters.
fn bench_forced_ucs2(criterion: &mut Criterion) {
    let text = "a".repeat(1_600);

    criterion.bench_function("segment/forced-ucs2-1600", |bencher| {
        bencher.iter(|| {
            segment(
                black_box(&text),
                EncodingChoice::Forced(Encoding::Ucs2),
                SegmentationMode::Udh,
                REFERENCE,
            )
        });
    });
}

criterion_group!(
    benches,
    bench_preview,
    bench_segment,
    bench_reassemble,
    bench_forced_ucs2
);
criterion_main!(benches);
