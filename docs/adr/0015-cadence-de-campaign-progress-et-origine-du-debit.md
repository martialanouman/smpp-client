# ADR 0015 — Cadence de `campaign:progress`, et d'où vient le débit affiché

> **Statut :** Acceptée · **Date :** 2026-08-02 · **Jalon :** step-010
> **Décideur :** Martial Anouman
> **Applique :** [ADR 0003](0003-generation-des-types-ipc.md) (contrat IPC généré)

## Contexte

CA-010-11 demande que `campaign:progress` soit **throttlé** et que « l'UI affiche
la progression et le débit sans dégradation pendant une campagne à débit
maximal ». La spec §15.3 décrit la charge utile de cet événement comme
« compteurs + débit ».

Deux questions en découlent, et elles se répondent séparément.

Ce dépôt a déjà payé deux fois le prix d'une mauvaise réponse à la première :

- **Jalon 007, `sessions:state`.** L'étranglement vivait à l'émetteur et
  *jetait* les émissions trop rapprochées. Comme l'état d'une session cesse de
  changer une fois `BOUND`, l'émission jetée était souvent la dernière : l'écran
  restait sur `CONNECTING` pour toujours (`events.rs`, `SESSIONS_STATE_INTERVAL`).
- **Jalon 009, `import:progress`.** La leçon a été appliquée : l'étranglement est
  passé **en amont**, dans le producteur, et `emit_import_progress` est devenu
  **inconditionnel** — parce que le dernier événement d'un import est celui qui
  porte `done`.

## Décision 1 — La cadence est **structurelle**, et l'événement final est hors boucle

`src-tauri/src/campaigns.rs::run_reporting` exécute la campagne et **échantillonne**
ses compteurs toutes les `CAMPAIGN_PROGRESS_INTERVAL` (250 ms, la même limite de
4 Hz que `metrics:tick`). L'échantillonnage et l'exécution sont deux moitiés d'un
`tokio::join!` ; l'exécution annule un `CancellationToken` en se terminant, et
l'échantillonneur l'observe dans un `select!` **biaisé**.

Quand `run_reporting` rend la main, l'échantillonneur **a déjà terminé** — c'est
la propriété du `join!`. L'appelant publie alors la lecture finale, portant le
statut terminal et `done: true`, par un émetteur qui n'applique **aucun**
étranglement.

Il en découle trois propriétés, chacune tenue par un test :

| Propriété | Test |
|---|---|
| le débit d'émission est l'intervalle, quel que soit le débit de la campagne | `the_sampling_rate_is_the_interval_whatever_the_throughput` |
| aucune lecture périmée ne suit la fin de la campagne | `no_reading_is_published_after_the_campaign_has_ended` |
| une campagne plus courte qu'un intervalle n'émet que sa lecture finale | `a_campaign_shorter_than_one_interval_samples_nothing` |

### Options examinées

**Option A — un `Throttle` sur `emit_campaign_progress`.** C'est exactement le
défaut du jalon 007 : le filtre serait libre de jeter l'événement `done`, et une
campagne de 200 000 messages terminée laisserait une barre de progression en
route sous un statut terminal. **Rejetée.**

**Option B — émettre à chaque message, laisser l'UI se débrouiller.** 500 000
événements pour 500 000 destinataires. C'est ce que CA-008-08 interdit déjà pour
le journal, et ce que CA-010-11 interdit ici.

**Option C — une tâche `spawn`ée annulée par `abort()`.** Fonctionne, mais
l'ordre n'est plus une propriété du code : un `abort` peut arriver avec une
émission déjà en vol, et rien dans le type ne dit le contraire. Le `join!` rend
l'ordre indémontrablement faux.

**Option D, retenue — la cadence dans le producteur, l'événement final hors
boucle.** C'est la forme que `metrics:tick` a déjà (`sessions.rs::tick`), plus la
publication finale que `contacts.rs` obtient en `await`ant son forwarder.

### Conséquences assumées

- Une lecture intermédiaire manquée coûte une image d'animation : chaque charge
  utile est une **lecture**, pas un delta. La suivante porte la vérité courante.
- Les compteurs vivent dans `messaging::CampaignProgress`, un
  `tokio::sync::watch` sur un `CampaignTally` `Copy`. Le runner y publie à chaque
  élément — un `send_replace`, sans réveil si personne n'attend — donc la
  précision des chiffres ne dépend **pas** de la cadence d'affichage. C'est la
  dépendance que le jalon 007 avait déjà refusé d'introduire pour le débit.

## Décision 2 — La charge utile ne porte **pas** de débit

`campaign:progress` porte les compteurs et le `session_id` de la session
d'envoi. Le débit affiché à côté de la barre est celui de `metrics:tick` pour
cette session (`ui/src/store/metrics.ts`).

**C'est un écart avec la lettre de la spec §15.3**, qui écrit « compteurs +
débit ». Il est délibéré et signalé plutôt que tranché en silence.

### Pourquoi

Le jalon 007 mesure déjà le débit d'une session : moyennes glissantes 1 s / 10 s,
occupation de fenêtre, RTT, tenues à **plein régime** dans `smpp-session` et
échantillonnées à 4 Hz (spec §9.6). Un second chiffre serait :

- **différent** — calculé sur une autre fenêtre, par un autre code, et affiché à
  côté des jauges des écrans Sessions et Tableau de bord qui montreraient autre
  chose pour la même chose ;
- **fragile** — s'il était dérivé côté WebView des quatre échantillons par
  seconde qui y parviennent, sa précision dépendrait de
  `CAMPAIGN_PROGRESS_INTERVAL`, si bien que resserrer la cadence pour protéger le
  pont dégraderait silencieusement une mesure. C'est exactement la dépendance que
  la fiche du jalon 007 demandait d'éviter.

### Conséquences assumées

- **Le chiffre affiché est celui de la session, pas de la campagne.** Un envoi
  unitaire effectué sur la même session pendant qu'une campagne tourne y est
  compté. Au jalon 010 une campagne cible une seule session et la garde pour la
  durée de son exécution, donc c'est un coin et non le cas courant ; la
  répartition sur plusieurs sessions est le jalon 011, qui devra reposer la
  question.
- Un consommateur du contrat IPC qui lirait la spec §15.3 et attendrait un champ
  `débit` ne le trouvera pas. Le champ `sessionId` est là pour ça, et la
  documentation de `CampaignProgressEvent` le dit à l'endroit où on la lira.

## Décision 3 — Le détail message par message n'est **pas** dans cet événement

Une campagne de 500 000 destinataires ne pousse pas 500 000 événements. Cet
événement porte des **compteurs agrégés** ; le détail se lit par pagination via
`logs_query`, filtré sur la campagne — l'écran Journaux du jalon 008.

C'est la même règle, pour la même raison, que `message:update` (CA-008-08) et
`import:progress` (CA-009-11). Elle est écrite dans l'en-tête de
`CampaignProgressEvent`, dans celui de `src-tauri/src/commands/campaign.rs` et
dans le wrapper `onCampaignProgress` — c'est-à-dire aux trois endroits où
quelqu'un serait tenté d'ajouter un champ par message.
