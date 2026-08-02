# Changelog

Toutes les modifications notables de ce projet sont consignées ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le
versionnage suit [SemVer](https://semver.org/lang/fr/) : `feat` → mineur,
`fix` → correctif, changement cassant du contrat IPC ou du format de données →
majeur.

## [Non publié]

### Ajouté — jalon 010, campagnes : exécution (alimentation, reprise, contrôle)

- **Alimentation en flux avec back-pressure de bout en bout**
  (`messaging::campaign::feeder`) : les destinataires sont lus depuis un
  `RecipientSource` (port déclaré ici, implémenté au-dessus de `contacts`) et
  poussés un par un dans une file **bornée** à 256 messages rendus. Aucun tampon
  intermédiaire sur ce chemin : si le SMSC ralentit, la fenêtre d'émission se
  remplit, la file se remplit, et la lecture de la base s'arrête. Mesuré, pas
  supposé — voir plus bas.
- **Le lecteur est asynchrone, et c'est ce qui évite l'interblocage du jalon
  009** : la poussée dans la file est une branche de `select!` dont l'autre est
  le jeton d'annulation, là où l'import du jalon 009 utilise `blocking_send`
  depuis `spawn_blocking` — un appel qui n'observe pas le jeton et qui a rendu
  nécessaire un `receiver.close()` avant la jointure. Test de non-régression
  dédié : annuler une campagne dont la file est pleine libère le lecteur.
- **Invariant central, écrit noir sur blanc**
  (`messaging::campaign::resume`) : *une campagne émet au plus un message
  accepté par destinataire*, à travers les pauses, les rejeux, les erreurs et
  les redémarrages à froid. Il repose sur deux mécanismes : un
  `client_message_id` **dérivé** (UUID v5 de `campaign_id` + MSISDN normalisé),
  donc la clé primaire de `messages` refuse la seconde ligne d'un même
  destinataire ; et une vérification d'état avant chaque émission. Test de
  propriété (`tests/campaign_invariant.rs`) sur des historiques arbitraires —
  réponses du SMSC, pause/reprise/annulation, jusqu'à trois redémarrages.
- **Arbitrage produit sur le message `SENT` sans réponse au moment du crash** :
  par défaut il est **réémis** (spec §10.5 le nomme, ENF-FIA-01 demande de ne
  perdre aucun message, et un doublon est visible et compté là où une
  sous-livraison est invisible). Le risque est chiffré dans le rapport de
  campagne (`reemitted_unanswered`) et journalisé. La politique inverse
  (`UnansweredPolicy::Abandon`) est disponible sans rien changer d'autre.
- **Contrôles démarrer / pause / reprise / annulation**
  (`messaging::campaign::control`) : un `watch` pour l'état, un
  `CancellationToken` pour l'arrêt, et une campagne peut être fille du jeton
  d'arrêt de l'application. La pause suspend l'alimentation ; les messages déjà
  dans la fenêtre se terminent et sont journalisés (CA-010-03). L'annulation
  interrompt aussi les attentes — délai de rejeu, démarrage différé — qui
  seraient sinon ce qui retient le plus longtemps une campagne (CA-010-09).
- **Planning différé et plage horaire** (`messaging::campaign::schedule`) :
  passage de minuit et fuseaux traités, horloge injectée, fonction pure. La
  plage est vérifiée **avant chaque message** et non une fois au départ, sans
  quoi une campagne lancée à 19 h 59 tournerait toute la nuit.
- **Exécuteur de campagne** (`messaging::campaign::runner`) : alimentation et
  émission dans **une seule tâche** (`tokio::join!`), aucune tâche orpheline,
  compteurs exacts. Les cinq compteurs partitionnent les destinataires, et
  l'égalité vérifiée est `total == file` — deux compteurs incrémentés par deux
  boucles différentes, non un contrôle du total contre lui-même.
- **CA-010-01 est mesuré** (`tests/campaign_volume.rs`) : la RSS du processus
  est échantillonnée *pendant* la campagne, tous les 5 000 messages. 3 520 kio
  de crête pour 5 000 destinataires, 3 584 kio pour 500 000 — 64 kio d'écart,
  soit 0,13 octet par destinataire supplémentaire. Vérifié par mutation :
  accumuler le texte de chaque message fait passer l'écart à 36 Mio et le test
  au rouge. Ce que la mesure **n'inclut pas** est écrit dans l'en-tête du test
  (la base de données, et une source qui matérialiserait ses lignes).

### Corrigé — jalon 010, revue

- **La transition `SENT` est commitée avant la socket, pas après la réponse.**
  Elle était empilée avec le verdict et écrite au retour de `submit_all` : un
  `kill -9` entre le `submit_sm` parti et ce commit laissait donc la ligne en
  `QUEUED`, état que la reprise lit comme « rien n'est parti, réémettre ne peut
  pas dupliquer ». L'arbitrage d'ADR 0014 portait sur une population vide —
  `UnansweredPolicy::Abandon` ne protégeait de rien, et `reemitted_unanswered`
  valait zéro au moment précis où des doublons partaient. Coût mesuré : une
  transaction SQLite de plus par message, 111,7 µs, soit ~56 s pour 500 000
  destinataires ; le chemin d'émission étant séquentiel et borné par
  l'aller-retour SMSC, c'est deux ordres de grandeur sous le débit atteint.
- **`SENT` ne veut plus dire deux choses.** Une tentative rejouable écrivait
  `SENT`, indistinguable d'un message en vol : sous `Abandon` on abandonnait
  définitivement un message dont on savait qu'il avait été **refusé**, et sa
  ligne restait non terminale à jamais. La reprise lit désormais `SENT` **et**
  `command_status` : un refus répondu n'est pas un risque de doublon et est
  rejoué sous les deux politiques.
- **Un message dont le délai de rejeu est annulé reçoit son verdict** (CA-010-09,
  « aucun message laissé dans un état indéterminé ») : la campagne étant
  terminée, sa dernière tentative est son verdict et la ligne devient terminale.
- **Un `resend` d'un message absent du journal n'émet plus rien** : il
  soumettait puis signalait `journalled: false`, c'est-à-dire un PDU sans ligne
  derrière lui.
- **Une clé en conflit dont la ligne est illisible n'est plus perdue** (deux
  exécutions de la même campagne en parallèle) : elle est comptée comme
  ignorée, au lieu de sortir de tous les compteurs et de casser l'égalité
  `total == file` en silence.
- **Le test de propriété atteignait une seule famille.** Le journal ne pouvait
  jamais échouer — donc la famille ci-dessus était inatteignable, ce qui explique
  qu'elle ait survécu à la suite — et le seul point de suspension était le délai
  de rejeu, si bien que les commandes de l'opérateur n'étaient servies qu'après
  la fin de la campagne. Le générateur injecte désormais une panne de journal par
  exécution, les doubles cèdent la main à chaque opération, et le script de
  l'opérateur avance en tours d'ordonnanceur et non en millisecondes. Un test de
  recensement compte les familles atteintes et échoue si l'une redevient
  inatteignable.
- **L'invariant est énoncé exactement.** « Au plus un message accepté par
  destinataire » est faux sous `Reemit`, qui réémet délibérément : la propriété
  vérifie que sous `Abandon` il n'y a **aucun** doublon, et que sous `Reemit`
  tout doublon réellement livré est couvert par `reemitted_unanswered`.

### Modifié — jalon 010

