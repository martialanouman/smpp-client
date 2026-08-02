//! E.164 validation and normalisation (deliverable L-009-04).
//!
//! # What milestone 006 deliberately left undone, and this closes
//!
//! `smpp_core::types::Msisdn` performs the **protocol-level** check: separators
//! removed, one optional `+`, 3 to 20 digits. `messaging::addressing` says in
//! so many words that country-plan validation was postponed to this milestone,
//! and names the consequence it accepted meanwhile — a national form typed
//! without a country code is accepted there and rejected by the SMSC.
//!
//! Here it is closed. A number is parsed against Google's numbering-plan
//! database with a **default region**, so `0700000000` + `CI` becomes
//! `+2250700000000` (CA-009-04); a number the plan refuses is rejected with a
//! reason precise enough to act on (CA-009-05); and the line type the plan
//! reports drives the "mobiles only" switch (CA-009-06).
//!
//! # The order the default region is resolved in
//!
//! 1. an explicit `+` or international prefix in the value itself — a number
//!    that says which country it is in is never overridden;
//! 2. the country column of the row, when the mapping declares one;
//! 3. the import-wide default region the operator chose.
//!
//! Step 1 is `phonenumber`'s own behaviour and is not re-implemented: passing a
//! default region to [`phonenumber::parse`] alongside a value carrying its own
//! country code leaves the value's country code winning.
//!
//! # Nothing here echoes the number it rejected
//!
//! An MSISDN is personal data (CLAUDE.md §8). A [`RejectionReason`] carries the
//! *kind* of failure and the line it happened on; the value stays in the
//! rejected-rows file the operator downloads, which is theirs already.

use core::str::FromStr as _;

use phonenumber::country::Id as CountryId;
use phonenumber::{Mode, Type};
use smpp_core::types::Msisdn;

use crate::model::LineType;

/// A region a national number is resolved against, ISO 3166-1 alpha-2.
///
/// A newtype over `phonenumber`'s own identifier rather than a `String`
/// (CLAUDE.md §4): "CI" and "ci" and "CIV" all look like country codes, and
/// only one of them is one. Parsed once, at the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Region(CountryId);

impl Region {
    /// Parses an ISO 3166-1 alpha-2 code, case-insensitively.
    ///
    /// Returns `None` for anything the plan database does not know, which is
    /// what makes an unknown country column a *rejection with a reason* rather
    /// than a silent fallback to some other country.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();

        if trimmed.len() != 2 {
            return None;
        }

        CountryId::from_str(&trimmed.to_ascii_uppercase())
            .ok()
            .map(Self)
    }

    /// The ISO 3166-1 alpha-2 code.
    ///
    /// Borrowed rather than `&'static str`: `phonenumber` exposes the code
    /// through `AsRef<str>`, whose signature ties the lifetime to the
    /// receiver. Nothing is allocated — the underlying strings are literals.
    #[must_use]
    pub fn code(&self) -> &str {
        self.0.as_ref()
    }
}

