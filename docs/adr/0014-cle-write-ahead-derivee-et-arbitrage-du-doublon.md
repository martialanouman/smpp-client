# ADR 0014 — Clé write-ahead dérivée, et arbitrage du doublon à la reprise

> **Statut :** Accepté
> **Date :** 2026-08-02 · **Jalon :** step-010 · **Décideur :** Martial Anouman
> **Applique :** [ADR 0010](0010-inversion-des-ports-du-chemin-d-envoi.md) (write-ahead et ports du chemin d'envoi)

## Contexte

Le jalon 010 pose un invariant : **au plus une émission acceptée par
destinataire**, à travers les pauses, les rejeux, les erreurs et les
redémarrages à froid (CA-010-03, CA-010-04, CA-010-05). La fiche §6 le nomme
comme l'invariant central du jalon et demande qu'il soit écrit dans le code.

La spec §10.5 décrit la reprise ainsi : « chaque message porte un
`client_message_id` (UUID) et un `campaign_id` ; en cas d'arrêt, la reprise
repart des messages en état `QUEUED`/`SENT` non confirmés ; un garde-fou empêche
le double envoi (vérification d'état avant émission) ».

Elle laisse deux questions ouvertes, et ce sont les deux qui décident si
l'invariant tient.

## Décision 1 — Le `client_message_id` d'un message de campagne est **dérivé**

`client_message_id = UUID v5(namespace = campaign_id, name = MSISDN normalisé)`

C'est une fonction pure, donc le processus qui redémarre après un `kill -9`
recalcule exactement les identifiants qu'il avait écrits.

### Options examinées

**Option A — Identifiant aléatoire (v4) + suivi d'une position de lecture.**
La campagne mémorise « j'en suis au destinataire *k* » et reprend à partir de là.
*Contre :* la position doit être persistée à une fréquence qui rouvre le même
problème (persistée trop rarement, elle fait rejouer ; persistée à chaque
message, elle double les écritures), et elle est fausse dès que la source change
entre deux exécutions — un contact ajouté à la liste décale tout.

**Option B — Identifiant aléatoire (v4) + recherche par `(campaign_id, MSISDN)`
avant chaque envoi.** *Contre :* exige un index supplémentaire, et surtout fait
reposer l'unicité sur une lecture suivie d'une écriture, c'est-à-dire sur une
fenêtre dans laquelle un crash peut tomber. La base ne garantit rien.

**Option C, retenue — Identifiant dérivé.** L'unicité devient une propriété de
la **clé primaire** de `messages` : la seconde insertion pour un même
destinataire échoue, dans la base, atomiquement. La reprise n'a rien à mémoriser
et rien à retrouver. Le garde-fou de la spec §10.5 — la vérification d'état —
reste nécessaire et devient la *seconde* barrière : il décide quoi faire de la
ligne qui existe déjà.

### Conséquences assumées

- Une campagne envoie **au plus un message par numéro distinct**. Un même numéro
  présent deux fois dans la source est un destinataire, pas deux. C'est la
  lecture forte de l'invariant, et elle est comptée (`skipped`) plutôt que
  silencieuse.
- L'identifiant n'est plus imprévisible. Il n'est pas un secret : il n'apparaît
  ni dans un PDU, ni dans un export destiné à un tiers, et il n'autorise rien.
- Le rejeu de la spec §10.7 réutilise la même ligne. `Sender::resend` existe pour
  cela : même chemin d'envoi, sans l'insertion.
- Une campagne fraîche ne fait **aucune lecture** du journal : l'insertion
  write-ahead est le contrôle. Seule une reprise interroge d'abord
  (`StartMode`).

## Décision 2 — Un message `SENT` **sans réponse journalisée** est réémis

### Préalable : la tentative est journalisée avant la socket

Cette décision n'a de sens que si l'état persisté dit la vérité. La transition
`SENT` est donc commitée **avant** `submit_all`, dans sa propre transaction.

Ce n'était pas le cas à la première rédaction de cette ADR, et une revue l'a
relevé : `SENT` était empilée avec le verdict et écrite après le retour du SMSC,
si bien qu'un `kill -9` entre le `submit_sm` parti et le commit laissait la ligne
en `QUEUED` — état que tout le reste du module lit comme « rien n'est parti ».
L'arbitrage ci-dessous portait donc sur une population **vide** :
`UnansweredPolicy::Abandon` ne protégeait de rien et le compteur de risque valait
zéro au moment précis où des doublons partaient. C'est aussi la lettre de
CLAUDE.md §4 : « un message est persisté avant émission ; ses transitions d'état
sont traçables ».

Coût mesuré : une transaction SQLite (WAL) de plus par message, 111,7 µs sur la
machine de développement, soit ~56 s pour 500 000 destinataires. Le chemin
d'émission étant séquentiel et borné par l'aller-retour vers le SMSC, cette
écriture plafonne le débit à ~8 900 messages/s : deux ordres de grandeur
au-dessus de ce que la campagne atteint.

### Ce que `SENT` dit, et ce qu'il ne dit pas

`SENT` dit **qu'un `submit_sm` a quitté ce processus**. Il ne dit pas qu'une
réponse est arrivée — c'est `command_status` qui le dit, et la reprise lit les
deux :

| Ligne | Le SMSC | Réémettre |
|---|---|---|
| `QUEUED` | ne l'a jamais vue | ne peut pas dupliquer |
| `SENT` + `command_status` | a répondu, en refusant | ne peut pas dupliquer — il ne l'a pas prise |
| `SENT` sans `command_status` | **l'a peut-être prise** | peut dupliquer |
| `ACCEPTED` et au-delà | l'a prise | interdit (CA-010-05) |

Seule la troisième ligne est incertaine, et seule elle est arbitrée. Les
confondre — ce que fait la lecture du seul état — fait abandonner sous `Abandon`
un message dont on **sait** qu'il a été refusé, et laisse sa ligne non terminale
à jamais.

### L'arbitrage lui-même

Pour cette troisième ligne : le SMSC l'a peut-être acceptée, ou jamais vue.
**SMPP ne permet pas de le demander** : `submit_sm` ne porte pas de clé
d'idempotence, et `query_sm` prend le `message_id` que la réponse manquante
aurait porté.

Les deux politiques possibles sont donc :

| | Réémettre (**retenu**, défaut) | Abandonner |
|---|---|---|
| Risque | un destinataire reçoit deux fois | un destinataire ne reçoit rien |
| Visibilité | le destinataire le voit, la ligne porte `attempts >= 2`, le rapport le chiffre | aucune |
| Cohérence | identique à l'ordre write-ahead d'ADR 0010 | le contredit |

**Retenu : réémettre**, pour trois raisons, dans l'ordre où elles pèsent.

1. ENF-FIA-01 demande qu'aucun message ne soit perdu, et la spec §10.5 nomme
   explicitement `SENT` parmi les états dont la reprise repart.
2. C'est l'arbitrage déjà rendu partout ailleurs dans ce dépôt. L'ordre
   « persister puis émettre » du jalon 006 a été choisi *parce que* le crash
   doit dupliquer plutôt que perdre ; une reprise qui abandonnerait les `SENT`
   contredirait le module qu'elle reprend.
3. Un doublon est **visible et borné** ; une sous-livraison ne l'est pas. Rien
   dans le journal ne distingue un message jamais reçu d'un message reçu.

### Conséquences assumées

- La fenêtre n'est pas nulle : elle couvre les messages en vol au moment de
  l'arrêt, donc au plus la taille de la fenêtre d'émission. Elle est chiffrée
  dans le rapport de campagne (`reemitted_unanswered`) et journalisée en
  `warn`. Le test de propriété vérifie que **tout doublon réellement livré est
  couvert par ce chiffre**, et que sous `Abandon` il n'y en a aucun — un doublon
  dont l'opérateur n'a pas été averti fait échouer la propriété.
- **Résidu, non résolu :** une soumission que la *session elle-même* refuse
  avant la socket (`SubmitError::NotBound`, session en reconnexion) laisse une
  ligne `SENT` sans `command_status`, donc classée incertaine alors que rien
  n'est parti. Dans une exécution le budget de rejeu la mène à `FAILED` ; seul un
  crash pendant ce cycle en laisse une. Sous `Reemit` elle est simplement
  renvoyée et seul le chiffre de risque sur-déclare ; sous `Abandon` elle est
  abandonnée, et c'est un message perdu qui n'était jamais parti. Réduire ce
  résidu supposerait d'écrire un `command_status` qu'aucun SMSC n'a envoyé, ce
  que le journal ne fait pas.
- L'arbitrage inverse est disponible sans rien changer d'autre :
  `UnansweredPolicy::Abandon`. Il est offert parce que c'est une décision
  produit, et qu'un opérateur dont le contenu est facturé au destinataire peut
  légitimement préférer sous-livrer.
- Une ligne `QUEUED` est réémise **quelle que soit** la politique : rien n'est
  parti, donc rien ne peut être dupliqué.

## Conséquence commune

L'invariant s'énonce sur les messages **acceptés**, pas sur les émissions : la
spec §10.7 rejoue délibérément un message refusé ou sans réponse. Le test de
propriété du jalon (`crates/messaging/tests/campaign_invariant.rs`) le vérifie
sur l'enregistrement du SMSC factice — ce qu'il a accepté — et non sur ce que
l'exécuteur déclare de lui-même.
