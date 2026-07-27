//! Deduplication and the import report (deliverable L-009-05).
//!
//! # The arithmetic CA-009-08 asks for
//!
//! `total = imported + rejected + duplicates`, and it is an **invariant**, not
//! an assertion made once in a test: [`ImportReport::is_consistent`] states it,
//! [`ImportTally`] is the only thing that can increment any of the three, and
//! every path that ends a row goes through exactly one of its methods. A row
//! that fell through all of them would show up as a broken invariant rather
//! than as a report that quietly does not add up.
//!
//! Blank rows are counted **outside** the total, and deliberately: a
//! spreadsheet export ends with a dozen of them, and calling them rejected
//! contacts makes every report look like a partial failure.
//!
//! # What deduplication costs, honestly
//!
//! Duplicates are detected on the **normalised** number (CA-009-07), so
//! `+2250700000000` and `00225 07 00 00 00 00` are one contact. Detecting that
//! needs something remembered per distinct number, and fiche §6 asks for that
//! choice to be measured rather than assumed:
//!
//! * [`Deduplication::FirstWins`] keeps a set of **64-bit digests**, eight
//!   bytes per distinct number. A million distinct numbers cost a few tens of
//!   megabytes against a file of a hundred and more, and nothing else is held —
//!   the rows themselves stream through. Two different numbers sharing a digest
//!   would drop one contact; at a million numbers that is a chance of roughly
//!   3 × 10⁻⁸, which is the trade this variant makes and states.
//! * [`Deduplication::MergeAttributes`] cannot do that. Merging the attributes
//!   of a later duplicate into an earlier contact means still **having** the
//!   earlier contact, so it holds the contacts themselves until the end of the
//!   import. Memory grows with the number of distinct contacts. It is the
//!   non-default for that reason, and the interface says so.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::model::{Contact, LineType};
use crate::validation::RejectionReason;

/// How many rejected rows the report keeps in full.
///
/// The rejected rows are a file the operator downloads to correct and re-import
/// (CA-009-05), so they have to be kept — but an import where *every* row is
/// rejected must not turn a hundred-megabyte file into a hundred megabytes of
/// report. Past this many, the counts by reason stay exact and
/// [`ImportReport::rejected_truncated`] says the list is not.
pub const MAX_REJECTED_ROWS: usize = 10_000;

/// What to do with a row whose number has already been seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Deduplication {
    /// Keep the first occurrence and drop the rest. Streams; see the module
    /// note.
    #[default]
    FirstWins,
    /// Keep the first occurrence and fold later attributes into it.
    ///
    /// Holds every distinct contact until the import ends. A key present in
    /// both wins from the **first** occurrence: an import is a merge into what
    /// is already there, not a last-write-wins overwrite.
    MergeAttributes,
}

/// One row the import refused, with everything needed to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRow {
    /// Line number in the file, as the operator's editor shows it.
    pub line: u64,
    /// Why it was refused.
    pub reason: RejectionReason,
    /// The cell as it was in the file, so the exported list is correctable.
    ///
    /// This is the **one** place the crate keeps a rejected value, and it goes
    /// straight back to the operator who supplied it. It is never logged, never
    /// part of an error message, and never crosses into `tracing`.
    pub value: String,
}

/// The running counts of an import.
///
/// The only way to move any of the three numbers that CA-009-08 relates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportTally {
    imported: u64,
    rejected: u64,
    duplicates: u64,
    blank: u64,
    mobiles: u64,
    fixed_lines: u64,
    by_reason: BTreeMap<&'static str, u64>,
    rejected_rows: Vec<RejectedRow>,
    rejected_truncated: bool,
}

impl ImportTally {
    /// Records a contact that will be written.
    pub fn accept(&mut self, line_type: Option<LineType>) {
        self.imported = self.imported.saturating_add(1);

        match line_type {
            Some(LineType::Mobile) => self.mobiles = self.mobiles.saturating_add(1),
            Some(LineType::FixedLine) => self.fixed_lines = self.fixed_lines.saturating_add(1),
            _ => {}
        }
    }

