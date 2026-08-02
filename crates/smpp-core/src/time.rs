//! The single instant format of the application, and the clock port.
//!
//! # Why this sits in `smpp-core`
//!
//! It arrived at milestone 002 inside `persistence`, where it was "the
//! database's timestamp format". Milestone 006 moved the message aggregate up
//! into `messaging`, so that crate could own its own repository port
//! (ADR 0010), and the aggregate carries five instants. Two crates on
//! different layers now need the *same* type — a second one would let a
//! `created_at` written by one be unreadable by the other — so it moved down
//! to the crate both already depend on.
//!
//! `persistence` re-exports it, so `persistence::Timestamp` still resolves.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::{FieldRejection, SmppError};

/// An instant, as stored in every `*_at` and `ts` column.
///
/// Spec §14.2 types those columns `TEXT`; step-002 §6 asks for one conversion
/// helper so a second format cannot appear. This type **is** that helper: it is
/// the only thing the repositories accept and return, and
/// [`Self::to_storage`] is the only function that produces the text SQLite
/// sees.
///
/// The stored form is RFC 3339 with a `Z` offset — a subset of ISO-8601 that
/// sorts lexicographically in the same order as chronologically, which is what
/// makes `ORDER BY created_at` mean anything.
///
/// # Why repositories never call [`Self::now`]
///
/// CLAUDE.md §7 requires a test to be deterministic, with the clock injected.
/// Rather than thread a [`Clock`] through four repositories, `persistence`
/// takes the simpler road: **a repository never reads the clock**. Every
/// timestamp arrives inside the record being written, so a test writes the
/// instants it chose and asserts on them exactly. The layers above, which do
/// mint fresh instants, take a [`Clock`] instead of calling [`Self::now`].
///
/// ```
/// use smpp_core::time::Timestamp;
///
/// let stored = Timestamp::parse("2026-07-26T12:00:00Z")?;
/// assert_eq!(stored.to_storage(), "2026-07-26T12:00:00Z");
/// # Ok::<(), smpp_core::SmppError>(())
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
    // the identifier newtypes: a silently defaulted timestamp in a struct
    // literal is a wrong `created_at` nobody notices.
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc().replace_nanosecond(0).unwrap_or(
            // INVARIANT: `replace_nanosecond` only rejects values above
            // 999_999_999; zero is always in range. The fallback is
            // unreachable and merely avoids an `expect` in production code.
            OffsetDateTime::now_utc(),
        ))
    }

    /// Wraps an instant a caller computed, truncated to the second and
    /// normalised to UTC.
    ///
    /// The counterpart of [`Self::as_offset_date_time`], and the only other way
    /// to build a value of this type. Milestone 010 needs it: the daily send
    /// window of CA-010-10 answers "when does the window next open", which is
    /// date arithmetic on an [`OffsetDateTime`], and the answer has to come back
    /// as the instant type the rest of the application speaks.
    ///
    /// Truncating here rather than at the call sites is what keeps the promise
    /// [`Self::now`] makes: no value of this type carries a sub-second
    /// component, so a round trip through the database never changes one.
    #[must_use]
    pub fn from_offset_date_time(instant: OffsetDateTime) -> Self {
        let utc = instant.to_offset(time::UtcOffset::UTC);

        Self(utc.replace_nanosecond(0).unwrap_or(utc))
    }

    /// Parses the text form held in the database.
    ///
    /// # Errors
    ///
    /// [`SmppError::InvalidField`] if the text is not RFC 3339. The offending
    /// value is deliberately absent from the error: a timestamp is not a
    /// secret, but the rule "no column value in an error message" is only
    /// worth anything if it has no exceptions.
    pub fn parse(raw: &str) -> Result<Self, SmppError> {
        OffsetDateTime::parse(raw, &Rfc3339)
            .map(|instant| Self(instant.to_offset(time::UtcOffset::UTC)))
            .map_err(|_| SmppError::invalid_field("timestamp", FieldRejection::MalformedTimestamp))
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

/// Where a layer that stamps records reads "now".
///
/// CLAUDE.md §7: a test must be deterministic, with the clock **injected**.
/// The write-ahead orchestrator of milestone 006 stamps `created_at`,
/// `sent_at` and `resp_at`, and a test that asserts on those cannot afford to
/// read the wall clock.
///
/// Deliberately not `async`: reading a clock does no I/O, and an async method
/// would force every call site into an `.await` for nothing.
pub trait Clock: Send + Sync {
    /// The current instant, truncated to the second.
    fn now(&self) -> Timestamp;
}

/// The clock that reads the operating system.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock as _, SystemClock, Timestamp};

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

    /// Milestone 010 computes on instants — "the next time the daily send
    /// window opens" — and has to hand the answer back as a [`Timestamp`].
    #[test]
    fn a_computed_instant_becomes_a_timestamp() {
        let instant = Timestamp::parse("2026-07-26T12:34:56Z").expect("valid RFC 3339");
        let later = *instant.as_offset_date_time() + time::Duration::hours(3);

        assert_eq!(
            Timestamp::from_offset_date_time(later).to_storage(),
            "2026-07-26T15:34:56Z"
        );
    }

    /// The stored form carries no sub-second component, so a value that kept
    /// one in memory would change on a round trip through the database — the
    /// same reason [`Timestamp::now`] truncates.
    #[test]
    fn a_computed_instant_is_truncated_to_the_second_and_normalised_to_utc() {
        let instant = time::OffsetDateTime::parse(
            "2026-07-26T14:34:56.789+02:00",
            &time::format_description::well_known::Rfc3339,
        )
        .expect("valid RFC 3339");

        assert_eq!(
            Timestamp::from_offset_date_time(instant).to_storage(),
            "2026-07-26T12:34:56Z"
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
    fn a_malformed_instant_is_rejected_without_being_echoed() {
        let rejection = Timestamp::parse("26/07/2026 12:34").expect_err("must be rejected");

        assert!(!rejection.to_string().contains("26/07/2026"));
    }

    #[test]
    fn the_system_clock_reads_a_plausible_instant() {
        let floor = Timestamp::parse("2020-01-01T00:00:00Z").expect("valid RFC 3339");

        assert!(SystemClock.now() > floor);
    }
}
