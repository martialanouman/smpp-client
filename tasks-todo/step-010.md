# Jalon 010 — Campagnes : envoi en masse, reprise et rejeu (= M3)

> **Statut :** À faire · **Dépend de :** step-008, step-009 · **Réf. spec :** §10.2–10.7 · **Exigences :** EF-MSG-02, EF-MSG-08, EF-MSG-09, ENF-FIA-01, ENF-FIA-02

## 1. Objectif

Exécuter une campagne d'envoi de masse stable sur plusieurs centaines de milliers de destinataires : modèle à variables, contrôle démarrer/pause/reprise/annulation, back-pressure de bout en bout, reprise après arrêt inopiné sans doublon, et politique de rejeu par code d'erreur. C'est le milestone **M3**.

Le défi n'est pas d'envoyer beaucoup, c'est de rester **borné** : lire les destinataires en flux, ne jamais matérialiser la liste complète en mémoire, et garantir qu'un arrêt brutal au pire moment ne perd ni ne duplique aucun message.

## 2. Périmètre

### Dans le périmètre

- Entité campagne persistée : source de destinataires (liste de contacts, génération, saisie manuelle), modèle de message, configuration d'envoi, planning optionnel (démarrage différé, plage horaire autorisée).
- Modèles à variables `{{prenom}}`, `{{ville}}` résolues par destinataire depuis les attributs de contact ; variable manquante gérée par une politique explicite (valeur par défaut ou rejet de la ligne).
- Cycle de vie `CREATED → VALIDATED → RUNNING → (PAUSED ⇄ RUNNING) → COMPLETED`, plus `CANCELLED` / `FAILED`.
- Contrôles démarrer / pause / reprise / annulation : la pause interrompt l'alimentation de la file sans casser la session ; les messages déjà dans la fenêtre se terminent proprement.
- Lecture des destinataires en **streaming** depuis la base, poussée dans une file **bornée**.
- Reprise : redémarrage depuis les messages `QUEUED`/`SENT` non confirmés, garde-fou d'idempotence par `client_message_id` avant émission.
- Politique de rejeu configurable : nombre maximal de tentatives, délai entre tentatives, filtrage **par code d'erreur** en s'appuyant sur la classification de step-003 (rejouer `ESME_RMSGQFUL`, `ESME_RTHROTTLED`, timeout ; ne pas rejouer `ESME_RINVDSTADR`).
- `submit_multi` pour les destinataires partageant le même contenu (jusqu'à ~254 par PDU), avec **repli automatique** sur des `submit_sm` individuels si le SMSC ne le supporte pas.
- Compteurs de campagne et événement `campaign:progress`.
- Commandes `campaign_create`, `campaign_start`, `campaign_pause`, `campaign_resume`, `campaign_cancel` ; écran Envoi › onglet Campagne.

### Hors périmètre

- La répartition sur plusieurs sessions et les stratégies de routage → **step-011** : ici, une campagne cible une session.
- Les statistiques agrégées et l'export → **step-014**.
- Les plafonds de volume et la confirmation de sécurité → **step-015**.
- L'adaptation dynamique du débit → **step-012**.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-010-01 | Entité campagne + machine d'états | `crates/messaging/src/campaign/mod.rs` |
| L-010-02 | Moteur d'alimentation en streaming avec back-pressure | `crates/messaging/src/campaign/feeder.rs` |
| L-010-03 | Moteur de modèles à variables | `crates/messaging/src/template.rs` |
| L-010-04 | Reprise et garde-fou d'idempotence | `crates/messaging/src/campaign/resume.rs` |
| L-010-05 | Politique de rejeu par code d'erreur | `crates/messaging/src/retry.rs` |
| L-010-06 | Support `submit_multi` + repli | `crates/messaging/src/submit_multi.rs` |
| L-010-07 | Commandes IPC + `campaign:progress` | `src-tauri/src/commands/campaign.rs` |
| L-010-08 | Écran Envoi › Campagne | `ui/src/views/Send/Campaign/` |

## 4. Critères d'acceptation

- [ ] **CA-010-01** — Une campagne de 500 000 destinataires s'exécute jusqu'à `COMPLETED` avec une empreinte mémoire **stable** (pas de croissance proportionnelle au nombre de destinataires).
- [ ] **CA-010-02** — Les compteurs finaux sont exacts : `total = envoyés + échoués + annulés`, contrôlé par test contre le contenu de la base.
- [ ] **CA-010-03** — Pause : l'alimentation s'arrête, les messages déjà dans la fenêtre reçoivent leur réponse, la session reste `BOUND`. Reprise : l'envoi repart exactement où il s'était arrêté, **aucun doublon**.
- [ ] **CA-010-04** — Arrêt brutal du processus au milieu d'une campagne (`kill -9`) puis redémarrage : la campagne reprend, et le nombre total de messages émis reste égal au nombre de destinataires — vérifié en comparant les `client_message_id` distincts.
- [ ] **CA-010-05** — Un message déjà `ACCEPTED` n'est jamais réémis à la reprise (vérification d'état avant émission).
- [ ] **CA-010-06** — Les variables de modèle sont résolues par destinataire ; une variable manquante suit la politique configurée, sans jamais émettre un texte contenant `{{…}}` non résolu.
- [ ] **CA-010-07** — La politique de rejeu respecte la classification : un `ESME_RINVDSTADR` n'est jamais rejoué ; un `ESME_RTHROTTLED` est rejoué après délai, dans la limite du nombre de tentatives.
- [ ] **CA-010-08** — `submit_multi` est utilisé quand il est activé et supporté ; un SMSC le refusant déclenche le repli automatique sur `submit_sm` **sans perdre de destinataire**.
- [ ] **CA-010-09** — L'annulation arrête l'émission en moins d'une seconde et marque proprement les messages non émis ; aucun message n'est laissé dans un état indéterminé.
- [ ] **CA-010-10** — Le planning différé démarre la campagne à l'heure prévue et respecte la plage horaire autorisée (test avec horloge injectée).
- [ ] **CA-010-11** — `campaign:progress` est throttlé ; l'UI affiche la progression et le débit sans dégradation pendant une campagne à débit maximal.
- [ ] **CA-010-12** — **Recette M3 :** campagne massive stable de bout en bout, avec pause/reprise et redémarrage à froid en cours de route.