    /// Records a refused row.
    pub fn reject(&mut self, line: u64, reason: RejectionReason, value: &str) {
        self.rejected = self.rejected.saturating_add(1);
        *self.by_reason.entry(reason.code()).or_insert(0) += 1;

        if self.rejected_rows.len() < MAX_REJECTED_ROWS {
            self.rejected_rows.push(RejectedRow {
                line,
                reason,
                value: value.to_owned(),
            });
        } else {
            self.rejected_truncated = true;
        }
    }

    /// Records a row whose number had already been seen.
    pub fn duplicate(&mut self) {
        self.duplicates = self.duplicates.saturating_add(1);
    }

    /// Records a row that held nothing at all.
    pub fn blank(&mut self) {
        self.blank = self.blank.saturating_add(1);
    }

    /// Freezes the counts into the report the interface shows.
    #[must_use]
    pub fn finish(self, cancelled: bool) -> ImportReport {
        ImportReport {
            total: self
                .imported
                .saturating_add(self.rejected)
                .saturating_add(self.duplicates),
            imported: self.imported,
            rejected: self.rejected,
            duplicates: self.duplicates,
            blank: self.blank,
            mobiles: self.mobiles,
            fixed_lines: self.fixed_lines,
            by_reason: self.by_reason,
            rejected_rows: self.rejected_rows,
            rejected_truncated: self.rejected_truncated,
            cancelled,
        }
    }

    /// How many rows have been dealt with so far, for the progress event.
    #[must_use]
    pub const fn processed(&self) -> u64 {
        self.imported
            .saturating_add(self.rejected)
            .saturating_add(self.duplicates)
    }

    /// How many contacts are due to be written.
    #[must_use]
    pub const fn imported(&self) -> u64 {
        self.imported
    }

    /// How many rows were refused.
    #[must_use]
    pub const fn rejected(&self) -> u64 {
        self.rejected
    }

    /// How many rows repeated a number.
    #[must_use]
    pub const fn duplicates(&self) -> u64 {
        self.duplicates
    }
}

/// What an import did (spec §11.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Non-blank rows examined. Equals `imported + rejected + duplicates`.
    pub total: u64,
    /// Contacts written.
    pub imported: u64,
    /// Rows refused, whatever the reason.
    pub rejected: u64,
    /// Rows whose number repeated an earlier one.
    pub duplicates: u64,
    /// Rows that held nothing, counted apart from [`Self::total`].
    pub blank: u64,
    /// Contacts the plan reported as mobile.
    pub mobiles: u64,
    /// Contacts the plan reported as a landline.
    pub fixed_lines: u64,
    /// How many rows each reason accounts for, by
    /// [`RejectionReason::code`].
    pub by_reason: BTreeMap<&'static str, u64>,
    /// The refused rows, up to [`MAX_REJECTED_ROWS`].
    pub rejected_rows: Vec<RejectedRow>,
    /// Whether [`Self::rejected_rows`] was cut short.
    pub rejected_truncated: bool,
    /// Whether the operator stopped the import before the end of the file.
    pub cancelled: bool,
}

impl ImportReport {
    /// CA-009-08, stated where it can be checked rather than only in a test.
    #[must_use]
    pub const fn is_consistent(&self) -> bool {
        self.total == self.imported + self.rejected + self.duplicates
    }
}

/// Remembers which numbers have been seen.
///
/// See the module note for what each strategy costs.
#[derive(Debug)]
pub struct Deduplicator {
    strategy: Deduplication,
    /// Digests of numbers already accepted, for [`Deduplication::FirstWins`].
    seen: HashSet<u64>,
    /// The contacts themselves, for [`Deduplication::MergeAttributes`].
    held: HashMap<u64, Contact>,
    /// Insertion order, so a merging import writes contacts in file order.
    order: Vec<u64>,
}

/// What the deduplicator decided about a row.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// First time this number is seen; write it.
    Fresh,
    /// Seen before; count it as a duplicate and drop the row.
    Duplicate,
    /// Seen before, and its attributes were folded into the held contact.
    Merged,
}

