# Jalon 009 — Contacts : import CSV/XLSX, validation E.164 et listes

> **Statut :** À faire · **Dépend de :** step-002 · **Réf. spec :** §11 · **Exigences :** EF-CTC-01 à EF-CTC-05, EF-CTC-07

## 1. Objectif

Importer des destinataires depuis un fichier CSV ou XLSX, mapper librement les colonnes, valider et normaliser chaque numéro au format E.164, dédoublonner, produire un rapport d'import exploitable, et organiser les contacts en listes réutilisables.

L'import est la porte d'entrée des données réelles : un fichier client contient des séparateurs exotiques, des encodages hérités, des numéros au format national avec espaces, des doublons et des lignes vides. Le traiter en streaming et rendre compte précisément des rejets évite les campagnes silencieusement amputées.

## 2. Périmètre

### Dans le périmètre

- Crate `contacts`.
- **CSV** : détection automatique du séparateur (`,`, `;`, tabulation) et de l'encodage (UTF-8, UTF-8 BOM, Latin-1), guillemets, retours à la ligne échappés, en-têtes ; lecture en streaming (crate `csv`).
- **XLSX** : lecture via `calamine`, choix de la feuille, lecture ligne à ligne.
- Assistant de **mapping des colonnes** : numéro (obligatoire), pays (optionnel), attributs libres réutilisables comme variables de modèle ; mapping mémorisable en profil d'import réutilisable.
- Validation/normalisation via `phonenumber` : parsing avec pays par défaut (colonne pays, indicatif détecté, ou pays global de l'import), `is_valid_number`, type de ligne (mobile/fixe) avec option « mobiles uniquement », normalisation E.164, motif de rejet précis pour les invalides.
- Déduplication sur le MSISDN normalisé (option : première occurrence conservée ou fusion des attributs).
- **Rapport d'import** : total, valides, invalides par motif, doublons, répartition mobile/fixe ; lignes rejetées exportables pour correction.
- Listes/groupes nommés, filtrables et combinables (union/intersection).
- Événement `import:progress`, import annulable.
- Commandes `contacts_import`, `contacts_query`.
- Écran Contacts : import, assistant de mapping, table virtualisée, recherche, rapport.

### Hors périmètre

- La génération de numéros → **step-013**.
- L'utilisation des contacts comme source de campagne → **step-010**.
- L'export des contacts au-delà du fichier de rejets → **step-014**.
- La liste d'exclusion / opt-out → **step-015**.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-009-01 | Lecteur CSV en streaming (détection séparateur/encodage) | `crates/contacts/src/import/csv.rs` |
| L-009-02 | Lecteur XLSX (`calamine`, multi-feuilles) | `crates/contacts/src/import/xlsx.rs` |
| L-009-03 | Mapping de colonnes + profils d'import persistés | `crates/contacts/src/import/mapping.rs` |
| L-009-04 | Validation/normalisation E.164 | `crates/contacts/src/validation.rs` |
| L-009-05 | Déduplication et rapport d'import | `crates/contacts/src/import/report.rs` |
| L-009-06 | Listes et groupes | `crates/contacts/src/lists.rs` |
| L-009-07 | Commandes IPC + `import:progress` | `src-tauri/src/commands/contacts.rs` |
| L-009-08 | Écran Contacts et assistant de mapping | `ui/src/views/Contacts/` |

## 4. Critères d'acceptation

- [ ] **CA-009-01** — Un CSV de 1 000 000 de lignes s'importe sans que la mémoire du processus ne croisse proportionnellement au fichier (streaming vérifié par mesure).
- [ ] **CA-009-02** — Les séparateurs `,`, `;` et tabulation sont détectés automatiquement ; un fichier UTF-8 avec BOM et un fichier Latin-1 sont lus sans caractères corrompus.
- [ ] **CA-009-03** — Un XLSX multi-feuilles permet de choisir la feuille ; les cellules numériques contenant un numéro (piège classique : perte du `+` et du zéro initial, notation scientifique) sont correctement récupérées ou explicitement rejetées avec un motif clair.
- [ ] **CA-009-04** — Un numéro au format national (`0700000000` + pays `CI`) est normalisé en `+2250700000000`.
- [ ] **CA-009-05** — Les numéros invalides sont rejetés avec un **motif précis** (trop court, indicatif inconnu, format incorrect, type de ligne exclu) et sont exportables pour correction.
- [ ] **CA-009-06** — L'option « mobiles uniquement » exclut effectivement les lignes fixes.
- [ ] **CA-009-07** — Les doublons sont détectés sur le MSISDN **normalisé** (`+2250700000000` et `00225 07 00 00 00 00` sont un seul contact) ; les deux stratégies (première occurrence / fusion des attributs) fonctionnent.
- [ ] **CA-009-08** — Le rapport d'import est exact : total = valides + invalides + doublons, contrôlé par test sur un fichier de référence.
- [ ] **CA-009-09** — Un profil de mapping enregistré est réutilisable sur un fichier de même structure sans ressaisie.
- [ ] **CA-009-10** — L'import est annulable en cours ; l'annulation laisse la base cohérente (transaction ou marquage explicite de l'import partiel), jamais à moitié écrite sans trace.
- [ ] **CA-009-11** — `import:progress` est throttlé et n'inonde pas l'IPC ; l'UI reste réactive pendant l'import.
- [ ] **CA-009-12** — Les listes supportent union et intersection, et servent de source sélectionnable.

## 5. Tests attendus

- **Unitaires :** détection de séparateur et d'encodage sur un corpus de fichiers ; normalisation E.164 par pays (au moins 10 pays de plans différents) ; motifs de rejet ; déduplication ; fusion d'attributs.
- **Intégration :** import complet CSV et XLSX avec fichiers de fixtures (nominal, colonnes désordonnées, lignes vides, en-tête absent, cellules numériques, caractères accentués) ; annulation en cours d'import.
- **Volumétrie :** import d'un fichier généré de 1 000 000 lignes, mémoire et durée mesurées.
- **Frontend :** assistant de mapping, affichage du rapport, table virtualisée.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest library "calamine" "read XLSX rows lazily and handle typed cell values in Rust"
  npx ctx7@latest library "phonenumber" "validate number, detect line type mobile vs fixed line and format E164"
  ```
- Le parsing XLSX est **CPU-intensif** : l'exécuter via `tokio::task::spawn_blocking`, jamais directement sur le runtime async (guide §7.1).
- Les cellules Excel contenant des numéros sont le piège majeur de ce jalon : Excel transforme volontiers `+225070000000` en nombre, perd le `+`, tronque les zéros de tête ou passe en notation scientifique. Traiter explicitement les variantes `String`, `Float`, `Int` et documenter le comportement retenu.
- La déduplication sur un million de lignes ne doit pas charger tous les MSISDN en mémoire sans réflexion : soit un index unique en base, soit une structure compacte — mesurer avant de choisir.
- Le rapport d'import est un livrable utilisateur autant que technique : les motifs de rejet doivent être compréhensibles et traduits (i18n), pas des codes internes.