- **`FAILED` n'est plus écrit sur une tentative que la campagne va rejouer.**
  `FAILED` est terminal (spec §14.3), donc un rejeu accepté ne pouvait pas être
  enregistré : le journal restait `FAILED` pour un message effectivement parti,
  et les compteurs contredisaient la base — exactement ce que CA-010-02 vérifie.
  Une tentative rejouable écrit désormais `SENT`, et le verdict est écrit par la
  tentative finale ; la condition est exactement celle sous laquelle
  `RetryPolicy::decide` abandonne. Nouveau champ `SendRequest::last_attempt`.
- **`SendReport::retryable` a une seule source de vérité.** Il lisait la
  classification pour un refus et une liste écrite à la main
  (`ResponseTimeout | Closed`) pour le reste, en désaccord avec la politique de
  rejeu qui rejoue aussi `Transport` et `NotBound`. Il délègue maintenant à
  `SendFailure::is_retryable`.
- **`SendRequest` porte son `campaign_id`** et accepte une clé write-ahead
  choisie par l'appelant ; `Sender::resend` envoie un message dont la ligne
  existe déjà (rejeu, reprise) sans insérer une seconde.
- **`Timestamp::from_offset_date_time`** (`smpp-core`) : contrepartie de
  `as_offset_date_time`, tronquée à la seconde, pour les calculs de plage
  horaire.

### Ajouté — jalon 010, campagnes : fondations (modèles, rejeu, cycle de vie)

- **Moteur de modèles à variables** (`messaging::template`) : `{{prenom}}` et
  `{{ville}}` résolues par destinataire depuis les attributs JSON du contact.
  L'invariant de CA-010-06 — aucun message porteur d'un `{{…}}` ne sort du
  moteur — est **vérifié sur le texte produit** et par position, non par
  comptage : un message ne peut pas contenir un `{{` suivi d'un `}}`, quelle que
  soit la source. Politique de variable manquante explicite (valeur de
  remplacement ou rejet de la ligne, rejet par défaut) ; une valeur vide compte
  comme absente, sans quoi la politique de rejet serait inatteignable pour les
  cellules vides que l'import écrit en `""`.
- **Deux restrictions volontaires du moteur de modèles**, conséquences de
  l'invariant ci-dessus et signalées comme telles : une **valeur** de contact ne
  peut contenir **aucune accolade** (`Yamoussoukro {ancienne capitale}` fait
  rejeter la ligne, nommément), et un modèle qui échappe `{{{{` avec un `}}`
  quelque part après est refusé **à la validation** de la campagne. Les deux
  ferment un contre-exemple trouvé en revue : `{{{{{{a}}` avec `a = "ville}}"`
  produisait `{{ville}}` chez le destinataire, l'échappement fournissant
  l'ouverture et la donnée la fermeture — aucune règle regardant les deux
  moitiés séparément, ni aucun comptage, ne le voyait.
- **Politique de rejeu par code d'erreur** (`messaging::retry`) : s'appuie sur la
  classification du jalon 003 et ne reclasse rien. `ESME_RINVDSTADR` jamais
  rejoué, `ESME_RTHROTTLED` et `ESME_RMSGQFUL` rejoués après délai, timeout
  rejoué (CA-010-07). Le délai est une fonction pure du numéro de tentative —
  l'attente appartient à l'exécuteur, qui détient le jeton d'annulation.
- **Machine d'états de campagne** (`messaging::campaign`) : le cycle de la spec
  §10.3, transitions énumérées et refus explicite des invalides, avec les deux
  propriétés héritées de la machine des messages — transition vers soi-même
  autorisée, statut terminal sans successeur. **Écart assumé avec le diagramme
  de la spec §10.3**, détaillé dans l'ADR 0013 : `CANCELLED`/`FAILED` sont
  atteignables depuis tout statut vivant et non depuis `RUNNING` seulement, et
  `PAUSED → COMPLETED` est autorisé. Le chemin nominal n'est pas raccourci —
  `CREATED → RUNNING` sans validation reste refusé.
- **Le débit de rejeu a un plancher** : une politique construite avec un délai
  nul est refusée, sans quoi un `ESME_RTHROTTLED` — le SMSC demandant
  explicitement de ralentir — recevait dix réémissions immédiates.
- **`CampaignStatus` déplacé de `persistence` vers `messaging`** (ADR 0013) : la
  crate qui possède le cycle de vie possède le type qui le porte. Format stocké
  inchangé, aucune migration, `persistence::CampaignStatus` résout toujours.

### Ajouté — jalon 009, contacts : import CSV/XLSX, E.164 et listes

- **Lecture en flux** (`contacts::import`) : le lecteur tourne sur
  `spawn_blocking`, l'écrivain sur le runtime, et entre les deux une file
  **bornée** à 1 024 lignes. Un disque lent fait attendre le lecteur au lieu de
  faire grossir le processus. Séparateur, encodage et ligne d'en-tête sont
  détectés sur un préfixe borné, le lecteur étant rendu non consommé
  (CA-009-02).
- **Validation E.164** (`contacts::validation`) : un numéro national plus un
  pays donnent une forme internationale ; les refus portent un **motif précis**
  parmi dix, exportable pour correction (CA-009-04, CA-009-05). L'option
  « mobiles uniquement » s'appuie sur le plan de numérotation et non sur le
  préfixe (CA-009-06).
- **Déduplication sur le numéro normalisé** (CA-009-07), en deux stratégies :
  première occurrence, qui ne retient que des empreintes de 64 bits, et fusion
  des attributs, qui retient les contacts eux-mêmes — dit là où l'opérateur
  choisit.
- **Rapport exact** : `total = importées + rejetées + doublons` est un
  invariant porté par le type qui compte, pas une assertion de test
  (CA-009-08). Les lignes vides sont comptées à part.
- **Annulation en cours d'import** : l'écrivain valide le lot qu'il tient — un
  lot est une transaction — et rend un rapport marqué annulé dont le compte
  d'importées est exactement ce qui est en base (CA-009-10).
- **Profils de mapping** réutilisables sans ressaisie (CA-009-09), **listes**
  avec union, intersection et exclusion (CA-009-12).
- **Port `ContactRepository` défini côté `contacts`** et implémenté côté
  `persistence` (CA-009-13, ADR 0012) : l'arête provisoire du jalon 002 est
  retirée.
- **Écran Contacts** : assistant d'import, rapport avec export des lignes
  rejetées, table virtualisée paginée côté backend, recherche et filtre par
  liste. `import:progress` est étranglé en amont, à raison d'un événement pour
  mille lignes (CA-009-11).

### Sécurité — jalon 009

- **Le sélecteur de fichier natif s'ouvre côté Rust**, qui mémorise ce que
  l'opérateur a désigné et refuse tout autre chemin. Sans cela, la commande
  d'import acceptait un chemin arbitraire venu d'un frontend que CLAUDE.md §3
  déclare non fiable, et le contenu du fichier revenait dans les lignes
  rejetées. La fenêtre n'a plus **aucune** permission de dialogue ni de système
  de fichiers.

### Ajouté — jalon 008, accusés de livraison et journal métier (M2)

- **Lecture d'un `deliver_sm`** (`messaging::dlr`) : la distinction accusé /
  message entrant se fait sur `esm_class` et sur rien d'autre. Un corps qui
  ressemble à un accusé mais dont le `message_type` dit « message normal » est
  un SMS entrant, et le traiter autrement ferait passer un message à
  `DELIVERED` parce qu'un abonné a tapé les bonnes lettres.
- **Parsing tolérant du corps d'accusé** : casse des clés, trois orthographes de
  `submit date`, espaces multiples, ordre libre, champs absents, champs
  propriétaires non documentés. `text:` avale la fin de la ligne, deux-points
  compris. Rien n'échoue jamais : un corps illisible produit un accusé vide qui
  garde le texte brut (CA-008-03).
