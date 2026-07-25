# Jalon 007 — Fenêtrage, contrôle de débit et métriques temps réel

> **Statut :** À faire · **Dépend de :** step-006 · **Réf. spec :** §9.1–9.3, §9.5, §9.6, §18.1 · **Exigences :** EF-DBT-01, EF-DBT-02, EF-DBT-04, ENF-PERF-01

## 1. Objectif

Garantir que l'émission ne dépasse jamais le débit configuré (`throughput_tps`) ni la fenêtre configurée (`window_size`), tout en exposant en temps réel le débit instantané, l'occupation de la fenêtre et le RTT des réponses.

Le débit réel résulte de **deux** contraintes indépendantes appliquées conjointement — jetons disponibles et slots de fenêtre libres. Les confondre, ou n'en implémenter qu'une, produit soit un débit incontrôlé qui fait bannir l'ESME par le SMSC, soit un débit effondré par la latence.

## 2. Périmètre

### Dans le périmètre

- Crate `rate-control` : limiteur de débit à token bucket (`governor`, GCRA), configurable par session, `0 = illimité`.
- Contrôle de fenêtre : au plus `window_size` PDU en attente de réponse, acquisition/libération par sémaphore, libération garantie sur réponse **et** sur timeout.
- Writer régulé conforme au pseudocode spec §9.3 : `recv → token → slot de fenêtre → séquence → oneshot → écriture → persistance SENT`.
- Back-pressure : la file d'émission est bornée ; quand le SMSC ralentit, la production ralentit d'autant — aucune croissance mémoire.
- Métriques par session : TPS instantané (moyennes glissantes 1 s et 10 s), TPS moyen, pic, occupation de fenêtre, RTT moyen des réponses, nombre de reconnexions, uptime, compteurs par état.
- Agrégation et **throttling** de l'événement `metrics:tick` côté backend (1–4 Hz), jamais un événement par message.
- UI : jauges de débit et de fenêtre, courbes temps réel sur l'écran Sessions et le Tableau de bord.
- Paramètres exposés : `throughput_tps`, `window_size`, `response_timeout_s`, bornes `min_tps`/`max_tps`.

### Hors périmètre

- L'adaptation dynamique au `congestion_state` et à `ESME_RTHROTTLED` (AIMD) → **step-012** : ce jalon pose les points d'accroche (facteur adaptatif réglable, coefficients exposés) mais applique un facteur constant de 1,0.
- La répartition du débit entre plusieurs sessions → **step-011**.
- Les tests de charge soutenue et les seuils de non-régression → **step-017**.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-007-01 | Limiteur de débit configurable | `crates/rate-control/src/limiter.rs` |
| L-007-02 | Contrôle de fenêtre (acquisition/libération sûres) | `crates/rate-control/src/window.rs` |
| L-007-03 | Writer régulé | `crates/smpp-session/src/actors/writer.rs` |
| L-007-04 | Collecteur de métriques par session | `crates/smpp-session/src/metrics.rs` |
| L-007-05 | Agrégation et throttling de `metrics:tick` | `src-tauri/src/events.rs` |
| L-007-06 | Jauges et courbes temps réel | `ui/src/views/Sessions/`, `ui/src/views/Dashboard/` |

## 4. Critères d'acceptation

- [ ] **CA-007-01** — Avec `throughput_tps = 100`, l'émission de 1 000 messages prend 10 s ±5 % et **aucune** seconde glissante ne dépasse 100 envois (mesure sur le serveur factice, horloge virtuelle).
- [ ] **CA-007-02** — Avec `window_size = 10` et un serveur qui ne répond pas, exactement 10 PDU sont émis puis le writer se met en pause ; aucune émission supplémentaire.
- [ ] **CA-007-03** — Un slot de fenêtre est libéré aussi bien par une réponse que par un timeout : après `response_timeout`, l'émission reprend (test avec serveur silencieux).
- [ ] **CA-007-04** — `throughput_tps = 0` signifie illimité et n'introduit aucun délai artificiel.
- [ ] **CA-007-05** — Débit soutenu ≥ **1 000 TPS** sur une session contre le serveur factice, sans dépassement de fenêtre ni perte (ENF-PERF-01).
- [ ] **CA-007-06** — Back-pressure vérifiée : alimenter la file plus vite que le SMSC ne consomme pendant 5 minutes laisse la mémoire du processus stable (variation < 10 % après stabilisation).
- [ ] **CA-007-07** — `metrics:tick` est émis au plus à 4 Hz quelle que soit la charge : test comptant les événements pendant un envoi à 1 000 TPS.
- [ ] **CA-007-08** — Les métriques affichées correspondent à la réalité : TPS mesuré ≈ TPS réel ±5 %, occupation de fenêtre exacte, RTT cohérent avec la latence injectée dans le serveur factice.
- [ ] **CA-007-09** — L'UI reste réactive (< 16 ms/frame) pendant un envoi à débit maximal (ENF-PERF-03, mesuré via les outils de performance du navigateur).
- [ ] **CA-007-10** — Aucune fuite de slot de fenêtre : après 100 000 messages mêlant succès, erreurs et timeouts, le compteur de fenêtre revient exactement à zéro.

## 5. Tests attendus

- **Unitaires :** limiteur (respect du TPS, comportement en burst initial, TPS nul), fenêtre (acquisition bloquante, libération sur les trois chemins : réponse, erreur, timeout ; absence de double libération).
- **Intégration :** writer régulé contre serveur factice à latence configurable ; scénario de saturation ; scénario de reprise après pause.
- **Propriété :** sur des séquences aléatoires d'acquisitions/libérations, le compteur de fenêtre ne devient jamais négatif et ne dépasse jamais `window_size`.
- **Performance :** `criterion` sur le chemin d'enfilement ; run de charge 1 000 TPS.
- **Déterminisme :** temps virtuel Tokio, pas de `sleep` réel.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest library "governor" "rate limiter GCRA quota per second async until_ready in Rust"
  ```
- **Le double comptage de la fenêtre est le bug classique.** Un message multi-segments consomme N slots, pas 1. Décider et documenter : la fenêtre compte des **PDU**, pas des messages logiques.
- La libération du slot doit être garantie même en cas de panique ou d'annulation de la tâche de traitement : préférer un garde RAII (`Drop`) à une libération manuelle dispersée dans les branches.
- Le burst initial du token bucket : `governor` autorise par défaut une rafale égale au quota. Décider si c'est acceptable (le SMSC peut throttler dès la première seconde) et configurer explicitement.
- Les moyennes glissantes doivent être calculées côté backend sur une fenêtre temporelle, pas côté UI à partir d'événements — sinon la précision dépend du throttling d'affichage.
