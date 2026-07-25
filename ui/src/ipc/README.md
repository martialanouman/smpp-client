# `src/ipc/` — frontière avec le backend

Deux règles, toutes deux outillées.

## 1. Les types sont générés, jamais écrits à la main

Les DTO sont définis **une seule fois en Rust** (`serde` + `specta`) ; leurs
équivalents TypeScript sont produits par `tauri-specta` dans
`src/ipc/generated/` — cf.
[ADR 0003](../../../docs/adr/0003-generation-des-types-ipc.md).

Redéclarer un type ici à la main crée une divergence silencieuse entre les deux
côtés de l'IPC : le compilateur Rust et `tsc` sont alors tous les deux verts
alors que la sérialisation échoue à l'exécution.

L'étape 4 de la CI régénère et compare (`git diff --exit-code`) : un diff non
commité fait échouer le build.

> Au jalon 000, le générateur n'est pas encore branché — il arrive au
> **jalon 001**. `scripts/check-ipc-types.sh` le détecte et laisse l'étape
> passer, mais **échoue** si un fichier généré apparaît ici sans générateur :
> l'état vide n'est toléré que tant qu'il est réellement vide.

## 2. Seul ce répertoire importe `@tauri-apps/api`

Les composants n'appellent jamais `invoke` directement ; ils passent par les
wrappers typés exposés ici (CLAUDE.md §4). La règle `no-restricted-imports` de
`eslint.config.js` interdit l'import de `@tauri-apps/*` partout **sauf** dans
ce répertoire.

Un wrapper y valide la forme de la réponse et traduit le DTO d'erreur
`{ code, message, details }` en type exploitable par l'interface.
