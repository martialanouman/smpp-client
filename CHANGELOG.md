# Changelog

Toutes les modifications notables de ce projet sont consignées ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le
versionnage suit [SemVer](https://semver.org/lang/fr/) : `feat` → mineur,
`fix` → correctif, changement cassant du contrat IPC ou du format de données →
majeur.

## [Non publié]

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