- **Les sept statuts de la spec §7.8** mappés vers les états internes
  (CA-008-05). `ACCEPTD` et `UNKNOWN` retombent sur `ACCEPTED` : ce sont les
  deux seuls qui ne disent rien de la livraison, et les mapper sur `DELIVERED`
  compterait un message que le destinataire n'a jamais vu.
- **Corrélation par la base, jamais par la mémoire** (`messaging::correlation`).
  Un accusé arrive après le `submit_sm_resp`, parfois après un redémarrage : la
  recherche passe par l'index de `smsc_message_id` du jalon 002.
- **Normalisation des identifiants** : la forme reçue est essayée en premier,
  puis la casse, les deux bases et la forme non paddée — la première cause
  d'accusés non corrélés en production (step-008 §6). Réglable par
  `IdMatching::Exact` pour un SMSC à identifiants opaques.
- **Journal des accusés orphelins** (table `dlr_orphans`) : un accusé qui ne
  corrèle à rien est conservé avec sa raison, son corps brut et son instant
  d'arrivée, et consultable dans l'écran Journaux (CA-008-04).
- **Écritures groupées** (`ReceiptPipeline`) : une transaction par lot de
  200 accusés ou par tranche de 250 ms, la borne de délai courant depuis le
  **premier** accusé du lot. Mille accusés font cinq transactions (CA-008-10),
  un accusé isolé est appliqué en 250 ms (CA-008-01).
- **Journal métier paginé** (`logging_export::journal`) : filtres session,
  campagne, état, plage de dates, préfixe de destinataire, code d'erreur et
  recherche plein texte. Le contenu des messages est **tronqué par défaut**
  (CLAUDE.md §8) ; l'option existe dans le modèle et le réglage utilisateur
  arrive au jalon 015, comme prévu.
- **Journal PDU activable** (`logging_export::pdu_log`), **désactivé par
  défaut** et sans constructeur qui l'allume (CA-008-09). C'est l'unique site
  d'appel de `DebugDumpAuthorisation::granted` de l'application. Les PDU y
  arrivent par `smpp_session::PduObserver`, un port **synchrone** que le lecteur
  et l'écrivain appellent pour chaque PDU — poignée de main comprise, c'est
  l'échange qu'on veut quand un bind est refusé. `src-tauri` l'implémente en
  poussant dans une file bornée qu'une tâche draine : attendre une écriture en
  base dans le lecteur ferait cadencer la session par l'interrupteur de
  débogage.
- **Commandes `logs_query`, `logs_orphans`, `logs_pdus`,
  `logs_set_pdu_logging`** et **écran Journaux** : table fenêtrée, filtres
  combinés, codes couleur par état — la couleur n'est jamais le seul signal, le
  code est écrit à côté — et panneau de détail au clic.
- **Le double SMSC émet des `deliver_sm`** (`smpp_session::testing`) : une file
  que le test remplit, donc du désordre, des doublons et des identifiants
  inconnus à volonté.

#### Ce que devient un message segmenté partiellement accusé

Un message découpé en trois segments reçoit **trois** identifiants du SMSC et,
si `registered_delivery` l'a demandé, **trois** accusés. La ligne `messages`
porte un seul `smsc_message_id`, celui du premier segment (jalon 006). Donc :

- l'accusé du segment 1 corrèle et pilote l'état du message ;
- les accusés des segments 2..n ne corrèlent pas et sont journalisés comme
  orphelins, avec la raison `UNKNOWN_ID`.

C'est une **limite assumée de ce jalon**, pas un oubli. Corréler chaque segment
demande une table d'identifiants par segment — donc une migration et une
modification du chemin d'émission — alors que step-008 §2 borne la corrélation
à « par `smsc_message_id`, l'index de step-002 », c'est-à-dire à cette colonne.
Rien n'est perdu : les accusés supplémentaires sont dans le journal des
orphelins avec leur `stat`, et un opérateur voit que les segments 2 et 3 ont
été livrés même si l'état de la ligne vient du segment 1.

#### Ce qu'un accusé ne peut pas faire

Les deux barrières du jalon 006 sont traversées, jamais contournées :

- une ligne `FAILED` partielle ne porte **aucun** `smsc_message_id`, donc
  l'accusé de son fragment accepté ne trouve rien et devient un orphelin —
  c'est le résultat voulu, pas un raté ;
- la machine d'états refuse `FAILED → DELIVERED`, et le refus est dans la
  clause `WHERE` de l'`UPDATE`. La corrélation émet la transition que l'accusé
  réclame et laisse la base la refuser ; elle ne lit jamais l'état courant pour
  décider elle-même, ce qui serait une course entre deux tâches.

Un accusé redélivré — le même, relu sur la socket — ne change ni l'état, ni le
`stat`, ni le code d'erreur, ni le compteur de tentatives. Il rafraîchit
`dlr_at`, qui est « quand cette application a eu l'accusé en main pour la
dernière fois » et non « quand le combiné a reçu le message » : le `done date`
du SMSC est la seconde chose, il est invérifiable, et il est conservé sur
l'accusé plutôt que dans cette colonne.

#### Limite connue — un accusé précoce devient un orphelin définitif

Le `sender` n'écrit `smsc_message_id` qu'au `update_states` final, après le
dernier `submit_sm_resp`. Un SMSC qui pousse son `deliver_sm` avant que ce
commit ait eu lieu — verrou SQLite tenu, disque lent, centre qui livre en
quelques millisecondes — rencontre un journal qui ne connaît pas encore
l'identifiant. L'accusé part en orphelin et **n'est jamais retenté** : le
message reste `ACCEPTED` pour toujours, alors que son accusé est là, dans le
journal des orphelins, avec l'identifiant qui aurait correspondu.

Non corrigé ici. Le moins cher serait un balayage périodique de `dlr_orphans`
retentant la corrélation — la table existe et porte déjà l'identifiant — mais
un balayage est une tâche planifiée avec une interaction de rétention, ce qui
appartient au jalon 014 plutôt qu'à un ajout ici.

Ce qui n'est **pas** une issue : écrire `smsc_message_id` plus tôt. Cela le
mettrait au journal avant que l'envoi soit connu comme abouti, c'est-à-dire
l'ordre write-ahead de CLAUDE.md §4 pris à l'envers.

### Corrigé — jalon 008

- **Boucle infinie dans le parseur d'accusés.** Le scanner testait
  `is_ascii_whitespace` sur l'octet de tête et sautait les jetons avec
  `char::is_whitespace`. Les deux divergent sur U+0085 et U+00A0 — deux octets
  d'un corps Latin-1 ordinaire — et le curseur se réassignait sa propre valeur :
  une boucle infinie dans la tâche qui tient la file de livraison, sur une
  entrée que le SMSC choisit. Trouvé par le test de propriété, corrigé, avec un
  test de non-régression.
- **Filtre par préfixe de destinataire sans résultat.** `Msisdn` stocke les
  chiffres seuls, sans `+`, donc un `LIKE '+225%'` littéral ne correspondait à
  rien. Pas une erreur, pas un état vide qu'on questionne : un écran qui dit
  silencieusement qu'il n'y a aucun message. Le `+` de tête est retiré à la
  construction du filtre.
- **`.gitignore` avalait l'écran Journaux.** La règle `logs/` n'était pas
  ancrée, donc elle attrapait `ui/src/views/Logs/` sur un système de fichiers
  insensible à la casse. L'écran entier était invisible pour git, et un
  `git add -A` l'aurait commité vide. Ancrée à la racine.
