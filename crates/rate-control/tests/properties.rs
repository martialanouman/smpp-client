//! Properties of the send window (fiche §5, "Propriété").
//!
//! The invariant the fiche states: over any sequence of acquisitions and
//! releases, the occupancy never goes negative and never exceeds
//! `window_size`. It is worth a property test rather than a handful of cases
//! because the failure mode — a slot released twice, or one leaked on a path
//! nobody enumerated — shows up as an *arithmetic* violation long before it
//! shows up as a session that stopped sending.
//!
//! Driven with [`SendWindow::try_acquire`] and `drop`, both synchronous: the
//! property is about the accounting, not about the waiting, and a synchronous
//! driver keeps the shrinking deterministic.

// A test double reports its failures by panicking, which is what an assertion
// is. `clippy.toml` reopens these under `cfg(test)`, and an integration test
// is compiled as its own crate rather than under that flag.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use proptest::prelude::*;
use rate_control::{SendWindow, WindowPermit};

/// One step of the random sequence.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Ask for a slot. May legitimately be refused.
    Acquire,
    /// Release the slot taken `index` positions from the front of the held
    /// set, if any. Not always the most recent one: a response arrives for
    /// whichever request the message centre answered first, which is not the
    /// last one sent.
    Release(usize),
}

fn a_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => Just(Step::Acquire),
        2 => (0_usize..64).prop_map(Step::Release),
    ]
}

proptest! {
    /// The occupancy stays inside `0..=size`, whatever the sequence.
    #[test]
    fn the_occupancy_never_leaves_its_bounds(
        size in 1_u32..32,
        steps in prop::collection::vec(a_step(), 0..400),
    ) {
        let window = SendWindow::new(size).expect("1..32 is a valid size");
        let mut held: Vec<WindowPermit> = Vec::new();

        for step in steps {
            match step {
                Step::Acquire => {
                    if let Some(permit) = window.try_acquire() {
                        held.push(permit);
                    }
                }
                Step::Release(index) => {
                    if !held.is_empty() {
                        drop(held.remove(index % held.len()));
                    }
                }
            }

            // `in_use` is derived from the semaphore, so "never negative" is
            // the statement that it never underflows into a huge number.
            prop_assert!(window.in_use() <= size, "{} slots in use of {size}", window.in_use());
            prop_assert_eq!(window.in_use(), u32::try_from(held.len()).unwrap());
            prop_assert!(window.occupancy() >= 0.0 && window.occupancy() <= 1.0);
        }

        // CA-007-10, in miniature: everything released, nothing left behind.
        drop(held);
        prop_assert_eq!(window.in_use(), 0);
        prop_assert_eq!(window.available(), size);
    }
}

/// CA-007-10 at the scale it is stated in: a hundred thousand acquisitions
/// mixing every ending — an ordinary release, an error path, a dropped
/// future — leave the counter at exactly zero.
///
/// Synchronous on purpose. The releases are `Drop`, and `Drop` does not care
/// which runtime it runs on; adding one would only make the test slower.
#[test]
fn a_hundred_thousand_slots_return_to_the_window_exactly() {
    let window = SendWindow::new(50).expect("a valid size");

    for index in 0_u32..100_000 {
        let permit = window
            .try_acquire()
            .expect("the window empties every round");

        match index % 3 {
            // The nominal path.
            0 => drop(permit),
            // An error path that returns early while holding the permit.
            1 => {
                let outcome: Result<(), WindowPermit> = Err(permit);

                assert!(outcome.is_err());
            }
            // A future that is dropped before it completes.
            _ => {
                // Never polled: the permit is still moved into the future, and
                // it is released when the future is dropped.
                let future = Box::pin(async move {
                    let _held = permit;

                    std::future::pending::<()>().await;
                });

                drop(future);
            }
        }

        assert_eq!(window.in_use(), 0, "a slot leaked at iteration {index}");
    }

    assert_eq!(window.available(), 50);
}
