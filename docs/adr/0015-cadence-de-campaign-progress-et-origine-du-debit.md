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

## Décision 2 — La charge utile porte le débit **de la campagne**

`campaign:progress` porte les compteurs, le `session_id` et
`accepted_per_second` : les messages que le SMSC a **acceptés** pour cette
campagne, par seconde, sur une fenêtre glissante de dix secondes.

Il est mesuré par `messaging::campaign::progress::AcceptanceRate`, dans le
producteur, contre l'horloge **injectée** du runner (CLAUDE.md §7) — jamais
dérivé côté WebView.

### Options examinées

**Option A — lire `metrics:tick` et ne rien porter ici.** C'est ce qui avait été
écrit d'abord, au motif que le jalon 007 mesure déjà un débit à plein régime et
qu'un second chiffre serait un second chiffre à tenir. **Écartée**, et par sa
propre réserve : `metrics:tick` mesure la **session**, donc un envoi unitaire
effectué pendant qu'une campagne tourne y est compté. Le nombre affiché à côté
des compteurs d'une campagne ne décrivait alors pas cette campagne. Deux nombres
présentés ensemble doivent parler de la même chose ; c'est aussi la lettre de la
spec §15.3, qui écrit « compteurs + débit » pour cet événement.

**Option B — dériver le débit dans la WebView, à partir des compteurs reçus.**
*Contre :* la précision dépendrait de `CAMPAIGN_PROGRESS_INTERVAL`, si bien que
resserrer la cadence pour protéger le pont dégraderait silencieusement une
mesure. C'est exactement la dépendance que la décision 1 est arrangée pour
éviter, et CLAUDE.md §3 garde ce genre de calcul hors de la WebView.

**Option C, retenue — mesurer dans le producteur, à partir des acceptations.**
La fenêtre vit dans la boucle du runner, qui la possède seule : pas de verrou,
pas d'atomique, et une horloge injectée qui rend une mesure de dix secondes
testable en microsecondes.

### Ce qui est compté, et ce qui ne l'est pas

Des **acceptations**, pas des soumissions. Un message refusé puis rejoué deux
fois représente une livraison de travail : compter les tentatives mettrait le
débit d'une campagne à son maximum précisément au moment où le SMSC refuse tout.

### Conséquences assumées

- `metrics:tick` garde sa place et son sens : c'est le débit de la **session**,
  et les écrans Sessions et Tableau de bord l'affichent à ce titre. Les deux
  chiffres peuvent différer, et c'est la raison d'être des deux.
- Une fenêtre de dix secondes est plus lisse qu'une seconde : un opérateur veut
  savoir si la campagne avance, pas ce qui s'est passé dans les 250 dernières
  millisecondes. Le diviseur est la portion de fenêtre réellement écoulée, sinon
  la première seconde d'une campagne s'afficherait au dixième de son débit réel.
- Un silence supérieur à la fenêtre ramène le débit à zéro tout seul, ce qui est
  la lecture juste : une campagne dont le SMSC ne répond plus est arrêtée.

## Décision 3 — Le détail message par message n'est **pas** dans cet événement

Une campagne de 500 000 destinataires ne pousse pas 500 000 événements. Cet
événement porte des **compteurs agrégés** ; le détail se lit par pagination via
`logs_query`, filtré sur la campagne — l'écran Journaux du jalon 008.

C'est la même règle, pour la même raison, que `message:update` (CA-008-08) et
`import:progress` (CA-009-11). Elle est écrite dans l'en-tête de
`CampaignProgressEvent`, dans celui de `src-tauri/src/commands/campaign.rs` et
dans le wrapper `onCampaignProgress` — c'est-à-dire aux trois endroits où
quelqu'un serait tenté d'ajouter un champ par message.
