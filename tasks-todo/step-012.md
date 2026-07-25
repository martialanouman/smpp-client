# Jalon 012 — SMPP v5.0 complet et adaptation dynamique du débit (= M4)

> **Statut :** À faire · **Dépend de :** step-011 · **Réf. spec :** §7.7, §9.4, §23.4 · **Exigences :** EF-CNX-04, EF-DBT-03, EF-MSG-06

## 1. Objectif

Couvrir intégralement SMPP v5.0 en complément de v3.4 : diffusion cellulaire, TLV additionnels, et surtout **adaptation dynamique du débit** pilotée par le `congestion_state` et par les erreurs de throttling. C'est le milestone **M4**.

L'adaptation dynamique est ce qui distingue un client de test d'un client de production : un ESME qui ignore les signaux de congestion se fait throttler, puis déconnecter. Le mécanisme AIMD fonctionne aussi bien en v3.4 (via `ESME_RTHROTTLED`/`ESME_RMSGQFUL`) qu'en v5.0 (via `congestion_state`).

## 2. Périmètre

### Dans le périmètre

- Sélection de l'`interface_version` par session (0x34 / 0x50) annoncée au bind, et **masquage dans l'UI** des paramètres non pertinents pour la version choisie (matrice spec §23.4).
- Lecture du TLV `congestion_state` (0x0428, 0–100) dans les PDU de réponse.
- **AIMD** : maintien de la zone cible 80–90 ; `congestion_state > 90` → réduction multiplicative (×0,8) ; `< 70` durablement → remontée additive (+0,05 par intervalle stable, plafonnée à 1,0) vers la cible utilisateur. `débit_effectif = clamp(cible × facteur, min_tps, cible)`.
- Réaction immédiate à `ESME_RTHROTTLED` et `ESME_RMSGQFUL` : réduction multiplicative + back-off, puis remontée additive — actif quelle que soit la version.
- Paramètres exposés : activation de l'adaptation, bornes `min_tps`/`max_tps`, coefficients AIMD.
- PDU de diffusion v5.0 : `broadcast_sm`, `query_broadcast_sm`, `cancel_broadcast_sm` et leurs TLV (`broadcast_area_identifier`, `broadcast_content_type`, `broadcast_rep_num`, `broadcast_frequency_interval`, `broadcast_service_group`).
- TLV v5.0 : portabilité (`dest_addr_np_country`, `dest_addr_np_information`, `dest_addr_np_resolution`), identification de nœud/réseau (`source_network_id`, `source_node_id`, `dest_network_id`, `dest_node_id`), `ussd_service_op`, `billing_identification`, `network_error_code` enrichi, `registered_delivery` étendu (« livraison réussie uniquement »).
- Opérations complémentaires exploitées : `query_sm`, `cancel_sm`, `replace_sm`, `data_sm`, traitement d'`outbind` et d'`alert_notification`.
- Affichage du `congestion_state` courant et du facteur adaptatif dans les métriques.

### Hors périmètre

- Les tests de charge validant l'AIMD sur longue durée → **step-017**.
- L'export des métriques de congestion → **step-014**.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-012-01 | Contrôleur AIMD | `crates/rate-control/src/adaptive.rs` |
| L-012-02 | Lecture et propagation du `congestion_state` | `crates/smpp-session/src/congestion.rs` |
| L-012-03 | PDU et TLV de diffusion v5.0 | `crates/smpp-core/src/pdus/broadcast.rs` |
| L-012-04 | TLV v5.0 additionnels | `crates/smpp-core/src/tlv/v5.rs` |
| L-012-05 | Opérations `query_sm` / `cancel_sm` / `replace_sm` / `data_sm` | `crates/messaging/src/operations.rs` |
| L-012-06 | Adaptation de l'UI à la version de session | `ui/src/views/Sessions/`, `ui/src/views/Send/` |
| L-012-07 | Métriques de congestion | `ui/src/views/Dashboard/` |

## 4. Critères d'acceptation

