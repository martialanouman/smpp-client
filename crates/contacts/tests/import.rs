//! Importing files that look like the ones customers actually send.
//!
//! Fiche §5 asks for "nominal, columns out of order, blank lines, no header,
//! numeric cells, accented characters", and for a cancellation mid-import. Each
//! of those is a way an import silently produces nothing, so each is a file
//! written out here rather than a well-formed three-liner.

// `tests/` is compiled without `cfg(test)`, so the relaxations of `clippy.toml`
// do not apply: an `unwrap` in a test is intended, and denying it here would
// mean an `#[allow]` on every function.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// `#[tokio::test(flavor = "multi_thread")]` expands to `Runtime::block_on`, and
// clippy attributes it to the test body. The ban exists for production code —
// a nested `block_on` there is a deadlock — not for the macro that starts a
// runtime in the first place.
#![allow(clippy::disallowed_methods)]

mod support;

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use contacts::import::{
    ColumnMapping, ColumnRef, Deduplication, HeaderMode, ImportOptions, ImportProgress,
    ImportSource, Importer,
};
use contacts::validation::{Region, RejectionReason, ValidationOptions};
use encoding_rs::WINDOWS_1252;
use tokio_util::sync::CancellationToken;

use support::{FrozenClock, MemoryStore};

/// Writes `bytes` to a file in `directory` and returns its path.
fn write(directory: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.path().join(name);
    let mut file = std::fs::File::create(&path).unwrap();

    file.write_all(bytes).unwrap();
    file.flush().unwrap();

    path
}

/// Options resolving national forms against Côte d'Ivoire.
fn ivorian(mapping: ColumnMapping) -> ImportOptions {
    ImportOptions::new(mapping).with_validation(ValidationOptions {
        default_region: Region::parse("CI"),
        mobiles_only: false,
    })
}

/// Runs an import to completion, with no cancellation and no progress channel.
async fn run(
    store: &MemoryStore,
    source: ImportSource,
    options: ImportOptions,
) -> contacts::import::ImportReport {
    Importer::new(store.clone(), FrozenClock::new())
        .run(source, options, None, CancellationToken::new())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_nominal_csv_imports_every_row() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        &directory,
        "clients.csv",
        b"telephone,ville\n0700000000,Abidjan\n0700000001,Bouake\n",
    );
    let store = MemoryStore::new();

    let report = run(
        &store,
        ImportSource::Csv { path },
        ivorian(ColumnMapping::by_name("telephone")),
    )
    .await;

    assert_eq!((report.total, report.imported, report.rejected), (2, 2, 0));
    assert!(report.is_consistent());

    let written = store.contacts().await;
    assert_eq!(written.len(), 2);
    assert_eq!(written[0].msisdn.to_e164(), "+2250700000000");
    assert_eq!(written[0].country.as_deref(), Some("CI"));
    assert_eq!(written[0].source.as_deref(), Some("import_csv"));
    assert_eq!(
        written[0].created_at,
        FrozenClock::new().instant(),
        "every contact of one import shares one injected instant"
    );
}

/// The file a French Excel writes: semicolons, CRLF, a UTF-8 BOM, and the
/// columns in whatever order the customer's export produced.
#[tokio::test]
async fn a_semicolon_bom_crlf_file_with_reordered_columns_imports() {
    let directory = tempfile::tempdir().unwrap();
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(
        b"ville;prenom;telephone\r\nAbidjan;Awa;0700000000\r\nBouake;Ali;0700000001\r\n",
    );
    let path = write(&directory, "export.csv", &bytes);
    let store = MemoryStore::new();

    let report = run(
        &store,
        ImportSource::Csv { path },
        ivorian(
            ColumnMapping::by_name("telephone")
                .with_attribute("prenom", ColumnRef::Name(String::from("prenom"))),
        ),
    )
    .await;

    assert_eq!((report.total, report.imported), (2, 2));

    let written = store.contacts().await;
    assert_eq!(written[0].msisdn.to_e164(), "+2250700000000");
    assert_eq!(
        written[0].attributes.as_deref(),
        Some(r#"{"prenom":"Awa"}"#)
    );
}

/// CA-009-02, the encoding half, end to end: the accented attribute has to
/// survive into the stored contact, not merely into the reader.
#[tokio::test]
async fn a_latin1_file_imports_with_its_accents_intact() {
    let directory = tempfile::tempdir().unwrap();
    let (bytes, _, _) = WINDOWS_1252.encode("telephone;prenom\n0700000000;Aimée\n");
    let path = write(&directory, "latin1.csv", &bytes);
    let store = MemoryStore::new();

    run(
        &store,
        ImportSource::Csv { path },
        ivorian(
            ColumnMapping::by_name("telephone")
                .with_attribute("prenom", ColumnRef::Name(String::from("prenom"))),
        ),
    )
    .await;

    let written = store.contacts().await;
    assert_eq!(
        written[0].attributes.as_deref(),
        Some(r#"{"prenom":"Aimée"}"#)
    );
}

/// A file with no header row is mapped by position, and its first row is data.
#[tokio::test]
async fn a_headerless_file_is_mapped_by_position() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        &directory,
        "brut.csv",
        b"0700000000,Abidjan\n0700000001,Bouake\n",
    );
    let store = MemoryStore::new();

    let mut options = ivorian(ColumnMapping::by_index(0));
    options.headers = HeaderMode::Absent;

    let report = run(&store, ImportSource::Csv { path }, options).await;

    assert_eq!((report.total, report.imported), (2, 2));
}

