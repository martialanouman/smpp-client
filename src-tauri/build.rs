//! Tauri build script.
//!
//! `tauri_build::build()` produces the compiled context from
//! `tauri.conf.json` and `capabilities/`: it is what turns the permission
//! list into checks baked into the binary, rather than controls evaluated at
//! runtime.

fn main() {
    tauri_build::build();

    manifest_the_test_binaries();
}

/// Gives the **test** executables the Common Controls 6 manifest on Windows.
///
/// # The failure this fixes
///
/// `tauri_build::build()` embeds an application manifest declaring a dependency
/// on `Microsoft.Windows.Common-Controls` version 6. The *test* executables are
/// a separate link, and they get no manifest — so they bind to comctl32
/// version 5, which does not export `TaskDialogIndirect`. The Windows loader
/// then refuses the process before a single test runs:
///
/// ```text
/// aborted with code 0xc0000139: The specified procedure could not be found.
/// ```
///
/// `cargo nextest` fails on its very first `--list`, so the whole package reads
/// as broken rather than as one failing test. It only started happening when
/// `tauri`'s `test` feature joined the dev-dependencies, because that is what
/// pulls the import into the test harness — see `tauri-apps/tauri#13419`, and
/// `pacifio/atlas#390` for the same fix.
///
/// # Why the plain `rustc-link-arg`, which is wider than one would like
///
/// The failing executable is the **library compiled as a test**, and Cargo has
/// no instruction aimed at it: `rustc-link-arg-tests` covers `tests/` targets
/// only, and this package has none — Cargo says so outright, `invalid
/// instruction … does not have a test target`. Of the instructions Cargo
/// documents, `rustc-link-arg` is the only one that reaches a test harness; it
/// also reaches the binaries, the `cdylib` and the examples.
///
/// Conditioning it on "we are building tests" is not available either: a build
/// script runs once per package, and `CARGO_CFG_TEST` is not among the
/// variables it is given (checked, rather than assumed).
///
/// So the argument also lands on `shinobismpp.exe`, which already carries the
/// very same declaration as a **resource** — `tauri_build` compiles its
/// `windows-app-manifest.xml` in. The two mechanisms differ (a linker-generated
/// manifest beside an embedded `RT_MANIFEST` resource), and whether MSVC merges
/// them or objects is not settled here: nothing in this repository links that
/// binary on Windows except the `Native packages` job, which pull requests
/// skip. If it objects, that job is where it will say so, and the fix is to
/// move the flag onto a `tests/` target that re-exports what these tests need.
///
/// The alternative in circulation — running `mt.exe` over the built test
/// executable afterwards — puts a post-processing step between `cargo test` and
/// the tests, which is worse than a flag that is one target too wide.
///
/// MSVC only: `/MANIFESTDEPENDENCY` is a `link.exe` option, and it is the
/// toolchain the CI matrix and the release workflow both use on Windows. Under
/// the GNU toolchain the flag would be passed to a linker that does not know
/// it, so the target environment is checked rather than the operating system
/// alone.
fn manifest_the_test_binaries() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");

    if !windows || !msvc {
        return;
    }

    println!(
        "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' \
         name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
         processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
}
