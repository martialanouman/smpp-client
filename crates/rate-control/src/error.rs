//! Error type for this crate.

/// Errors produced by this crate.
///
/// Per guide §6.1, every crate exposes **one** exhaustive `thiserror` type.
/// No public API returns a `Box<dyn Error>`: callers must be able to
/// discriminate between cases.
///
/// `#[non_exhaustive]` lets later milestones add variants without breaking
/// `match` expressions in calling crates.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateControlError {
    /// A window of this size cannot be built.
    ///
    /// Zero would admit nothing at all — an ESME that never sends — and the
    /// upper bound is what the underlying semaphore can hold.
    /// `SessionProfile` validates `1..=1000` long before this is reached; the
    /// check is here as well because this crate is usable without it.
    #[error("a send window of {requested} slots is outside 1..={maximum}")]
    WindowSizeOutOfRange {
        /// What was asked for.
        requested: u32,
        /// The largest window this implementation accepts.
        maximum: u32,
    },

    /// The window was closed while a sender was waiting for a slot.
    ///
    /// Only reachable on shutdown: a waiter that would otherwise block for
    /// ever is woken with this instead.
    #[error("the send window is closed")]
    WindowClosed,

    /// The floor of the adaptive band is above its ceiling.
    ///
    /// `min_tps` may not exceed the user's target: spec §9.4 clamps the
    /// effective rate into `min_tps..=target`, and an empty band has no
    /// meaning.
    #[error("min_tps {min_tps} is above the target of {target_tps} messages per second")]
    ThroughputBandEmpty {
        /// The user's target.
        target_tps: u32,
        /// The floor the adaptation may not go below.
        min_tps: u32,
    },
}
