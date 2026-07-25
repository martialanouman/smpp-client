# Jalon 016 — Packaging, signature, notarisation et mises à jour (= M6)

> **Statut :** À faire · **Dépend de :** step-015 · **Réf. spec :** §20 · **Réf. guide :** §19 · **Exigences :** ENF-POR-01, ENF-POR-02

## 1. Objectif

Produire des binaires installables et **signés** pour Windows, macOS et Linux à partir d'un tag, avec un mécanisme de mise à jour signé et une procédure de release documentée incluant le rollback. C'est le milestone **M6** et la condition d'une distribution réelle.

Sans signature ni notarisation, macOS bloque l'application via Gatekeeper et Windows affiche un avertissement SmartScreen : le produit est techniquement fonctionnel mais pratiquement indistribuable.

## 2. Périmètre

### Dans le périmètre

- Bundles complets via `tauri build` : `.msi` (WiX) et `.exe` (NSIS) ; `.dmg`/`.app` macOS pour Intel et Apple Silicon ; `.deb`, `.rpm` et AppImage Linux.
- **Signature Windows** : Authenticode.
- **Signature et notarisation macOS** : Developer ID, soumission à Apple, agrafage (stapling), vérification Gatekeeper.
- **Linux** : checksums publiés et signature des paquets.
- Complétion du workflow `release.yml` de step-000 avec les étapes de signature et les secrets prévus.
- **Updater Tauri** : manifeste signé, vérification de signature, canal stable ; ou désactivation explicite et documentée si la distribution reste manuelle.
- Procédure de release documentée (guide §19.3) : gel de `main`, checklist de déploiement, bump de version + tag `vX.Y.Z`, build/signature, publication, vérification post-release.
- Politique de **rollback** : conservation des artefacts N-1, critères de retour arrière définis à l'avance.
- CHANGELOG de release généré à partir des Conventional Commits.
- Prérequis d'exécution documentés par OS (WebView2, WKWebView, `webkit2gtk`).

### Hors périmètre

- La distribution via des magasins d'applications (Microsoft Store, App Store) et les dépôts de paquets Linux tiers.
- Les mises à jour delta.
- L'infrastructure d'hébergement du manifeste updater : à décider et documenter, non implémentée ici.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-016-01 | Configuration complète du bundler par cible | `src-tauri/tauri.conf.json` |
| L-016-02 | Workflow de release avec signature et notarisation | `.github/workflows/release.yml` |
| L-016-03 | Configuration de l'updater et manifeste signé | `src-tauri/tauri.conf.json`, `src-tauri/src/updater.rs` |
| L-016-04 | Runbook de release et de rollback | `docs/runbooks/release.md` |
| L-016-05 | Documentation des secrets CI requis | `CONTRIBUTING.md` |
| L-016-06 | Génération du CHANGELOG de release | `CHANGELOG.md`, outillage |
| L-016-07 | Checklist de vérification post-release | `docs/runbooks/release.md` |

## 4. Critères d'acceptation

- [ ] **CA-016-01** — Un tag `vX.Y.Z` produit automatiquement **tous** les artefacts : `.msi`, `.exe`, `.dmg` (aarch64 et x86_64), `.deb`, `.rpm`, `.AppImage`.
- [ ] **CA-016-02** — L'exécutable Windows est signé Authenticode : `signtool verify /pa` réussit et l'installation n'affiche pas d'avertissement d'éditeur inconnu.
- [ ] **CA-016-03** — L'application macOS est signée Developer ID **et notarisée** : `spctl -a -vvv` et `stapler validate` réussissent ; l'ouverture sur une machine propre ne déclenche aucun blocage Gatekeeper.
- [ ] **CA-016-04** — Les artefacts Linux sont accompagnés de checksums vérifiables publiés avec la release.
- [ ] **CA-016-05** — **Installation propre vérifiée sur chaque OS** depuis l'artefact publié, suivie d'un smoke test : lancement, bind sur le SMSC de test, envoi d'un message, réception du DLR.
- [ ] **CA-016-06** — La mise à jour d'une version N-1 vers N fonctionne via l'updater, avec vérification de signature du manifeste ; un manifeste dont la signature est invalide est **refusé**.
- [ ] **CA-016-07** — Les données utilisateur (base, configuration) sont préservées à travers la mise à jour, et les migrations éventuelles s'appliquent au premier démarrage.
- [ ] **CA-016-08** — L'empreinte mémoire au repos est inférieure à **150 Mo** sur les trois OS (ENF-PERF-04), mesurée sur le binaire packagé et non sur un build de développement.
- [ ] **CA-016-09** — Le CHANGELOG de la release liste ajouts, corrections, changements cassants et migrations requises, généré depuis les commits.
- [ ] **CA-016-10** — Le runbook de release est exécutable par un tiers sans connaissance implicite ; il a été suivi **intégralement** au moins une fois pour une release de test.
- [ ] **CA-016-11** — Les artefacts N-1 sont conservés et le rollback documenté est testé au moins une fois.
- [ ] **CA-016-12** — En l'absence des secrets de signature, le workflow produit des artefacts non signés et **le signale explicitement** dans les logs, sans échouer silencieusement ni prétendre avoir signé.
- [ ] **CA-016-13** — **Recette M6 :** binaires signés Windows/macOS/Linux, installés et vérifiés.

## 5. Tests attendus

- **Manuels scriptés :** installation propre + smoke test sur les trois OS (procédure consignée dans le runbook, résultats archivés).
- **Automatisés :** vérification de signature dans le workflow après build ; contrôle de présence de tous les artefacts attendus ; validation du manifeste updater.
- **Mise à jour :** N-1 → N avec préservation des données et application des migrations ; refus d'un manifeste mal signé.
- **Performance :** mesure de l'empreinte mémoire au repos sur les binaires packagés.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /tauri-apps/tauri-docs "code signing macOS notarization Windows Authenticode and updater configuration for Tauri 2"
  ```
- **La notarisation est asynchrone et peut prendre plusieurs minutes** : prévoir un timeout généreux dans le workflow et un traitement explicite du rejet Apple (avec le motif) plutôt qu'un échec opaque.
- Les certificats et secrets ne doivent jamais apparaître dans les logs de CI : utiliser exclusivement `${{ secrets.* }}` et vérifier qu'aucune commande ne les imprime (attention aux `set -x` et aux `echo` de débogage).
- Le keychain temporaire créé sur le runner macOS doit être supprimé en fin de job, y compris en cas d'échec (`if: always()`).
- La clé privée de l'updater est le secret le plus sensible du projet : sa fuite permettrait de distribuer une mise à jour malveillante à tous les utilisateurs. Documenter sa conservation et sa procédure de rotation.
- Le build universel macOS et les deux builds séparés (aarch64/x86_64) sont deux stratégies différentes : choisir explicitement et documenter le choix, car il conditionne le nombre d'artefacts et la configuration de l'updater.