/// Why a number was refused.
///
/// The variants are what an operator can *do something about*, which is the
/// whole point of CA-009-05: "invalid" on fifty thousand rows is not a report,
/// it is a shrug. They are rendered by the interface through i18n keys, never
/// by `Display` — the fiche §6 asks for reasons a user understands, not
/// internal codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The cell was empty, or held no digit at all.
    #[error("the number column is empty on this row")]
    Empty,

    /// Fewer digits than any number of the plan.
    #[error("the number is too short for its country")]
    TooShort,

    /// More digits than any number of the plan.
    #[error("the number is too long for its country")]
    TooLong,

    /// The leading country code belongs to no country.
    #[error("the country code is not one this numbering plan knows")]
    UnknownCountryCode,

    /// A national form, and no region to resolve it against.
    ///
    /// Distinct from [`Self::UnknownCountryCode`] and deliberately: the fix is
    /// different. This one means "tell me which country", the other means "this
    /// number claims a country that does not exist".
    #[error("the number has no country code and the import has no default region")]
    MissingRegion,

    /// The country column held something that is not an ISO 3166-1 alpha-2
    /// code.
    #[error("the country column does not hold a two-letter country code")]
    UnknownRegion,

    /// The digits do not match any pattern of the plan for that country.
    #[error("the number does not match any pattern of its country's numbering plan")]
    NotInPlan,

    /// A landline, on an import that asked for mobiles only (CA-009-06).
    #[error("the line type is excluded by the mobiles-only option")]
    LineTypeExcluded,

    /// The value held a character no phone number holds.
    #[error("the number holds a character that is not a digit or a separator")]
    IllegalCharacter,

    /// A spreadsheet cell no text could be read out of (CA-009-03).
    ///
    /// An error cell, a date, or a number with a fractional part. Distinct from
    /// [`Self::Empty`] on purpose: the operator opening the file would find the
    /// cell full, and "empty" would send them looking at the wrong thing.
    #[error("the spreadsheet cell holds a value no number can be read out of")]
    UnreadableCell,
}

impl RejectionReason {
    /// The stable key the interface translates.
    ///
    /// Stable across releases: it ends up in an exported rejected-rows file
    /// that an operator may keep, and in the i18n catalogues.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "EMPTY",
            Self::TooShort => "TOO_SHORT",
            Self::TooLong => "TOO_LONG",
            Self::UnknownCountryCode => "UNKNOWN_COUNTRY_CODE",
            Self::MissingRegion => "MISSING_REGION",
            Self::UnknownRegion => "UNKNOWN_REGION",
            Self::NotInPlan => "NOT_IN_PLAN",
            Self::LineTypeExcluded => "LINE_TYPE_EXCLUDED",
            Self::IllegalCharacter => "ILLEGAL_CHARACTER",
            Self::UnreadableCell => "UNREADABLE_CELL",
        }
    }

    /// Every variant, so the report can enumerate them and the i18n test can
    /// check each has a translation.
    pub const ALL: &'static [Self] = &[
        Self::Empty,
        Self::TooShort,
        Self::TooLong,
        Self::UnknownCountryCode,
        Self::MissingRegion,
        Self::UnknownRegion,
        Self::NotInPlan,
        Self::LineTypeExcluded,
        Self::IllegalCharacter,
        Self::UnreadableCell,
    ];
}

/// A number that passed the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedNumber {
    msisdn: Msisdn,
    region: Option<Region>,
    line_type: LineType,
}

impl ValidatedNumber {
    /// The number, normalised to its international form (digits only).
    #[must_use]
    pub const fn msisdn(&self) -> &Msisdn {
        &self.msisdn
    }

    /// The E.164 presentation form, `+` included.
    #[must_use]
    pub fn to_e164(&self) -> String {
        self.msisdn.to_e164()
    }

    /// The country the plan resolved the number to.
    #[must_use]
    pub const fn region(&self) -> Option<Region> {
        self.region
    }

    /// What kind of line the plan says this is.
    #[must_use]
    pub const fn line_type(&self) -> LineType {
        self.line_type
    }
}

/// How strict an import is about what it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ValidationOptions {
    /// The region national forms are resolved against when the row says
    /// nothing.
    pub default_region: Option<Region>,
    /// Keep only lines [`LineType::is_mobile`] accepts (CA-009-06).
    pub mobiles_only: bool,
}

