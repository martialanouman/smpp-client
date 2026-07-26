# Changelog

Toutes les modifications notables de ce projet sont consignées ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le
versionnage suit [SemVer](https://semver.org/lang/fr/) : `feat` → mineur,
`fix` → correctif, changement cassant du contrat IPC ou du format de données →
majeur.

## [Non publié]

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

### Corrigé — jalon 006

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
  scénario des réponses `submit_sm`. `messaging` s'en sert au lieu d'en écrire
  un second.

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
