//! When a campaign is allowed to send (spec §10.2, CA-010-10).
//!
//! Two settings, and they compose: a **deferred start** — an instant before
//! which nothing goes out — and a **daily window**, the hours of the day during
//! which sending is allowed. A campaign with neither sends at once.
//!
//! # Pure, and answered against an injected instant
//!
//! Nothing here reads a clock or sleeps. [`Schedule::decide`] takes the current
//! instant and returns either "send" or "wait until this instant"; the runner
//! owns the clock and the cancellation token that has to interrupt the wait
//! (CA-010-09). That is what makes CA-010-10 testable at all: the midnight
//! crossing and the time-zone cases below are twelve assertions, not twelve
//! hours.
//!
//! # A fixed offset, not a named zone
//!
//! [`DailyWindow`] carries a [`UtcOffset`], so `08:00–20:00` at `+00:00` is a
//! window on Abidjan and `+02:00` one on Lagos. It is **not** a named zone, and
//! the consequence is stated rather than glossed over: a window configured in a
//! zone that observes daylight saving shifts by an hour twice a year, and the
//! operator has to move it.
//!
//! Doing better means shipping the IANA database — `time-tz` or `chrono-tz`,
//! several megabytes of tables — for a campaign setting whose whole purpose is
//! "do not text people at three in the morning", where an hour of drift twice a
//! year is not the failure that matters. CLAUDE.md §2 asks for a new dependency
//! to be justified, and this one is not.

use core::time::Duration;

use smpp_core::time::Timestamp;
use time::{Time, UtcOffset};

/// Why a schedule was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ScheduleError {
    /// The window opens and closes at the same time of day.
    ///
    /// `08:00–08:00` reads as "never" and as "the whole day" equally well, and
    /// an operator who typed it meant one of them. *Parse, don't validate*
    /// (CLAUDE.md §4): it is refused where the setting comes in, so no
    /// [`DailyWindow`] that exists is ambiguous.
    ///
    /// **Where an operator who wanted "all day" lands:** they ask for no window
    /// at all — a [`Schedule`] built with [`Schedule::immediate`] and never
    /// given [`Schedule::within`] allows sending at every hour. A twenty-four
    /// hour [`DailyWindow`] is deliberately inexpressible, because the only way
    /// to write one is the pair this refuses.
    #[error("a daily window opens and closes at the same time of day")]
    EmptyWindow,

    /// One of the two ends is not an `HH:MM` time of day.
    ///
    /// Refused here rather than in the layer that reads the form, so the
    /// interface has nothing to get out of step with (CLAUDE.md §4: *parse,
    /// don't validate*, at the one point where the outside gets in).
    #[error("a daily window is bounded by two HH:MM times of day")]
    MalformedTime,

    /// The zone offset is not one a time zone can have.
    ///
    /// The real range is `-12:00` (Baker Island) to `+14:00` (Kiribati), and
    /// the bound is the real one rather than what the underlying type happens
    /// to accept: `time` would take `+18:00` without complaint, and an operator
    /// who meant `+08:00` and typed minutes instead of hours would get a window
    /// silently ten hours out.
    #[error("a time-zone offset lies between -720 and +840 minutes")]
    OffsetOutOfRange,
}

/// What a campaign is allowed to do at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScheduleDecision {
    /// Sending is allowed now.
    Send,
    /// Nothing may go out before this instant.
    ///
    /// Strictly in the future — see the test that holds every decision to it.
    /// A decision naming an instant already past would put the runner in a
    /// zero-length sleep it never leaves.
    WaitUntil(Timestamp),
}

/// The hours of the day during which a campaign may send.
///
/// # Bounds
///
/// The opening bound is inclusive, the closing one is not: `08:00–20:00`
/// includes 08:00:00 and excludes 20:00:00. Anything else makes two adjacent
/// windows overlap on one second.
///
/// # Crossing midnight
///
/// `22:00–06:00` is a night window, and it is not a mistake to be corrected: it
/// is what an operator buys the night rate for. So the containment test is not
/// `open <= t < close` — which for that pair is empty — but a disjunction, and
/// the module's tests pin both readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyWindow {
    open: Time,
    close: Time,
    offset: UtcOffset,
}

