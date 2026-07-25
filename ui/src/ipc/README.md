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

> At milestone 000 the generator is not wired in yet — it lands at
> **milestone 001**. `scripts/check-ipc-types.sh` detects this and lets the
> step pass, but **fails** if a generated file appears here without a
> generator: the empty state is tolerated only while it is genuinely empty.

## 2. Only this directory imports `@tauri-apps/api`

Components never call `invoke` directly; they go through the typed wrappers
exposed here (CLAUDE.md §4). The `no-restricted-imports` rule in
`eslint.config.js` bans importing `@tauri-apps/*` everywhere **except** this
directory.

A wrapper validates the shape of the response and translates the
`{ code, message, details }` error DTO into a type the interface can use.