- **La conversion de base d'identifiant créditait le mauvais message.** Le
  défaut relisait l'identifiant d'un accusé dans l'autre base et prenait le
  premier candidat trouvé ; comme le chemin « identifiant introuvable » est le
  chemin nominal — un orphelin par segment supplémentaire — un accusé pour un
  identifiant inconnu atterrissait de façon fiable sur un message sans rapport.
  La relecture de base est désormais opt-in par profil (`dlr_id_matching`), et
  le défaut ne tente plus que des différences d'orthographe, qui sont sans
  perte.
- **L'interface annonçait des transitions que la base avait refusées.** La
  barrière `FAILED → DELIVERED` protège le journal, pas l'événement : un
  UNDELIV tardif laissait la ligne à DELIVERED et affichait FAILED.
  `update_states` remonte maintenant le nombre de transitions écrites, et seules
  celles-là sont annoncées.
- **Le corps brut d'un accusé orphelin était tronqué à 24 caractères**, ce qui
  amputait le seul diagnostic disponible sur un accusé qui n'a rien corrélé.
- **`find_message_by_smsc_id` ignorait la session.** `smsc_message_id` n'est
  unique que par centre : deux fournisseurs séquentiels attribuent tous deux
  « 1234 », et la ligne la plus ancienne gagnait.
- **Le filtre de recherche ne trouvait pas un numéro collé avec son `+`**, même
  cause que le préfixe. Le retrait est conditionnel, pour ne pas casser la
  recherche d'un `+` dans un corps de message.
- **Vingt-cinq clés i18n demandées par des composants n'existaient pas** — toute
  la racine `metrics` du jalon 007, plus cinq clés du formulaire de profil. Le
  tableau de bord affichait `metrics.throughput` en toutes lettres. Trouvé en
  lançant l'application ; le correctif durable est le test qui balaie les
  sources pour les `t("…")` littéraux et vérifie que chacun se résout.
- **`messaging` ne déclarait pas la fonctionnalité `macros` de Tokio**, alors
  que la boucle de lots utilise `tokio::select!`. La crate ne compilait que
  parce qu'une dev-dépendance activait la fonctionnalité — donc
  `cargo check --workspace --all-targets` restait vert et
  `cargo check -p logging-export` non.

### Modifié — jalon 008

- **`message:update` porte un lot** (`{ updates: [...] }`) au lieu d'un seul
  message. **Changement cassant du contrat IPC.** Un SMSC qui rejoue un arriéré
  produit des milliers de transitions par seconde, et un événement chacun est
  ce que CA-008-08 interdit : le volume passe par `logs_query` paginée et
  l'événement ne porte que des incréments agrégés. Le lot est celui que le
  pipeline commet déjà, donc l'événement décrit exactement une transaction.
- **`MessageFilter`** gagne la plage de dates, le préfixe de destinataire, le
  code d'erreur DLR et la recherche plein texte. Aucun n'est indexé, donc tous
  prennent la forme `(? IS NULL OR …)` — ce qui ne coûte rien ici et garde
  quatre littéraux SQL au lieu de soixante-quatre.
- **`PduLogRepository`** gagne `insert_entries` (une transaction pour le lot) et
  `page_entries` renvoie un `StoredPduEntry` porteur de son identifiant.
- **La file de livraison d'une session n'est plus drainée et jetée** : le
  placeholder du jalon 005 est remplacé par le pipeline d'accusés.

### Décisions — jalon 008

- **ADR 0011 — virtualisation de la table des journaux.**
  `@tanstack/react-virtual` a été intégré puis retiré : le compilateur React
  refuse de compiler un composant qui l'utilise, et sous Vitest + jsdom il ne se
  réaffiche jamais après le montage, donc CA-008-07 n'était pas testable du
  tout. La fenêtre est une fonction **pure** de trente lignes, vérifiée sur
  100, 200 000 et 2 000 000 de lignes. Écart signalé par rapport à CLAUDE.md §2
  et step-008 §2.

### Ajouté — jalon 006, envoi simple de bout en bout (M1)

- **Orchestrateur d'envoi unitaire** (`messaging::sender`) : valide → encode →
  segmente → construit → **persiste** → émet → corrèle → enregistre. L'ordre
  est porteur dans les deux sens : tout ce qui peut être refusé l'est **avant**
  l'insertion, donc rien n'est persisté qui ne pouvait pas partir ; et rien ne
  quitte la socket avant que l'insertion soit validée, ce qui rend un arrêt
  brutal reprenable au jalon 010 sans duplication.
- **Validation et normalisation des adresses** (`messaging::addressing`) :
  destinataire E.164, expéditeur numérique ou alphanumérique ≤ 11 caractères
  imposant `source_addr_ton = 5`, signalé dans l'interface plutôt que découvert
  dans un rejet.
- **Construction complète du `submit_sm`** (`messaging::submit`) : les seize
  champs de la spec §7.3, tous réglables depuis l'interface, plus les TLV
  personnalisés. `registered_delivery = 1` par défaut (spec §23.3).