impl DailyWindow {
    /// Builds a window from its two ends and the zone they are read in.
    ///
    /// # Errors
    ///
    /// [`ScheduleError::EmptyWindow`] when the two ends are the same time of
    /// day.
    pub fn new(open: Time, close: Time, offset: UtcOffset) -> Result<Self, ScheduleError> {
        if open == close {
            return Err(ScheduleError::EmptyWindow);
        }

        Ok(Self {
            open,
            close,
            offset,
        })
    }

    /// Builds a window from two `HH:MM` times and a zone offset in minutes.
    ///
    /// What the campaign form sends: two text fields and a signed number of
    /// minutes east of UTC (`0` for Abidjan, `60` for Lagos, `-300` for New
    /// York). Seconds are deliberately not accepted — a send window is an hour
    /// policy, and `08:00:30` is a typing accident, not an intention.
    ///
    /// # Errors
    ///
    /// [`ScheduleError::MalformedTime`] when an end is not `HH:MM` or names an
    /// hour or a minute that does not exist, [`ScheduleError::OffsetOutOfRange`]
    /// when the offset is not one a zone can have, and
    /// [`ScheduleError::EmptyWindow`] when the two ends are the same.
    pub fn parse(open: &str, close: &str, offset_minutes: i32) -> Result<Self, ScheduleError> {
        if !(Self::MIN_OFFSET_MINUTES..=Self::MAX_OFFSET_MINUTES).contains(&offset_minutes) {
            return Err(ScheduleError::OffsetOutOfRange);
        }

        // TRUNCATING division, not Euclidean. `UtcOffset::from_hms` refuses a
        // pair whose parts disagree in sign, and `-270` is `-04:30`: Euclidean
        // arithmetic yields `(-5, 30)`, which is both refused and, read
        // literally, a different offset.
        let hours = offset_minutes / 60;
        let minutes = offset_minutes % 60;

        // INVARIANT: the range check above bounds `hours` to -12..=14 and
        // `minutes` to -59..=59, both of which fit an `i8`, and `from_hms`
        // accepts up to ±25:59:59.
        let offset = i8::try_from(hours)
            .ok()
            .zip(i8::try_from(minutes).ok())
            .and_then(|(hours, minutes)| UtcOffset::from_hms(hours, minutes, 0).ok())
            .ok_or(ScheduleError::OffsetOutOfRange)?;

        Self::new(parse_time(open)?, parse_time(close)?, offset)
    }

    /// Westernmost zone offset, in minutes: `-12:00`, Baker Island.
    pub const MIN_OFFSET_MINUTES: i32 = -720;

    /// Easternmost zone offset, in minutes: `+14:00`, Kiribati.
    pub const MAX_OFFSET_MINUTES: i32 = 840;

    /// The time of day sending starts.
    #[must_use]
    pub const fn open(&self) -> Time {
        self.open
    }

    /// The time of day sending stops.
    #[must_use]
    pub const fn close(&self) -> Time {
        self.close
    }

    /// The zone the two ends are read in.
    #[must_use]
    pub const fn offset(&self) -> UtcOffset {
        self.offset
    }

    /// Whether `instant` falls inside the window.
    #[must_use]
    pub fn contains(&self, instant: Timestamp) -> bool {
        let local = self.local_time(instant);

        if self.open < self.close {
            local >= self.open && local < self.close
        } else {
            // Crossing midnight: the window is the *complement* of the closed
            // hours, so the two halves are joined by an `or` rather than cut by
            // an `and`.
            local >= self.open || local < self.close
        }
    }

    /// The first instant at or after `instant` at which the window is open.
    ///
    /// Equal to `instant` when it is already inside.
    #[must_use]
    pub fn next_opening(&self, instant: Timestamp) -> Timestamp {
        if self.contains(instant) {
            return instant;
        }

        // Computed on the LOCAL date, never the UTC one: at `-05:00`, 02:00 UTC
        // on the 26th is 21:00 on the 25th, and an opening computed from the
        // 26th would be a day late.
        let local = instant.as_offset_date_time().to_offset(self.offset);
        let candidate = local.replace_time(self.open);

        let candidate = if candidate > local {
            candidate
        } else {
            candidate.saturating_add(time::Duration::DAY)
        };

        Timestamp::from_offset_date_time(candidate)
    }

