//! Column mapping and reusable import profiles (deliverable L-009-03).
//!
//! A customer file names its columns whatever it likes, in whatever order, and
//! sometimes not at all. The mapping is the one place that says "the number is
//! in *that* column", and everything downstream works on positions.
//!
//! # Names or positions, and why both exist
//!
//! A file with headers is mapped by [`ColumnRef::Name`], which survives a
//! reordering of the columns — the same profile keeps working when the customer
//! moves a column (CA-009-09). A file **without** headers has nothing to name,
//! so [`ColumnRef::Index`] is the only option there.
//!
//! Matching a name is case- and accent-tolerant in the sense that matters:
//! trimmed, case-folded, and with a UTF-8 BOM already removed by the reader.
//! Anything cleverer — fuzzy matching, synonyms — belongs to
//! [`ColumnMapping::detect`], which *suggests* a mapping the operator confirms,
//! rather than to the resolution, which must be predictable.

use serde::{Deserialize, Serialize};
use smpp_core::time::Timestamp;

use crate::model::ProfileId;

/// Which column a role reads from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "by", content = "value")]
pub enum ColumnRef {
    /// By header name, matched case-insensitively after trimming.
    Name(String),
    /// By zero-based position, for a file with no header row.
    Index(usize),
}

impl ColumnRef {
    /// Resolves the reference against the header row, or against the width of
    /// the first data row when the file has none.
    fn resolve(&self, headers: Option<&[String]>, width: usize) -> Result<usize, MappingError> {
        match self {
            Self::Name(name) => {
                let headers = headers.ok_or_else(|| MappingError::NoHeaderRow {
                    column: name.clone(),
                })?;

                headers
                    .iter()
                    .position(|header| normalise(header) == normalise(name))
                    .ok_or_else(|| MappingError::UnknownColumn {
                        column: name.clone(),
                    })
            }
            Self::Index(index) => {
                if *index < width {
                    Ok(*index)
                } else {
                    Err(MappingError::ColumnOutOfRange {
                        index: *index,
                        width,
                    })
                }
            }
        }
    }

    /// How the interface shows the reference.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Name(name) => name.clone(),
            Self::Index(index) => index.to_string(),
        }
    }
}

/// One free attribute column, usable as a template variable (spec §11.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeColumn {
    /// Name of the variable a template refers to.
    pub variable: String,
    /// Where its value comes from.
    pub column: ColumnRef,
}

/// What each column of the file means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnMapping {
    /// The recipient number. The only mandatory role.
    pub msisdn: ColumnRef,
    /// ISO 3166-1 alpha-2 country, when the file carries one.
    pub country: Option<ColumnRef>,
    /// Free columns kept as template variables.
    #[serde(default)]
    pub attributes: Vec<AttributeColumn>,
}

/// Why a mapping could not be applied to a file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MappingError {
    /// The mapping names a column the file does not have.
    #[error("the file has no column named {column:?}")]
    UnknownColumn {
        /// The name that was looked for.
        column: String,
    },

    /// The mapping names a column, but the file was read without headers.
    #[error(
        "the file was read without a header row, so column {column:?} cannot be found by name"
    )]
    NoHeaderRow {
        /// The name that was looked for.
        column: String,
    },

    /// The mapping points past the last column.
    #[error("column {index} does not exist: the file has {width}")]
    ColumnOutOfRange {
        /// The position that was asked for.
        index: usize,
        /// How many columns the file actually has.
        width: usize,
    },

    /// Two roles were pointed at the same column.
    #[error("the number column and the country column cannot be the same column")]
    OverlappingRoles,
}

