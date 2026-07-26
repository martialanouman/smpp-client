# Standardised tasks — guide §3.4, CLAUDE.md §5.
#
# WARNING — these recipes and .github/workflows/ci.yml must stay aligned. CI
# deliberately does NOT call `just`: it repeats the commands verbatim, which
# avoids installing one more tool on three operating systems. The trade-off is
# a drift risk with no satisfying automatic guard. Any change here means
# checking ci.yml, and the other way round.
#
# Every recipe is a sequence of plain commands with no shell constructs: they
# must run identically under sh and under PowerShell.

# List the available recipes.
default:
    @just --list

# --- Formatting -------------------------------------------------------------

# Format Rust and TypeScript code.
fmt:
    cargo fmt --all
    pnpm -C ui format

# Check formatting without modifying anything (CI step 1).
fmt-check:
    cargo fmt --all --check
    pnpm -C ui format:check

# --- Quality ----------------------------------------------------------------

# Lint Rust and TypeScript, plus type checking (CI steps 2 and 3).
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    pnpm -C ui typecheck
    pnpm -C ui lint

# Check that the generated IPC types are up to date (CI step 4).
ipc-check:
    bash scripts/check-ipc-types.sh

# --- Tests ------------------------------------------------------------------

# Unit tests, doctests and frontend tests (CI steps 5 and 6).
test:
    cargo nextest run --workspace
    cargo test --doc --workspace
    pnpm -C ui test --run

# --- Supply chain -----------------------------------------------------------

# Vulnerabilities and licences (CI step 7).
audit:
    cargo audit
    cargo deny check advisories bans licenses sources

# --- Database ---------------------------------------------------------------

# Requires `sqlx-cli` and a DATABASE_URL — see .env.example. The application
# itself never calls this: it embeds the same migrations through
# `sqlx::migrate!()` and applies them on open. This recipe is for the
# developer database the compile-time checked queries are built against.

# Apply migrations to the database DATABASE_URL points at.
migrate:
    sqlx migrate run

# `.sqlx/` is committed so the workspace compiles with no database in reach
# (ADR 0002). Changing a `query!` without running this makes the build fail
# with "no cached data for this query"; changing the SCHEMA without running it
# is worse — the build passes on stale metadata, which is what CI step 8
# catches.

# Regenerate the offline query cache after changing a query or the schema.
sqlx-prepare:
    cargo sqlx prepare --workspace -- --all-targets

# Check migrations against a fresh database (CI step 8).
migrate-check:
    bash scripts/check-migrations.sh

# --- Application ------------------------------------------------------------

# Run the application with hot reload.
dev:
    pnpm tauri dev

# Produce the native packages (CI step 9).
build:
    pnpm tauri build

# --- Shortcut ---------------------------------------------------------------

# Everything that must be green before a commit (CLAUDE.md §5).
check: fmt-check lint test audit