    /// The time of day `instant` falls on, in this window's own zone.
    fn local_time(&self, instant: Timestamp) -> Time {
        instant.as_offset_date_time().to_offset(self.offset).time()
    }
}

/// Reads one `HH:MM` end of a window.
///
/// Hand-written rather than a `time` format description, because the rejections
/// have to be exactly as narrow as the doc above claims: `8:00`, `08:00:00` and
/// `08h00` are all refused, and a two-field split over `:` says so in four
/// lines.
fn parse_time(raw: &str) -> Result<Time, ScheduleError> {
    let (hours, minutes) = raw.split_once(':').ok_or(ScheduleError::MalformedTime)?;

    if hours.len() != 2 || minutes.len() != 2 {
        return Err(ScheduleError::MalformedTime);
    }

    let hours: u8 = hours.parse().map_err(|_| ScheduleError::MalformedTime)?;
    let minutes: u8 = minutes.parse().map_err(|_| ScheduleError::MalformedTime)?;

    Time::from_hms(hours, minutes, 0).map_err(|_| ScheduleError::MalformedTime)
}

/// The optional planning of a campaign (spec §10.2).
///
/// ```
/// use messaging::campaign::schedule::{Schedule, ScheduleDecision};
/// use smpp_core::time::Timestamp;
///
/// let noon = Timestamp::parse("2026-07-26T12:00:00Z")?;
/// let evening = Timestamp::parse("2026-07-26T18:00:00Z")?;
///
/// assert_eq!(Schedule::immediate().decide(noon), ScheduleDecision::Send);
/// assert_eq!(
///     Schedule::immediate().starting_at(evening).decide(noon),
///     ScheduleDecision::WaitUntil(evening),
/// );
/// # Ok::<(), smpp_core::SmppError>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Schedule {
    start_at: Option<Timestamp>,
    window: Option<DailyWindow>,
}

impl Schedule {
    /// No planning at all: the campaign sends as soon as it is started.
    #[must_use]
    pub const fn immediate() -> Self {
        Self {
            start_at: None,
            window: None,
        }
    }

    /// The same schedule, held back until `instant`.
    #[must_use]
    pub const fn starting_at(mut self, instant: Timestamp) -> Self {
        self.start_at = Some(instant);
        self
    }

    /// The same schedule, restricted to a daily window.
    #[must_use]
    pub const fn within(mut self, window: DailyWindow) -> Self {
        self.window = Some(window);
        self
    }

    /// The instant before which nothing goes out, when there is one.
    #[must_use]
    pub const fn start_at(&self) -> Option<Timestamp> {
        self.start_at
    }

    /// The daily window, when there is one.
    #[must_use]
    pub const fn window(&self) -> Option<DailyWindow> {
        self.window
    }

    /// What the campaign may do at `now`.
    ///
    /// The two settings compose in the only order that respects both: the
    /// earliest instant the campaign could start is `max(now, start_at)`, and
    /// the answer is the first opening of the window at or after that.
    #[must_use]
    pub fn decide(&self, now: Timestamp) -> ScheduleDecision {
        let earliest = match self.start_at {
            Some(start_at) if start_at > now => start_at,
            _ => now,
        };

        let opening = match self.window {
            Some(window) => window.next_opening(earliest),
            None => earliest,
        };

        if opening > now {
            ScheduleDecision::WaitUntil(opening)
        } else {
            ScheduleDecision::Send
        }
    }

