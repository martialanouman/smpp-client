# Index des jalons

Suivi d'avancement. Une case est cochée **au moment du merge** de la PR
correspondante, dans le dernier commit de cette PR. La fiche passe alors de
`tasks-todo/` à `tasks-done/`.

| ☑ | Jalon | Titre | Dépend de | PR |
|---|---|---|---|---|
| ☑ | [000](../tasks-done/step-000.md) | Fondations du dépôt, tooling, CI et release | — | [#1](https://github.com/martialanouman/smpp-client/pull/1) |
| ☑ | [001](../tasks-done/step-001.md) | Socle applicatif : shell Tauri, UI et contrat IPC typé | 000 | [#4](https://github.com/martialanouman/smpp-client/pull/4) |
| ☑ | [002](../tasks-done/step-002.md) | Persistance SQLite : schéma, migrations et repositories | 001 | [#6](https://github.com/martialanouman/smpp-client/pull/6) |
| ☑ | [003](../tasks-done/step-003.md) | Cœur protocolaire `smpp-core` | 000 | [#3](https://github.com/martialanouman/smpp-client/pull/3) |
| ☑ | [004](../tasks-done/step-004.md) | Encodage du texte et segmentation des messages longs | 003 | [#5](https://github.com/martialanouman/smpp-client/pull/5) |
| ☑ | [005](../tasks-done/step-005.md) | Session SMPP unique : acteurs, bind, keep-alive et reconnexion | 002, 003 | [#7](https://github.com/martialanouman/smpp-client/pull/7) |
| ☑ | [006](../tasks-done/step-006.md) | Envoi simple de bout en bout — **M1** | 004, 005 | [#8](https://github.com/martialanouman/smpp-client/pull/8) |
| ☐ | [007](step-007.md) | Fenêtrage, contrôle de débit et métriques temps réel | 006 | |
| ☐ | [008](step-008.md) | Accusés de livraison, journal métier et vue temps réel — **M2** | 007 | |
| ☐ | [009](step-009.md) | Contacts : import CSV/XLSX, validation E.164 et listes | 002 | |
| ☐ | [010](step-010.md) | Campagnes : envoi en masse, reprise et rejeu — **M3** | 008, 009 | |
| ☐ | [011](step-011.md) | Sessions multiples, multi-bind et routage | 010 | |
| ☐ | [012](step-012.md) | SMPP v5.0 complet et adaptation dynamique du débit — **M4** | 011 | |
| ☐ | [013](step-013.md) | Génération automatique de numéros valides par pays | 009 | |
| ☐ | [014](step-014.md) | Exports, statistiques et rétention — **M5** | 012, 013 | |
| ☐ | [015](step-015.md) | Sécurité : secrets, TLS, durcissement et usage responsable | 014 | |
| ☐ | [016](step-016.md) | Packaging, signature, notarisation et mises à jour — **M6** | 015 | |
| ☐ | [017](step-017.md) | Simulateur SMSC intégré et bancs de charge — **M7**, optionnel | 016 | |

## Ordre d'exécution

La chaîne critique compte quatorze arêtes :

```
000 → 001 → 002 → 005 → 006 → 007 → 008 → 010 → 011 → 012 → 014 → 015 → 016 → 017
```

Deux branches latérales s'en détachent et peuvent avancer en parallèle :

```
003 → 004 ─────────────────► 006
002 → 009 → 013 ───────────► 014
```

### Ce qui peut réellement tourner en parallèle

Deux conditions cumulatives : indépendance dans le graphe **et** empreinte de
fichiers disjointe. Les paires qui remplissent les deux :

| Lot | Jalons | Empreintes |
|---|---|---|
| 1 | **001 ∥ 003** | `src-tauri/` + `ui/` contre `crates/smpp-core/` + `docs/adr/` |
| 2 | **002 ∥ 004** | `crates/persistence/` + `migrations/` contre `crates/messaging/src/encoding/` |
| 3 | **009 ∥ 005** | `crates/contacts/` contre `crates/smpp-session/` |

Trois jalons ont une empreinte **totalement disjointe** du reste de l'arbre et
ne peuvent donc jamais entrer en conflit de fichiers : **009** (`contacts`),
**013** (`numbers-gen`), **017** (`smsc-sim`).

À l'inverse, deux grappes sont fortement couplées et se sérialisent :
005 · 007 · 011 · 012 sur `crates/smpp-session/`, et
006 · 010 · 011 · 012 · 015 sur `crates/messaging/`. Le jalon **015** est celui
dont l'empreinte s'étale le plus — sept zones — et ne se parallélise avec rien.

## Dette connue

| Sujet | Depuis | Où c'est décrit |
|---|---|---|
| CA-000-09 — la CI n'a jamais été vue échouer | jalon 000 | [CONTRIBUTING.md §6.1](../CONTRIBUTING.md#61-la-ci-échoue-t-elle-vraiment-et-sur-le-bon-job--ca-000-09) |
| CA-000-10 — `release.yml` n'a jamais produit d'artefact | jalon 000 | [CONTRIBUTING.md §6.2](../CONTRIBUTING.md#62-la-release-produit-elle-les-artefacts-attendus--ca-000-10) |
