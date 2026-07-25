# Jalon 013 — Génération automatique de numéros valides par pays

> **Statut :** À faire · **Dépend de :** step-009 · **Réf. spec :** §12 · **Exigences :** EF-CTC-06

## 1. Objectif

Produire, pour un pays donné, un ensemble de numéros **structurellement valides** au sens du plan de numérotation national, en quantité arbitraire, avec option d'unicité et génération reproductible par graine.

Cette fonction sert les tests de charge et la validation de plateformes. Elle ne prétend pas que les numéros soient attribués à un abonné réel — cette distinction doit être visible dans l'interface, pas seulement dans la documentation.

## 2. Périmètre

### Dans le périmètre

- Crate `numbers-gen` : chargement des métadonnées libphonenumber par pays (indicatif, préfixes/plages mobiles, longueurs nationales, motifs de validation).
- Algorithme de la spec §12.3 : sélection d'un préfixe valide (aléatoire pondéré ou imposé), complétion aléatoire jusqu'à la longueur nationale, assemblage E.164, **validation systématique** par `is_valid_number`, rejet et régénération des invalides.
- **RNG injecté** et graine optionnelle → génération reproductible.
- Option d'unicité (rejet des doublons) et option de type de ligne (mobile / fixe / tous).
- Génération en **streaming** vers la base pour de gros volumes (millions), sans saturation mémoire, annulable, avec progression.
- Statistiques de génération : taux de validité, répartition par préfixe.
- Commande `numbers_generate` ; écran Générateur : sélecteur de pays (drapeau + indicatif), quantité, type de ligne, préfixe/opérateur optionnel, unicité, graine, destination (nouvelle liste ou alimentation d'une campagne), **aperçu de quelques exemples avant génération en masse**.
- Avertissement d'usage explicite et visible dans l'écran (spec §12.1, §17.6).

### Hors périmètre

- La liste d'exclusion appliquée avant envoi et les plafonds de volume → **step-015**.
- L'export des listes générées → **step-014**.
- L'envoi vers les numéros générés → jalons 006/010, inchangés.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-013-01 | Chargement des métadonnées par pays | `crates/numbers-gen/src/metadata.rs` |
| L-013-02 | Générateur (RNG injecté, graine, unicité) | `crates/numbers-gen/src/generator.rs` |
| L-013-03 | Écriture en streaming vers la base | `crates/numbers-gen/src/sink.rs` |
| L-013-04 | Statistiques de génération | `crates/numbers-gen/src/stats.rs` |
| L-013-05 | Commande `numbers_generate` + progression | `src-tauri/src/commands/numbers.rs` |
| L-013-06 | Écran Générateur avec aperçu et avertissement | `ui/src/views/Numbers/` |

## 4. Critères d'acceptation

- [ ] **CA-013-01** — **100 %** des numéros générés passent `is_valid_number` pour le pays demandé : test sur au moins 10 pays de plans différents, 10 000 numéros chacun.
- [ ] **CA-013-02** — Deux générations avec la **même graine** produisent exactement la même séquence ; deux graines différentes produisent des séquences différentes.
- [ ] **CA-013-03** — L'option d'unicité garantit zéro doublon sur 1 000 000 de numéros générés.
- [ ] **CA-013-04** — L'option de type de ligne est respectée : en mode mobile, aucun numéro fixe n'est produit.
- [ ] **CA-013-05** — Un préfixe opérateur imposé est respecté et validé ; un préfixe invalide pour le pays est refusé avec un message explicite avant toute génération.
- [ ] **CA-013-06** — La génération de 1 000 000 de numéros s'effectue en streaming, à mémoire bornée, et est annulable en cours sans laisser la base incohérente.
- [ ] **CA-013-07** — L'aperçu affiche des exemples réels issus du même algorithme que la génération en masse (pas une simulation approchée).
- [ ] **CA-013-08** — Les statistiques (taux de validité, répartition par préfixe) sont exactes, contrôlées par test.
- [ ] **CA-013-09** — Le cas « quantité demandée impossible à atteindre » (unicité + espace de numérotation trop petit) est détecté et signalé, sans boucle infinie : test avec un pays à espace restreint et une quantité supérieure au nombre de combinaisons possibles.
- [ ] **CA-013-10** — L'avertissement d'usage (numéros syntaxiquement valides, non attribués ; légalité de l'envoi non sollicité) est visible dans l'écran, non masquable, et traduit FR/EN.
- [ ] **CA-013-11** — Le RNG est injecté ; aucun appel direct à un générateur global dans la logique métier (`rg` sur les sources de la crate).

## 5. Tests attendus

- **Propriété :** tout numéro généré est valide ; l'unicité tient sur de gros volumes ; la reproductibilité par graine tient sur des paramètres arbitraires.
- **Unitaires :** chargement des métadonnées pour un échantillon de pays (dont Côte d'Ivoire, France, Nigeria, États-Unis, Inde — plans de structures différentes) ; complétion à la bonne longueur ; refus de préfixe invalide ; détection de la quantité inatteignable.
- **Volumétrie :** génération d'un million de numéros — mémoire, durée, annulation en cours.
- **Frontend :** aperçu, sélecteur de pays, présence de l'avertissement.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest library "phonenumber" "access country metadata, national number patterns and validate generated numbers in Rust"
  ```
- La génération naïve « préfixe + chiffres aléatoires » a un **taux de rejet** qui peut être élevé selon les pays. Mesurer ce taux et, s'il dégrade les performances, restreindre l'espace de tirage aux motifs valides plutôt que de multiplier les essais — mais **sans jamais** contourner la validation finale par `is_valid_number`, qui reste le seul juge.
- La détection de la quantité inatteignable doit être faite **avant** de lancer la génération quand c'est calculable, et par un compteur d'échecs consécutifs sinon. Sans cela, l'unicité produit une boucle infinie sur un espace saturé — c'est le principal risque de blocage de ce jalon.
- La génération est CPU-intensive : `spawn_blocking` ou tâche dédiée, jamais sur le runtime async.
- Marquer la provenance des contacts générés (`source = "generated"`) afin de pouvoir les distinguer, les filtrer et les purger indépendamment des contacts importés.