/// Blank lines are not contacts, and must not make a clean import look like a
/// partial failure.
#[tokio::test]
async fn blank_lines_sit_outside_the_total() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        &directory,
        "trous.csv",
        b"telephone;autre\n0700000000;x\n\n;\n0700000001;y\n;\n",
    );
    let store = MemoryStore::new();

    let report = run(
        &store,
        ImportSource::Csv { path },
        ivorian(ColumnMapping::by_name("telephone")),
    )
    .await;

    assert_eq!((report.total, report.imported, report.rejected), (2, 2, 0));
    assert!(report.is_consistent());

    // Counted, and counted APART. Asserting only the total would pass whether
    // blank rows were tallied or silently swallowed by the reader — and the
    // report shows the figure to the operator, so a permanent zero is a lie
    // the screen tells about their file.
    //
    // The two rows counted here are the `;` ones: a row of bare separators is
    // a record the `csv` crate hands over with every field empty. The wholly
    // empty line between them is consumed by the crate itself before any code
    // here sees it — there is no option to keep it — so `blank` counts the
    // blank rows the reader is *able* to see, which is what
    // `ImportReport::blank` documents.
    assert_eq!(
        report.blank, 2,
        "the rows of bare separators are counted apart"
    );
}

/// CA-009-05 and CA-009-08 together, on one file: every rejection names its
/// line and its reason, and the three numbers add up.
#[tokio::test]
async fn every_rejection_carries_its_line_and_its_reason() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        &directory,
        "sale.csv",
        b"telephone\n0700000000\n07\n\n+999123456789\nabcdef\n0700000000\n",
    );
    let store = MemoryStore::new();

    let report = run(
        &store,
        ImportSource::Csv { path },
        ivorian(ColumnMapping::by_name("telephone")),
    )
    .await;

    assert!(report.is_consistent());
    assert_eq!(report.imported, 1);
    assert_eq!(report.duplicates, 1, "the repeated number");
    assert_eq!(report.rejected, 3);

    let by_line: Vec<(u64, RejectionReason)> = report
        .rejected_rows
        .iter()
        .map(|row| (row.line, row.reason))
        .collect();

    assert_eq!(
        by_line,
        vec![
            (3, RejectionReason::TooShort),
            (4, RejectionReason::UnknownCountryCode),
            (6, RejectionReason::IllegalCharacter),
        ],
        "each rejection points at its own line, and each names its own reason. \
         Line 4 is the blank line: `csv` reports the record after a blank one \
         line short, which the reader documents and which does not accumulate"
    );

    assert_eq!(report.by_reason.get("TOO_SHORT"), Some(&1));
    assert_eq!(report.rejected_rows[0].value, "07");
}

/// CA-009-07 end to end, on the two spellings the criterion names.
#[tokio::test]
async fn the_two_spellings_of_one_number_import_as_one_contact() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        &directory,
        "doublons.csv",
        "telephone\n+2250700000000\n00225 07 00 00 00 00\n0700000000\n".as_bytes(),
    );
    let store = MemoryStore::new();

    let report = run(
        &store,
        ImportSource::Csv { path },
        ivorian(ColumnMapping::by_name("telephone")),
    )
    .await;

    assert_eq!(report.imported, 1);
    assert_eq!(report.duplicates, 2);
    assert_eq!(store.contacts().await.len(), 1);
}

