//! The enqueue latency of a unit send — CA-006-10, ENF-PERF-02.
//!
//! # What is measured, and what is deliberately not
//!
//! The criterion asks for the latency between "command called" and "message in
//! the queue" to stay under 1 ms at the 99th percentile. This benchmark
//! measures the whole path from a [`SendRequest`] to the PDUs handed to the
//! session port: validation, encoding, segmentation, and the construction of
//! one `submit_sm` per segment. That is the CPU work a send does before
//! anything leaves the process.
//!
//! Two things are **outside** the measurement, and saying so is more useful
//! than a number that hides them.
//!
//! * **The durable write.** The write-ahead insert is one `INSERT` and one
//!   `fsync` against SQLite in WAL mode, so its cost is the disk's, not this
//!   crate's, and mixing it in would turn a code regression into noise. It is
//!   measured where it belongs, by the volumetry benchmarks of `persistence`.
//! * **The queue itself.** There is no in-flight window at this milestone —
//!   the emission is sequential and unregulated (fiche §2, milestone 007 owns
//!   both). "In the queue" therefore means "built and about to be submitted",
//!   which is exactly where this stops.
//!
//! So the number below is a **lower bound** on the criterion, and an honest
//! one: it is the part of the latency this milestone controls.
//!
//! Run it with `cargo bench -p messaging --bench enqueue`.

// `criterion_group!` expands to an undocumented public function, and an
// attribute cannot be attached to a macro expansion, so the allow has to sit
// at file scope. Everything written by hand below carries its own `///`.
#![allow(missing_docs)]
// `benches/` is compiled without `cfg(test)`, so the relaxations of
// `clippy.toml` do not reach it. A benchmark whose fixture does not build has
// nothing to measure, and a panic there IS the failure report — the
// alternative is an error path that would quietly benchmark a `None`.
#![allow(clippy::expect_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use messaging::addressing::{Destination, SourceAddress};
use messaging::segmentation::{segment, ConcatenationReference, SegmentationOptions};
use messaging::submit::{build_submit_sm, CustomTlv, SubmitOptions};

const REFERENCE: ConcatenationReference = ConcatenationReference::new(0x1234);

/// The bodies measured, from one segment to a bulk-sized message.
const BODIES: [(&str, usize); 4] = [
    ("1-segment", 100),
    ("3-segments", 400),
    ("7-segments", 1_000),
    ("unicode-3-segments", 150),
];

/// The options an ordinary send carries: a sender ID, a recipient, no TLV.
fn ordinary_options() -> SubmitOptions {
    SubmitOptions::to(Destination::parse("+2250102030405").expect("the fixture is valid"))
        .with_source(SourceAddress::parse("ShinobiSMS").expect("the fixture is valid"))
}

/// The same, with three custom TLVs — what an operator with a vendor contract
/// sends on every message.
fn options_with_tlvs() -> SubmitOptions {
    ordinary_options().with_tlvs(vec![
        CustomTlv::new(0x1403, vec![0xDE, 0xAD, 0xBE, 0xEF]).expect("short enough"),
        CustomTlv::new(0x020C, vec![0x00, 0x01]).expect("short enough"),
        CustomTlv::new(0x0424, b"vendor".to_vec()).expect("short enough"),
    ])
}

/// The body of `size` characters for `name`.
fn body(name: &str, size: usize) -> String {
    if name.starts_with("unicode") {
        // UCS2, so the budget per segment halves and the encoder walks code
        // units rather than septets.
        "é".repeat(size)
    } else {
        "a".repeat(size)
    }
}

/// Segment, then build one `submit_sm` per segment.
///
/// The whole preparation, in the order [`messaging::sender::Sender::send`]
/// does it and with the same calls, so a regression here is a regression
/// there.
///
/// The PDUs are **collected**, not counted through a `map`: a `map(…).count()`
/// is elided by the iterator — the closure never runs — and the benchmark would
/// have measured the segmentation alone while claiming to measure the whole
/// preparation. `Sender::send` collects too, so this is also the shape it has
/// in production.
fn prepare(text: &str, options: &SubmitOptions) -> Vec<smpp_core::pdus::SubmitSm> {
    let split = segment(text, &SegmentationOptions::default(), REFERENCE)
        .expect("the fixture text encodes");

    split
        .segments()
        .iter()
        .map(|part| build_submit_sm(options, part).expect("the fixture builds"))
        .collect()
}

/// The measurement.
fn enqueue(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("enqueue");
    let ordinary = ordinary_options();
    let with_tlvs = options_with_tlvs();

    for (name, size) in BODIES {
        let text = body(name, size);

        group.throughput(Throughput::Elements(1));

        group.bench_with_input(BenchmarkId::new("plain", name), &text, |bencher, text| {
            bencher.iter(|| black_box(prepare(black_box(text), black_box(&ordinary))));
        });

        group.bench_with_input(
            BenchmarkId::new("with-tlvs", name),
            &text,
            |bencher, text| {
                bencher.iter(|| black_box(prepare(black_box(text), black_box(&with_tlvs))));
            },
        );
    }

    // Address parsing is on the same path and is measured on its own, because
    // it is the one step whose cost does **not** grow with the message: a
    // regression there would be invisible inside a seven-segment run.
    group.bench_function("parse-addresses", |bencher| {
        bencher.iter(|| {
            black_box((
                Destination::parse(black_box("+225 01 02 03 04 05")),
                SourceAddress::parse(black_box("ShinobiSMS")),
            ))
        });
    });

    group.finish();
}

criterion_group!(benches, enqueue);
criterion_main!(benches);
