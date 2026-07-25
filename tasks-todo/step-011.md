# Jalon 011 — Sessions multiples, multi-bind et routage

> **Statut :** À faire · **Dépend de :** step-010 · **Réf. spec :** §8.1, §8.3–8.6 · **Exigences :** EF-CNX-03, EF-CNX-08

## 1. Objectif

Gérer N sessions SMPP simultanées vers des SMSC identiques ou distincts, chacune isolée avec ses propres files, fenêtre, limiteur et métriques ; ouvrir plusieurs connexions TCP par session logique pour agréger le débit ; et router les envois d'une campagne selon une stratégie configurable.

L'isolation est le point critique : une session lente, saturée ou en erreur ne doit dégrader ni le débit ni la stabilité des autres. C'est ce que garantit l'absence de ressource mutable partagée entre sessions.

## 2. Périmètre

### Dans le périmètre

- `SessionManager` : registre `HashMap<SessionId, SessionHandle>`, démarrage/arrêt à la demande, supervision (redémarrage d'une session tombée, escalade des erreurs fatales sans boucle inutile), agrégation des métriques, événement `sessions:state`.
- Isolation stricte : files, compteurs de fenêtre, limiteur et connexion propres à chaque session ; communication uniquement par messages.
- **Multi-bind** : `bind_count` connexions TCP par session logique, répartition des `submit_sm` en round-robin pondéré par la fenêtre disponible, agrégation de la fenêtre et du débit au niveau logique.
- Stratégies de routage d'une campagne (spec §8.6) : manuelle, round-robin, moins chargée, par pays/préfixe E.164 (table de correspondance), basculement (failover) vers une session de secours.
- UI : bandeau multi-sessions, sélection de la stratégie de routage dans la configuration de campagne, métriques par session et agrégées.

### Hors périmètre

- L'adaptation dynamique du débit par session → **step-012**.
- TLS et le stockage chiffré des identifiants par profil → **step-015**.
- Les tests de charge multi-sessions → **step-017**.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-011-01 | `SessionManager` et supervision | `crates/smpp-session/src/manager.rs` |
| L-011-02 | Multi-bind et répartition intra-session | `crates/smpp-session/src/multibind.rs` |
| L-011-03 | Stratégies de routage | `crates/messaging/src/routing.rs` |
| L-011-04 | Table de correspondance préfixe → session | `crates/messaging/src/routing/prefix.rs` |
| L-011-05 | Agrégation des métriques multi-sessions | `crates/smpp-session/src/metrics.rs` |
| L-011-06 | Bandeau de sessions et configuration du routage | `ui/src/views/Sessions/`, `ui/src/views/Send/Campaign/` |

## 4. Critères d'acceptation

- [ ] **CA-011-01** — Cinq sessions vers des SMSC distincts sont `BOUND` simultanément, chacune avec son débit et ses métriques propres.
- [ ] **CA-011-02** — **Isolation :** une session dont le SMSC ne répond plus n'affecte ni le débit, ni la latence, ni l'état des quatre autres (mesure comparative avant/pendant l'incident).
- [ ] **CA-011-03** — Une session qui tombe est automatiquement redémarrée par le superviseur ; une session rejetée pour identifiants invalides ne l'est **pas** et remonte une alerte.
- [ ] **CA-011-04** — Avec `bind_count = 4`, quatre connexions TCP sont ouvertes vers le même SMSC et le débit agrégé est significativement supérieur à celui d'un lien unique dans les mêmes conditions de latence.
- [ ] **CA-011-05** — La répartition intra-session pondérée par la fenêtre disponible n'envoie pas vers un lien saturé tant qu'un lien libre existe.
- [ ] **CA-011-06** — Round-robin : sur 10 000 messages et 3 sessions, la répartition est équilibrée à ±2 %.
- [ ] **CA-011-07** — Moins chargée : la session ayant la plus grande fenêtre disponible est choisie, vérifié en injectant des latences asymétriques.
- [ ] **CA-011-08** — Routage par préfixe : un numéro `+225…` part sur la session configurée pour `+225` ; un préfixe non couvert suit la règle par défaut explicite (session de repli ou rejet, jamais un choix arbitraire silencieux).
- [ ] **CA-011-09** — Failover : la chute de la session primaire bascule le trafic sur la session de secours **sans perte de message** ; le retour à la normale est également testé.
- [ ] **CA-011-10** — Aucun état mutable partagé entre sessions : revue de code explicite ; toute structure partagée est immuable ou justifiée ligne à ligne.
- [ ] **CA-011-11** — `sessions:state` reste throttlé avec 10 sessions actives ; l'UI ne se dégrade pas.
- [ ] **CA-011-12** — L'arrêt de l'application ferme proprement **toutes** les sessions (unbind sur chacune) dans un délai borné.

## 5. Tests attendus

- **Intégration :** N serveurs factices avec comportements différenciés (rapide, lent, en panne, throttlant) ; scénarios de chute et de reprise ; failover et retour.
- **Unitaires :** chaque stratégie de routage (répartition, sélection, table de préfixes avec correspondances les plus longues, cas non couvert) ; répartition multi-bind pondérée.
- **Propriété :** quelle que soit la séquence d'états de sessions, aucun message n'est routé vers une session non `BOUND`, et aucun n'est perdu lors d'un basculement.
- **Non-régression :** tout déséquilibre de répartition observé devient un test.

## 6. Notes d'implémentation

- Le routage par préfixe doit résoudre par **correspondance la plus longue** (`+2250` avant `+225`), sinon les règles fines sont masquées par les générales.
- Le multi-bind partage un `system_id` : certains SMSC limitent le nombre de binds simultanés par compte et rejettent au-delà. Traiter ce rejet comme une erreur de configuration explicite, pas comme une panne à reconnecter en boucle.
- Le failover doit distinguer « session indisponible » de « message rejeté par le SMSC » : rebasculer un message rejeté pour adresse invalide vers une autre session ne fera que le faire rejeter à nouveau.
- L'agrégation des métriques multi-sessions ne doit pas introduire de verrou global sur le chemin chaud : privilégier des compteurs par session collectés périodiquement.