/// The merging strategy folds the attributes of later rows into the first
/// contact, rather than dropping them with the row.
#[tokio::test]
async fn the_merging_strategy_keeps_the_attributes_of_every_duplicate() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        &directory,
        "fusion.csv",
        b"telephone,prenom,ville\n0700000000,Awa,\n0700000000,,Abidjan\n",
    );
    let store = MemoryStore::new();

    let options = ivorian(
        ColumnMapping::by_name("telephone")
            .with_attribute("prenom", ColumnRef::Name(String::from("prenom")))
            .with_attribute("ville", ColumnRef::Name(String::from("ville"))),
    )
    .with_deduplication(Deduplication::MergeAttributes);

    let report = run(&store, ImportSource::Csv { path }, options).await;

    assert_eq!((report.imported, report.duplicates), (1, 1));

    let attributes = store.contacts().await[0]
        .attributes
        .clone()
        .expect("attributes were mapped");

    assert!(attributes.contains(r#""prenom":"Awa""#), "{attributes}");
    assert!(attributes.contains(r#""ville":"Abidjan""#), "{attributes}");
}

/// CA-009-06 on a whole file: the landline is refused with the reason that
/// says so, and the mobile beside it still gets in.
#[tokio::test]
async fn mobiles_only_refuses_the_landlines_of_a_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        &directory,
        "mixte.csv",
        b"telephone\n0612345678\n0142685300\n",
    );
    let store = MemoryStore::new();

    let options = ImportOptions::new(ColumnMapping::by_name("telephone")).with_validation(
        ValidationOptions {
            default_region: Region::parse("FR"),
            mobiles_only: true,
        },
    );

    let report = run(&store, ImportSource::Csv { path }, options).await;

    assert_eq!((report.imported, report.rejected), (1, 1));
    assert_eq!(
        report.rejected_rows[0].reason,
        RejectionReason::LineTypeExcluded
    );
    assert_eq!(store.contacts().await[0].msisdn.to_e164(), "+33612345678");
}

/// CA-009-10. The batches committed before the cancellation are in the store,
/// the report says exactly how many, and nothing was written that the report
/// does not account for.
#[tokio::test]
async fn a_cancelled_import_reports_exactly_what_it_wrote() {
    let directory = tempfile::tempdir().unwrap();
    let mut file = String::from("telephone\n");

    for index in 0..200 {
        file.push_str(&format!("+2250700{index:06}\n"));
    }

    let path = write(&directory, "gros.csv", file.as_bytes());
    let store = MemoryStore::new();
    let cancel = CancellationToken::new();

    // Cancelled before a single row is read: the strongest form of the
    // criterion, since there is then no batch at all and the report must still
    // add up rather than claim the file's length.
    cancel.cancel();

    let report = Importer::new(store.clone(), FrozenClock::new())
        .with_batch_size(10)
        .run(
            ImportSource::Csv { path },
            ivorian(ColumnMapping::by_name("telephone")),
            None,
            cancel,
        )
        .await
        .unwrap();

    assert!(report.cancelled);
    assert!(report.is_consistent());
    assert_eq!(
        u64::try_from(store.contacts().await.len()).unwrap(),
        report.imported,
        "the store holds exactly what the report claims"
    );
}

/// The other half of CA-009-10: a batch that fails leaves nothing of itself
/// behind, and the batches before it stand.
#[tokio::test]
async fn a_failing_batch_leaves_the_committed_ones_alone() {
    let directory = tempfile::tempdir().unwrap();
    let mut file = String::from("telephone\n");

    for index in 0..25 {
        file.push_str(&format!("+2250700{index:06}\n"));
    }

    let path = write(&directory, "casse.csv", file.as_bytes());
    let store = MemoryStore::failing_at_batch(2);

    let outcome = Importer::new(store.clone(), FrozenClock::new())
        .with_batch_size(10)
        .run(
            ImportSource::Csv { path },
            ivorian(ColumnMapping::by_name("telephone")),
            None,
            CancellationToken::new(),
        )
        .await;

    assert!(outcome.is_err(), "the failure is reported, not swallowed");
    assert_eq!(
        store.batches().await,
        vec![10, 10],
        "two batches committed whole, the third one not at all"
    );
    assert_eq!(store.contacts().await.len(), 20);
}

/// CA-009-11: a thousand-row import must not put a thousand events on the IPC
/// bridge, and the last event must be the one that says it is over.
#[tokio::test]
async fn progress_is_throttled_and_always_ends_with_a_final_event() {
    let directory = tempfile::tempdir().unwrap();
    let mut file = String::from("telephone\n");

    for index in 0..2_500 {
        file.push_str(&format!("+2250700{index:06}\n"));
    }

    let path = write(&directory, "progress.csv", file.as_bytes());
    let store = MemoryStore::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);

    let report = Importer::new(store.clone(), FrozenClock::new())
        .run(
            ImportSource::Csv { path },
            ivorian(ColumnMapping::by_name("telephone")),
            Some(sender),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let mut events: Vec<ImportProgress> = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }

    assert!(
        events.len() <= 5,
        "2 500 rows produced {} events; the throttle is not throttling",
        events.len()
    );
    assert!(
        events.iter().any(|event| event.done),
        "the final event never arrived"
    );

    let final_event = events.iter().find(|event| event.done).unwrap();
    assert_eq!(final_event.processed, report.total);
    assert_eq!(final_event.imported, report.imported);
}

