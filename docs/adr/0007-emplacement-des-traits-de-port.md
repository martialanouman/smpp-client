# ADR 0007 — Héberger les traits de port dans `persistence` jusqu'à l'arrivée de leurs consommateurs

> **Statut :** Accepté
> **Date :** 2026-07-26 · **Jalon :** step-002 · **Décideur :** Martial Anouman

## Contexte

Le guide §8.1 et CLAUDE.md §3 énoncent la même règle : un trait de port est
défini **dans la couche haute** et implémenté dans la couche basse. L'exemple
donné est nommément `MessageRepository`, « défini côté `messaging`, implémenté
côté `persistence` ». C'est l'inversion de dépendance classique : `messaging`
ne connaît alors aucune crate en dessous de lui et se teste contre un double.

Le jalon 002 doit livrer quatre traits de port (`MessageRepository`,
`ContactRepository`, `CampaignRepository`, `SessionProfileRepository`) et leurs
implémentations SQLx. Sa fiche, en L-002-04, laisse l'emplacement ouvert —
« `crates/persistence/src/ports.rs` (ou définis côté consommateur) » — et sa
note §6 demande de **trancher explicitement et de le documenter**.

Trois contraintes pèsent sur ce choix :

1. Les crates consommatrices n'existent qu'en coquille. `messaging` est le
   jalon 004, `contacts` le jalon 006 ; elles ne contiennent aujourd'hui qu'un
   `lib.rs` et un type d'erreur.
2. Un autre agent travaille en parallèle sur `crates/messaging/` pour le jalon
   004 ; le périmètre du jalon 002 exclut d'y toucher.
3. La doc de crate écrite au jalon 000 dans `crates/persistence/src/lib.rs`
   affirmait déjà deux choses incompatibles : que `MessageRepository`
   « appartient à `messaging` » et que `persistence` « ne dépend d'aucune autre
   crate interne ». Si le trait vit dans `messaging` et son implémentation dans
   `persistence`, alors `persistence` dépend de `messaging`. L'une des deux
   affirmations devait tomber.

## Options envisagées

### Option A — Définir les traits dans les crates consommatrices dès maintenant

`MessageRepository` dans `messaging`, `ContactRepository` dans `contacts`, etc.
`persistence` déclare `messaging` et `contacts` en dépendances pour les
implémenter.

**Pour :** conforme à la lettre du guide §8.1 dès le premier jour ; aucun
déplacement ultérieur.
**Contre :** crée immédiatement les arêtes `persistence → messaging` et
`persistence → contacts` — c'est-à-dire le **coût** de l'inversion, l'arête
remontante, sans le bénéfice, puisque aucun consommateur n'existe encore pour
s'en servir. Impose de modifier `crates/messaging/`, hors périmètre du jalon et
en cours de modification par un autre agent. Oblige aussi à placer
`CampaignId`, `ContactId` et `ListId` dans ces coquilles, alors que le jalon
004 décidera peut-être d'une autre forme.

### Option B — Définir les traits dans `persistence`, à côté de leur implémentation

Un module `ports` public dans `persistence`. Les consommateurs les importeront
depuis là ; le jour où l'un d'eux veut réellement inverser la dépendance, il
déclare le trait chez lui.

**Pour :** `persistence` reste sans dépendance interne autre que `smpp-core`
(vers le bas) ; aucune arête remontante ; aucune modification hors périmètre.
Les traits existent, donc les doubles de test aussi — ce que la fiche demande.
**Contre :** ne respecte pas la lettre du guide §8.1 tant que le déplacement
n'a pas eu lieu, et un déplacement remis à plus tard a une tendance connue à ne
jamais avoir lieu.

### Option C — Une crate `ports` dédiée, dont tout le monde dépend

**Pour :** neutre vis-à-vis du sens des dépendances.
**Contre :** dixième crate au workspace pour héberger quatre traits ; déplace
le problème sans le résoudre — la couche haute ne possède toujours pas son
contrat — et ajoute un niveau d'indirection que le guide ne prévoit pas.

## Décision

**Option B.** Les quatre traits — plus `PduLogRepository`, ajouté par symétrie
— vivent dans `crates/persistence/src/ports.rs`.

Le critère d'arbitrage est le suivant : l'inversion de dépendance n'a de valeur
que **pour un consommateur qui l'utilise**. Son bénéfice est de rendre
`messaging` testable sans `persistence` et remplaçable sans le toucher. Payer
son coût — l'arête `persistence → messaging` — quand `messaging` est vide, ce
n'est pas respecter le principe, c'est en imiter la forme.

Le déplacement, lorsqu'il arrivera, est une modification de déclaration : la
forme du trait, ses implémentations et les doubles écrits contre lui ne
changent pas. Le module `ports` porte cette intention en tête de fichier, et la
doc de crate ne prétend plus que les traits appartiennent ailleurs.

## Conséquences

- **Positives :** `persistence` ne dépend que de `smpp-core`, une arête
  descendante ; le graphe reste acyclique et lisible. Les doubles de test sont
  possibles dès aujourd'hui. Aucune modification hors du périmètre du jalon.
- **Négatives / dette assumée :** l'écart avec le guide §8.1 est réel et reste
  ouvert jusqu'aux jalons 004 et 006. Un consommateur qui se contenterait
  d'importer `persistence::ports` sans jamais rapatrier son contrat
  perpétuerait l'écart sans que rien ne le signale — aucun test ne peut
  détecter une inversion qu'on a renoncé à faire.
- **Impacts opérationnels :** aucun sur la CI, le packaging ou la sécurité.
  `CampaignId`, `ContactId` et `ListId` suivent la même logique et le même
  sort : définis dans `persistence::records::ids` avec la même note.
- **Point de réexamen :** au jalon 004, quand `messaging` acquiert sa logique.
  Si `messaging` a besoin d'un double de `MessageRepository`, c'est le moment
  de rapatrier le trait chez lui et de faire dépendre `persistence` de
  `messaging`. Si à la fin du jalon 006 le déplacement n'a toujours pas eu
  lieu, il faut soit le faire, soit superséder cette ADR en assumant que
  l'inversion n'aura pas lieu.

## Références

- Guide §8.1 (sens des dépendances), §8.2 (injection de dépendances)
- CLAUDE.md §3 (frontières et inversion par traits)
- `tasks-todo/step-002.md` L-002-04 et note §6
- ADR [0002](0002-persistance-sqlite-sqlx.md)