- **Machine d'états du message** (`messaging::message`) : `QUEUED → SENT →
  ACCEPTED | FAILED`, rejeu d'une transition autorisé, retour en arrière et
  sortie d'un état terminal refusés.
- **Commandes IPC `message_send` et `message_preview`**, événement
  `message:update`. Le compteur de l'éditeur passe par le backend et appelle la
  **même** fonction que le segmenteur : compteur et segmentation coïncident par
  construction, pas par coïncidence.
- **Écran Envoi › Simple** : sélecteurs TON/NPI/DCS documentés (l'octet est
  affiché à côté du libellé), compteur en direct, éditeur de TLV, et le
  `command_status` du SMSC affiché tel quel — valeur, symbole et libellé
  (ENF-UTI-02).
- **Micro-benchmark d'enfilement** (`cargo bench -p messaging --bench
  enqueue`) : 0,8 µs pour un segment, 10,5 µs pour sept segments avec TLV, très
  au-dessous du plafond d'une milliseconde d'ENF-PERF-02. Ce que la mesure
  exclut est écrit dans son en-tête.

#### Sémantique d'un message segmenté en échec partiel

Deux segments acceptés et un rejeté font un message **`FAILED`**, et les
segments suivants ne sont **pas** émis.

Le motif est ce que voit le destinataire : un combiné ne réassemble un message
concaténé qu'une fois toutes ses parties reçues, et n'affiche rien tant qu'il en
manque une. Le message écrit par l'opérateur n'a donc pas été délivré, et le
compter comme accepté gonflerait toutes les statistiques du jalon 014. Émettre
la suite d'un message dont le milieu a été refusé produit des parties que le
combiné ne pourra jamais assembler, et consomme du quota pour cela.

Un état `PARTIAL` a été envisagé et écarté : `messages.state` porte une
contrainte `CHECK` listant les six états de la spec §14.3, un septième serait
une migration et une modification de tout écran qui groupe par état — pour une
distinction que l'opérateur lit déjà, segment par segment, dans le résultat.

Le message ne conserve **aucun** `smsc_message_id`, alors même que ses segments
acceptés en ont chacun reçu un. Garder celui du premier armerait un bug trois
jalons plus loin : le SMSC tentera de délivrer ce fragment et enverra un accusé
pour lui, le jalon 008 corrèle un accusé par `find_message_by_smsc_id`,
trouverait cette ligne et la ferait passer `FAILED → DELIVERED`. Les
identifiants ne sont pas perdus : chacun reste sur son segment, dans le
résultat.

**Ce qui n'est pas fait :** le fragment déjà accepté n'est ni annulé ni tracé en
base. Renvoyer le message produira donc un **deuxième exemplaire du segment 1**
chez le SMSC. Annuler un segment déjà soumis demande `cancel_sm`, qu'aucun jalon
n'a inscrit à son périmètre.

### Corrigé — jalon 006

- **Un échec du journal *après* l'émission faisait renvoyer le message.**
  L'erreur des transitions finales remontait comme celle de l'insert
  write-ahead, sur le même code `MESSAGE_STORAGE`, dont la traduction affirme
  « rien n'a été envoyé ». Trois segments partis et acceptés, base verrouillée
  au commit : l'opérateur, informé que rien n'était parti, renvoyait. Un échec
  postérieur à l'émission est désormais porté par `journalled: false` sur un
  résultat **réussi**, et l'interface dit de ne pas renvoyer.
- **La machine d'états était décorative.** `MessageState::can_move_to` n'avait
  aucun appelant : `UPDATE messages SET state = ?` s'appliquait sans condition.
  Un accusé arrivant pour un message déjà `FAILED` le faisait passer
  `DELIVERED` — jamais vu par le destinataire, compté comme délivré — et un lot
  `[SENT, ACCEPTED]` rejoué rétrogradait une ligne `DELIVERED`. La clause
  `WHERE` porte maintenant les états qui peuvent légalement précéder celui
  qu'on écrit.
- **`sent_at` et `attempts` étaient écrits pour un message jamais émis.** Un
  `submit_sm` refusé par la session — bind receiver, session tombée — est
  refusé *avant* la socket, mais la transition `SENT` partait quand même. Le
  journal aurait affiché une date d'émission pour un message qui n'est jamais
  parti et le budget de rejeu de la spec §10.7 aurait consommé une tentative.
- **Un `FAILED` partiel gardait l'identifiant d'un fragment accepté** — voir la
  section sur l'échec partiel ci-dessus.
- **L'aperçu n'avait pas de garde d'obsolescence.** Deux réponses dans le
  désordre figeaient un compteur décrivant un texte déjà dépassé, ce que
  CA-006-09 interdit.
- **`priority_flag` n'était borné nulle part côté Rust**, alors que la WebView
  est traitée comme non fiable.
- **`esm_class` valait `0x08` sur tout message ordinaire.**
  `EsmClass::default()` de rusmpp n'est pas nul : son champ `ansi41_specific` a
  pour défaut « short message contains delivery acknowledgement ». Chaque
  `submit_sm` s'annonçait donc comme un accusé ANSI-41 plutôt que comme un
  message. Le défaut a traversé le jalon 004 parce que ses tests s'écrivaient
  « le bit UDHI est posé, donc l'octet n'est pas nul » — vrai, mais pour la
  mauvaise raison, l'octet n'étant jamais nul. Le mode `sar`, où il doit valoir
  exactement zéro, l'a fait sortir. Trois tests assertent désormais l'octet
  exact.

### Modifié — jalon 006

- **Le port `MessageRepository` est rapatrié dans `messaging`**, échéance que
  l'ADR 0007 s'était fixée. `persistence` l'implémente ; la moitié
  pagination/streaming devient `persistence::ports::MessageJournal` et attend
  son consommateur au jalon 013.
- **`messaging` ne dépend plus que de `smpp-core`.** Le déplacement fermait la
  boucle `messaging → smpp-session → persistence → messaging` ; l'arête vers
  `smpp-session` est remplacée par le port `SmscSession`, que `smpp-session`
  implémente. L'orchestrateur se teste sans base et sans socket.
- `Timestamp` et `CampaignId` descendent dans `smpp-core` — les deux crates les
  utilisent maintenant. `persistence` les ré-exporte, aucun site d'appel n'a
  changé.
- Le double de SMSC en mémoire du jalon 005 passe de `tests/support/` à
  `smpp_session::testing`, derrière la feature `test-support`, et gagne le
  scénario des réponses `submit_sm`. Les tests d'envoi bout en bout s'en
  servent au lieu d'en écrire un second.
- Les tests bout en bout du chemin d'envoi vivent dans
  `crates/smpp-session/tests/`, non dans `messaging`. Les y laisser demandait
  une dev-dépendance `messaging → smpp-session`, donc un cycle — que Cargo
  tolère pour un dev-kind, mais que CLAUDE.md §3 interdit sans distinguer les
  kinds. `messaging` n'atteint plus que `smpp-core`, en normal comme en dev.

### Décisions — jalon 006

- [ADR 0010](docs/adr/0010-inversion-des-ports-du-chemin-d-envoi.md) : inverser
  **les deux** ports du chemin d'envoi plutôt qu'un seul. L'arête remontante
  n'est pas un coût que l'inversion fait payer par accident, c'est ce qu'est
  l'inversion ; et n'en inverser qu'un fermait un cycle. L'ADR porte aussi le
  tableau des trois ports restants avec leur échéance.

### Ajouté — jalon 001, socle applicatif

- **Contrat IPC typé** : DTO définis une seule fois en Rust, TypeScript généré
  par tauri-specta dans `ui/src/ipc/generated/`. L'étape 4 de la CI régénère
  et compare à chaque run — le cliquet du jalon 000 a basculé tout seul.
- **DTO d'erreur `{ code, message, details }`** avec un `code` stable et
  machine-lisible. Un test empoisonne chaque variante avec un chemin absolu et
  un mot de passe, puis balaie le message et `details` : la garantie de
  non-fuite est structurelle, pas une question de soin.
- **Coquille applicative** : navigation vers les huit écrans, thèmes clair,
  sombre et système, i18n FR/EN, notifications d'erreur.
- **Commandes témoins `config_get` / `config_set`**, préférences persistées
  dans le répertoire de données standard de l'OS et nulle part ailleurs.
- **Journalisation `tracing`** vers un fichier rotatif du répertoire de logs.

### Corrigé

- `default-run` manquait : l'ajout du binaire `gen_ipc` rendait `cargo run`
  ambigu et `tauri dev` ne démarrait plus. Aucun test ne l'attrapait, seul le
  lancement de l'application le révèle.
- Une rejection non conforme au DTO — la chaîne nue que Tauri renvoie quand il
  ne sait pas désérialiser les arguments — était étiquetée « backend », ce qui
  produisait un toast sans code ni message.
- Un échec d'abonnement à `error:notify` faisait sombrer tout le pont, y
  compris la lecture des préférences.
- L'écriture des préférences bloquait un worker Tokio, garde du verrou en
  main.

### Ajouté — jalon 003, cœur protocolaire

- **Codec PDU SMPP v3.4 et v5.0** : `encode`/`decode` d'un PDU complet, les
  33 opérations de la spec §7.2 couvertes et vérifiées par un test qui échoue
  si le codec en connaît une que la liste ignore.
- **Frontière du PDU décidée depuis `command_length`**, et non depuis ce que
  le codec laisse dans le tampon : un TLV vendeur appendu à un `bind` est
  reconnu comme interne au PDU, et le framing du jalon 005 ne peut plus se
  désynchroniser en silence.
- **Table des `command_status`** exhaustive v3.4/v5.0 — valeur, nom
  symbolique, libellés FR et EN, classification Fatal / Récupérable /
  Throttling. Un statut inconnu retombe sur `Fatal`, le choix conservateur.
- **Newtypes du domaine** (`Msisdn`, `SessionId`, `ClientMessageId`,
  `SequenceNumber`) en *parse, don't validate* : champ privé, validation dans
  le constructeur, invalidité irreprésentable en aval.
- **`debug::redacted`** — la voie sanctionnée pour journaliser une commande.
  Le `derive(Debug)` des types réexportés laisse fuir le mot de passe SMSC ;
  un test l'assert dans les deux sens.
- **Job CI `msrv`** : la version minimale de Rust est calculée depuis le
  graphe complet et vérifiée par une toolchain épinglée. Elle était déclarée
  à 1.78 depuis le jalon 000, valeur jamais vraie et que rien ne vérifiait.

### Ajouté — jalon 002, persistance SQLite

- **Base SQLite en mode WAL** : `Database::open` applique `journal_mode=WAL`,
  `synchronous=NORMAL`, `busy_timeout` et `foreign_keys` par les options de
  connexion du pool, donc sur **chaque** connexion. Un test lit les pragmas
  huit fois de suite pour le prouver : WAL est une demande que SQLite peut
  décliner en silence.
- **Schéma de la spec §14.2** dans une migration réversible unique, embarquée
  dans le binaire par `sqlx::migrate!()` — une application packagée n'a aucun
  fichier à emporter à côté.
- **Cinq repositories typés** (messages, contacts, campagnes, profils de
  session, journal PDU) derrière autant de traits de port. Pagination **par
  curseur**, jamais par `OFFSET`, qui reparcourt les lignes qu'il saute ;
  parcours en flux dont la mémoire ne grandit pas avec le nombre de lignes,
  mesuré sur 100 000 messages avec un allocateur compteur.
- **Écritures groupées en une transaction** : un lot de N transitions d'état
  produit un commit, pas N. La propriété est vérifiée par son observable —
  l'atomicité — puisque SQLite n'expose aucun compteur de transactions.
- **Aucun SQL hors de `persistence`** : le pool est `pub(crate)`, donc la règle
  est tenue par le compilateur et non par la revue. Toutes les requêtes passent
  par `query!`/`query_as!` et le cache `.sqlx/` est commité, ce qui rend la
  compilation possible sans base accessible.
- **Étape 8 de la CI réellement vérifiante** : migrations appliquées sur base
  neuve, réappliquées pour l'idempotence et la validation des empreintes, puis
  cache `.sqlx` comparé au schéma obtenu. Les empreintes SHA-256 des migrations
  livrées sont aussi épinglées dans un test — `sqlx` ne détecte une migration
  éditée que sur une base déjà migrée, jamais sur un clone neuf.

### Corrigé — revue du jalon 002

- **Les index de `messages` étaient inatteignables.** Écrite
  `(? IS NULL OR colonne = ?)` pour tenir en un seul littéral vérifié à la
  compilation, la clause interdisait à SQLite d'utiliser un index :
  `count_messages` scannait la table entière et la pagination par curseur
  reparcourait depuis le curseur à chaque page — le coût linéaire qu'elle
  existe pour supprimer. Les colonnes indexées sont maintenant discriminées
  côté Rust vers des requêtes littérales distinctes. Un test compare
  désormais les **plans**, pas seulement les résultats, en lisant les requêtes
  depuis `.sqlx/` pour qu'il ne puisse pas dériver.
- **Une transition rejouée comptait une tentative de trop.** `attempts` était
  incrémenté ; un lot committé puis réappliqué après une coupure amputait en
  silence le budget de rejeu. Devient `MAX(attempts, ?)` avec un numéro de
  tentative explicite. Le test de rejeu passait à côté : il empruntait le seul
  constructeur qui ne touche aucun compteur.
- **`smsc_message_id` ne pouvait plus être corrigé une fois écrit.** Sur un
  réenvoi après timeout le SMSC attribue un nouvel identifiant ; la fusion
  refusait de l'écrire par-dessus l'ancien, et le DLR du second envoi
  n'aurait jamais corrélé. Le champ devient un `Keep`/`Set` explicite.
- **La mesure mémoire de CA-002-05 ne pouvait pas échouer** : elle lisait des
  compteurs cumulés après coup, donc mesurait « le résultat s'échappe-t-il »
  et non « est-il matérialisé ». Prise flux vivant désormais, et vérifiée par
  mutation.
- **CA-002-06 est compté, plus seulement déduit** : l'atomicité ne réfute pas
  une implémentation qui committe ligne par ligne après validation. Le volume
  écrit dans le journal WAL, lui, compte les commits. `PRAGMA data_version` a
  été essayé et écarté : SQLite ne promet qu'une différence, pas un compte.
- **L'étape 8 de la CI n'avait jamais tourné sous Windows** : `mktemp -d`
  renvoie un chemin MSYS que Git-bash ne traduit pas à l'intérieur d'une URL.
- **`sqlx` émettait un `WARN` portant le SQL entier** à chaque parcours long.

### Modifié

- **MSRV portée de 1.93 à 1.94** : `sqlx` 0.9 la déclare et 1.93 refuse la
  compilation. Le processus de l'[ADR 0006](docs/adr/0006-version-minimale-de-rust.md)
  a fonctionné — plancher lu dans le graphe, puis vérifié en compilant. Le job
  CI `msrv` lit la valeur depuis `Cargo.toml` et n'a rien demandé.

### Décisions

- [ADR 0007](docs/adr/0007-emplacement-des-traits-de-port.md) : les traits de
  port vivent dans `persistence` jusqu'à ce que `messaging` et `contacts`
  existent. L'inversion de dépendance n'a de valeur que pour un consommateur
  qui l'utilise ; en payer le coût — l'arête remontante — face à une crate vide
  imite la forme du principe sans en obtenir le bénéfice.
- [ADR 0001](docs/adr/0001-choix-de-la-pile-smpp.md) passe en **Accepté** :
  niveau d'API rusmpp tranché au **niveau bas** (`CommandCodec`), `rusmppc`
  écarté. Le critère décisif est la propriété de la corrélation des
  `sequence_number`, dont les jalons 005, 007 et 012 ont besoin.
- [ADR 0006](docs/adr/0006-version-minimale-de-rust.md) : MSRV portée à
  **1.88**, calculée depuis le graphe et vérifiée en CI.

### Ajouté — jalon 005, session SMPP unique

- **Machine à états de la spec §7.9** dont les arêtes sont un `Result` : une
  transition que le diagramme ne dessine pas ne compile pas en silence, elle
  échoue. `ERROR → CONNECTING` n'existe pas — la garantie de CA-005-03 est
  structurelle et non confiée au superviseur.
- **Acteurs Tokio** : le superviseur possède la moitié écriture du socket et
  fait aussi writer, keep-alive et faucheur ; le lecteur, seule tâche qui
  bloque vraiment, est la seule tâche engendrée par connexion. Toutes les
  files sont **bornées**, aucune tâche n'est détachée, et `shutdown()` joint
  ce qu'il a lancé.
- **Bind TX / RX / TRX** avec choix explicite de `interface_version`
  (0x34 / 0x50). Émettre sur une session RX est refusé **avant** l'émission,
  par un type, et non par un `ESME_RINVBNDSTS` du SMSC.
- **Corrélation `sequence_number` → réponse** : un numéro en vol n'est jamais
  réattribué, et une entrée quitte la table à son échéance que quelqu'un
  écoute encore ou non. Pas de `Drop` — un `Drop` ne peut pas `await` le
  verrou dont il aurait besoin.
- **Keep-alive `enquire_link`** à période configurable, et détection *effective*
  de la session morte : deux réponses manquantes consécutives font transiter
  vers `RECONNECT` alors que le socket TCP reste parfaitement ouvert.
- **Reconnexion** à back-off exponentiel plafonné avec *equal jitter*
  (`[base/2, base]`). Le full jitter disperse mieux mais détruit la
  croissance. Un statut classé `Fatal` au jalon 003 — `ESME_RINVPASWD`,
  `ESME_RINVSYSID` — n'ouvre **aucune** boucle : la classification produite
  alors est ici réellement utilisée.
- **Codec de session** propre à `smpp-session` plutôt que `CommandCodec` monté
  tel quel : la frontière de trame est décidée depuis `command_length` et les
  octets sont passés à `smpp_core::codec::decode`. Un PDU malformé devient un
  *élément* `Err` et non une erreur de flux — `generic_nack`, journal, et le
  PDU suivant se lit sur un tampon toujours aligné.
- **Arrêt propre** : `unbind`, attente bornée de `unbind_resp`, fermeture ; à
  la sortie de l'application aussi, sur `ExitRequested`.
- **Sept commandes IPC** (`session_create`, `session_update`, `session_delete`,
  `session_list`, `session_bind`, `session_unbind`, `session_status`) et
  l'événement `sessions:state`. **Le DTO de profil n'a pas de champ mot de
  passe** : le mot de passe n'arrive qu'avec `session_bind` et ne repart ni
  vers la base ni vers le pont.
- **Écran Sessions** : création et édition de profil, bind, unbind, état en
  direct, et le motif d'un abandon définitif traduit depuis un code stable.
- **Double de test** : un SMSC en mémoire sur `tokio::io::duplex`, avec son
  propre codec, et sept scénarios de panne. Toute la recette tourne sur
  l'horloge virtuelle de Tokio.

### Ajouté — jalon 005, l'alt-charset (ADR 0009)

- **`Gsm7BitCharset` tranche la dette laissée ouverte par l'ADR 0008** :
  l'interprétation des octets GSM non packés. Défaut `Gsm0338` (`@` = 0x00) ;
  `Latin1` (`@` = 0x40) pour les SMSC configurés à la Kannel. C'est un réglage
  de session, persisté, exposé par l'IPC et réglable dans l'écran Sessions.
- Sous `Latin1` l'alphabet est l'**intersection** GSM 03.38 ∩ ISO-8859-1 : la
  table d'extension en sort, parce que le SMSC la développe en deux septets
  alors qu'elle n'occupe qu'un octet ici, et le budget de segment se compte en
  septets. Les capitales grecques en sortent faute de point de code.
- `Latin1` + `Packed` est refusé à la construction du profil : `é` vaut 0xE9
  et le packing jette le bit de poids fort.
- Les tests sont écrits sur `@ £ $ € é` et les accents. Un test nommé épingle
  le fait qu'un texte ASCII traverse les deux lectures à l'identique — c'est
  le piège, et il devait rester visible dans la suite.
- `Gsm7BitPacking` descend de `messaging` vers `smpp-core` pour y rejoindre
  `Gsm7BitCharset` : le profil de session les porte et il vit sous
  `messaging`. Ni l'un ni l'autre n'est `#[non_exhaustive]` — une troisième
  convention de disposition doit casser la compilation partout où des octets
  sont écrits.