/// A receiver that never reads must not hold the import up: `try_send` drops
/// intermediate events, and the import finishes with a correct report.
#[tokio::test]
async fn a_progress_receiver_that_never_reads_does_not_stall_the_import() {
    let directory = tempfile::tempdir().unwrap();
    let mut file = String::from("telephone\n");

    for index in 0..5_000 {
        file.push_str(&format!("+2250700{index:06}\n"));
    }

    let path = write(&directory, "sourd.csv", file.as_bytes());
    let store = MemoryStore::new();
    // Capacity one, never drained: every event after the first is dropped.
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);

    let report = Importer::new(store.clone(), FrozenClock::new())
        .run(
            ImportSource::Csv { path },
            ivorian(ColumnMapping::by_name("telephone")),
            Some(sender),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(report.imported, 5_000);
}

/// The mapping error names the column it could not find, and nothing else —
/// a header is structure, a cell is personal data.
#[tokio::test]
async fn a_mapping_that_does_not_fit_the_file_is_refused_by_name() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(&directory, "autre.csv", b"nom,ville\nAwa,Abidjan\n");
    let store = MemoryStore::new();

    let error = Importer::new(store, FrozenClock::new())
        .run(
            ImportSource::Csv { path },
            ivorian(ColumnMapping::by_name("telephone")),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    let rendered = error.to_string();
    assert!(rendered.contains("telephone"), "{rendered}");
    assert!(!rendered.contains("Awa"), "{rendered}");
}

/// Every imported contact joins the list the assistant created, batch by
/// batch, so a cancellation cannot leave contacts enrolled in nothing.
#[tokio::test]
async fn an_import_enrols_its_contacts_in_the_list_it_was_given() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        &directory,
        "liste.csv",
        b"telephone\n0700000000\n0700000001\n",
    );
    let store = MemoryStore::new();
    let list = contacts::model::ListId::new();

    run(
        &store,
        ImportSource::Csv { path },
        ivorian(ColumnMapping::by_name("telephone")).into_list(list),
    )
    .await;

    let memberships = store.memberships().await;
    assert_eq!(memberships.len(), 2);
    assert!(memberships.iter().all(|(held, _)| *held == list));
}

/// CA-009-10, the half the other cancellation test does not reach: an import
/// cancelled **while it is running**, with the queue between the reader and the
/// writer full.
///
/// # Why this shape, and not a plain `cancel()` mid-flight
///
/// The reader runs on `spawn_blocking` and offers rows with `blocking_send`,
/// which does not watch the cancellation token — it returns only when the
/// receiver is dropped or closed. So the failure needs three things at once: a
/// file longer than the queue, a writer slow enough that the queue actually
/// fills, and a cancellation while the reader is parked in `blocking_send`.
/// A fast double never reaches that state, which is why the store is slowed
/// down here.
///
/// Without `receiver.close()` before the reader is joined, this test does not
/// fail — it **hangs**, which is exactly what the operator sees: the progress
/// bar frozen, the cancel button doing nothing, and the import never returning.
/// The timeout is what turns that hang into a red test.
#[tokio::test]
async fn a_cancellation_mid_import_returns_rather_than_hanging() {
    let directory = tempfile::tempdir().unwrap();
    let mut file = String::from("telephone\n");

    // Comfortably more than ROW_QUEUE_CAPACITY, so the reader is certain to be
    // parked in `blocking_send` when the cancellation lands.
    for index in 0..5_000 {
        file.push_str(&format!("+2250700{index:06}\n"));
    }

    let path = write(&directory, "long.csv", file.as_bytes());
    let store = MemoryStore::slow(Duration::from_millis(300));
    let cancel = CancellationToken::new();

    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        trigger.cancel();
    });

    let report = tokio::time::timeout(
        Duration::from_secs(10),
        Importer::new(store.clone(), FrozenClock::new())
            .with_batch_size(100)
            .run(
                ImportSource::Csv { path },
                ivorian(ColumnMapping::by_name("telephone")),
                None,
                cancel,
            ),
    )
    .await
    .expect("a cancelled import must return, not hang")
    .unwrap();

    assert!(report.cancelled, "the report says it was cancelled");
    assert!(report.is_consistent(), "the counts still add up");
    assert_eq!(
        u64::try_from(store.contacts().await.len()).unwrap(),
        report.imported,
        "the store holds exactly what the report claims"
    );
    assert!(
        report.imported < 5_000,
        "the cancellation actually cut the import short"
    );
}
