#!/usr/bin/env bash
#
# CI step 4 — "generated IPC types" (milestone 000 §5.1).
#
# The tauri-specta generator only arrives at milestone 001. The classic trap
# would be to write a step here that does nothing: it would always pass, and
# nobody would remember to un-permissive it when the time came.
#
# So this script checks its own precondition and switches over by itself:
#
#   1. Generator present     → run it and compare. Nominal, final behaviour.
#   2. Absent, but generated
#      types are present     → HARD FAILURE. This is the ratchet: a generated
#                              file cannot appear without a generator.
#   3. Neither               → success, with an explicit message.
#
# The switch depends on THE PRESENCE OF THE GENERATOR FILE, not on an
# environment variable or a flag to remove: there is nothing to remember.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

generator="src-tauri/src/bin/gen_ipc.rs"
# Only the GENERATED directory, not `ui/src/ipc` as a whole: the wrappers
# beside it are hand-written, and diffing the parent made any uncommitted
# edit to them fail step 4 for no reason.
output="ui/src/ipc/generated"

if [ -f "$generator" ]; then
  echo "→ Generator found ($generator): regenerating IPC types."
  cargo run --quiet --package shinobismpp --bin gen_ipc

  # `git diff` compares the work tree to the index, so an UNTRACKED file is
  # invisible to it. Today the generator writes one already-tracked file and
  # the check holds — but tauri-specta can split its output, and a new file
  # would then arrive untracked with the step still green. `add -N` registers
  # the paths without staging content, which makes them visible to the diff.
  git add --intent-to-add -- "$output" >/dev/null 2>&1 || true

  if ! git diff --exit-code -- "$output"; then
    echo >&2
    echo "✗ Generated IPC types differ from the committed ones." >&2
    echo "  Run 'just ipc-check' and commit the result." >&2
    exit 1
  fi

  echo "✓ IPC types up to date."
  exit 0
fi

# The generator is not wired in. No generated file may exist.
if [ -d "$output" ]; then
  orphans="$(grep -rl -- '@generated' "$output" 2>/dev/null || true)"
  if [ -n "$orphans" ]; then
    echo >&2
    echo "✗ Generated types are present while the generator is missing:" >&2
    echo "$orphans" | sed 's/^/    /' >&2
    echo >&2
    echo "  Either these files were written by hand — which" >&2
    echo "  ui/src/ipc/README.md forbids — or the generator was deleted." >&2
    exit 1
  fi
fi

echo "✓ IPC type generator not wired in yet (arrives at milestone 001)."
echo "  No generated types present: state is consistent."