### Corrigé — revue du jalon 005

Cinq défauts d'une même famille — un `await` que rien ne borne, un état que
rien ne remet à zéro — plus sept points mineurs.

- **Le keep-alive ne détectait plus un lien mort dès que la période était
  plus courte que le délai de réponse.** Le tick écrasait un `enquire_link`
  encore en vol : l'ancien waiter était droppé, son entrée balayée en
  silence parce que le récepteur avait disparu, et le compteur de réponses
  manquées restait à zéro pour toujours. Un SMSC devenu trou noir, socket
  ouvert, laissait la session `BOUND` indéfiniment. Le profil refuse
  désormais `response_timeout_s >= enquire_link_s` — les deux bornes étaient
  validées séparément, jamais leur relation — et le superviseur compte le
  tick comme une réponse manquée si un waiter est encore là.
- **`sessions:state` pouvait perdre définitivement un état.** Le throttle
  jetait l'émission refusée sans rien pour la rejouer ; l'état d'une session
  saine cesse de changer une fois `BOUND` et le frontend ne fait aucun
  polling, donc l'émission supprimée était souvent la dernière. Contre un
  SMSC local la poignée de main tient en quelques millisecondes, et l'écran
  affichait `CONNECTING` en permanence sur une session bindée. Le rythme
  descend dans le forwarder, qui dort puis **relit** le registre : un
  throttle chez l'émetteur ne peut pas faire ça, il a déjà reçu une charge
  utile en train de devenir périmée.
