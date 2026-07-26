//! What both integration suites of this crate share.
//!
//! The in-memory message centre used to be written out here. Milestone 006
//! needs the same double for the send path — and a second copy of a test
//! double is a second thing to keep honest — so it moved into
//! `smpp_session::testing`, behind the `test-support` feature. What is left
//! here is that redirection, plus [`journal`], the doubles the send tests need
//! and the bind tests do not.

// `tests/` is compiled without `cfg(test)`, so the relaxations of
// `clippy.toml` do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: `#[tokio::test]` expands to `Runtime::block_on`,
//     which `clippy.toml` reserves for "the binary entry point". A test
//     harness is one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
// Each integration test file is its own crate and compiles the WHOLE support
// module, so everything the *other* file uses looks unused from here.
// `session.rs` never builds a `Journal`; `sending.rs` never calls
// `assert_state`. Both are used, just not by the same binary.
#![allow(dead_code, unused_imports)]

pub(crate) mod journal;

pub(crate) use smpp_session::testing::{
    a_profile, assert_state, drain, start, tight_backoff, wait_for_code, wait_until_bound, Script,
    Seen, Smsc, PASSWORD_TEXT, QUIET_PERIOD,
};
