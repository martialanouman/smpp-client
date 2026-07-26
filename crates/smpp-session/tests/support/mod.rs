//! The in-memory message centre, re-exported from the crate under test.
//!
//! It used to be written out here. Milestone 006 needs the same double from
//! `messaging` — to exercise the send path against a real session rather than
//! a hand-written stub — and a second copy of a test double is a second thing
//! to keep honest, so it moved into `smpp_session::testing` behind the
//! `test-support` feature. This file is the redirection; nothing else changed
//! for the tests that were written against it.

// `tests/` is compiled without `cfg(test)`, so the relaxations of
// `clippy.toml` do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: `#[tokio::test]` expands to `Runtime::block_on`,
//     which `clippy.toml` reserves for "the binary entry point". A test
//     harness is one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

pub(crate) use smpp_session::testing::{
    a_profile, assert_state, drain, start, tight_backoff, wait_for_code, wait_until_bound, Script,
    Seen, Smsc, PASSWORD_TEXT, QUIET_PERIOD,
};