- **L'écriture sur le socket n'écoutait ni le jeton d'annulation ni une
  échéance.** Un SMSC qui cesse de lire sans fermer parquait le superviseur,
  qui ne revoyait plus son `select!` : `unbind` prenait le verrou du
  registre, attendait un `JoinHandle` qui ne finissait jamais, toutes les
  commandes de session se bloquaient derrière, et la fermeture de
  l'application — un `block_on` sur le thread principal — gelait la fenêtre.
- **Interblocage lecteur ↔ superviseur au shutdown.** Le lecteur se parquait
  sur une file pleine que plus personne ne drainait, sur un canal qui ne
  pouvait pas se fermer puisque le superviseur en détient un `Sender`. Il
  écoute maintenant le jeton pendant qu'il met en file, et le join est borné.
- **Le compteur de tentatives ne repartait jamais de zéro.** Six échecs au
  démarrage laissaient le back-off à six : la première micro-coupure après
  une journée saine attendait le plafond au lieu d'une seconde, et chaque
  coupure suivante coûtait une minute d'indisponibilité.
- **Les PDU en file survivaient à leur connexion** — écrits sur la suivante
  avec un `sequence_number` dont l'entrée de corrélation avait disparu.
- **`GiveUp(FatalStatus)` était rendu pour des erreurs qui ne sont pas un
  refus du SMSC**, et l'interface disait « vérifiez les identifiants » pour
  une session simplement terminée. Troisième motif, `SESSION_ENDED`.
- **`messaging::segment` acceptait `Latin1` + `Packed`**, l'invariant de
  l'ADR 0009 §7 n'étant appliqué qu'au profil.
- **Pas de timeout sur la poignée de main TCP** : un hôte qui absorbe les SYN
  bloquait deux minutes sans que le back-off s'engage.
- **Une poignée de session abandonnée fuyait deux tâches** ; `Drop` annule
  désormais le jeton.
- **`error!` sur un chemin normal** — annulation avant la première connexion.
- **`SessionBindInput` exposait un mot de passe nu sous `derive(Debug)`.**

Deux tests passaient **grâce** aux défauts : celui du keep-alive utilisait le
seul ordre de paramètres qui le masque, et celui du back-off ne voyait
croître les intervalles que parce que le compteur ne se remettait jamais à
zéro — son double acceptait puis coupait à chaque fois, donc chaque tentative
était en réalité un succès.

### Ajouté — jalon 004, encodage et segmentation

- **Alphabet GSM 03.38**, table de base et table d'extension, avec détection
  automatique de l'encodage (spec §7.5) et forçage manuel GSM 7-bit /
  Latin-1 / UCS2. Un forçage impossible retourne une erreur nommant le
  caractère et sa position, jamais un message corrompu.
- **Encodeurs** : GSM 7-bit, Latin-1, UCS2 en UTF-16BE. La disposition des
  septets GSM dans `short_message` est **configurable par session** —
  `unpacked` (défaut) ou `packed`. GSM 03.38 §6.1.2.1.1 décrit le format
  radio, pas le contenu du champ : le parc réel attend un septet par octet et
  laisse le SMSC packer. Se tromper est silencieux — `ESME_ROK`, DLR
  `DELIVRD`, charabia sur le combiné.
