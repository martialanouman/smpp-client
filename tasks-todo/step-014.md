# Jalon 014 — Exports, statistiques et rétention (= M5)

> **Statut :** À faire · **Dépend de :** step-012, step-013 · **Réf. spec :** §13.4–13.6, §18.1 · **Exigences :** EF-LOG-03, EF-LOG-04, EF-LOG-05, EF-LOG-06

## 1. Objectif

Exporter les journaux et les statistiques dans les formats attendus (CSV, XLSX, JSON, JSONL, dump hexadécimal de PDU), présenter des tableaux de bord agrégés par campagne et par session, et maintenir la taille de la base sous contrôle par une politique de rétention. C'est le milestone **M5**.

L'export est le livrable final du travail de l'utilisateur : il doit fonctionner sur des millions de lignes sans limite mémoire, donc être **streamé** de bout en bout — de la requête base jusqu'à l'écriture fichier.

## 2. Périmètre

### Dans le périmètre

- Exports en streaming, avec progression (`export:progress`) et annulation :
  - **CSV** — séparateur et encodage configurables, en-têtes localisés ;
  - **XLSX** — via `rust_xlsxwriter`, colonnes typées, feuille de données + feuille de statistiques ;
  - **JSON** et **JSONL** — JSONL pour le streaming ligne à ligne des gros volumes ;
  - **dump hexadécimal** d'une sélection de PDU pour le débogage.
- Portées d'export : messages filtrés, campagne, ensemble ; agrégats exportables.
- Sélection de l'emplacement via dialogue natif.
- Statistiques et tableaux de bord : totaux envoyés/acceptés/livrés/échoués, taux de livraison, débit moyen et pic, latence moyenne, répartition des codes d'erreur, courbes temporelles, par campagne et par session ; historique des campagnes.
- Rétention : durée configurable, purge manuelle ou automatique, archivage compressé (export puis suppression), `VACUUM` planifié.
- Commandes `logs_export`, `stats_get` ; écran Statistiques.

### Hors périmètre

- Le masquage du contenu des messages dans les exports → **step-015** (l'option existe dans le modèle, elle est appliquée là-bas).
- Le bundle de diagnostic support → **step-015**.
- L'export vers des systèmes externes ou une API : hors périmètre du projet.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-014-01 | Moteur d'export streamé, commun aux formats | `crates/logging-export/src/export/mod.rs` |
| L-014-02 | Formatteurs CSV / XLSX / JSON / JSONL / hex | `crates/logging-export/src/export/formats/` |
| L-014-03 | Requêtes d'agrégation statistique | `crates/logging-export/src/stats.rs` |
| L-014-04 | Politique de rétention, purge et VACUUM | `crates/logging-export/src/retention.rs` |
| L-014-05 | Commandes `logs_export` / `stats_get` + `export:progress` | `src-tauri/src/commands/export.rs` |
| L-014-06 | Écran Statistiques et tableaux de bord | `ui/src/views/Stats/` |

## 4. Critères d'acceptation

- [ ] **CA-014-01** — Export CSV de 1 000 000 de messages : le fichier est complet, la mémoire du processus reste bornée, la progression avance et l'annulation fonctionne (fichier partiel supprimé ou clairement marqué).
- [ ] **CA-014-02** — Export XLSX : les colonnes sont typées (dates en dates, entiers en entiers), la feuille de statistiques est présente, et le fichier s'ouvre sans avertissement dans un tableur.
- [ ] **CA-014-03** — Export JSONL : une ligne = un objet JSON valide ; le fichier est relisible en streaming ligne par ligne.
- [ ] **CA-014-04** — Les en-têtes exportés sont localisés selon la langue de l'application (FR/EN).
- [ ] **CA-014-05** — Les filtres actifs sur l'écran Journaux sont ceux appliqués à l'export : test comparant le nombre de lignes exportées au nombre affiché.
- [ ] **CA-014-06** — Les agrégats sont exacts : total = livrés + échoués + en attente ; le taux de livraison, le débit moyen et le pic sont contrôlés contre un jeu de données de référence.
- [ ] **CA-014-07** — Le calcul des statistiques sur une base d'un million de messages répond en moins de 2 secondes (index appropriés ; ajouter les index manquants via une **nouvelle** migration, sans jamais éditer une migration livrée).
- [ ] **CA-014-08** — La purge par rétention supprime exactement les enregistrements au-delà de la durée configurée, et rien d'autre.
- [ ] **CA-014-09** — L'archivage compressé exporte avant de supprimer ; une interruption en cours d'archivage **ne supprime rien** (l'ordre exporter → vérifier → supprimer est vérifié par test).
- [ ] **CA-014-10** — `VACUUM` réduit effectivement la taille du fichier après une purge importante, et ne s'exécute pas pendant une campagne active.
- [ ] **CA-014-11** — L'export d'un dump hexadécimal ne fonctionne que sur les PDU journalisés et signale clairement si le journal PDU était désactivé.
- [ ] **CA-014-12** — **Recette M5 :** génération, exports dans les quatre formats et tableaux de bord démontrés de bout en bout.

## 5. Tests attendus

- **Intégration :** export de chaque format sur un jeu de 100 000 lignes, relecture et comparaison au contenu de la base ; annulation en cours d'export ; export avec filtres combinés.
- **Unitaires :** échappement CSV (séparateur, guillemets et retours à la ligne **dans** le contenu du message), typage des cellules XLSX, sérialisation JSON des champs optionnels, calculs d'agrégats.
- **Volumétrie :** export d'un million de lignes — mémoire, durée.
- **Rétention :** purge aux bornes exactes de la durée, archivage interrompu, VACUUM.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest library "rust_xlsxwriter" "write large XLSX in constant memory mode with typed columns and multiple worksheets"
  ```
- **Le contenu d'un SMS peut contenir le séparateur CSV, des guillemets et des retours à la ligne.** L'échappement doit être délégué à la crate `csv` et testé explicitement — un export mal échappé décale silencieusement toutes les colonnes suivantes.
- `rust_xlsxwriter` propose un mode d'écriture à mémoire constante : le privilégier pour les gros volumes.
- XLSX a une limite d'environ 1 048 576 lignes par feuille : décider du comportement au-delà (découpage en plusieurs feuilles ou refus explicite avec suggestion de CSV/JSONL) plutôt que de produire un fichier tronqué.
- Les agrégats doivent être calculés **en base** (SQL) et non en Rust après chargement : c'est le seul moyen de tenir l'objectif de 2 secondes sur un million de lignes.
- `VACUUM` verrouille la base : ne jamais le déclencher pendant une campagne active, et le documenter dans le runbook.