/// Validates and normalises one number.
///
/// `row_region` is the country column of this row, when the mapping declares
/// one; it wins over [`ValidationOptions::default_region`] and loses to a
/// country code carried by the value itself.
///
/// # Errors
///
/// A [`RejectionReason`] naming what to fix. The rejected value is **never**
/// part of the error (CLAUDE.md §8).
pub fn validate(
    raw: &str,
    row_region: Option<&str>,
    options: ValidationOptions,
) -> Result<ValidatedNumber, RejectionReason> {
    let trimmed = raw.trim();

    if trimmed.is_empty() {
        return Err(RejectionReason::Empty);
    }

    let region = match row_region.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => Some(Region::parse(value).ok_or(RejectionReason::UnknownRegion)?),
        None => options.default_region,
    };

    // `00` is the international prefix in most of the world and the commonest
    // form a spreadsheet carries — CA-009-07 names `00225 07 …` explicitly.
    // `phonenumber` only strips it when it knows the dialling country, which
    // it does not when the region is absent, so it is rewritten to the `+`
    // that means the same thing everywhere.
    let candidate = international_form(trimmed);

    let parsed = phonenumber::parse(region.map(|value| value.0), &candidate)
        .map_err(|error| classify(&error, &candidate, region))?;

    if !phonenumber::is_valid(&parsed) {
        return Err(refine_invalid(&parsed));
    }

    let line_type = line_type_of(parsed.number_type(&phonenumber::metadata::DATABASE));

    if options.mobiles_only && !line_type.is_mobile() {
        return Err(RejectionReason::LineTypeExcluded);
    }

    // Through `Msisdn` rather than kept as the formatter's output: it is the
    // type every layer below speaks, and routing the value through its parser
    // is what guarantees the two normalisations cannot disagree.
    let msisdn = Msisdn::parse(&parsed.format().mode(Mode::E164).to_string())
        .map_err(|_| RejectionReason::NotInPlan)?;

    Ok(ValidatedNumber {
        msisdn,
        region: parsed.country().id().map(Region).or(region),
        line_type,
    })
}

/// Says why a parsed number failed the plan, as precisely as the crate allows.
///
/// `phonenumber` exposes `is_valid` as a boolean and keeps its per-country
/// length table behind a private type, so "too short for Cote d'Ivoire" is not
/// something that can be asked for. What *is* knowable without that table is
/// the E.164 envelope every plan sits inside — a national number shorter than
/// four digits or longer than fifteen is too short or too long for **every**
/// country — and anything inside the envelope which the plan still refuses is a
/// number that matches no pattern.
///
/// The approximation is stated rather than hidden: a seven-digit number in a
/// country whose plan needs nine is reported as [`RejectionReason::NotInPlan`],
/// not as [`RejectionReason::TooShort`]. Both send the operator to the same
/// cell.
fn refine_invalid(parsed: &phonenumber::PhoneNumber) -> RejectionReason {
    let national = parsed.national();
    let significant = national
        .value()
        .checked_ilog10()
        .map_or(1, |log| log.saturating_add(1));
    let digits = usize::from(national.zeros())
        .saturating_add(usize::try_from(significant).unwrap_or(usize::MAX));

    if digits < MIN_NATIONAL_DIGITS {
        RejectionReason::TooShort
    } else if digits > MAX_E164_DIGITS {
        RejectionReason::TooLong
    } else {
        RejectionReason::NotInPlan
    }
}

/// Below this, a national number is too short for every plan on earth.
const MIN_NATIONAL_DIGITS: usize = 4;

/// E.164 caps a whole number, country code included, at fifteen digits.
const MAX_E164_DIGITS: usize = 15;

/// Rewrites a leading international prefix as a `+`.
///
/// Only `00`, and only when what follows is a digit: `+` is already the
/// international form, and a national number that merely starts with two zeros
/// does not exist in any plan this ships.
fn international_form(value: &str) -> String {
    match value.strip_prefix("00") {
        Some(rest) if rest.starts_with(|character: char| character.is_ascii_digit()) => {
            format!("+{rest}")
        }
        _ => value.to_owned(),
    }
}

