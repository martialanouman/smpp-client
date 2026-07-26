# `src/ipc/` — the backend boundary

Two rules, both enforced by tooling.

## 1. Types are generated, never hand-written

DTOs are declared **once, in Rust** (`serde` + `specta`); their TypeScript
counterparts are produced by `tauri-specta` into `src/ipc/generated/` — see
[ADR 0003](../../../docs/adr/0003-generation-des-types-ipc.md).

Re-declaring a type here by hand creates a silent divergence between the two
sides of the IPC: the Rust compiler and `tsc` are then both green while
serialisation fails at runtime.

CI step 4 regenerates and compares (`git diff --exit-code`): an uncommitted
diff fails the build.

> Wired in since milestone 001: `src-tauri/src/bin/gen_ipc.rs` produces
> `generated/bindings.ts`, and CI step 4 regenerates and compares on every run.

## 2. Only this directory imports `@tauri-apps/api`

Components never call `invoke` directly; they go through the typed wrappers
exposed here (CLAUDE.md §4). The `no-restricted-imports` rule in
`eslint.config.js` bans importing `@tauri-apps/*` everywhere **except** this
directory.

A wrapper narrows the failure into one of two shapes. `backend` carries a
well-formed `ErrorDto` with its stable `code`; anything else is `transport`,
including the bare JSON string Tauri rejects with when it cannot deserialise a
command's arguments. Trusting that string to be a DTO produced a toast with
neither code nor message — hence the explicit shape check in `call()`.
