//! The send window: at most `window_size` PDUs awaiting a response.
//!
//! Deliverable L-007-02, spec §9.2 mechanism 2.
//!
//! # What it counts
//!
//! **PDUs, not messages.** A 400-character text is one message and three
//! `submit_sm`, and it occupies three slots, not one. The window exists to
//! bound what the message centre has to hold on our behalf, and the message
//! centre counts PDUs — a window that counted logical messages would let a
//! campaign of long texts put three times the agreed number of requests in
//! flight. The fiche calls this out as the classic bug; it is settled here,
//! and [`SendWindow::acquire`] is called once per PDU.
//!
//! # Why a permit and not a counter
//!
//! [`WindowPermit`] releases its slot in `Drop`. That is not a stylistic
//! preference: the slot has to come back on **every** path — the response
//! arrives, the response times out, the session dies, the caller's future is
//! cancelled mid-flight — and a manual `release()` spread across those
//! branches is one `?` away from a leak. A leaked slot is permanent: the
//! window shrinks by one for the life of the session, and after
//! `window_size` of them the session stops sending altogether with nothing in
//! the logs to say why.
//!
//! CA-007-10 is the statement of that guarantee, and it is met by
//! construction rather than by discipline.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::RateControlError;

/// The largest window this implementation accepts.
///
/// Spec §8.2 caps a profile at 1 000; this is deliberately looser, because a
/// crate has no business restating another layer's policy. It exists to keep
/// the conversion into the semaphore's `usize` honest on every target.
pub const MAX_WINDOW_SIZE: u32 = 65_535;

/// A bounded number of PDUs in flight (spec §9.2).
///
/// Cheap to clone: every clone shares the same slots, which is what makes it
/// usable from the several tasks of one session.
#[derive(Debug, Clone)]
pub struct SendWindow {
    size: u32,
    slots: Arc<Semaphore>,
}

impl SendWindow {
    /// A window of `size` slots.
    ///
    /// # Errors
    ///
    /// [`RateControlError::WindowSizeOutOfRange`] when `size` is zero or
    /// above [`MAX_WINDOW_SIZE`].
    pub fn new(size: u32) -> Result<Self, RateControlError> {
        if size == 0 || size > MAX_WINDOW_SIZE {
            return Err(RateControlError::WindowSizeOutOfRange {
                requested: size,
                maximum: MAX_WINDOW_SIZE,
            });
        }

        // `usize::try_from` rather than `as`: `cast_possible_truncation` is
        // denied workspace-wide, and a 16-bit target would genuinely truncate.
        let permits = usize::try_from(size).unwrap_or(usize::MAX);

        Ok(Self {
            size,
            slots: Arc::new(Semaphore::new(permits)),
        })
    }

    /// A window of exactly one slot.
    ///
    /// Infallible, which is why it exists: a caller that cannot fail — a
    /// session being spawned — needs somewhere to fall back to that still
    /// sends, and one PDU at a time is the safest thing a misconfigured
    /// session can do.
    #[must_use]
    pub fn single() -> Self {
        Self {
            size: 1,
            slots: Arc::new(Semaphore::new(1)),
        }
    }

    /// Waits for a free slot and takes it.
    ///
    /// **Waits — it does not refuse.** A full window means the message centre
    /// has not answered yet, which is a reason to slow down, never a reason to
    /// fail a message the operator asked to send. The wait is what propagates
    /// back-pressure to whoever is producing.
    ///
    /// # Errors
    ///
    /// [`RateControlError::WindowClosed`] if the window was closed while
    /// waiting.
    pub async fn acquire(&self) -> Result<WindowPermit, RateControlError> {
        Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .map(|permit| WindowPermit { _permit: permit })
            .map_err(|_| RateControlError::WindowClosed)
    }

    /// Takes a slot if one is free right now.
    ///
    /// What a test uses to observe saturation without arranging a deadlock.
    #[must_use]
    pub fn try_acquire(&self) -> Option<WindowPermit> {
        Arc::clone(&self.slots)
            .try_acquire_owned()
            .ok()
            .map(|permit| WindowPermit { _permit: permit })
    }

    /// Wakes every waiter with [`RateControlError::WindowClosed`].
    ///
    /// Called when a session stops: a task parked on `acquire` for a slot that
    /// will never come back would otherwise never finish, and CA-005-08 wants
    /// every task joined rather than abandoned.
    pub fn close(&self) {
        self.slots.close();
    }

    /// How many slots the window has in total.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// How many PDUs are in flight right now.
    ///
    /// Derived from the free slots rather than kept as a second counter: a
    /// counter next to a semaphore is a counter that can disagree with it, and
    /// CA-007-08 asks for the displayed occupancy to be exact.
    #[must_use]
    pub fn in_use(&self) -> u32 {
        self.size.saturating_sub(self.available())
    }

    /// How many slots are free right now.
    #[must_use]
    pub fn available(&self) -> u32 {
        u32::try_from(self.slots.available_permits()).unwrap_or(u32::MAX)
    }

    /// Occupancy as a fraction of the window, in `0.0..=1.0`.
    #[must_use]
    pub fn occupancy(&self) -> f64 {
        f64::from(self.in_use()) / f64::from(self.size)
    }
}

