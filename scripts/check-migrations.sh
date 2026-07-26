#!/usr/bin/env bash
#
# CI step 8 — "migrations" (milestone 000 §5.1, active from milestone 002).
#
# Same ratchet pattern as scripts/check-ipc-types.sh: a single mental model
# for both deferred steps of milestone 000.
#
#   1. Migrations exist → apply them to a fresh database and check the schema
#                         builds.
#   2. None             → success, with an explicit message.
#
# The switch depends on files being present in migrations/, not on a flag: as
# soon as milestone 002 writes its first migration, the step starts checking
# for real without anyone editing this script.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

migrations="migrations"

if [ ! -d "$migrations" ] || [ -z "$(find "$migrations" -name '*.sql' -print -quit 2>/dev/null)" ]; then
  echo "✓ No SQL migrations (the schema arrives at milestone 002)."
  exit 0
fi

if ! command -v sqlx >/dev/null 2>&1; then
  echo >&2
  echo "✗ Migrations exist but sqlx-cli is missing." >&2
  echo "  Install it: cargo install sqlx-cli --no-default-features --features sqlite" >&2
  exit 1
fi

database="$(mktemp -d)/check.db"
export DATABASE_URL="sqlite://${database}?mode=rwc"

echo "→ Applying migrations to a fresh database: $database"
sqlx database create
sqlx migrate run

# Second run, on the database the first one just built. Two things are checked
# here that the first run cannot:
#   · idempotence — the migrator runs on every application start, so a second
#     run must be a no-op rather than an error;
#   · immutability — sqlx compares the checksum of every applied migration
#     against the file on disk, and refuses a shipped migration that was
#     edited (guide §11.2). `tests/migrations.rs` pins the same hashes for the
#     case this one cannot see: an edit made before anything was ever applied.
echo "→ Re-applying to check idempotence and the recorded checksums"
sqlx migrate run
sqlx migrate info

# The compile-time checked queries build from `.sqlx/` when no DATABASE_URL is
# set, which is how CI compiles offline. A cache that no longer matches the
# schema still compiles — it just describes columns that have moved. This is
# the "assumed debt" of ADR 0002, and this is where it is paid: the cache is
# regenerated against the database built above and compared.
echo "→ Checking the offline query cache against the fresh schema"
cargo sqlx prepare --check --workspace -- --all-targets

echo "✓ Migrations applied cleanly to a pristine database, and .sqlx is in sync."