impl ColumnMapping {
    /// Header names commonly used for the recipient number, normalised.
    ///
    /// Order matters: the first match wins, so the unambiguous names come
    /// before `tel`, which also prefixes `telecopie` and would otherwise claim
    /// a fax column.
    const NUMBER_ALIASES: &'static [&'static str] = &[
        "msisdn",
        "telephone",
        "phone",
        "phonenumber",
        "phone_number",
        "numero",
        "number",
        "mobile",
        "portable",
        "gsm",
        "cell",
        "tel",
    ];

    /// Header names commonly used for the country.
    const COUNTRY_ALIASES: &'static [&'static str] = &["country", "pays", "iso", "region"];

    /// Maps a file by the position of its number column.
    #[must_use]
    pub const fn by_index(msisdn: usize) -> Self {
        Self {
            msisdn: ColumnRef::Index(msisdn),
            country: None,
            attributes: Vec::new(),
        }
    }

    /// Maps a file by the name of its number column.
    #[must_use]
    pub fn by_name(msisdn: impl Into<String>) -> Self {
        Self {
            msisdn: ColumnRef::Name(msisdn.into()),
            country: None,
            attributes: Vec::new(),
        }
    }

    /// The same mapping, reading the country from a column.
    #[must_use]
    pub fn with_country(mut self, column: ColumnRef) -> Self {
        self.country = Some(column);
        self
    }

    /// The same mapping, keeping one more column as a template variable.
    #[must_use]
    pub fn with_attribute(mut self, variable: impl Into<String>, column: ColumnRef) -> Self {
        self.attributes.push(AttributeColumn {
            variable: variable.into(),
            column,
        });
        self
    }

    /// Suggests a mapping from a header row.
    ///
    /// A **suggestion**, which the assistant shows and the operator confirms
    /// (spec §11.4). It never guesses the number column from the *data*: a
    /// column of five-digit customer identifiers looks exactly like a column of
    /// short codes, and silently importing the wrong one is worse than asking.
    ///
    /// Returns `None` when no header resembles a number column, which is the
    /// assistant's cue to ask rather than to propose.
    #[must_use]
    pub fn detect(headers: &[String]) -> Option<Self> {
        let msisdn = Self::NUMBER_ALIASES.iter().find_map(|alias| {
            headers
                .iter()
                .position(|header| normalise(header) == *alias)
        })?;

        let country = Self::COUNTRY_ALIASES.iter().find_map(|alias| {
            headers
                .iter()
                .position(|header| normalise(header) == *alias)
        });

        let attributes = headers
            .iter()
            .enumerate()
            .filter(|(index, header)| {
                *index != msisdn && Some(*index) != country && !header.trim().is_empty()
            })
            .map(|(_, header)| AttributeColumn {
                variable: header.trim().to_owned(),
                column: ColumnRef::Name(header.trim().to_owned()),
            })
            .collect();

        Some(Self {
            msisdn: ColumnRef::Name(headers.get(msisdn)?.trim().to_owned()),
            country: country
                .and_then(|index| headers.get(index))
                .map(|header| ColumnRef::Name(header.trim().to_owned())),
            attributes,
        })
    }

    /// Turns names and positions into positions, once, for the whole file.
    ///
    /// # Errors
    ///
    /// [`MappingError`] naming the column that could not be found. The message
    /// carries a **header name**, never a cell value: a header is structure, a
    /// cell is personal data.
    pub fn resolve(
        &self,
        headers: Option<&[String]>,
        width: usize,
    ) -> Result<ResolvedMapping, MappingError> {
        let msisdn = self.msisdn.resolve(headers, width)?;

        let country = self
            .country
            .as_ref()
            .map(|column| column.resolve(headers, width))
            .transpose()?;

        if country == Some(msisdn) {
            return Err(MappingError::OverlappingRoles);
        }

        let attributes = self
            .attributes
            .iter()
            .map(|attribute| {
                attribute
                    .column
                    .resolve(headers, width)
                    .map(|index| (attribute.variable.clone(), index))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ResolvedMapping {
            msisdn,
            country,
            attributes,
        })
    }
}

/// A mapping reduced to column positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMapping {
    msisdn: usize,
    country: Option<usize>,
    attributes: Vec<(String, usize)>,
}

impl ResolvedMapping {
    /// The cell holding the number, or `""` when the row is short.
    ///
    /// A short row is ordinary in a hand-edited CSV — trailing empty columns
    /// are simply absent — and must produce an empty value rather than a panic.
    #[must_use]
    pub fn msisdn<'row>(&self, row: &'row [String]) -> &'row str {
        row.get(self.msisdn).map_or("", String::as_str)
    }

    /// Whether the number cell of this row is one the reader could not render.
    ///
    /// Only ever true for a spreadsheet; a CSV cell is text by construction.
    #[must_use]
    pub fn holds_unreadable_number(&self, row: &crate::import::RawRow) -> bool {
        row.unreadable.contains(&self.msisdn)
    }

    /// The cell holding the country, when the mapping declares one.
    #[must_use]
    pub fn country<'row>(&self, row: &'row [String]) -> Option<&'row str> {
        self.country
            .map(|index| row.get(index).map_or("", String::as_str))
    }

    /// The attribute columns, as a JSON object, or `None` when there are none.
    ///
    /// `None` rather than `{}`: an empty object in the column would make every
    /// contact of an attribute-less import carry a byte of noise, and would
    /// make "has attributes" a string comparison downstream.
    #[must_use]
    pub fn attributes(&self, row: &[String]) -> Option<String> {
        if self.attributes.is_empty() {
            return None;
        }

        let object = self
            .attributes
            .iter()
            .map(|(variable, index)| {
                (
                    variable.clone(),
                    serde_json::Value::String(row.get(*index).cloned().unwrap_or_default()),
                )
            })
            .collect::<serde_json::Map<_, _>>();

        serde_json::to_string(&serde_json::Value::Object(object)).ok()
    }
}