## 5. Tests attendus

- **Intégration :** campagne complète contre serveur factice — nominal, avec pause/reprise, avec annulation, avec redémarrage du processus, avec SMSC lent (back-pressure), avec taux d'erreur injecté.
- **Unitaires :** moteur de modèles (variables présentes, manquantes, imbriquées, littéral `{{` échappé), machine d'états de campagne (toutes les transitions valides et le rejet des invalides), politique de rejeu par code.
- **Propriété :** pour toute séquence d'événements (pause, reprise, erreur, timeout, redémarrage), l'invariant « chaque destinataire reçoit au plus un message émis » tient.
- **Volumétrie :** campagne de 500 000 destinataires, mémoire et durée mesurées.

## 6. Notes d'implémentation

- **L'invariant central est « au plus une émission par destinataire ».** Il repose sur la vérification d'état avant émission et sur l'unicité du `client_message_id`. Toute optimisation ultérieure du chemin d'émission doit le préserver — l'écrire noir sur blanc dans le code, avec le test de propriété correspondant.
- La back-pressure doit être **de bout en bout** : si la file d'émission est pleine, le lecteur de destinataires doit se bloquer sur l'envoi, pas accumuler dans un tampon intermédiaire. Un seul tampon non borné annule tout le dispositif.
- « Reprise sans doublon » et « aucune perte » sont en tension : un message `SENT` sans réponse au moment du crash a pu être reçu ou non par le SMSC. Décider explicitement de la politique (rejouer au risque d'un doublon, ou marquer incertain) et la documenter — c'est un arbitrage produit, pas un détail technique.
- Le planning horaire doit gérer le passage de minuit et les fuseaux : utiliser une horloge injectée et tester les bornes.

## 7. Dette connue à la fin de la sous-PR D (jalon 010)

Consignée ici plutôt que dans une description de PR, parce que c'est ici qu'on la
relira.

- **CA-010-12 (recette M3) n'est pas vérifié.** Il n'existe aucun
  `src-tauri/tests/`, et aucune campagne n'a jamais envoyé un message à travers
  l'IPC. Chaque pièce est couverte séparément — le runner contre un SMSC factice,
  la source de destinataires et le cycle de vie contre une vraie base, la
  validation des commandes — mais pas l'assemblage, ni le redémarrage à froid au
  milieu d'une campagne massive.
- **CA-010-11 n'est vérifié que comme mécanisme.** Le débit d'émission de
  `campaign:progress` est prouvé à 4 Hz quel que soit le débit de la campagne, et
  c'est testé ; personne n'a lancé une campagne à débit maximal avec l'écran
  ouvert pour mesurer une dégradation.
- **`campaigns.delivered_count` n'est alimenté par rien.** La colonne existe, le
  DTO la porte, et l'écran ne l'affiche **pas** — un zéro permanent à côté de
  cinq chiffres exacts se lit comme « le SMSC a tout accepté et rien n'est
  arrivé ». Elle revient avec les statistiques du jalon 014.
- **`campaigns.send_config` n'a ni version ni migration.** `#[serde(default)]`
  sur le conteneur fait qu'un champ ajouté plus tard ne casse pas la relecture
  d'une ligne existante ; un champ dont le *sens* change demanderait un numéro de
  version, que ce document n'a pas.
- **Le groupage `submit_multi` n'est pas branché au runner** (arbitrage produit :
  il le sera, mais seulement lorsque `registered_delivery = 0`).
- **L'arrêt de l'application ne joint pas les campagnes**, il signale leur
  annulation et rend la main : un message déjà en route vers `submit` peut encore
  partir pendant que les sessions se délient. C'est la reprise qui couvre le
  résidu.
