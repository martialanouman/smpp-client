/**
 * Commit message linting — CLAUDE.md §6, guide §14.2.
 *
 * Commit messages are written IN FRENCH, but the `type` stays in English
 * (`feat`, `fix`, …): it is not prose, it is a grammar machines read — it
 * drives the SemVer bump and feeds the CHANGELOG. Translating `feat` into
 * `fonc` would break both.
 *
 * This config is applied in two places, and both are necessary:
 *   · locally by `.husky/commit-msg`, which rejects the commit as it is
 *     written;
 *   · in CI by the `commitlint` job, which catches whatever slipped through a
 *     `--no-verify`.
 */
export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    // The eleven conventional types. Guide §14.2 names five explicitly (feat,
    // fix, refactor, test, docs); the other six cover the project's real
    // needs without opening the door to invented types.
    "type-enum": [
      2,
      "always",
      [
        "feat",
        "fix",
        "refactor",
        "perf",
        "test",
        "docs",
        "style",
        "build",
        "ci",
        "chore",
        "revert",
      ],
    ],

    // "scope = crate or area name" (CLAUDE.md §6). The rule only fires when a
    // scope is present, so `docs: …` without a scope stays valid — which
    // preserves the existing history.
    "scope-enum": [
      2,
      "always",
      [
        // The nine business crates.
        "smpp-core",
        "smpp-session",
        "rate-control",
        "messaging",
        "contacts",
        "numbers-gen",
        "persistence",
        "logging-export",
        "security",
        // Cross-cutting areas.
        "crates", // the Cargo workspace itself, when no single crate is meant
        "app", // src-tauri
        "ui",
        "ipc",
        "ci",
        "release",
        "deps",
        "repo",
        "just",
        "adr",
        "tasks",
        "supply-chain",
      ],
    ],

    // DELIBERATELY DISABLED. `@commitlint/config-conventional` rejects
    // `sentence-case` and `start-case` subjects, which produces false
    // positives on a French subject starting with a proper noun or a protocol
    // acronym: "ajoute le PDU broadcast_sm" would be refused.
    "subject-case": [0],

    "header-max-length": [2, "always", 100],
    "body-max-line-length": [1, "always", 100],
  },
};
