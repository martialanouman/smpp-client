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
/// # Why `rustc-link-arg-tests` and not a manifest file
///
/// This argument reaches the **test** targets only. A manifest embedded for
/// every target of the package would be a second copy in the application
/// binary, which the linker rejects — and running `mt.exe` over the built test
/// executable afterwards, the other workaround in circulation, puts a
/// post-processing step between `cargo test` and the tests.
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
        "cargo:rustc-link-arg-tests=/MANIFESTDEPENDENCY:type='win32' \
         name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
         processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
}
