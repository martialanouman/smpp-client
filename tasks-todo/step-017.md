# Jalon 017 — Simulateur SMSC intégré et bancs de charge (= M7, optionnel)

> **Statut :** À faire (optionnel) · **Dépend de :** step-016 · **Réf. spec :** §19.2–19.4, §4.1, §22 (M7) · **Réf. guide :** §13

## 1. Objectif

Disposer d'un banc de test complet : un simulateur SMSC embarqué capable d'injecter des fautes réalistes, et des scénarios de charge automatisés validant les objectifs de performance de la spec §4.1 avec détection de régression en CI.

Jusqu'ici, chaque jalon a utilisé des serveurs factices ponctuels. Ce jalon les consolide en un outil unique et réutilisable, et transforme les objectifs de performance — jusque-là vérifiés au cas par cas — en garde-fous permanents.

## 2. Périmètre

### Dans le périmètre

- **Simulateur SMSC** embarqué (serveur `rusmpps` ou équivalent) : bind TX/RX/TRX, `submit_sm` avec latence configurable, émission de DLR différés, `enquire_link`, `unbind`, support v3.4 et v5.0 avec `congestion_state` pilotable.
- **Injection de fautes** : coupures TCP à un instant donné, PDU malformés, réponses hors séquence, timeouts, `generic_nack`, `ESME_RTHROTTLED` sur seuil de débit, DLR en double, DLR pour identifiant inconnu, refus de `submit_multi`.
- Consolidation des serveurs factices des jalons précédents sur ce simulateur.
- **Scénarios de charge** automatisés : ≥ 1 000 TPS sur une session, débit agrégé multi-sessions, campagne d'un million de destinataires, stabilité mémoire sur longue durée, convergence de l'AIMD.
- Micro-benchmarks `criterion` sur les chemins chauds (encodage, segmentation, enfilement, sérialisation).
- **Seuils de non-régression en CI** : performance et couverture du cœur ne doivent pas régresser.
- Tests E2E frontend (`tauri-driver`/Playwright) sur les parcours critiques.
- Mesure de couverture (`cargo llvm-cov`) avec seuil bloquant sur le cœur protocolaire.

### Hors périmètre

- Un SMSC de production ou un rôle serveur exposé aux utilisateurs : le simulateur est un **outil de test**, jamais un livrable applicatif (spec §1.4).
- Les tests de charge contre un SMSC réel ou un environnement d'un fournisseur.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-017-01 | Simulateur SMSC configurable | `crates/smsc-sim/` (crate de développement, non publiée) |
| L-017-02 | Bibliothèque d'injection de fautes | `crates/smsc-sim/src/faults.rs` |
| L-017-03 | Scénarios de charge automatisés | `tests/load/` |
| L-017-04 | Micro-benchmarks des chemins chauds | `benches/` |
| L-017-05 | Job CI de non-régression performance et couverture | `.github/workflows/ci.yml` |
| L-017-06 | Tests E2E des parcours critiques | `ui/e2e/` |
| L-017-07 | Runbook de diagnostic d'une session | `docs/runbooks/diagnostic-session.md` |

## 4. Critères d'acceptation

- [ ] **CA-017-01** — Le simulateur supporte les trois types de bind, v3.4 et v5.0, et pilote `congestion_state` à la demande.
- [ ] **CA-017-02** — Chaque type de faute listé est injectable de façon **déterministe** (à un numéro de séquence ou à un instant donné), et reproductible.
- [ ] **CA-017-03** — Les serveurs factices des jalons précédents sont remplacés par ce simulateur ; l'ensemble des tests d'intégration reste vert après migration.
- [ ] **CA-017-04** — Scénario ≥ **1 000 TPS** sur une session soutenu 5 minutes, sans perte, sans dépassement de fenêtre, sans dérive mémoire (ENF-PERF-01).
- [ ] **CA-017-05** — Campagne d'un million de destinataires menée à terme, mémoire stable, compteurs exacts.
- [ ] **CA-017-06** — Scénario de robustesse : coupures répétées pendant une campagne — **aucune perte, aucun doublon** à l'arrivée (comparaison des `client_message_id` émis et reçus côté simulateur).
- [ ] **CA-017-07** — Scénario AIMD : sous congestion variable, le débit converge et n'oscille pas de façon permanente (critère quantitatif : amplitude d'oscillation décroissante sur 10 minutes).
- [ ] **CA-017-08** — Les micro-benchmarks `criterion` ont une ligne de base commitée ; une régression supérieure au seuil défini fait échouer la CI.
- [ ] **CA-017-09** — La couverture du cœur protocolaire est ≥ 80 % et ne régresse pas (seuil bloquant en CI, ENF-MNT-02).
- [ ] **CA-017-10** — Les tests E2E couvrent les parcours critiques : créer une session et binder, envoyer un message simple, importer des contacts, lancer une campagne, exporter les journaux.
- [ ] **CA-017-11** — Les tests de charge sont exécutables en local par une commande unique (`just load-test`) et documentés.
- [ ] **CA-017-12** — **Recette M7 :** banc de test complet démontré, avec un rapport de résultats archivé.

## 5. Tests attendus

Ce jalon **est** de la mise en test ; les critères d'acceptation ci-dessus constituent ses tests. À vérifier en complément :

- Le simulateur lui-même est testé (ses réponses sont conformes au protocole), sinon un bug du simulateur se lit comme un bug du client.
- Les scénarios de charge sont stables sur exécutions répétées (pas de faux positifs qui feraient désactiver le job en CI).
- Les tests E2E ne sont pas *flaky* : 10 exécutions consécutives vertes avant d'être rendus bloquants.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /rusmpp/rusmpp "rusmpps server implementation to build an SMSC simulator for testing"
  npx ctx7@latest library "criterion" "benchmark baseline comparison and regression detection in CI"
  ```
- **Les tests de charge en CI sont fragiles** : les runners partagés ont des performances variables, ce qui produit des échecs aléatoires et mène à désactiver le job — perdant tout le bénéfice. Décider explicitement : soit un job séparé non bloquant avec suivi de tendance, soit des seuils larges sur machine dédiée. Ne pas mettre un seuil serré sur un runner partagé.
- Le simulateur appartient au périmètre de test, pas au produit : il ne doit **jamais** être empaqueté dans les artefacts de release. Le vérifier explicitement (crate de développement, exclue du binaire final).
- L'injection de fautes déterministe (par numéro de séquence) vaut mieux que probabiliste : un test qui échoue une fois sur cent est un test qu'on finit par ignorer.
- Ce jalon est marqué optionnel dans la spec, mais c'est lui qui rend démontrables les objectifs ENF-PERF et ENF-FIA. Le repousser revient à laisser ces exigences vérifiées par échantillonnage manuel.