    /// How long the campaign has to wait at `now`, or `None` to send.
    ///
    /// What the runner sleeps on — under a cancellation token, since a campaign
    /// deferred to tomorrow morning must still be cancellable this afternoon
    /// (CA-010-09).
    #[must_use]
    pub fn wait_for(&self, now: Timestamp) -> Option<Duration> {
        match self.decide(now) {
            ScheduleDecision::Send => None,
            ScheduleDecision::WaitUntil(instant) => {
                let gap = *instant.as_offset_date_time() - *now.as_offset_date_time();

                // `WaitUntil` is strictly in the future, so the conversion
                // cannot fail; a negative gap would mean the decision lied, and
                // the answer to that is "do not wait", not a panic.
                gap.try_into().ok()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DailyWindow, Schedule, ScheduleDecision, ScheduleError};
    use core::time::Duration;
    use smpp_core::time::Timestamp;
    use time::{Time, UtcOffset};

    fn at(raw: &str) -> Timestamp {
        Timestamp::parse(raw).expect("the fixture is RFC 3339")
    }

    fn window(open: (u8, u8), close: (u8, u8), offset_hours: i8) -> DailyWindow {
        DailyWindow::new(
            Time::from_hms(open.0, open.1, 0).expect("a valid time of day"),
            Time::from_hms(close.0, close.1, 0).expect("a valid time of day"),
            UtcOffset::from_hms(offset_hours, 0, 0).expect("a valid offset"),
        )
        .expect("the two ends differ")
    }

    #[test]
    fn a_campaign_with_no_schedule_starts_at_once() {
        assert_eq!(
            Schedule::immediate().decide(at("2026-07-26T12:00:00Z")),
            ScheduleDecision::Send
        );
    }

    #[test]
    fn a_deferred_start_holds_the_campaign_until_its_instant() {
        let schedule = Schedule::immediate().starting_at(at("2026-07-26T18:00:00Z"));

        assert_eq!(
            schedule.decide(at("2026-07-26T12:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-26T18:00:00Z"))
        );
    }

    #[test]
    fn a_deferred_start_already_past_sends_at_once() {
        let schedule = Schedule::immediate().starting_at(at("2026-07-26T06:00:00Z"));

        assert_eq!(
            schedule.decide(at("2026-07-26T12:00:00Z")),
            ScheduleDecision::Send
        );
        assert_eq!(
            schedule.decide(at("2026-07-26T06:00:00Z")),
            ScheduleDecision::Send
        );
    }

    #[test]
    fn an_instant_inside_the_window_sends() {
        let schedule = Schedule::immediate().within(window((8, 0), (20, 0), 0));

        assert_eq!(
            schedule.decide(at("2026-07-26T12:00:00Z")),
            ScheduleDecision::Send
        );
    }

    /// The opening bound is inclusive and the closing one is not: a window
    /// written `08:00–20:00` is what an operator reads as "during the day", and
    /// 20:00:00 is the first second of the evening.
    #[test]
    fn the_opening_bound_is_inclusive_and_the_closing_one_is_not() {
        let schedule = Schedule::immediate().within(window((8, 0), (20, 0), 0));

        assert_eq!(
            schedule.decide(at("2026-07-26T08:00:00Z")),
            ScheduleDecision::Send
        );
        assert_eq!(
            schedule.decide(at("2026-07-26T19:59:59Z")),
            ScheduleDecision::Send
        );
        assert_eq!(
            schedule.decide(at("2026-07-26T20:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-27T08:00:00Z"))
        );
    }

    #[test]
    fn an_instant_before_the_window_waits_for_the_same_day() {
        let schedule = Schedule::immediate().within(window((8, 0), (20, 0), 0));

        assert_eq!(
            schedule.decide(at("2026-07-26T03:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-26T08:00:00Z"))
        );
    }

    #[test]
    fn an_instant_after_the_window_waits_for_the_next_day() {
        let schedule = Schedule::immediate().within(window((8, 0), (20, 0), 0));

        assert_eq!(
            schedule.decide(at("2026-07-26T21:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-27T08:00:00Z"))
        );
    }

    /// A window that crosses midnight — the night rate an operator buys — is
    /// the case a naive `start <= t && t < end` gets exactly backwards: it
    /// would send during the day and hold at night.
    #[test]
    fn a_window_that_crosses_midnight_holds_the_middle_of_the_day() {
        let schedule = Schedule::immediate().within(window((22, 0), (6, 0), 0));

        assert_eq!(
            schedule.decide(at("2026-07-26T23:30:00Z")),
            ScheduleDecision::Send
        );
        assert_eq!(
            schedule.decide(at("2026-07-26T05:59:00Z")),
            ScheduleDecision::Send
        );
        assert_eq!(
            schedule.decide(at("2026-07-26T00:00:00Z")),
            ScheduleDecision::Send
        );
        assert_eq!(
            schedule.decide(at("2026-07-26T06:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-26T22:00:00Z"))
        );
        assert_eq!(
            schedule.decide(at("2026-07-26T21:59:59Z")),
            ScheduleDecision::WaitUntil(at("2026-07-26T22:00:00Z"))
        );
    }

    /// The window is read in the operator's own time zone, which is the whole
    /// reason it carries an offset: 21:00 UTC is 23:00 in Abidjan+2 and a
    /// window closing at 20:00 local has long shut.
    #[test]
    fn the_window_is_read_in_its_own_time_zone() {
        let utc = Schedule::immediate().within(window((8, 0), (20, 0), 0));
        let east = Schedule::immediate().within(window((8, 0), (20, 0), 2));

        // 19:00 UTC is inside the UTC window and 21:00 east of it, which is not.
        assert_eq!(
            utc.decide(at("2026-07-26T19:00:00Z")),
            ScheduleDecision::Send
        );
        assert_eq!(
            east.decide(at("2026-07-26T19:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-27T06:00:00Z"))
        );

        // 06:30 UTC is before the UTC window and 08:30 east of it, which is
        // inside.
        assert_eq!(
            utc.decide(at("2026-07-26T06:30:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-26T08:00:00Z"))
        );
        assert_eq!(
            east.decide(at("2026-07-26T06:30:00Z")),
            ScheduleDecision::Send
        );
    }

    /// A window west of Greenwich puts the local date a day behind the UTC one,
    /// which is where an implementation that computes the next opening from the
    /// **UTC** date lands a day early or a day late.
    #[test]
    fn a_window_west_of_greenwich_computes_its_opening_on_the_local_date() {
        let schedule = Schedule::immediate().within(window((8, 0), (20, 0), -5));

        // 02:00 UTC on the 26th is 21:00 on the 25th, local: the window has
        // closed, and the next opening is 08:00 on the 26th, local — 13:00 UTC
        // on the 26th.
        assert_eq!(
            schedule.decide(at("2026-07-26T02:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-26T13:00:00Z"))
        );
    }

    /// The two settings compose: the campaign starts no earlier than its
    /// deferred instant, and no earlier than the first opening at or after it.
    #[test]
    fn a_deferred_start_inside_a_closed_window_waits_for_the_window() {
        let schedule = Schedule::immediate()
            .starting_at(at("2026-07-26T22:00:00Z"))
            .within(window((8, 0), (20, 0), 0));

        assert_eq!(
            schedule.decide(at("2026-07-26T12:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-27T08:00:00Z"))
        );
    }

    #[test]
    fn a_deferred_start_inside_an_open_window_waits_for_the_start() {
        let schedule = Schedule::immediate()
            .starting_at(at("2026-07-26T18:00:00Z"))
            .within(window((8, 0), (20, 0), 0));

        assert_eq!(
            schedule.decide(at("2026-07-26T12:00:00Z")),
            ScheduleDecision::WaitUntil(at("2026-07-26T18:00:00Z"))
        );
    }

    /// A window whose two ends are the same instant says either "never" or
    /// "always", and there is no reading of `08:00–08:00` that tells an
    /// operator which one they asked for.
    #[test]
    fn a_window_with_two_identical_ends_is_refused() {
        assert_eq!(
            DailyWindow::new(
                Time::from_hms(8, 0, 0).expect("a valid time of day"),
                Time::from_hms(8, 0, 0).expect("a valid time of day"),
                UtcOffset::UTC,
            ),
            Err(ScheduleError::EmptyWindow)
        );
    }

    #[test]
    fn the_wait_is_reported_as_a_duration_the_runner_can_sleep_on() {
        let schedule = Schedule::immediate().starting_at(at("2026-07-26T12:30:00Z"));

        assert_eq!(
            schedule.wait_for(at("2026-07-26T12:00:00Z")),
            Some(Duration::from_secs(1_800))
        );
        assert_eq!(schedule.wait_for(at("2026-07-26T13:00:00Z")), None);
    }

    /// Time only moves forward: a decision that answered "wait until an instant
    /// already past" would spin the runner in a zero-length sleep for ever.
    #[test]
    fn every_wait_a_schedule_returns_is_in_the_future() {
        let schedule = Schedule::immediate()
            .starting_at(at("2026-07-26T09:00:00Z"))
            .within(window((22, 0), (6, 0), 3));

        for hour in 0..24 {
            let now = at(&format!("2026-07-26T{hour:02}:00:00Z"));

            if let ScheduleDecision::WaitUntil(instant) = schedule.decide(now) {
                assert!(instant > now, "at {now}, waiting until {instant}");
            }
        }
    }

    // --- reading a window off the campaign form (L-010-07) ------------------

    #[test]
    fn a_window_is_read_from_two_hh_mm_ends_and_an_offset() {
        let window = DailyWindow::parse("08:00", "20:00", 60).expect("a valid window");

        assert_eq!(window.open(), Time::from_hms(8, 0, 0).expect("valid"));
        assert_eq!(window.close(), Time::from_hms(20, 0, 0).expect("valid"));
        assert_eq!(
            window.offset(),
            UtcOffset::from_hms(1, 0, 0).expect("a valid offset")
        );
    }

    /// A zone west of Greenwich is a NEGATIVE offset in both its parts, and
    /// `-300` minutes is `-05:00` and not `-4:-60`. Written because the obvious
    /// `(m / 60, m % 60)` gives `(-5, 0)` here but `(0, -30)` for `-00:30`,
    /// and `time` refuses a pair whose signs disagree.
    #[test]
    fn a_window_west_of_greenwich_keeps_both_parts_negative() {
        let window = DailyWindow::parse("08:00", "20:00", -270).expect("a valid window");

        assert_eq!(
            window.offset(),
            UtcOffset::from_hms(-4, -30, 0).expect("a valid offset")
        );
        assert_eq!(window.offset().whole_minutes(), -270);
    }

    #[test]
    fn a_window_at_utc_reads_as_utc() {
        let window = DailyWindow::parse("22:00", "06:00", 0).expect("a valid window");

        assert_eq!(window.offset(), UtcOffset::UTC);
        assert!(
            window.contains(at("2026-07-26T23:30:00Z")),
            "the night window"
        );
    }

    #[test]
    fn a_malformed_end_is_refused_rather_than_guessed() {
        for (open, close) in [
            ("8:00", "20:00"),
            ("08:00:00", "20:00"),
            ("08h00", "20:00"),
            ("08:00", ""),
            ("25:00", "20:00"),
            ("08:60", "20:00"),
            ("ab:cd", "20:00"),
            ("08:0", "20:00"),
        ] {
            assert_eq!(
                DailyWindow::parse(open, close, 0),
                Err(ScheduleError::MalformedTime),
                "{open:?}-{close:?}"
            );
        }
    }

    #[test]
    fn an_impossible_zone_offset_is_refused() {
        for minutes in [841, -721, 100_000, -100_000] {
            assert_eq!(
                DailyWindow::parse("08:00", "20:00", minutes),
                Err(ScheduleError::OffsetOutOfRange),
                "{minutes}"
            );
        }
    }

    /// The extremes that DO exist: Baker Island at `-12:00`, Kiribati at
    /// `+14:00`. A bound that refused them would refuse a real operator.
    #[test]
    fn the_real_extremes_of_the_zone_range_are_accepted() {
        for minutes in [-720, 840, 345] {
            assert!(
                DailyWindow::parse("08:00", "20:00", minutes).is_ok(),
                "{minutes}"
            );
        }
    }

    /// The ambiguity `DailyWindow::new` refuses survives the text form: an
    /// operator who typed the same time twice meant one of two opposite things.
    #[test]
    fn two_identical_ends_are_still_refused_when_they_come_as_text() {
        assert_eq!(
            DailyWindow::parse("08:00", "08:00", 0),
            Err(ScheduleError::EmptyWindow)
        );
    }
}