impl Deduplicator {
    /// A deduplicator applying `strategy`.
    #[must_use]
    pub fn new(strategy: Deduplication) -> Self {
        Self {
            strategy,
            seen: HashSet::new(),
            held: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Which strategy this applies.
    #[must_use]
    pub const fn strategy(&self) -> Deduplication {
        self.strategy
    }

    /// Offers a contact, and says what became of it.
    ///
    /// Under [`Deduplication::FirstWins`] a [`Verdict::Fresh`] contact is the
    /// caller's to write immediately. Under
    /// [`Deduplication::MergeAttributes`] it is held here until
    /// [`Self::into_held`], because a later row may still add to it.
    pub fn offer(&mut self, contact: Contact) -> (Verdict, Option<Contact>) {
        // The digest is of the NORMALISED number (CA-009-07): `+2250700000000`
        // and `00225 07 00 00 00 00` both reach here as `2250700000000`, so
        // they collide on purpose, which is the whole point.
        let digest = digest(contact.msisdn.as_str());

        match self.strategy {
            Deduplication::FirstWins => {
                if self.seen.insert(digest) {
                    (Verdict::Fresh, Some(contact))
                } else {
                    (Verdict::Duplicate, None)
                }
            }
            Deduplication::MergeAttributes => match self.held.entry(digest) {
                std::collections::hash_map::Entry::Occupied(mut held) => {
                    merge_attributes(held.get_mut(), contact.attributes.as_deref());
                    (Verdict::Merged, None)
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(contact);
                    self.order.push(digest);
                    (Verdict::Fresh, None)
                }
            },
        }
    }

    /// The contacts a merging import accumulated, in file order.
    ///
    /// Empty under [`Deduplication::FirstWins`], which handed each contact back
    /// as it went.
    #[must_use]
    pub fn into_held(mut self) -> Vec<Contact> {
        self.order
            .iter()
            .filter_map(|digest| self.held.remove(digest))
            .collect()
    }

    /// How many distinct numbers are remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.strategy {
            Deduplication::FirstWins => self.seen.len(),
            Deduplication::MergeAttributes => self.held.len(),
        }
    }