/// A saved mapping, reusable on the next file of the same shape (CA-009-09).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportProfile {
    /// Primary key.
    pub profile_id: ProfileId,
    /// Name the operator gave it.
    pub name: String,
    /// The mapping itself.
    pub mapping: ColumnMapping,
    /// When the profile was saved.
    pub created_at: Timestamp,
}

impl ImportProfile {
    /// Renders the mapping as the opaque JSON document the store keeps.
    ///
    /// Storage keeps the mapping as text, exactly like `contacts.attributes`
    /// and `session_profiles.tls_config` (spec §14.2): the shape belongs to
    /// this crate, and a schema that mirrored it would need a migration every
    /// time a role is added.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] — unreachable in practice, since every field is a
    /// string or a number, and returned rather than unwrapped because an
    /// `expect` here would be a `panic!` in production code.
    pub fn mapping_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.mapping)
    }

    /// Rebuilds a profile from the stored document.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] if the stored text is not a mapping this version
    /// understands.
    pub fn from_stored(
        profile_id: ProfileId,
        name: String,
        mapping_json: &str,
        created_at: Timestamp,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            profile_id,
            name,
            mapping: serde_json::from_str(mapping_json)?,
            created_at,
        })
    }
}

/// Case-folds, trims and de-accents a header for comparison.
///
/// Three things, each earning its place in a real customer file:
///
/// * spaces, underscores, dashes and dots are dropped, so `Phone Number`,
///   `phone_number` and `phonenumber` are one column;
/// * case is folded;
/// * the Latin-1 accented letters are folded onto their base letter, so
///   `Telephone` spelled with accents matches `telephone`. Without this the
///   alias list would have to carry every accented spelling of every language,
///   and the spelling a French file actually uses would not be detected at all.
///
/// A full Unicode normalisation (NFD plus combining-mark removal) would be more
/// general and would mean another dependency; the Latin-1 range covers the
/// languages this application ships in and the ones its market writes.
fn normalise(header: &str) -> String {
    header
        .trim()
        .chars()
        .filter(|character| !matches!(character, ' ' | '_' | '-' | '.'))
        .flat_map(char::to_lowercase)
        .map(fold_accent)
        .collect()
}

