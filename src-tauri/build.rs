//! Tauri build script.
//!
//! `tauri_build::build()` produces the compiled context from
//! `tauri.conf.json` and `capabilities/`: it is what turns the permission
//! list into checks baked into the binary, rather than controls evaluated at
//! runtime.

fn main() {
    tauri_build::build();
}
