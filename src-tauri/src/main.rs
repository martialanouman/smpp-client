//! ShinobiSMPP binary entry point.
//!
//! Only delegates to [`shinobismpp_lib::run`]: the whole application setup
//! lives in the library, which alone is testable and alone usable as a mobile
//! entry point.

// Prevents an extra console window on Windows in release builds.
// DO NOT REMOVE: without this attribute the shipped application shows a black
// terminal window behind its interface.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // The error is propagated rather than caught: because `main` returns a
    // `Result`, the runtime prints it and sets a non-zero exit code. That is
    // what lets us do without the Tauri template's `.expect()`, which our
    // lints forbid (CLAUDE.md §4).
    shinobismpp_lib::run()
}
