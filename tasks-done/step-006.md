# Jalon 006 — Envoi simple de bout en bout (= M1)

> **Statut :** Terminé (2026-07-26) — 11 critères sur 12 ; CA-006-12 est une recette opérateur · **Dépend de :** step-004, step-005 · **Réf. spec :** §5.4, §10.1, §7.3 · **Exigences :** EF-MSG-01, EF-MSG-05, EF-MSG-06, EF-MSG-07 (partiel)

## 1. Objectif

Envoyer un SMS depuis l'interface et voir sa réponse : c'est le premier jalon qui produit de la valeur observable pour l'utilisateur, et il correspond au milestone **M1** de la spec §22.

Il assemble les briques précédentes selon le flux nominal de la spec §5.4 : validation → encodage → segmentation → **persistance avant émission** → mise en file → émission → corrélation → mise à jour d'état. L'ordre importe : persister avant d'émettre est la garantie « aucune perte de message » (ENF-FIA-01), et l'inverser rendrait la reprise du step-010 impossible.

## 2. Périmètre

### Dans le périmètre

- Crate `messaging` : orchestrateur d'envoi unitaire, port `MessageRepository`, cycle d'état `QUEUED → SENT → ACCEPTED | FAILED`.
- Validation et normalisation E.164 du destinataire ; validation de l'adresse source (numérique ou alphanumérique ≤ 11 caractères).
- Construction complète du `submit_sm` avec **tous** les champs de la spec §7.3 exposés, valeurs par défaut sûres (spec §23.3) : `registered_delivery = 1`, TON/NPI destination International/E.164.
- TLV personnalisés (tag/valeur) au niveau du message.
- Corrélation du `submit_sm_resp` : récupération du `smsc_message_id`, mise à jour d'état, ou `FAILED` avec `command_status` et libellé clair.
- Traitement multi-segments : N PDU pour un message logique, état agrégé cohérent.
- Commande IPC `message_send` ; écran Envoi › onglet Simple : sélecteurs documentés TON/NPI/DCS, éditeur de message avec compteur de caractères/segments et encodage détecté en direct (API de step-004), éditeur de TLV, affichage du résultat en direct.

### Hors périmètre

- Le fenêtrage et la régulation de débit → **step-007** (l'émission reste séquentielle et non régulée ici).
- Les DLR → **step-008** : ce jalon s'arrête à `ACCEPTED`.
- L'envoi en masse, les modèles à variables, le rejeu → **step-010**.
- Le choix automatique de session → **step-011** : la session est choisie explicitement par l'utilisateur.
- `submit_multi` → **step-010**.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-006-01 | Orchestrateur d'envoi unitaire | `crates/messaging/src/sender.rs` |
| L-006-02 | Modèle de message et machine d'états | `crates/messaging/src/message.rs` |
| L-006-03 | Validation/normalisation des adresses | `crates/messaging/src/addressing.rs` |
| L-006-04 | Construction du `submit_sm` (tous champs + TLV) | `crates/messaging/src/submit.rs` |
| L-006-05 | Commande IPC `message_send` + événement `message:update` | `src-tauri/src/commands/message.rs` |
| L-006-06 | Écran Envoi › Simple | `ui/src/views/Send/Simple/` |
| L-006-07 | Composants transverses : compteur, sélecteurs TON/NPI/DCS, éditeur de TLV | `ui/src/components/` |

## 4. Critères d'acceptation

- [x] **CA-006-01** — Depuis l'UI, un message court part vers le SMSC de test et son `smsc_message_id` s'affiche ; l'état passe `QUEUED → SENT → ACCEPTED`.
- [x] **CA-006-02** — Le message est présent en base à l'état `QUEUED` **avant** que le `submit_sm` ne soit écrit sur le socket (test d'ordonnancement avec repository instrumenté).
- [x] **CA-006-03** — Un arrêt brutal simulé entre la persistance et l'émission laisse le message en `QUEUED` — récupérable, jamais perdu ni dupliqué.
- [x] **CA-006-04** — Un message de 400 caractères GSM produit 3 segments, 3 `submit_sm`, et un état de message logique cohérent (agrégation des réponses des segments).
- [x] **CA-006-05** — Un `submit_sm_resp` en erreur passe l'état à `FAILED` en conservant le `command_status` brut **et** son libellé en clair, affiché tel quel dans l'UI (ENF-UTI-02).
- [x] **CA-006-06** — Tous les champs de la spec §7.3 sont réglables depuis l'UI et effectivement transmis dans le PDU (test comparant le PDU émis aux valeurs saisies).
- [x] **CA-006-07** — Un destinataire invalide est rejeté **avant** toute persistance et toute émission, avec un `ErrorDto` explicite.
- [x] **CA-006-08** — Un TLV personnalisé saisi dans l'UI apparaît dans le PDU émis avec le bon tag et la bonne longueur.
- [x] **CA-006-09** — Le compteur de l'éditeur affiche en direct l'encodage détecté, les caractères utilisés/restants et le nombre de segments, et coïncide avec la segmentation réelle.
- [x] **CA-006-10** — La latence d'enfilement (appel de commande → message en file) reste sous 1 ms au p99 sur 10 000 envois (mesure `criterion`, ENF-PERF-02).
- [x] **CA-006-11** — Aucune logique métier dans `src-tauri` : la commande `message_send` valide, appelle le service, sérialise — revue explicite du fichier.
- [ ] **CA-006-12** — **Recette M1 :** un opérateur qui n'a jamais vu l'application envoie un SMS et lit sa réponse en moins de 5 minutes (ENF-UTI-01).

## 5. Tests attendus

- **Intégration :** envoi bout en bout contre le serveur SMPP factice de step-005 — succès, erreur `command_status`, timeout de réponse, message multi-segments.
- **Unitaires :** normalisation E.164 (numéros nationaux, internationaux, avec espaces/tirets, alphanumériques), construction du PDU champ par champ, agrégation d'état multi-segments (tous segments OK, un segment en échec, timeout partiel).
- **Frontend :** compteur de caractères, sélecteurs, validation de formulaire, affichage de l'erreur SMPP.
- **Performance :** micro-benchmark de la latence d'enfilement.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /rusmpp/rusmpp "SubmitSm builder fields service_type registered_delivery validity_period and TLV"
  npx ctx7@latest library "phonenumber" "parse and format a number to E164 in Rust"
  ```
- **Décider explicitement de la sémantique d'un message multi-segments en échec partiel** (2 segments acceptés, 1 en échec) : état global `FAILED` ? `PARTIAL` ? Ce cas se produit en production et une décision implicite produira des statistiques fausses en step-014. Le documenter dans le code et dans le CHANGELOG.
- Le chemin d'émission est un **chemin chaud** : éviter les `clone()` réflexes sur le contenu du message et réutiliser les buffers (guide §18).
- Le port `MessageRepository` est défini côté `messaging` et implémenté côté `persistence` (inversion de dépendance, guide §8.1) — permet les doubles de test sans base.
- L'adresse source alphanumérique n'est pas acceptée par tous les SMSC et impose `source_addr_ton = 5` : le signaler dans l'UI plutôt que de laisser l'utilisateur découvrir un rejet.