/// Folds one accented Latin-1 letter onto its base letter.
const fn fold_accent(character: char) -> char {
    match character {
        '\u{e0}' | '\u{e1}' | '\u{e2}' | '\u{e3}' | '\u{e4}' | '\u{e5}' => 'a',
        '\u{e7}' => 'c',
        '\u{e8}' | '\u{e9}' | '\u{ea}' | '\u{eb}' => 'e',
        '\u{ec}' | '\u{ed}' | '\u{ee}' | '\u{ef}' => 'i',
        '\u{f1}' => 'n',
        '\u{f2}' | '\u{f3}' | '\u{f4}' | '\u{f5}' | '\u{f6}' => 'o',
        '\u{f9}' | '\u{fa}' | '\u{fb}' | '\u{fc}' => 'u',
        '\u{fd}' | '\u{ff}' => 'y',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{ColumnMapping, ColumnRef, ImportProfile, MappingError};
    use crate::model::ProfileId;
    use smpp_core::time::Timestamp;

    fn headers(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn row(values: &[&str]) -> Vec<String> {
        headers(values)
    }

    #[test]
    fn a_named_column_is_matched_whatever_its_case_and_spacing() {
        let mapping = ColumnMapping::by_name("phone number");

        let resolved = mapping
            .resolve(Some(&headers(&["Nom", "Phone_Number"])), 2)
            .expect("resolves");

        assert_eq!(
            resolved.msisdn(&row(&["Awa", "+2250700000000"])),
            "+2250700000000"
        );
    }

    /// CA-009-09: the saved profile keeps working when the customer reorders
    /// the columns, which is the whole difference between a name and an index.
    #[test]
    fn a_named_mapping_survives_a_reordering_of_the_columns() {
        let mapping = ColumnMapping::by_name("telephone")
            .with_country(ColumnRef::Name(String::from("pays")))
            .with_attribute("prenom", ColumnRef::Name(String::from("prenom")));

        let first = mapping
            .resolve(Some(&headers(&["telephone", "pays", "prenom"])), 3)
            .expect("resolves");
        let reordered = mapping
            .resolve(Some(&headers(&["prenom", "telephone", "pays"])), 3)
            .expect("resolves");

        assert_eq!(first.msisdn(&row(&["+225070", "CI", "Awa"])), "+225070");
        assert_eq!(reordered.msisdn(&row(&["Awa", "+225070", "CI"])), "+225070");
        assert_eq!(
            reordered.country(&row(&["Awa", "+225070", "CI"])),
            Some("CI")
        );
    }

    #[test]
    fn a_missing_column_names_itself_without_echoing_a_cell() {
        let error = ColumnMapping::by_name("telephone")
            .resolve(Some(&headers(&["nom", "ville"])), 2)
            .expect_err("absent");

        assert_eq!(
            error,
            MappingError::UnknownColumn {
                column: String::from("telephone")
            }
        );
    }

    #[test]
    fn a_named_column_cannot_be_resolved_against_a_headerless_file() {
        let error = ColumnMapping::by_name("telephone")
            .resolve(None, 3)
            .expect_err("no headers");

        assert!(matches!(error, MappingError::NoHeaderRow { .. }));
    }

    #[test]
    fn a_position_past_the_last_column_is_rejected_rather_than_read_as_empty() {
        let error = ColumnMapping::by_index(4)
            .resolve(None, 3)
            .expect_err("out of range");

        assert_eq!(error, MappingError::ColumnOutOfRange { index: 4, width: 3 });
    }

    #[test]
    fn the_number_and_country_roles_may_not_share_a_column() {
        let error = ColumnMapping::by_index(0)
            .with_country(ColumnRef::Index(0))
            .resolve(None, 2)
            .expect_err("overlap");

        assert_eq!(error, MappingError::OverlappingRoles);
    }

    /// A hand-edited CSV drops trailing empty columns; the row is then shorter
    /// than the header. That must read as empty, not panic.
    #[test]
    fn a_row_shorter_than_the_header_reads_as_empty() {
        let resolved = ColumnMapping::by_index(0)
            .with_country(ColumnRef::Index(2))
            .resolve(None, 3)
            .expect("resolves");

        assert_eq!(resolved.country(&row(&["+225070"])), Some(""));
    }

    #[test]
    fn detection_proposes_the_number_column_and_keeps_the_rest_as_variables() {
        let mapping =
            ColumnMapping::detect(&headers(&["Prénom", "Téléphone", "Pays"])).expect("detected");

        assert_eq!(mapping.msisdn, ColumnRef::Name(String::from("Téléphone")));
        assert_eq!(mapping.country, Some(ColumnRef::Name(String::from("Pays"))));
        assert_eq!(mapping.attributes.len(), 1);
        assert_eq!(mapping.attributes[0].variable, "Prénom");
    }

    /// `tel` must not claim a fax column. The alias order is what makes this
    /// hold, so this is the test that breaks if someone sorts the list.
    /// A French file says the word with accents, and an alias list that had to
    /// carry every accented spelling would miss the next one.
    #[test]
    fn an_accented_header_matches_its_unaccented_alias() {
        let file = headers(&["Nom", "T\u{e9}l\u{e9}phone"]);

        let mapping = ColumnMapping::detect(&file).expect("detected");
        assert_eq!(
            mapping.msisdn,
            ColumnRef::Name(String::from("T\u{e9}l\u{e9}phone"))
        );

        let resolved = ColumnMapping::by_name("telephone")
            .resolve(Some(&file), 2)
            .expect("resolves");
        assert_eq!(resolved.msisdn(&row(&["Awa", "+225070"])), "+225070");
    }

    #[test]
    fn detection_prefers_an_unambiguous_header_over_a_prefix_match() {
        let mapping = ColumnMapping::detect(&headers(&["Telecopie", "Mobile"])).expect("detected");

        assert_eq!(mapping.msisdn, ColumnRef::Name(String::from("Mobile")));
    }

    #[test]
    fn detection_declines_rather_than_guessing_when_no_header_looks_like_a_number() {
        assert!(ColumnMapping::detect(&headers(&["nom", "ville", "code"])).is_none());
    }

    #[test]
    fn attributes_become_a_json_object_and_absent_ones_become_nothing() {
        let with = ColumnMapping::by_index(0)
            .with_attribute("prenom", ColumnRef::Index(1))
            .resolve(None, 2)
            .expect("resolves");

        assert_eq!(
            with.attributes(&row(&["+225070", "Awa"])),
            Some(String::from(r#"{"prenom":"Awa"}"#))
        );

        let without = ColumnMapping::by_index(0)
            .resolve(None, 1)
            .expect("resolves");

        assert_eq!(without.attributes(&row(&["+225070"])), None);
    }

    /// CA-009-09: what is saved is what comes back.
    #[test]
    fn a_profile_survives_a_round_trip_through_its_stored_document() {
        let profile = ImportProfile {
            profile_id: ProfileId::new(),
            name: String::from("fichier client"),
            mapping: ColumnMapping::by_name("telephone")
                .with_country(ColumnRef::Name(String::from("pays")))
                .with_attribute("prenom", ColumnRef::Index(2)),
            created_at: Timestamp::now(),
        };

        let stored = profile.mapping_json().expect("serialises");
        let restored = ImportProfile::from_stored(
            profile.profile_id,
            profile.name.clone(),
            &stored,
            profile.created_at,
        )
        .expect("deserialises");

        assert_eq!(restored, profile);
    }
}