- Les budgets de la spec §7.5 sont des **septets**, pas des caractères : 160
  et 153 valent dans les deux dispositions, et un texte de 153 caractères
  contenant un `€` fait 154 septets.
- **Bourrage `CR`** des sept bits libres d'un dernier octet packé, suivant
  TS 23.038 §6.1.2.3.1 — sans lui le téléphone affiche un `@` fantôme.
- **Segmenteur** UDH (IEI 0x00, six octets, bit UDHI), TLV `sar_*`, et
  `message_payload` jusqu'à 64 Ko. Le mode est fourni par l'appelant, jamais
  déduit du texte.
- **La coupure est décidée au caractère, jamais au septet.** Un caractère de
  la table d'extension coûte deux septets et ne peut pas être scindé : s'il
  ne rentre pas, il passe entier au segment suivant et le septet restant est
  perdu. Même règle pour les paires de substitution UCS2. Les tests balaient
  toutes les positions autour de la coupure — c'est le bug que rien d'autre
  n'attrape, puisque le nombre de segments reste juste et chaque segment reste
  bien formé.
- **API de prévisualisation** pour le compteur en direct de l'éditeur, sans
  aucune allocation. Elle partage son remplisseur avec la segmentation
  réelle : leur accord est structurel, pas vérifié après coup.
- **Réassemblage** des segments, dans n'importe quel ordre, qui refuse un
  corps finissant sur un échappement orphelin ou une demi-paire de
  substitution.
- **Propriétés `proptest`** : aller-retour d'encodage, restitution du message
  par concaténation inverse pour les trois modes, accord prévision ⇄
  segmentation, croissance monotone du nombre de segments.
- **Banc `criterion`** de référence pour le jalon 017.

### Corrigé

- **Retour à la ligne parasite au milieu d'un message packé.** Un segment
  ramené à 152 septets par la règle de la paire d'échappement occupe les mêmes
  134 octets que 153 ; un récepteur qui ne dispose que de `sm_length` divise,
  lit 153 et trouve le `CR` de bourrage. Le remplisseur refuse désormais de
  fermer un segment **non final** sur un compte que le récepteur ne
  retrouverait pas, et rend son dernier caractère au suivant. Le segment final
  n'est pas réparable — c'est le cas que TS 23.038 §6.1.2.3.1 couvre, et le
  réassembleur retire le bourrage.
- **Le réassembleur recalcule le nombre de septets depuis la longueur en
  octets** au lieu de le lire dans la structure du segment. Un récepteur n'a
  que `sm_length` ; l'ancien oracle était structurellement aveugle à cette
  classe d'erreurs, ce qui est précisément pourquoi aucun test ne la voyait.
- **Le compteur de référence de concaténation est amorcé aléatoirement.** Un
  départ à zéro donne, après un redémarrage, la référence de segments encore
  en vol au message suivant, et le combiné les fusionne.
- Le jalon 003 réexportait `EsmClass` sans les types de ses quatre champs, et
  laissait `MessagePayload` sous un chemin que la liste curatée ne couvrait
  pas : l'alternative `message_payload` de la spec §7.5 était
  inconstructible depuis l'extérieur de `smpp-core` et le bit UDHI n'était
  pas assertable. Le module `udhs` est exposé pour la même raison. Un test
  d'intégration tient désormais cette surface.

### Décisions

- [ADR 0008](docs/adr/0008-strategie-de-segmentation.md) : découpage au
  caractère avec un remplisseur glouton unique, budget de la spec §7.5 en
  septets appliqué quel que soit le mode — le mode `sar_*` renonce à 7 septets
  par segment parce que de nombreux SMSC retraduisent les TLV en UDH sur la
  branche de livraison. GSM 7-bit non packé par défaut, packé disponible par
  session. Référence de concaténation par compteur cyclique de session, amorcé
  aléatoirement.

  L'ADR consigne aussi un point **à trancher au jalon 005** : l'interprétation
  des octets non packés, valeurs GSM 03.38 ou valeurs Latin-1 transcodées par
  le SMSC (l'`alt-charset` de Kannel). Un texte purement ASCII traverse les
  deux à l'identique, donc les tests restent verts et seuls `@ £ $ €` et les
  accents se corrompent en production. C'est une caractéristique de session,
  au même titre que le packing.

### Ajouté

- **Workspace Cargo** avec les neuf crates métier squelettes (`smpp-core`,
  `smpp-session`, `rate-control`, `messaging`, `contacts`, `numbers-gen`,
  `persistence`, `logging-export`, `security`) et `src-tauri`, membre du même
  workspace. Les frontières de dépendance du guide §4.2 sont inscrites dans
  les manifestes : un import remontant est rejeté par cargo comme cycle.
- **Règles de code appliquées par la machine** via `[workspace.lints]` et
  `clippy.toml` : `unwrap`, `expect`, `panic`, `todo`, `println!`,
  `std::thread::sleep`, `std::sync::Mutex`, les casts tronquants et les items
  publics sans documentation deviennent des erreurs de compilation.
  `unsafe_code` est en `forbid`, inlevable localement. `unwrap` et `expect`
  restent permis sous `#[cfg(test)]`.
- **Coquille Tauri 2** avec CSP stricte, capacités réduites à `core:default`,
  et démarrage sans `.expect()` — l'erreur remonte par `anyhow` jusqu'à `main`.
- **Frontend** Vite, React 19, TypeScript strict (`strict`,
  `noUncheckedIndexedAccess`), Tailwind v4, vitest, i18n amorcé en FR/EN.
  ESLint interdit `any`, `console` et l'import de `@tauri-apps/*` hors de
  `ui/src/ipc/`.
- **Pipeline CI** multi-OS (`ubuntu-22.04`, `macos-latest`,
  `windows-latest`) : formatage, lint Rust et TypeScript, types IPC générés,
  tests, doctests, chaîne d'approvisionnement, migrations, et paquets natifs
  sur `main` uniquement.
- **Pipeline release** déclenché par tag `v*`, avec un job de garde qui refuse
  d'écraser une release publiée et vérifie la concordance entre le tag et la
  version applicative.
- **Vérification des messages de commit** en local (hook husky) et en CI,
  Conventional Commits en français.
- **Contrôle de la chaîne d'approvisionnement** : `cargo audit` et
  `cargo deny` (licences, bannissements, provenance). `openssl` est banni —
  TLS exclusivement via rustls.
- **Scripts à cliquet** `check-ipc-types.sh` et `check-migrations.sh` : ils
  passent tant que le générateur et les migrations n'existent pas, mais
  échouent dès qu'un artefact apparaît sans son producteur.
- **Recettes `just`** : `fmt`, `fmt-check`, `lint`, `test`, `audit`, `check`,
  `dev`, `build`, `migrate`, `ipc-check`, `migrate-check`.
- **Documentation** : README, CONTRIBUTING, modèle de PR, CODEOWNERS, et les
  ADR 0001 à 0005.

### Notes

Ce jalon ne produit **aucune fonctionnalité SMPP**. L'application démarre sur
une page d'attente ; les crates métier sont des squelettes documentés.

Deux critères du jalon 000 restent à vérifier — CA-000-09 (démonstration
d'échec de la CI) et CA-000-10 (tag de test produisant une *draft release*) —
faute de portée distante accordée. Les protocoles sont décrits dans
[CONTRIBUTING.md](CONTRIBUTING.md#6-vérification-des-pipelines).

[Non publié]: https://github.com/martialanouman/smpp-client/commits/main