/// Maps a parse failure onto the reason an operator can act on.
fn classify(
    error: &phonenumber::ParseError,
    candidate: &str,
    region: Option<Region>,
) -> RejectionReason {
    match error {
        phonenumber::ParseError::InvalidCountryCode => {
            // The same upstream variant covers two very different situations,
            // and telling them apart is what makes the message useful: a
            // national form with no region to resolve it against is the
            // operator forgetting to pick a country, while a `+999…` is a
            // number claiming a country that does not exist.
            if region.is_none() && !candidate.starts_with('+') {
                RejectionReason::MissingRegion
            } else {
                RejectionReason::UnknownCountryCode
            }
        }
        phonenumber::ParseError::TooShortNsn | phonenumber::ParseError::TooShortAfterIdd => {
            RejectionReason::TooShort
        }
        phonenumber::ParseError::TooLong => RejectionReason::TooLong,
        phonenumber::ParseError::NoNumber => {
            if candidate
                .chars()
                .any(|character| character.is_ascii_digit())
            {
                RejectionReason::TooShort
            } else {
                RejectionReason::IllegalCharacter
            }
        }
        _ => RejectionReason::NotInPlan,
    }
}

/// Projects the plan's line type onto the three cases that change a decision.
const fn line_type_of(kind: Type) -> LineType {
    match kind {
        Type::Mobile => LineType::Mobile,
        Type::FixedLine => LineType::FixedLine,
        Type::FixedLineOrMobile => LineType::FixedLineOrMobile,
        Type::Unknown => LineType::Unknown,
        _ => LineType::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::{validate, Region, RejectionReason, ValidationOptions};
    use crate::model::LineType;

    fn with_region(code: &str) -> ValidationOptions {
        ValidationOptions {
            default_region: Region::parse(code),
            mobiles_only: false,
        }
    }

    /// CA-009-04, literally.
    #[test]
    fn a_national_form_is_normalised_against_the_default_region() {
        let validated = validate("0700000000", None, with_region("CI")).expect("valid");

        assert_eq!(validated.to_e164(), "+2250700000000");
        assert_eq!(validated.msisdn().as_str(), "2250700000000");
    }

    /// Ten plans of different shapes (fiche §5): trunk prefixes that are
    /// dropped, trunk prefixes that are kept, variable national lengths.
    #[test]
    fn ten_national_plans_normalise_to_their_international_form() {
        let cases = [
            ("CI", "0700000000", "+2250700000000"),
            ("FR", "06 12 34 56 78", "+33612345678"),
            ("US", "(415) 555-0132", "+14155550132"),
            ("GB", "07911 123456", "+447911123456"),
            ("DE", "0151 23456789", "+4915123456789"),
            ("SN", "77 123 45 67", "+221771234567"),
            ("MA", "0612-345678", "+212612345678"),
            ("NG", "0803 123 4567", "+2348031234567"),
            ("IN", "09876543210", "+919876543210"),
            ("BR", "11 91234-5678", "+5511912345678"),
        ];

        for (region, national, expected) in cases {
            let validated = validate(national, None, with_region(region))
                .unwrap_or_else(|reason| panic!("{region} {national}: {reason}"));

            assert_eq!(validated.to_e164(), expected, "{region} {national}");
        }
    }

    /// The country column wins over the import-wide default, which is the
    /// whole reason the column exists.
    #[test]
    fn the_country_column_overrides_the_import_default() {
        let validated = validate("06 12 34 56 78", Some("FR"), with_region("CI")).expect("valid");

        assert_eq!(validated.to_e164(), "+33612345678");
    }

    /// …and a value carrying its own country code overrides both. This is the
    /// case that a "prepend the default region's dialling code" shortcut would
    /// have mangled into `+225 +33 …`.
    #[test]
    fn an_international_value_overrides_every_default() {
        for raw in ["+33612345678", "0033 6 12 34 56 78"] {
            let validated = validate(raw, Some("CI"), with_region("US")).expect("valid");

            assert_eq!(validated.to_e164(), "+33612345678", "{raw}");
        }
    }

    /// CA-009-05: each rejection names what to fix, and the four cases are
    /// genuinely distinguished rather than collapsed onto one "invalid".
    #[test]
    fn each_rejection_names_a_distinct_thing_to_fix() {
        assert_eq!(
            validate("   ", None, with_region("CI")).expect_err("empty"),
            RejectionReason::Empty
        );
        assert_eq!(
            validate("07", None, with_region("CI")).expect_err("short"),
            RejectionReason::TooShort
        );
        assert_eq!(
            validate("+999123456789", None, ValidationOptions::default()).expect_err("no country"),
            RejectionReason::UnknownCountryCode
        );
        assert_eq!(
            validate("0700000000", None, ValidationOptions::default()).expect_err("no region"),
            RejectionReason::MissingRegion
        );
        assert_eq!(
            validate("0700000000", Some("XX"), ValidationOptions::default())
                .expect_err("bad country column"),
            RejectionReason::UnknownRegion
        );
        // Ten digits, the right length for a North American number, and an
        // area code the plan does not allocate: length alone cannot reject it,
        // which is what makes it the `NotInPlan` case rather than another
        // spelling of "too short".
        assert_eq!(
            validate("(999) 555-0132", None, with_region("US")).expect_err("not in plan"),
            RejectionReason::NotInPlan
        );
    }

    /// CLAUDE.md §8: a rejection is shown next to a row, and must not turn the
    /// screen or the log into a second copy of the address book.
    #[test]
    fn a_rejection_never_echoes_the_number() {
        let reason = validate("+999123456789", None, ValidationOptions::default())
            .expect_err("unknown country");

        assert!(!reason.to_string().contains("123456789"));
    }

    #[test]
    fn every_rejection_reason_has_a_stable_distinct_code() {
        let mut codes: Vec<&str> = RejectionReason::ALL
            .iter()
            .map(|reason| reason.code())
            .collect();
        let total = codes.len();

        codes.sort_unstable();
        codes.dedup();

        assert_eq!(codes.len(), total, "two reasons share a code");
    }

    /// CA-009-06 in both directions: the landline is excluded, and the mobile
    /// of the same country still passes.
    #[test]
    fn mobiles_only_excludes_a_fixed_line_and_keeps_the_mobile() {
        let strict = ValidationOptions {
            default_region: Region::parse("FR"),
            mobiles_only: true,
        };

        assert_eq!(
            validate("01 42 68 53 00", None, strict).expect_err("landline"),
            RejectionReason::LineTypeExcluded
        );
        assert!(validate("06 12 34 56 78", None, strict).is_ok());
    }

    /// The same landline passes when the option is off — otherwise the test
    /// above would also pass with a validator that rejects every French
    /// landline for an unrelated reason.
    #[test]
    fn a_fixed_line_is_kept_when_the_option_is_off() {
        let validated = validate("01 42 68 53 00", None, with_region("FR")).expect("valid");

        assert_eq!(validated.line_type(), LineType::FixedLine);
    }

    /// North America does not split the two, so a "mobiles only" that compared
    /// against `Mobile` alone would drop every American number.
    #[test]
    fn mobiles_only_keeps_a_plan_that_does_not_split_the_two() {
        let strict = ValidationOptions {
            default_region: Region::parse("US"),
            mobiles_only: true,
        };

        let validated = validate("(415) 555-0132", None, strict).expect("kept");

        assert_eq!(validated.line_type(), LineType::FixedLineOrMobile);
    }

    #[test]
    fn a_region_code_is_parsed_case_insensitively_and_only_when_it_exists() {
        assert_eq!(Region::parse("ci").expect("known").code(), "CI");
        assert_eq!(Region::parse(" FR ").expect("known").code(), "FR");
        assert!(Region::parse("CIV").is_none());
        assert!(Region::parse("ZZ").is_none());
    }

    #[test]
    fn the_resolved_region_is_reported_back() {
        let validated = validate("+2250700000000", None, ValidationOptions::default())
            .expect("international form");

        assert_eq!(validated.region().as_ref().map(Region::code), Some("CI"));
    }
}
