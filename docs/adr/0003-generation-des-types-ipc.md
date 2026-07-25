# ADR 0003 — Générer les types IPC avec tauri-specta

> **Statut :** Accepté — **supersède** la mention `ts-rs` du guide §15.1
> **Date :** 2026-07-25 · **Jalon :** step-000 · **Décideur :** Martial Anouman

## Contexte

Le frontend et le backend échangent des DTO à travers l'IPC Tauri. Si les
types sont déclarés des deux côtés, ils divergent — c'est une question de
quand, pas de si. Et la divergence est **silencieuse** : `cargo build` et
`tsc` restent verts, la sérialisation échoue à l'exécution, chez
l'utilisateur.

Le guide §9.2 tranche déjà le principe : les DTO sont définis **une seule fois
en Rust**, les types TypeScript sont **générés**, et un diff non commité fait
échouer la CI (étape 4). Reste l'outil.

Le guide §15.1 nomme `ts-rs` ; la spec §15.4 laisse le choix entre `ts-rs` et
`tauri-specta` ; le jalon 000 §8 et CLAUDE.md §4 désignent `tauri-specta`.
Cette ADR lève la contradiction.

## Options envisagées

### Option A — `ts-rs`

Dérive `#[derive(TS)]` sur les structures et exporte des types TypeScript.

**Pour :** simple, stable, largement utilisé, indépendant de Tauri.
**Contre :** ne génère **que les types**. Les signatures de commandes — nom,
paramètres, type de retour, type d'erreur — restent à écrire à la main côté
TypeScript. C'est précisément là que se produisent les divergences : renommer
une commande Rust ou ajouter un paramètre ne casse rien à la compilation.
Aucune couverture des événements.

### Option B — `tauri-specta`

Génère les types **et** des fonctions d'appel typées à partir des commandes
`#[tauri::command]` annotées, ainsi que les définitions d'événements.

**Pour :** couvre toute la frontière, pas seulement les structures : commandes
et événements inclus. Renommer une commande côté Rust casse la compilation
TypeScript — le seul mécanisme qui rende la règle « jamais d'`invoke` brut »
de CLAUDE.md §4 réellement tenable, puisque les wrappers deviennent générés
plutôt qu'écrits.
**Contre :** en `2.0.0-rc.x`, pas encore stable ; couplé à Tauri, donc son
cycle de vie suit celui du framework.

## Décision

**Option B — tauri-specta.**

L'argument décisif est la **surface couverte**. Avec `ts-rs`, la moitié du
contrat — les signatures — reste manuelle, or c'est cette moitié qui casse. Un
générateur qui ne couvre que les structures laisse intacte la classe d'erreurs
que l'on cherche à éliminer.

Le risque de préversion est réel mais borné : la sortie est du TypeScript
ordinaire, commité dans le dépôt. Si `tauri-specta` devenait indisponible, le
code généré continuerait de fonctionner le temps de migrer.

Cette ADR **supersède explicitement** la mention `ts-rs` du guide §15.1.

## Conséquences

- **Positives :** une seule source de vérité pour les DTO, les commandes et
  les événements ; l'étape 4 de la CI protège réellement la frontière ; les
  wrappers de `ui/src/ipc/` sont générés, ce qui rend la règle « pas
  d'`invoke` brut » vérifiable au lieu d'être une convention de revue.
- **Négatives / dette assumée :** dépendance à une version *release
  candidate*. À réévaluer au jalon 001, quand le générateur sera réellement
  branché.
- **Impacts opérationnels :** `ui/src/ipc/generated/` est exclu de prettier et
  d'ESLint — sa mise en forme appartient au générateur, la reformater
  produirait un diff permanent contre l'étape 4.
  `scripts/check-ipc-types.sh` bascule automatiquement dès que
  `src-tauri/src/bin/gen_ipc.rs` existe, et **échoue** si un fichier généré
  apparaît sans générateur.
- **Point de réexamen :** au jalon 001. Si `tauri-specta` se révèle
  inutilisable, le repli est `ts-rs` + une revue disciplinée des signatures,
  ce qui serait une régression assumée et documentée par une nouvelle ADR.

## Références

- Spec §15.4 · Guide §9.2, §15.1 (étape 4) · CLAUDE.md §4
- `ui/src/ipc/README.md`, `scripts/check-ipc-types.sh`
- `tasks-todo/step-001.md`
- <https://github.com/specta-rs/tauri-specta>
