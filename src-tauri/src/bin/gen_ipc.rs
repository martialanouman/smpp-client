//! Generator of the TypeScript IPC bindings (ADR 0003).
//!
//! `scripts/check-ipc-types.sh` keys off the **presence of this file** to
//! switch CI step 4 from "vacuously green" to "really checks". Moving it
//! silently disarms that step — the path is part of the contract.
//!
//! ```text
//! cargo run --package shinobismpp --bin gen_ipc
//! ```

use std::path::Path;

/// Where the bindings land, relative to `src-tauri/`.
const OUTPUT: &str = "../ui/src/ipc/generated/bindings.ts";

fn main() -> anyhow::Result<()> {
    // `CARGO_MANIFEST_DIR` rather than the current directory: the generator
    // must write to the same place whether it is run from the repository root,
    // from `src-tauri/`, or by the CI script.
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join(OUTPUT);

    shinobismpp_lib::export_ipc_bindings(&output)
}
