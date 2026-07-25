# Jalon 008 — Accusés de livraison, journal métier et vue temps réel (= M2)

> **Statut :** À faire · **Dépend de :** step-007 · **Réf. spec :** §7.8, §13.1–13.3 · **Exigences :** EF-LOG-01, EF-LOG-02, EF-MSG-07

## 1. Objectif

Recevoir les accusés de livraison, les corréler au message d'origine, faire évoluer son état jusqu'à `DELIVERED`/`FAILED`/`EXPIRED`, et présenter le flux complet dans une table temps réel filtrable — c'est le milestone **M2** de la spec §22.

Le DLR est le seul retour qui dit si le message est réellement arrivé. Sa corrélation est délicate : le `receipted_message_id` n'est pas toujours présent en TLV, le corps texte n'est pas normalisé entre SMSC, et l'identifiant peut être renvoyé dans une casse ou une base différente de celle du `submit_sm_resp`.

## 2. Périmètre

### Dans le périmètre

- Réception et acquittement des `deliver_sm` ; distinction **DLR** vs **MO** via `esm_class`.
- Extraction de l'identifiant : TLV `receipted_message_id` en priorité, sinon parsing du corps texte standard `id:… sub:… dlvrd:… submit date:… done date:… stat:… err:… text:…`.
- Corrélation par `smsc_message_id` (index de step-002), avec stratégie explicite en cas d'identifiant introuvable (DLR orphelin conservé et signalé, jamais silencieusement jeté).
- Mise à jour d'état : `DELIVRD`, `EXPIRED`, `DELETED`, `UNDELIV`, `ACCEPTD`, `REJECTD`, `UNKNOWN` → états internes, avec `dlr_stat`, `dlr_err`, `dlr_at`.
- Journal métier complet (spec §13.2) : tous les champs de la table `messages` alimentés.
- Journal PDU (`pdu_log`) : direction, `command_id`, `command_status`, `sequence_number`, hexadécimal brut et décodé — **activable/désactivable**, désactivé par défaut.
- Écran Journaux : table virtualisée (TanStack Virtual), pagination côté backend, filtres (session, campagne, état, plage de dates, destinataire/préfixe, code d'erreur), recherche plein texte, codes couleur par état, panneau de détail PDU au clic.
- Événement `message:update` agrégé et throttlé.
- Commande `logs_query` (filtre + pagination).

### Hors périmètre

- L'export des journaux → **step-014**.
- Les statistiques agrégées et tableaux de bord → **step-014**.
- La rétention/purge → **step-014**.
- Le masquage du contenu des messages → **step-015** (l'option est prévue dans le modèle, appliquée en step-015).
- Les MO entrants métier (autres que DLR) : reçus, acquittés et journalisés, mais aucun traitement applicatif.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-008-01 | Détection DLR vs MO, parsing du corps de DLR | `crates/messaging/src/dlr.rs` |
| L-008-02 | Corrélation par `smsc_message_id` + gestion des orphelins | `crates/messaging/src/correlation.rs` |
| L-008-03 | Journal métier (écritures groupées) | `crates/logging-export/src/journal.rs` |
| L-008-04 | Journal PDU activable | `crates/logging-export/src/pdu_log.rs` |
| L-008-05 | Commande `logs_query` + événement `message:update` | `src-tauri/src/commands/logs.rs` |
| L-008-06 | Écran Journaux (table virtualisée, filtres, détail PDU) | `ui/src/views/Logs/` |

## 4. Critères d'acceptation

- [ ] **CA-008-01** — Un DLR `stat:DELIVRD` fait passer le message correspondant à `DELIVERED`, avec `dlr_at` renseigné, visible dans l'UI en moins d'une seconde.
- [ ] **CA-008-02** — La corrélation fonctionne dans les deux cas : TLV `receipted_message_id` présent, et corps texte seul.
- [ ] **CA-008-03** — Les formats de corps de DLR non standard (champs manquants, ordre différent, casse variable, espaces multiples) sont tolérés ; un corps illisible produit un DLR orphelin journalisé, **jamais** un panic ni une perte silencieuse.
- [ ] **CA-008-04** — Un DLR dont l'identifiant est inconnu est conservé, marqué orphelin et consultable dans l'UI.
- [ ] **CA-008-05** — Les sept statuts de DLR sont mappés vers les états internes ; un test paramétré couvre les sept.
- [ ] **CA-008-06** — Tout `deliver_sm` reçoit un `deliver_sm_resp` ; un test vérifie qu'aucun n'est laissé sans acquittement même sous charge.
- [ ] **CA-008-07** — La table de journaux affiche 200 000 lignes sans dégradation perceptible : défilement fluide, filtre appliqué en moins d'une seconde (virtualisation + pagination backend).
- [ ] **CA-008-08** — Les gros volumes ne transitent jamais par événement : `message:update` porte des mises à jour incrémentales agrégées, la table se remplit via `logs_query` paginée (vérifié en inspectant le trafic IPC).
- [ ] **CA-008-09** — Le journal PDU est **désactivé par défaut** ; une fois activé, le détail au clic affiche en-tête, corps décodé, TLV et hexadécimal brut.
- [ ] **CA-008-10** — Les mises à jour d'état sont écrites en lot : à 1 000 TPS, le nombre de transactions par seconde reste très inférieur au nombre de messages (mesure explicite).
- [ ] **CA-008-11** — **Recette M2 :** envoi régulé de 1 000 messages, débit respecté, 100 % des DLR reçus corrélés, journal complet et cohérent.

## 5. Tests attendus

- **Unitaires :** parseur de corps de DLR — corpus de formats réels de plusieurs SMSC, champs manquants, dates malformées, `text:` contenant des `:` ; mapping des sept statuts ; détection DLR vs MO par `esm_class`.
- **Propriété :** aucun corps de DLR arbitraire ne provoque de panique.
- **Intégration :** serveur factice émettant des DLR après un délai, dans le désordre, en double (idempotence : un DLR reçu deux fois ne compte qu'une fois), et pour un identifiant inconnu.
- **Frontend :** virtualisation, filtres combinés, panneau de détail.
- **Non-régression :** chaque format de DLR problématique rencontré devient une fixture.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /rusmpp/rusmpp "deliver_sm delivery receipt esm_class receipted_message_id TLV and message_state"
  ```
- **La casse et le format du `message_id` diffèrent selon les SMSC** : certains renvoient l'identifiant en hexadécimal dans le `submit_sm_resp` et en décimal dans le DLR. Prévoir une normalisation à la corrélation, et la rendre configurable par session si nécessaire — c'est la première cause de DLR non corrélés en production.
- Les DLR arrivent **après** la réponse de soumission, parfois plusieurs heures plus tard, éventuellement après un redémarrage de l'application : la corrélation passe par la base, jamais par un état en mémoire.
- Le journal PDU est volumineux et contient potentiellement du contenu de message : désactivé par défaut, purgé agressivement, et jamais exporté par accident (voir step-015).
- L'écriture du journal ne doit pas ralentir le chemin d'émission : découpler par canal et écrire en lot depuis une tâche dédiée.