/// One occupied slot of the window.
///
/// Held from just before a PDU is written until its response arrives, times
/// out or is abandoned. Dropping it gives the slot back — see this module's
/// header for why that is the whole design.
#[derive(Debug)]
pub struct WindowPermit {
    /// Never read. The `Drop` of the semaphore permit is the entire contract,
    /// and naming it `_permit` says so to anyone tempted to remove a field
    /// nothing touches.
    _permit: OwnedSemaphorePermit,
}

#[cfg(test)]
// `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
// reserves for "the binary entry point". A test harness is one.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn a_window_of_zero_slots_is_refused_rather_than_silently_widened() {
        assert_eq!(
            SendWindow::new(0).err(),
            Some(RateControlError::WindowSizeOutOfRange {
                requested: 0,
                maximum: MAX_WINDOW_SIZE,
            })
        );
        assert!(SendWindow::new(MAX_WINDOW_SIZE + 1).is_err());
        assert!(SendWindow::new(1).is_ok());
        assert!(SendWindow::new(MAX_WINDOW_SIZE).is_ok());
    }

    /// CA-007-02, at the level of the window: the tenth PDU gets a slot and
    /// the eleventh does not.
    #[tokio::test]
    async fn exactly_window_size_slots_are_handed_out_before_the_window_is_full() {
        let window = SendWindow::new(10).expect("a valid size");
        let mut held = Vec::new();

        for expected in 1..=10 {
            held.push(window.acquire().await.expect("the window is open"));

            assert_eq!(window.in_use(), expected);
        }

        assert!(
            window.try_acquire().is_none(),
            "an eleventh PDU must not get a slot"
        );
        assert_eq!(window.available(), 0);
        assert!((window.occupancy() - 1.0).abs() < f64::EPSILON);
    }

    /// The wait is a wait, not a rejection: a sender parked on a full window
    /// resumes the instant a slot comes back.
    #[tokio::test(start_paused = true)]
    async fn a_sender_parked_on_a_full_window_resumes_when_a_slot_is_released() {
        let window = SendWindow::new(1).expect("a valid size");
        let held = window.acquire().await.expect("the first slot");

        let waiting = tokio::spawn({
            let window = window.clone();

            async move { window.acquire().await.map(drop) }
        });

        // Long enough that a rejection would already have come back.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!waiting.is_finished(), "the second sender must still wait");

        drop(held);

        waiting
            .await
            .expect("the task ran")
            .expect("the window is open");
        assert_eq!(window.in_use(), 0);
    }

    /// The three release paths of the fiche — a response, an error, a
    /// cancellation — are one path here, because they are all `Drop`.
    #[tokio::test]
    async fn a_slot_comes_back_however_its_holder_ended() {
        let window = SendWindow::new(3).expect("a valid size");

        // Path 1: the ordinary end of a scope, as a response would.
        {
            let _permit = window.acquire().await.expect("open");
            assert_eq!(window.in_use(), 1);
        }
        assert_eq!(window.in_use(), 0);

        // Path 2: an early return carrying an error.
        async fn fails(window: &SendWindow) -> Result<(), &'static str> {
            let _permit = window.acquire().await.map_err(|_| "closed")?;

            Err("the message centre rejected it")
        }
        assert!(fails(&window).await.is_err());
        assert_eq!(window.in_use(), 0);

        // Path 3: the holder's future is dropped before it ever completes.
        let permit = window.acquire().await.expect("open");
        let future = async move {
            let _held = permit;

            std::future::pending::<()>().await;
        };
        let mut future = Box::pin(future);
        assert!(tokio::time::timeout(Duration::from_millis(1), &mut future)
            .await
            .is_err());
        drop(future);
        assert_eq!(window.in_use(), 0);
    }

    /// A closed window wakes its waiters instead of holding them for ever.
    #[tokio::test(start_paused = true)]
    async fn closing_the_window_releases_a_waiter_with_an_error() {
        let window = SendWindow::new(1).expect("a valid size");
        let _held = window.acquire().await.expect("the only slot");

        let waiting = tokio::spawn({
            let window = window.clone();

            async move { window.acquire().await }
        });

        tokio::time::sleep(Duration::from_millis(1)).await;
        window.close();

        assert_eq!(
            waiting.await.expect("the task ran").err(),
            Some(RateControlError::WindowClosed)
        );
    }

    /// Clones share the slots. A clone with its own set would be a second
    /// window, and two windows of ten are a window of twenty.
    #[tokio::test]
    async fn every_clone_of_a_window_draws_on_the_same_slots() {
        let window = SendWindow::new(2).expect("a valid size");
        let twin = window.clone();

        let _first = window.acquire().await.expect("open");
        let _second = twin.acquire().await.expect("open");

        assert!(window.try_acquire().is_none());
        assert!(twin.try_acquire().is_none());
        assert_eq!(window.in_use(), 2);
        assert_eq!(twin.in_use(), 2);
    }

    #[tokio::test]
    async fn occupancy_is_the_fraction_of_the_window_in_use() {
        let window = SendWindow::new(4).expect("a valid size");

        assert!((window.occupancy() - 0.0).abs() < f64::EPSILON);

        let _first = window.acquire().await.expect("open");

        assert!((window.occupancy() - 0.25).abs() < f64::EPSILON);
    }
}
