//! Throughput limiting and congestion adaptation.
//!
//! Implements the two mechanisms spec §9.2 applies **jointly**, and keeps them
//! as two types because they bound two different things:
//!
//! | Type | Bounds | Full when |
//! |------|--------|-----------|
//! | [`RateLimiter`] | PDUs per second | the quota is spent for this instant |
//! | [`SendWindow`] | PDUs awaiting a response | the message centre has not answered |
//!
//! `effective rate = min(quota, window ÷ round-trip time)`. Implementing only
//! the first gives a sender that floods a slow message centre until it is
//! banned; only the second gives one whose rate is whatever the latency
//! happens to allow. Merging them into a single number gives neither, because
//! the window is emptied by *responses* and the quota by *time*.
//!
//! # No internal dependencies
//!
//! This crate reasons about instants and quotas, never about PDUs. That is
//! what makes it testable with a virtual clock — everything here reads
//! `tokio::time`, so `#[tokio::test(start_paused = true)]` exercises a
//! ten-second campaign at no wall-clock cost, which the determinism
//! requirement of CLAUDE.md §7 demands.
//!
//! Implemented at milestone 007.
//!
//! ```
//! # use rate_control::{RateLimiter, SendWindow};
//! # async fn example() -> Result<(), rate_control::RateControlError> {
//! let limiter = RateLimiter::at(100)?;
//! let window = SendWindow::new(50)?;
//!
//! limiter.acquire().await;
//! let permit = window.acquire().await?;
//!
//! // …write the PDU, then wait for its response…
//! drop(permit); // the slot comes back here, whatever happened
//! # Ok(())
//! # }
//! ```

mod error;
mod limiter;
mod window;

pub use error::RateControlError;
pub use limiter::{AdaptiveFactor, RateLimiter, ThroughputConfig, DEFAULT_THROTTLE_COOLDOWN};
pub use window::{SendWindow, WindowPermit, MAX_WINDOW_SIZE};

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!rate_control::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