- [ ] **CA-012-01** — Une session v3.4 et une session v5.0 fonctionnent simultanément, chacune annonçant sa version au bind et n'exposant que ses fonctions valides (matrice §23.4).
- [ ] **CA-012-02** — Un `congestion_state` de 95 injecté par le serveur factice réduit le débit effectif d'environ 20 % en moins de deux intervalles d'ajustement.
- [ ] **CA-012-03** — Un `congestion_state` durablement inférieur à 70 fait remonter le débit **progressivement** (additivement) jusqu'à la cible utilisateur, sans jamais la dépasser.
- [ ] **CA-012-04** — Le débit effectif reste borné par `min_tps` et par la cible utilisateur en toutes circonstances : test de propriété sur des séquences aléatoires de signaux de congestion.
- [ ] **CA-012-05** — Un `ESME_RTHROTTLED` déclenche une réduction immédiate et un back-off, y compris sur une session **v3.4** (sans `congestion_state`).
- [ ] **CA-012-06** — Le système converge : après une rafale de throttling suivie d'une période saine, le débit revient à la cible et **ne s'installe pas** dans une oscillation permanente (test sur 10 minutes de temps virtuel).
- [ ] **CA-012-07** — L'adaptation désactivée laisse le débit strictement égal à la cible.
- [ ] **CA-012-08** — `broadcast_sm`, `query_broadcast_sm` et `cancel_broadcast_sm` sont encodés/décodés correctement avec leurs TLV, et refusés proprement sur une session v3.4 avec un message explicite.
- [ ] **CA-012-09** — Tous les TLV v5.0 listés sont encodables, décodables et réglables depuis l'UI quand la session est en v5.0.
- [ ] **CA-012-10** — `query_sm`, `cancel_sm` et `replace_sm` fonctionnent contre le serveur factice et mettent à jour l'état du message concerné.
- [ ] **CA-012-11** — Un `outbind` ou une `alert_notification` reçus sont traités sans faire tomber la session.
- [ ] **CA-012-12** — **Recette M4 :** couverture v3.4 + v5.0 démontrée par la matrice §23.4, chaque ligne étant rattachée à un test.
- [ ] **CA-012-13** — Deux approbations requises (cœur protocolaire).

## 5. Tests attendus

- **Unitaires :** contrôleur AIMD (réduction, remontée, bornage, hystérésis), encodage/décodage de chaque PDU et TLV v5.0, matrice de compatibilité version ⇄ fonction.
- **Propriété :** le facteur adaptatif reste dans `[min_tps/cible, 1.0]` pour toute séquence de signaux ; le débit ne dépasse jamais la cible utilisateur.
- **Intégration :** serveur factice pilotant `congestion_state` et injectant `ESME_RTHROTTLED` ; scénario de convergence longue durée en temps virtuel.
- **Non-régression :** les tests v3.4 des jalons précédents doivent tous rester verts (aucune régression introduite par le support v5.0).

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /rusmpp/rusmpp "SMPP v5 broadcast_sm TLV congestion_state and interface_version"
  ```
- **L'oscillation est le risque principal de l'AIMD.** Une réduction trop agressive combinée à une remontée trop rapide produit un débit en dents de scie qui dégrade le rendement réel. Prévoir une hystérésis (intervalle minimal entre deux ajustements) et le vérifier par le test de convergence — pas seulement par les tests unitaires de chaque branche.
- Le `congestion_state` est renvoyé par le SMSC **dans les réponses** : sa lecture appartient au reader, sa consommation au limiteur. Ne pas coupler directement les deux ; passer par un canal, cohérent avec le modèle d'acteurs.
- La matrice §23.4 est le contrat de ce jalon : la matérialiser sous forme de test paramétré rend la couverture démontrable plutôt que déclarative.
- Certains SMSC annoncent v5.0 au bind mais se comportent en v3.4 : prévoir un mode de repli explicite et journalisé plutôt qu'un échec obscur.
