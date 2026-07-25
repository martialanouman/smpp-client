/**
 * Vérification des messages de commit — CLAUDE.md §6, guide §14.2.
 *
 * Les messages sont rédigés EN FRANÇAIS, mais le `type` reste en anglais
 * (`feat`, `fix`, …) : ce n'est pas de la prose, c'est une grammaire lue par
 * les machines — elle détermine le bump SemVer et alimente le CHANGELOG.
 * Traduire `feat` en `fonc` casserait les deux.
 *
 * Ce fichier est appliqué à deux endroits, et les deux sont nécessaires :
 *   · localement par `.husky/commit-msg`, qui rejette le commit à l'écriture ;
 *   · en CI par le job `commitlint`, qui rattrape ce qu'un `--no-verify` a
 *     laissé passer.
 */
export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    // Les 11 types conventionnels. Le guide §14.2 en cite explicitement cinq
    // (feat, fix, refactor, test, docs) ; les six autres couvrent les besoins
    // réels du projet sans ouvrir la porte à des types inventés.
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

    // « scope = nom de crate ou de zone » (CLAUDE.md §6). La règle ne se
    // déclenche que si un scope est présent : `docs: …` sans scope reste
    // valide, ce qui préserve l'historique existant.
    "scope-enum": [
      2,
      "always",
      [
        // Les neuf crates métier.
        "smpp-core",
        "smpp-session",
        "rate-control",
        "messaging",
        "contacts",
        "numbers-gen",
        "persistence",
        "logging-export",
        "security",
        // Zones transverses.
        "crates", // le workspace Cargo lui-même, quand aucune crate n'est visée
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

    // DÉSACTIVÉ VOLONTAIREMENT. `@commitlint/config-conventional` refuse les
    // sujets en `sentence-case` et `start-case`, ce qui produit des faux
    // positifs sur un sujet français commençant par un nom propre ou un
    // acronyme protocolaire : « ajoute le PDU broadcast_sm » serait rejeté.
    "subject-case": [0],

    "header-max-length": [2, "always", 100],
    "body-max-line-length": [1, "always", 100],
  },
};