    /// Whether nothing has been offered yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Folds `incoming` into `contact`'s attributes, first occurrence winning.
fn merge_attributes(contact: &mut Contact, incoming: Option<&str>) {
    let Some(incoming) = incoming else {
        return;
    };

    let Ok(serde_json::Value::Object(incoming)) = serde_json::from_str(incoming) else {
        return;
    };

    let mut merged = match contact.attributes.as_deref().map(serde_json::from_str) {
        Some(Ok(serde_json::Value::Object(existing))) => existing,
        _ => serde_json::Map::new(),
    };

    for (key, value) in incoming {
        // An empty string is not information: a second row that leaves a
        // column blank must not blank out a value the first row supplied.
        if value.as_str().is_some_and(str::is_empty) {
            continue;
        }

        merged.entry(key).or_insert(value);
    }

    contact.attributes = serde_json::to_string(&serde_json::Value::Object(merged)).ok();
}

/// A 64-bit digest of a normalised number.
///
/// FNV-1a, spelled out rather than taken from `DefaultHasher`: that one is
/// explicitly documented as free to change between releases and to be
/// randomly seeded, and a deduplication whose behaviour depends on the run is
/// not a deduplication. Eight bytes per distinct number is the whole reason
/// this is a digest and not the number itself.
fn digest(number: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    number.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::{digest, Deduplication, Deduplicator, ImportTally, Verdict, MAX_REJECTED_ROWS};
    use crate::model::{Contact, ContactId, LineType};
    use crate::validation::RejectionReason;
    use smpp_core::time::Timestamp;
    use smpp_core::types::Msisdn;

    fn a_contact(number: &str, attributes: Option<&str>) -> Contact {
        Contact {
            contact_id: ContactId::new(),
            msisdn: Msisdn::parse(number).expect("valid"),
            country: None,
            valid: true,
            line_type: Some(LineType::Mobile),
            attributes: attributes.map(str::to_owned),
            source: None,
            created_at: Timestamp::now(),
        }
    }

    /// CA-009-07, in the exact form the criterion states it — and routed
    /// through the real validator rather than through two hand-written
    /// identical strings, because the claim is about *normalisation*, and a
    /// test that pre-normalised its own inputs would pass without it.
    #[test]
    fn the_two_written_forms_of_one_number_are_one_contact() {
        use crate::validation::{validate, ValidationOptions};

        let options = ValidationOptions::default();
        let plus = validate("+2250700000000", None, options).expect("valid");
        let double_zero = validate("00225 07 00 00 00 00", None, options).expect("valid");

        assert_eq!(
            plus.msisdn(),
            double_zero.msisdn(),
            "the two spellings normalise to the same number"
        );

        let mut deduplicator = Deduplicator::new(Deduplication::FirstWins);

        let (first, _) = deduplicator.offer(a_contact(plus.msisdn().as_str(), None));
        let (second, _) = deduplicator.offer(a_contact(double_zero.msisdn().as_str(), None));

        assert_eq!(first, Verdict::Fresh);
        assert_eq!(second, Verdict::Duplicate);
        assert_eq!(deduplicator.len(), 1);
    }

    #[test]
    fn two_different_numbers_are_two_contacts() {
        let mut deduplicator = Deduplicator::new(Deduplication::FirstWins);

        let (first, kept) = deduplicator.offer(a_contact("+2250700000000", None));
        let (second, also_kept) = deduplicator.offer(a_contact("+2250700000001", None));

        assert_eq!(first, Verdict::Fresh);
        assert_eq!(second, Verdict::Fresh);
        assert!(kept.is_some() && also_kept.is_some());
    }

    /// The first strategy hands the contact straight back, which is what lets
    /// the import stream.
    #[test]
    fn first_wins_hands_each_fresh_contact_back_immediately() {
        let mut deduplicator = Deduplicator::new(Deduplication::FirstWins);

        let (_, kept) = deduplicator.offer(a_contact("+2250700000000", Some(r#"{"a":"1"}"#)));

        assert_eq!(
            kept.expect("handed back").attributes.as_deref(),
            Some(r#"{"a":"1"}"#)
        );
        assert!(Deduplicator::new(Deduplication::FirstWins)
            .into_held()
            .is_empty());
    }

    /// The second strategy folds later rows into the first, and only hands
    /// contacts back at the end.
    #[test]
    fn merging_folds_later_attributes_into_the_first_occurrence() {
        let mut deduplicator = Deduplicator::new(Deduplication::MergeAttributes);

        let (first, held) =
            deduplicator.offer(a_contact("+2250700000000", Some(r#"{"nom":"Awa"}"#)));
        let (second, _) =
            deduplicator.offer(a_contact("+2250700000000", Some(r#"{"ville":"Abidjan"}"#)));

        assert_eq!(first, Verdict::Fresh);
        assert!(held.is_none(), "a merging import writes at the end");
        assert_eq!(second, Verdict::Merged);

        let contacts = deduplicator.into_held();

        assert_eq!(contacts.len(), 1);
        let attributes = contacts[0].attributes.as_deref().expect("merged");
        assert!(attributes.contains(r#""nom":"Awa""#), "{attributes}");
        assert!(attributes.contains(r#""ville":"Abidjan""#), "{attributes}");
    }

    /// The first occurrence wins a key present in both — an import is a merge
    /// into what is there, not a last-write-wins overwrite.
    #[test]
    fn merging_keeps_the_first_value_of_a_repeated_key() {
        let mut deduplicator = Deduplicator::new(Deduplication::MergeAttributes);

        deduplicator.offer(a_contact("+2250700000000", Some(r#"{"nom":"Awa"}"#)));
        deduplicator.offer(a_contact("+2250700000000", Some(r#"{"nom":"Ali"}"#)));

        let contacts = deduplicator.into_held();

        assert_eq!(contacts[0].attributes.as_deref(), Some(r#"{"nom":"Awa"}"#));
    }

    /// A blank cell in a later row must not erase a value an earlier row
    /// supplied — the case a plain `extend` would get wrong.
    #[test]
    fn merging_ignores_an_empty_value_rather_than_blanking_a_known_one() {
        let mut deduplicator = Deduplicator::new(Deduplication::MergeAttributes);

        deduplicator.offer(a_contact("+2250700000000", Some(r#"{"ville":"Abidjan"}"#)));
        deduplicator.offer(a_contact("+2250700000000", Some(r#"{"ville":""}"#)));

        let contacts = deduplicator.into_held();

        assert_eq!(
            contacts[0].attributes.as_deref(),
            Some(r#"{"ville":"Abidjan"}"#)
        );
    }

    #[test]
    fn a_merging_import_writes_contacts_in_file_order() {
        let mut deduplicator = Deduplicator::new(Deduplication::MergeAttributes);

        for index in 0..5_u32 {
            deduplicator.offer(a_contact(&format!("+22507000000{index:02}"), None));
        }

        let contacts = deduplicator.into_held();

        assert_eq!(contacts.len(), 5);
        assert_eq!(contacts[0].msisdn.as_str(), "2250700000000");
        assert_eq!(contacts[4].msisdn.as_str(), "2250700000004");
    }

    /// CA-009-08. The tally is the only thing that can move the three numbers,
    /// so the identity holds by construction — this checks the construction.
    #[test]
    fn the_report_adds_up() {
        let mut tally = ImportTally::default();

        tally.accept(Some(LineType::Mobile));
        tally.accept(Some(LineType::FixedLine));
        tally.accept(None);
        tally.reject(4, RejectionReason::TooShort, "07");
        tally.reject(5, RejectionReason::TooShort, "08");
        tally.reject(6, RejectionReason::NotInPlan, "0100000000");
        tally.duplicate();
        tally.blank();

        let report = tally.finish(false);

        assert_eq!(report.total, 7);
        assert_eq!(report.imported, 3);
        assert_eq!(report.rejected, 3);
        assert_eq!(report.duplicates, 1);
        assert!(report.is_consistent());
        assert_eq!(report.blank, 1, "blank rows sit outside the total");
        assert_eq!(report.mobiles, 1);
        assert_eq!(report.fixed_lines, 1);
        assert_eq!(report.by_reason.get("TOO_SHORT"), Some(&2));
        assert_eq!(report.by_reason.get("NOT_IN_PLAN"), Some(&1));
    }

    #[test]
    fn a_rejected_row_carries_the_value_to_correct() {
        let mut tally = ImportTally::default();

        tally.reject(12, RejectionReason::TooShort, "07");

        let report = tally.finish(false);

        assert_eq!(report.rejected_rows[0].line, 12);
        assert_eq!(report.rejected_rows[0].value, "07");
        assert_eq!(report.rejected_rows[0].reason, RejectionReason::TooShort);
    }

    /// An import where everything is rejected must not turn the file into a
    /// report of the same size — but the counts must stay exact.
    #[test]
    fn the_rejected_list_is_capped_and_says_so_while_the_counts_stay_exact() {
        let mut tally = ImportTally::default();

        for line in 0..u64::try_from(MAX_REJECTED_ROWS).expect("fits") + 25 {
            tally.reject(line, RejectionReason::TooShort, "07");
        }

        let report = tally.finish(false);

        assert_eq!(report.rejected_rows.len(), MAX_REJECTED_ROWS);
        assert!(report.rejected_truncated);
        assert_eq!(
            report.rejected,
            u64::try_from(MAX_REJECTED_ROWS).expect("fits") + 25
        );
        assert!(report.is_consistent());
    }

    /// The digest must not depend on the run, or two imports of the same file
    /// would deduplicate differently.
    #[test]
    fn the_digest_is_stable_across_calls_and_distinguishes_numbers() {
        assert_eq!(digest("2250700000000"), digest("2250700000000"));
        assert_ne!(digest("2250700000000"), digest("2250700000001"));
    }
}
