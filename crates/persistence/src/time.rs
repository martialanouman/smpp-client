//! The single timestamp format of the database.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::PersistenceError;

/// An instant, as stored in every `*_at` and `ts` column.
///
/// Spec §14.2 types those columns `TEXT`; step-002 §6 asks for one conversion
/// helper so a second format cannot appear. This type **is** that helper: it is
/// the only thing the repositories accept and return, and
/// [`Self::to_storage`] is the only function in the crate that produces the
/// text SQLite sees.
///
/// The stored form is RFC 3339 with a `Z` offset — a subset of ISO-8601 that
/// sorts lexicographically in the same order as chronologically, which is what
/// makes `ORDER BY created_at` mean anything.
///
/// # Why repositories never call [`Self::now`]
///
/// CLAUDE.md §7 requires a test to be deterministic, with the clock injected.
/// Rather than thread a `Clock` trait through four repositories, this crate
/// takes the simpler road: **a repository never reads the clock**. Every
/// timestamp arrives inside the record being written, so a test writes the
/// instants it chose and asserts on them exactly. [`Self::now`] exists for the
/// callers who legitimately mint a fresh instant, in the layers above.
///
/// ```
/// use persistence::Timestamp;
///
/// let stored = Timestamp::parse("2026-07-26T12:00:00Z")?;
/// assert_eq!(stored.to_storage(), "2026-07-26T12:00:00Z");
/// # Ok::<(), persistence::PersistenceError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// Reads the system clock.
    ///
    /// The value is truncated to the second: the stored form carries no
    /// sub-second component, so keeping one in memory would make a round trip
    /// through the database change the value.
    #[must_use]
    // clippy::new_without_default does not fire here (this is `now`, not
    // `new`), and `Default` is deliberately absent for the same reason as on
    // the identifier newtypes of `smpp-core`: a silently defaulted timestamp
    // in a struct literal is a wrong `created_at` nobody notices.
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc().replace_nanosecond(0).unwrap_or(
            // INVARIANT: `replace_nanosecond` only rejects values above
            // 999_999_999; zero is always in range. The fallback is
            // unreachable and merely avoids an `expect` in production code.
            OffsetDateTime::now_utc(),
        ))
    }

    /// Parses the text form held in the database.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::MalformedRow`] if the text is not RFC 3339. The
    /// offending value is deliberately absent from the error: a timestamp is
    /// not a secret, but the rule "no column value in an error message" is
    /// only worth anything if it has no exceptions.
    pub fn parse(raw: &str) -> Result<Self, PersistenceError> {
        OffsetDateTime::parse(raw, &Rfc3339)
            .map(|instant| Self(instant.to_offset(time::UtcOffset::UTC)))
            .map_err(|_| PersistenceError::MalformedRow {
                table: "any",
                column: "a timestamp column",
                expected: "an RFC 3339 instant such as 2026-07-26T12:00:00Z",
            })
    }

    /// Renders the text form written to the database.
    ///
    /// Falls back to the Unix epoch on a formatting failure, which
    /// [`OffsetDateTime`] only produces for years outside the four-digit
    /// range — unrepresentable here, since every value comes from the system
    /// clock or from [`Self::parse`].
    #[must_use]
    pub fn to_storage(&self) -> String {
        self.0
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
    }

    /// The underlying instant, for callers that need to compute on it.
    #[must_use]
    pub const fn as_offset_date_time(&self) -> &OffsetDateTime {
        &self.0
    }
}

impl core::fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.to_storage())
    }
}

#[cfg(test)]
mod tests {
    use super::Timestamp;

    #[test]
    fn a_stored_instant_survives_a_round_trip() {
        let parsed = Timestamp::parse("2026-07-26T12:34:56Z").expect("valid RFC 3339");

        assert_eq!(parsed.to_storage(), "2026-07-26T12:34:56Z");
    }

    #[test]
    fn an_offset_instant_is_normalised_to_utc() {
        let parsed = Timestamp::parse("2026-07-26T14:34:56+02:00").expect("valid RFC 3339");

        assert_eq!(parsed.to_storage(), "2026-07-26T12:34:56Z");
    }

    #[test]
    fn the_clock_reading_survives_a_round_trip() {
        let now = Timestamp::now();

        assert_eq!(
            Timestamp::parse(&now.to_storage()).expect("own output parses"),
            now
        );
    }

    #[test]
    fn the_stored_form_sorts_chronologically_as_text() {
        let earlier = Timestamp::parse("2026-07-26T12:34:56Z").expect("valid RFC 3339");
        let later = Timestamp::parse("2026-07-26T12:34:57Z").expect("valid RFC 3339");

        assert!(earlier.to_storage() < later.to_storage());
        assert!(earlier < later);
    }

    #[test]
    fn a_malformed_instant_is_rejected() {
        assert!(Timestamp::parse("26/07/2026 12:34").is_err());
    }
}
