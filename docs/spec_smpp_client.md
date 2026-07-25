# Spécifications Techniques — Client SMPP Multiplateforme

**Nom de code du projet :** *ShinobiSMPP* (Client SMPP GUI multiplateforme)
**Version du document :** 1.0
**Date :** 23 juillet 2026
**Statut :** Spécification de référence pour l'implémentation
**Pile technologique cible :** Tauri 2.x (backend Rust) + Frontend web (SPA)
**Protocoles cibles :** SMPP v3.4 (Issue 1.2) et SMPP v5.0

---

## Table des matières

1. [Introduction et périmètre](#1-introduction-et-périmètre)
2. [Glossaire et terminologie SMPP](#2-glossaire-et-terminologie-smpp)
3. [Exigences fonctionnelles](#3-exigences-fonctionnelles)
4. [Exigences non fonctionnelles](#4-exigences-non-fonctionnelles)
5. [Architecture générale](#5-architecture-générale)
6. [Pile technologique détaillée](#6-pile-technologique-détaillée)
7. [Cœur protocolaire SMPP (v3.4 et v5.0)](#7-cœur-protocolaire-smpp-v34-et-v50)
8. [Gestion des sessions multiples](#8-gestion-des-sessions-multiples)
9. [Contrôle de débit (throughput) et de congestion](#9-contrôle-de-débit-throughput-et-de-congestion)
10. [Envoi simple et envoi en masse (batch)](#10-envoi-simple-et-envoi-en-masse-batch)
11. [Gestion des contacts et import XLSX/CSV](#11-gestion-des-contacts-et-import-xlsxcsv)
12. [Génération automatique de numéros valides par pays](#12-génération-automatique-de-numéros-valides-par-pays)
13. [Journalisation des messages et export](#13-journalisation-des-messages-et-export)
14. [Modèle de données et persistance](#14-modèle-de-données-et-persistance)
15. [Interface applicative interne (commandes Tauri / IPC)](#15-interface-applicative-interne-commandes-tauri--ipc)
16. [Interface utilisateur (UI/UX)](#16-interface-utilisateur-uiux)
17. [Sécurité](#17-sécurité)
18. [Observabilité, métriques et supervision](#18-observabilité-métriques-et-supervision)
19. [Stratégie de tests et qualité](#19-stratégie-de-tests-et-qualité)
20. [Construction, packaging et déploiement multiplateforme](#20-construction-packaging-et-déploiement-multiplateforme)
21. [Structure du dépôt et organisation du code](#21-structure-du-dépôt-et-organisation-du-code)
22. [Feuille de route et jalons](#22-feuille-de-route-et-jalons)
23. [Annexes](#23-annexes)

---

## 1. Introduction et périmètre

### 1.1 Objet du document

Ce document constitue la spécification technique complète et exhaustive du client SMPP *ShinobiSMPP*. Il décrit l'ensemble des exigences fonctionnelles et non fonctionnelles, l'architecture logicielle, le modèle de données, les protocoles supportés, les algorithmes clés (contrôle de débit, génération de numéros), ainsi que les stratégies de test, de sécurité et de déploiement. Il sert de référence contractuelle pour l'implémentation, la revue de code et la recette.

### 1.2 Contexte

SMPP (*Short Message Peer-to-Peer*) est le protocole de référence de l'industrie des télécommunications pour l'échange de SMS entre une entité externe (ESME — *External Short Messaging Entity*) et un centre de messagerie (SMSC — *Short Message Service Centre*) ou un fournisseur/agrégateur SMS. L'application est un **client ESME** de qualité production destiné aux intégrateurs, opérateurs, agrégateurs et équipes QA qui doivent se connecter à un ou plusieurs SMSC pour émettre des messages, recevoir des accusés de livraison (*delivery receipts*) et tester des plateformes de messagerie à haut débit.

### 1.3 Périmètre fonctionnel

L'application couvre :

- La connexion à un ou plusieurs SMSC simultanément via des **sessions SMPP multiples** (transmitter, receiver, transceiver).
- Le support **complet** des protocoles **SMPP v3.4** et **SMPP v5.0**.
- L'**envoi unitaire** (message unique) et l'**envoi en masse** (*batch*) avec parallélisation contrôlée.
- L'ensemble des **paramètres de configuration** d'un client SMPP de qualité production (fenêtrage, TLV, encodages, TON/NPI, mode d'accusé, etc.).
- L'**import de contacts** depuis des fichiers **XLSX** et **CSV**.
- La **génération automatique de numéros valides** pour un pays donné (plans de numérotation E.164).
- La définition d'un **débit (throughput)** cible par session (messages/seconde) avec régulation.
- La **journalisation** exhaustive des messages émis/reçus avec **export**.
- Une application **desktop multiplateforme** (Windows, macOS, Linux).

### 1.4 Hors périmètre (non-objectifs)

Le présent projet **n'inclut pas** :

- La mise en œuvre d'un **SMSC** ou d'un serveur SMPP (rôle serveur) ; seul le rôle **ESME/client** est traité. Un mode simulateur SMSC pourra néanmoins être ajouté ultérieurement pour les tests (voir §22).
- La facturation, le routage inter-opérateurs ou la médiation.
- La conformité réglementaire spécifique à un pays (consentement, opt-out) au-delà des garde-fous techniques (voir §17.6).
- Les canaux non-SMPP (SMPP over HTTP, API REST tierces, RCS, WhatsApp, etc.).

### 1.5 Public visé

Ingénieurs d'intégration télécom, développeurs backend, équipes QA/tests de charge, administrateurs de plateformes SMS et agrégateurs.

---

## 2. Glossaire et terminologie SMPP

| Terme | Définition |
|-------|-----------|
| **SMPP** | *Short Message Peer-to-Peer* — protocole de niveau applicatif au-dessus de TCP/IP pour l'échange de messages courts. |
| **ESME** | *External Short Messaging Entity* — l'entité cliente (notre application). |
| **SMSC** | *Short Message Service Centre* — le serveur/centre de messagerie de l'opérateur ou de l'agrégateur. |
| **MC** | *Message Centre* — terme générique v5.0 englobant SMSC et CBC. |
| **PDU** | *Protocol Data Unit* — unité de données protocolaire (en-tête de 16 octets + corps). |
| **Bind** | Opération d'authentification/ouverture de session (`bind_transmitter`, `bind_receiver`, `bind_transceiver`). |
| **TX / RX / TRX** | Transmitter (émission), Receiver (réception), Transceiver (bidirectionnel). |
| **TON** | *Type Of Number* — type d'adresse (international, national, alphanumérique, etc.). |
| **NPI** | *Numbering Plan Indicator* — plan de numérotation (E.164, télex, etc.). |
| **DCS** | *Data Coding Scheme* — encodage des données du message (GSM 7-bit, Latin-1, UCS2, etc.). |
| **TLV** | *Tag-Length-Value* — paramètre optionnel étendu (appelé *optional parameter* en v3.4). |
| **UDH** | *User Data Header* — en-tête dans le champ `short_message` utilisé notamment pour la concaténation. |
| **DLR** | *Delivery Receipt* — accusé de livraison retourné par le SMSC via un `deliver_sm`. |
| **Window** | Nombre maximal de PDU envoyés en attente d'accusé (fenêtrage). |
| **Throughput** | Débit d'émission, en messages par seconde (MPS/TPS). |
| **enquire_link** | PDU de *keep-alive* pour maintenir la session active. |
| **Message ID** | Identifiant du message attribué par le SMSC dans `submit_sm_resp`. |
| **CBC** | *Cell Broadcast Centre* — cible des opérations de diffusion cellulaire (v5.0). |
| **congestion_state** | TLV v5.0 (0–100) indiquant le niveau de congestion du pair. |

---

## 3. Exigences fonctionnelles

Les exigences sont identifiées par `EF-<domaine>-<n°>` et priorisées selon **MoSCoW** (M = Must, S = Should, C = Could).

### 3.1 Connexion et sessions

| ID | Exigence | Priorité |
|----|----------|----------|
| EF-CNX-01 | L'application doit permettre de configurer un profil de connexion SMSC (host, port, system_id, password, system_type, bind type, version). | M |
| EF-CNX-02 | L'application doit supporter les binds TX, RX et TRX. | M |
| EF-CNX-03 | L'application doit gérer **plusieurs sessions simultanées** vers des SMSC identiques ou différents. | M |
| EF-CNX-04 | L'application doit permettre le choix explicite de la version **v3.4** ou **v5.0** par session. | M |
| EF-CNX-05 | L'application doit envoyer périodiquement des `enquire_link` configurables et détecter les pertes de session. | M |
| EF-CNX-06 | L'application doit reconnecter automatiquement avec back-off exponentiel en cas de rupture. | M |
| EF-CNX-07 | L'application doit supporter TLS/SSL sur la connexion TCP lorsque le SMSC l'exige. | S |
| EF-CNX-08 | L'application doit permettre plusieurs connexions TCP (*binds*) par profil pour augmenter le débit agrégé. | S |

### 3.2 Envoi de messages

| ID | Exigence | Priorité |
|----|----------|----------|
| EF-MSG-01 | L'application doit permettre l'envoi d'un message unitaire à un destinataire. | M |
| EF-MSG-02 | L'application doit permettre l'envoi en masse (*batch*) à partir d'une liste de destinataires. | M |
| EF-MSG-03 | L'application doit gérer automatiquement la **segmentation/concaténation** des messages longs (UDH ou `sar_*` TLV / `message_payload`). | M |
| EF-MSG-04 | L'application doit détecter automatiquement l'encodage optimal (GSM 7-bit vs UCS2) et permettre le forçage manuel. | M |
| EF-MSG-05 | L'application doit exposer tous les paramètres de `submit_sm` (TON/NPI source & destination, DCS, validity_period, registered_delivery, priority_flag, etc.). | M |
| EF-MSG-06 | L'application doit permettre l'ajout de **TLV personnalisés** (tag/valeur) par message ou par campagne. | S |
| EF-MSG-07 | L'application doit corréler les `submit_sm_resp` et les DLR au message d'origine. | M |
| EF-MSG-08 | L'application doit supporter les modèles de message avec variables (ex. `{{prenom}}`) pour le batch. | S |
| EF-MSG-09 | L'application doit permettre la planification différée d'un envoi (`schedule_delivery_time`). | C |

### 3.3 Contacts et numéros

| ID | Exigence | Priorité |
|----|----------|----------|
| EF-CTC-01 | L'application doit importer des contacts depuis un fichier **CSV**. | M |
| EF-CTC-02 | L'application doit importer des contacts depuis un fichier **XLSX**. | M |
| EF-CTC-03 | L'application doit valider et normaliser les numéros au format **E.164**. | M |
| EF-CTC-04 | L'application doit permettre le mapping des colonnes du fichier importé. | M |
| EF-CTC-05 | L'application doit dédoublonner et signaler les numéros invalides. | M |
| EF-CTC-06 | L'application doit **générer automatiquement des numéros valides** pour un pays donné. | M |
| EF-CTC-07 | L'application doit permettre d'organiser les contacts en listes/groupes. | S |

### 3.4 Débit et régulation

| ID | Exigence | Priorité |
|----|----------|----------|
| EF-DBT-01 | L'application doit permettre de définir un **débit cible en messages/seconde** par session. | M |
| EF-DBT-02 | L'application doit réguler l'émission pour ne pas dépasser le débit configuré (limiteur de débit). | M |
| EF-DBT-03 | L'application doit adapter le débit en fonction du `congestion_state` (v5.0) et des erreurs de throttling (`ESME_RTHROTTLED`). | S |
| EF-DBT-04 | L'application doit respecter la **fenêtre** (window size) configurée. | M |

### 3.5 Journalisation et export

| ID | Exigence | Priorité |
|----|----------|----------|
| EF-LOG-01 | L'application doit journaliser chaque PDU émis/reçu avec horodatage, session, statut et métadonnées. | M |
| EF-LOG-02 | L'application doit afficher les logs en temps réel avec filtres (session, statut, destinataire, période). | M |
| EF-LOG-03 | L'application doit exporter les journaux au format **CSV** et **XLSX**. | M |
| EF-LOG-04 | L'application doit exporter les journaux au format **JSON** et **JSONL**. | S |
| EF-LOG-05 | L'application doit conserver l'historique des campagnes avec statistiques agrégées. | M |
| EF-LOG-06 | L'application doit permettre la purge/rotation des logs selon une politique de rétention. | S |

### 3.6 Configuration et administration

| ID | Exigence | Priorité |
|----|----------|----------|
| EF-CFG-01 | L'application doit permettre la sauvegarde/chargement de profils de connexion. | M |
| EF-CFG-02 | L'application doit chiffrer les identifiants stockés (voir §17). | M |
| EF-CFG-03 | L'application doit permettre l'import/export de la configuration (hors secrets en clair). | S |
| EF-CFG-04 | L'application doit proposer des thèmes clair/sombre et une internationalisation (FR/EN). | C |

---

## 4. Exigences non fonctionnelles

### 4.1 Performance

- **ENF-PERF-01 :** Le cœur d'émission doit soutenir au minimum **1 000 messages/seconde** par session sur un poste de développement standard (limité en pratique par le SMSC et le réseau).
- **ENF-PERF-02 :** La latence d'ajout d'un message dans la file d'émission doit être inférieure à **1 ms** (p99).
- **ENF-PERF-03 :** L'interface graphique doit rester réactive (< 16 ms/frame) même lors d'un batch de plusieurs centaines de milliers de destinataires ; le traitement lourd s'exécute dans le backend Rust, jamais dans le thread UI.
- **ENF-PERF-04 :** L'empreinte mémoire au repos doit rester inférieure à **150 Mo** (bénéfice de Tauri par rapport à une solution Electron).

### 4.2 Fiabilité et robustesse

- **ENF-FIA-01 :** Aucune perte de message : tout message accepté par l'application doit être persisté avant émission (journalisation *write-ahead*) et son état de cycle de vie tracé.
- **ENF-FIA-02 :** Reprise après coupure : en cas d'arrêt inopiné, l'application doit pouvoir reprendre une campagne interrompue.
- **ENF-FIA-03 :** Les erreurs SMPP (codes `command_status`) doivent être gérées, journalisées et, le cas échéant, déclencher un rejeu.

### 4.3 Portabilité

- **ENF-POR-01 :** L'application doit fonctionner sur **Windows 10/11 (x64)**, **macOS 12+ (Intel & Apple Silicon)** et **Linux (x64, distributions récentes)**.
- **ENF-POR-02 :** Un seul socle de code (Rust + web) produit les trois cibles via Tauri.

### 4.4 Sécurité

- Voir §17. Chiffrement au repos des secrets, support TLS, principe de moindre privilège dans les capacités Tauri.

### 4.5 Maintenabilité et extensibilité

- **ENF-MNT-01 :** Architecture modulaire en couches, séparation stricte entre le moteur protocolaire, la logique métier et l'IHM.
- **ENF-MNT-02 :** Couverture de tests unitaires ≥ 80 % sur le cœur protocolaire et les modules critiques.

### 4.6 Utilisabilité

- **ENF-UTI-01 :** Prise en main d'un envoi simple en moins de 5 minutes pour un utilisateur connaissant SMPP.
- **ENF-UTI-02 :** Messages d'erreur explicites incluant le `command_status` SMPP et sa signification en clair.

---

## 5. Architecture générale

### 5.1 Vue en couches

L'application suit une architecture en couches strictes. Le **backend Rust** (processus principal Tauri) contient toute la logique métier et protocolaire ; le **frontend web** (WebView) ne fait que présenter l'état et émettre des commandes. Les deux communiquent via l'IPC Tauri (commandes + événements).

```
┌───────────────────────────────────────────────────────────────┐
│                    FRONTEND (WebView / SPA)                     │
│  UI React/Svelte · Vues (Sessions, Envoi, Contacts, Logs)       │
│  Store d'état · Graphiques temps réel · i18n                    │
└───────────────▲───────────────────────────────┬───────────────┘
                │  Événements (tauri emit)       │  Commandes (invoke)
                │  logs, métriques, statuts       │  bind, submit, import…
┌───────────────┴───────────────────────────────▼───────────────┐
│                   COUCHE IPC / APPLICATION (Rust)               │
│  Commandes Tauri · Sérialisation (serde) · Validation entrée    │
│  Orchestrateur de campagnes · Gestion d'état applicatif         │
└───────────────▲───────────────────────────────┬───────────────┘
                │                                 │
┌───────────────┴─────────────┐   ┌───────────────▼───────────────┐
│      COUCHE MÉTIER (Rust)    │   │     SERVICES TRANSVERSAUX     │
│  · Gestionnaire de sessions  │   │  · Contacts & import XLSX/CSV │
│  · Moteur d'envoi (queue)    │   │  · Générateur de numéros      │
│  · Limiteur de débit         │   │  · Journalisation & export    │
│  · Segmentation/encodage     │   │  · Config & secrets (crypto)  │
└───────────────▲─────────────┘   └───────────────┬───────────────┘
                │                                   │
┌───────────────┴───────────────────────────────────▼───────────┐
│                COUCHE PROTOCOLAIRE SMPP (Rust)                  │
│  rusmpp / rusmppc · Codec PDU · Machine à états de session      │
│  Fenêtrage · Corrélation seq_number · v3.4 & v5.0               │
└───────────────▲───────────────────────────────────┬───────────┘
                │                                     │
┌───────────────┴─────────┐            ┌──────────────▼───────────┐
│   TRANSPORT (tokio TCP)  │            │  PERSISTANCE (SQLite)    │
│  TcpStream (+ TLS)       │            │  SQLx/rusqlite · WAL     │
└──────────────────────────┘            └──────────────────────────┘
```

### 5.2 Principes directeurs

1. **Backend-heavy, frontend-thin :** toute opération lourde (I/O réseau, parsing de fichiers volumineux, cryptographie, régulation de débit) réside dans Rust. Le frontend ne manipule jamais de socket ni de gros volumes de données brutes.
2. **Asynchrone de bout en bout :** le moteur repose sur **Tokio**. Chaque session SMPP tourne dans sa propre tâche asynchrone ; l'émission, la réception et le *keep-alive* sont des tâches concurrentes coordonnées par des canaux (`tokio::sync::mpsc`, `oneshot`, `watch`).
3. **Découplage par messages :** les couches communiquent par passage de messages (acteurs légers) plutôt que par état partagé verrouillé, réduisant la contention et facilitant les tests.
4. **Persistance write-ahead :** un message est écrit en base **avant** émission ; son état évolue (`QUEUED → SENT → ACCEPTED → DELIVERED/FAILED`) au fil des réponses.
5. **Idempotence et reprise :** l'identifiant interne (`client_message_id`, UUID) permet de retrouver et rejouer sans doublon.

### 5.3 Modèle d'acteurs pour une session

Chaque session SMPP est modélisée comme un ensemble de tâches Tokio coopérantes :

- **Connection Actor** : possède le `Framed<TcpStream, CommandCodec>`, effectue le bind, lit/écrit les PDU. Point unique d'accès au socket.
- **Writer / Sender loop** : consomme la file d'émission régulée, applique le fenêtrage, écrit les `submit_sm`.
- **Reader loop** : lit les PDU entrants (`submit_sm_resp`, `deliver_sm`, `enquire_link`, `unbind`…), résout les `oneshot` en attente et pousse les DLR vers la journalisation.
- **Keep-alive timer** : émet les `enquire_link` selon l'intervalle configuré.
- **Supervisor** : surveille la santé de la session, déclenche reconnexion/back-off, publie les changements d'état vers l'UI.

### 5.4 Flux nominal d'un envoi

```
UI (invoke submit) → Commande Tauri → Orchestrateur
   → Validation + normalisation E.164
   → Encodage + segmentation (n PDU)
   → Persistance (état QUEUED)
   → File d'émission de la session choisie
        → Limiteur de débit (token bucket)
        → Contrôle de fenêtre (window)
        → Writer: submit_sm (seq=N)  ─────────► SMSC
        ◄───────── submit_sm_resp (seq=N, message_id, status)
   → Corrélation + MAJ état (SENT/ACCEPTED)
   → Persistance + événement UI
        ◄───────── deliver_sm (DLR: message_id, stat=DELIVRD)
   → Corrélation par message_id + MAJ état (DELIVERED)
   → Persistance + événement UI + métriques
```

---

## 6. Pile technologique détaillée

### 6.1 Backend / cœur

| Composant | Choix | Justification |
|-----------|-------|--------------|
| Langage | **Rust (édition 2021+)** | Sûreté mémoire, performance native, excellent pour du réseau haut débit et de la concurrence sans data-race. |
| Runtime desktop | **Tauri 2.x** | Binaire léger, WebView système (pas de Chromium embarqué), sécurité par capacités, multiplateforme (Win/macOS/Linux). |
| Runtime async | **Tokio** | Standard de facto pour l'I/O asynchrone en Rust ; ordonnanceur multi-thread, timers, canaux. |
| Pile SMPP | **rusmpp** (codec/PDU v5.0, rétrocompatible v3.4) + **rusmppc** (client) | Implémentation Rust du protocole SMPP v5 avec codec Tokio ; `CommandCodec` gère l'encodage/décodage des PDU et l'attribution des command-id. |
| Sérialisation | **serde** / **serde_json** | Échange IPC et export JSON. |
| Base de données | **SQLite** via **SQLx** (async) ou **rusqlite** | Embarquée, sans serveur, mode WAL pour la concurrence lecture/écriture. |
| Cryptographie | **ring** / **aes-gcm** + **argon2** + **keyring** | Chiffrement des secrets au repos et intégration au trousseau OS. |
| Fichiers tabulaires | **calamine** (lecture XLSX), **rust_xlsxwriter** (écriture XLSX), **csv** | Import/export performant sans dépendance à un tableur. |
| Numéros de téléphone | **phonenumber** (port Rust de libphonenumber) | Validation, normalisation E.164, métadonnées de plans de numérotation par pays. |
| Journalisation technique | **tracing** + **tracing-subscriber** | Traces structurées, corrélation par session/campagne. |
| Limiteur de débit | **governor** (token bucket / GCRA) | Régulation précise du débit par session. |

### 6.2 Frontend

| Composant | Choix recommandé | Alternative |
|-----------|-----------------|-------------|
| Framework UI | **React 18 + TypeScript** | Svelte / SvelteKit, Vue 3 |
| Bundler | **Vite** | — |
| Composants | **shadcn/ui** + Tailwind CSS | Mantine, Radix UI |
| État | **Zustand** ou **Redux Toolkit** | Jotai |
| Graphiques | **Recharts** / **visx** | ECharts |
| Tables virtualisées | **TanStack Table + TanStack Virtual** | ag-grid (batchs volumineux) |
| i18n | **i18next** | — |

> **Note :** le frontend est interchangeable. La seule contrainte est de rester une SPA statique servie par la WebView et de communiquer exclusivement via l'IPC Tauri. Aucune logique SMPP ne doit résider côté frontend.

### 6.3 Versions minimales

- Rust ≥ 1.78, Tauri ≥ 2.0, Node.js ≥ 20 (build frontend), SQLite ≥ 3.40.

---

## 7. Cœur protocolaire SMPP (v3.4 et v5.0)

### 7.1 Format général d'un PDU

Chaque PDU se compose d'un **en-tête de 16 octets** suivi d'un **corps** optionnel :

| Champ | Taille | Description |
|-------|--------|-------------|
| `command_length` | 4 octets | Longueur totale du PDU (en-tête + corps), entier non signé, big-endian. |
| `command_id` | 4 octets | Identifiant de l'opération (ex. `0x00000004` = submit_sm). |
| `command_status` | 4 octets | Code de résultat (0 = `ESME_ROK` dans les réponses ; 0 dans les requêtes). |
| `sequence_number` | 4 octets | Numéro de séquence (1 à 0x7FFFFFFF) pour corréler requête/réponse. |

Le corps varie selon le `command_id`. Les paramètres optionnels (**TLV** en v3.4 / *optional parameters*) sont ajoutés en fin de corps sous forme `tag (2o) | length (2o) | value (n o)`.

### 7.2 Opérations (command_id) supportées

L'application implémente ou consomme, via rusmpp, l'ensemble des opérations suivantes. La colonne « Rôle ESME » précise si le client **émet** (→) ou **reçoit** (←) l'opération.

| Opération | command_id | v3.4 | v5.0 | Rôle ESME | Usage dans l'app |
|-----------|-----------|:----:|:----:|:---------:|------------------|
| `bind_transmitter` / `_resp` | 0x00000002 / 0x80000002 | ✔ | ✔ | → | Ouverture session TX |
| `bind_receiver` / `_resp` | 0x00000001 / 0x80000001 | ✔ | ✔ | → | Ouverture session RX |
| `bind_transceiver` / `_resp` | 0x00000009 / 0x80000009 | ✔ | ✔ | → | Ouverture session TRX |
| `outbind` | 0x0000000B | ✔ | ✔ | ← | Invitation SMSC → ESME |
| `unbind` / `_resp` | 0x00000006 / 0x80000006 | ✔ | ✔ | ↔ | Fermeture propre |
| `submit_sm` / `_resp` | 0x00000004 / 0x80000004 | ✔ | ✔ | → | **Envoi de message (principal)** |
| `submit_multi` / `_resp` | 0x00000021 / 0x80000021 | ✔ | ✔ | → | Envoi multi-destinataires (jusqu'à ~254) |
| `deliver_sm` / `_resp` | 0x00000005 / 0x80000005 | ✔ | ✔ | ← | Réception MO **et DLR** |
| `data_sm` / `_resp` | 0x00000103 / 0x80000103 | ✔ | ✔ | ↔ | Transfert via `message_payload` |
| `query_sm` / `_resp` | 0x00000003 / 0x80000003 | ✔ | ✔ | → | Interrogation d'état d'un message |
| `cancel_sm` / `_resp` | 0x00000008 / 0x80000008 | ✔ | ✔ | → | Annulation d'un message |
| `replace_sm` / `_resp` | 0x00000007 / 0x80000007 | ✔ | ✔ | → | Remplacement d'un message |
| `enquire_link` / `_resp` | 0x00000015 / 0x80000015 | ✔ | ✔ | ↔ | Keep-alive |
| `alert_notification` | 0x00000102 | ✔ | ✔ | ← | Notification d'alerte |
| `generic_nack` | 0x80000000 | ✔ | ✔ | ↔ | Rejet PDU invalide |
| `broadcast_sm` / `_resp` | 0x00000111 / 0x80000111 | — | ✔ | → | **Diffusion cellulaire (v5.0)** |
| `query_broadcast_sm` / `_resp` | 0x00000112 / 0x80000112 | — | ✔ | → | Interrogation d'une diffusion (v5.0) |
| `cancel_broadcast_sm` / `_resp` | 0x00000113 / 0x80000113 | — | ✔ | → | Annulation d'une diffusion (v5.0) |

### 7.3 Paramètres de `submit_sm` exposés

Tous les champs obligatoires et optionnels de `submit_sm` sont configurables dans l'IHM (avec valeurs par défaut sûres) :

**Champs obligatoires du corps :**

| Champ | Type | Description / valeurs |
|-------|------|-----------------------|
| `service_type` | C-Octet String | Type de service (vide, `CMT`, `WAP`, etc.). |
| `source_addr_ton` | Integer(1) | TON source (0=Unknown, 1=International, 2=National, 5=Alphanumeric…). |
| `source_addr_npi` | Integer(1) | NPI source (0=Unknown, 1=ISDN/E.164, 8=National…). |
| `source_addr` | C-Octet String | Adresse émettrice (numéro ou alphanumérique ≤ 11 car.). |
| `dest_addr_ton` | Integer(1) | TON destinataire. |
| `dest_addr_npi` | Integer(1) | NPI destinataire. |
| `destination_addr` | C-Octet String | Numéro destinataire (E.164). |
| `esm_class` | Integer(1) | Mode (défaut, datagram, forward), présence UDHI, type de message. |
| `protocol_id` | Integer(1) | Identifiant protocole (réseau GSM=0). |
| `priority_flag` | Integer(1) | Priorité (0–3 GSM). |
| `schedule_delivery_time` | C-Octet String | Heure de livraison programmée (format absolu/relatif SMPP) ou vide (immédiat). |
| `validity_period` | C-Octet String | Durée de validité ou vide (défaut SMSC). |
| `registered_delivery` | Integer(1) | Demande de DLR (0=aucun, 1=DLR final, 2=échec seul, +SME ack/intermediate). |
| `replace_if_present_flag` | Integer(1) | Remplacement si présent. |
| `data_coding` | Integer(1) | DCS (voir §7.5). |
| `sm_default_msg_id` | Integer(1) | Message préenregistré. |
| `sm_length` | Integer(1) | Longueur de `short_message` (0 si `message_payload` utilisé). |
| `short_message` | Octet String | Contenu (≤ 254 o ; au-delà → `message_payload` TLV). |

**TLV / paramètres optionnels fréquents :** `message_payload`, `sar_msg_ref_num`, `sar_total_segments`, `sar_segment_seqnum` (concaténation), `user_message_reference`, `source_port`, `dest_port`, `payload_type`, `privacy_indicator`, `callback_num`, `dpf_result`, `network_error_code`, `receipted_message_id`, `message_state`, `ussd_service_op`, ainsi que les TLV v5.0 (voir §7.7).

### 7.4 Types d'adresses (TON / NPI)

L'IHM propose des listes déroulantes documentées pour TON et NPI, avec un préréglage « International / E.164 » par défaut pour la destination et un choix « Alphanumérique » pour l'expéditeur lorsqu'un nom de marque est utilisé.

| TON | Valeur | | NPI | Valeur |
|-----|:------:|-|-----|:------:|
| Unknown | 0 | | Unknown | 0 |
| International | 1 | | ISDN (E.163/E.164) | 1 |
| National | 2 | | Data (X.121) | 3 |
| Network Specific | 3 | | Telex (F.69) | 4 |
| Subscriber Number | 4 | | Land Mobile (E.212) | 6 |
| Alphanumeric | 5 | | National | 8 |
| Abbreviated | 6 | | Private | 9 |

### 7.5 Encodage des données (Data Coding Scheme) et segmentation

L'application détermine automatiquement l'encodage optimal et gère la segmentation :

| DCS | Encodage | Longueur max 1 segment | Longueur/segment concaténé |
|-----|----------|:----------------------:|:--------------------------:|
| 0x00 | GSM 7-bit (défaut) | 160 caractères | 153 caractères |
| 0x01 | ASCII / IA5 | 140 octets | 134 octets |
| 0x03 | Latin-1 (ISO-8859-1) | 140 octets | 134 octets |
| 0x08 | UCS2 (UTF-16BE) | 70 caractères | 67 caractères |

**Algorithme de choix d'encodage :**

1. Si tous les caractères appartiennent à l'alphabet **GSM 03.38** (7-bit, y compris l'extension) → GSM 7-bit (DCS 0x00).
2. Sinon → **UCS2** (DCS 0x08).
3. L'utilisateur peut **forcer** un encodage (GSM/Latin-1/UCS2) au niveau du message ou de la campagne.

**Segmentation :** au-delà de la limite d'un segment, le message est découpé et chaque segment porte soit un **UDH** de concaténation (IEI 0x00, 6 octets : ref, total, index) avec `esm_class` UDHI activé, soit les TLV `sar_msg_ref_num` / `sar_total_segments` / `sar_segment_seqnum` (mode configurable). Un `message_payload` (jusqu'à 64 Ko) peut être utilisé à la place lorsque le SMSC le supporte.

### 7.6 Codes de statut (command_status) et gestion des erreurs

L'application interprète et affiche en clair les codes `command_status`. Extrait des codes gérés :

| Code | Nom | Signification | Réaction de l'app |
|------|-----|--------------|-------------------|
| 0x00000000 | ESME_ROK | Succès | Poursuite |
| 0x00000005 | ESME_RINVMSGLEN | Longueur message invalide | Échec, journalisé |
| 0x0000000A | ESME_RINVSRCADR | Adresse source invalide | Échec, journalisé |
| 0x0000000B | ESME_RINVDSTADR | Adresse destination invalide | Échec, marquage contact |
| 0x00000014 | ESME_RMSGQFUL | File SMSC pleine | Ralentissement + rejeu |
| 0x00000058 | ESME_RTHROTTLED | Débit dépassé | **Réduction dynamique du débit + back-off** |
| 0x00000045 | ESME_RSUBMITFAIL | Échec de soumission | Rejeu selon politique |
| 0x0000000E | ESME_RINVPASWD | Mot de passe invalide | Arrêt bind, alerte UI |
| 0x0000000F | ESME_RINVSYSID | system_id invalide | Arrêt bind, alerte UI |
| 0x00000054–0x00000058 | plage congestion/throttling | Contrôle de flux | Régulation adaptative |

> La table complète des codes v3.4 et v5.0 est intégrée en ressource (`status_codes.rs`) et consultable dans l'IHM via une info-bulle sur chaque code.

### 7.7 Spécificités et support de SMPP v5.0

En complément de v3.4, l'application implémente les apports de v5.0 :

- **Contrôle de congestion par `congestion_state`** (TLV 0x0428, valeur 0–100 dans les PDU de réponse) : le client lit la valeur retournée par le SMSC et **ajuste dynamiquement son débit** (cible 80–90 pour un débit optimal ; réduction au-delà). Voir §9.
- **Broadcast (diffusion cellulaire)** : `broadcast_sm`, `query_broadcast_sm`, `cancel_broadcast_sm` avec les TLV associés (`broadcast_area_identifier`, `broadcast_content_type`, `broadcast_rep_num`, `broadcast_frequency_interval`, `broadcast_service_group`…).
- **Mode d'accusé « livraison réussie uniquement »** (`registered_delivery` étendu).
- **`network_error_code` enrichi** pour une classification plus fine des échecs.
- **TLV de portabilité** : `dest_addr_np_country`, `dest_addr_np_information`, `dest_addr_np_resolution`.
- **TLV d'identification de nœud/réseau** : `source_network_id`, `source_node_id`, `dest_network_id`, `dest_node_id`.
- **`ussd_service_op`** dans `deliver_sm` pour l'USSD bidirectionnel.
- **`billing_identification`** pour la transmission d'informations de facturation.

Le choix de version est fait par session (`interface_version` 0x34 pour v3.4, 0x50 pour v5.0, annoncé dans le bind). Le codec rusmpp encode/décode les corps et TLV correspondants ; l'application masque dans l'IHM les paramètres non pertinents pour la version sélectionnée.

### 7.8 Accusés de livraison (DLR)

Les DLR arrivent via `deliver_sm` avec `esm_class` indiquant un *delivery receipt*. L'application :

1. Extrait le `receipted_message_id` (TLV) ou parse le corps texte standard : `id:… sub:… dlvrd:… submit date:… done date:… stat:… err:… text:…`.
2. Corrèle par `message_id` avec le message émis.
3. Met à jour l'état : `DELIVRD`, `EXPIRED`, `DELETED`, `UNDELIV`, `ACCEPTD`, `REJECTD`, `UNKNOWN`.
4. Persiste et notifie l'UI et les métriques.

### 7.9 Machine à états d'une session

```
        ┌──────────┐  connect()   ┌────────────┐  bind_*     ┌─────────┐
        │  CLOSED  │─────────────►│ CONNECTING │────────────►│ BINDING │
        └──────────┘              └────────────┘             └────┬────┘
             ▲                          │ échec TCP                │ bind_resp OK
             │ unbind_resp / erreur     ▼                          ▼
        ┌────┴─────┐              ┌────────────┐             ┌───────────┐
        │ UNBOUND  │◄─────────────│  ERROR /   │◄────────────│   BOUND   │
        └──────────┘   back-off   │  RECONNECT │  perte lien │ (TX/RX/TRX)│
                                   └────────────┘             └─────┬─────┘
                                        ▲                           │ enquire_link
                                        └───────────────────────────┘ (keep-alive)
```

L'état `BOUND` est le seul état d'émission. Toute perte de `enquire_link_resp` au-delà d'un seuil déclenche la transition vers `RECONNECT` avec back-off exponentiel plafonné (jitter inclus).

---

## 8. Gestion des sessions multiples

### 8.1 Objectifs

L'application doit gérer **N sessions SMPP simultanées** vers des SMSC identiques ou distincts, chacune avec sa propre configuration, son état, son débit et ses statistiques. Les cas d'usage incluent : agréger le débit vers un même SMSC (plusieurs binds), router selon le préfixe/pays, comparer plusieurs fournisseurs, ou séparer TX et RX.

### 8.2 Modèle de session

Chaque session est identifiée par un `session_id` (UUID) et décrite par un **profil** :

```jsonc
{
  "session_id": "uuid",
  "name": "Opérateur A - TRX #1",
  "host": "smsc.example.com",
  "port": 2775,
  "bind_type": "transceiver",        // transmitter | receiver | transceiver
  "interface_version": "v5.0",       // v3.4 | v5.0
  "system_id": "esme01",
  "password": "***chiffré***",
  "system_type": "",
  "addr_ton": 0, "addr_npi": 0, "address_range": "",
  "tls": { "enabled": false, "verify_peer": true, "ca_path": null },
  "window_size": 50,
  "throughput_tps": 100,             // messages/seconde cible (0 = illimité)
  "enquire_link_interval_s": 30,
  "response_timeout_s": 10,
  "reconnect": { "enabled": true, "min_backoff_s": 1, "max_backoff_s": 60, "jitter": true },
  "bind_count": 1                    // nombre de connexions TCP pour cette session
}
```

### 8.3 Gestionnaire de sessions (SessionManager)

Le `SessionManager` est un acteur central qui :

- Maintient un registre `HashMap<SessionId, SessionHandle>`.
- Démarre/arrête les sessions (spawn de tâches Tokio) à la demande de l'IPC.
- Agrège les métriques et publie l'état global vers l'UI (événement `sessions:state`).
- Route les demandes d'émission vers la bonne session (sélection manuelle par l'utilisateur, ou stratégie automatique : round-robin, moins chargée, par pays/préfixe).
- Applique une supervision : redémarrage automatique d'une session tombée, escalade des erreurs fatales (mauvais identifiants) sans boucle de reconnexion inutile.

### 8.4 Concurrence et isolation

Chaque session possède ses propres files, compteurs de fenêtre, limiteur de débit et connexion. Aucune ressource mutable n'est partagée entre sessions autrement que par messages. Cela garantit qu'une session lente ou en erreur n'impacte pas les autres.

### 8.5 Multi-bind d'une même session logique

Pour dépasser la limite de débit d'un seul lien, une session logique peut ouvrir `bind_count` connexions TCP en parallèle vers le même SMSC. Le moteur répartit les `submit_sm` entre les liens (round-robin pondéré par la fenêtre disponible) et agrège la fenêtre et le débit au niveau logique.

### 8.6 Stratégies de routage (batch multi-sessions)

| Stratégie | Description |
|-----------|-------------|
| **Manuelle** | L'utilisateur choisit explicitement la session d'émission. |
| **Round-robin** | Répartition cyclique entre sessions actives. |
| **Moins chargée** | Sélection de la session avec la plus grande fenêtre disponible. |
| **Par pays/préfixe** | Table de correspondance préfixe E.164 → session (routage opérateur). |
| **Basculement (failover)** | En cas d'échec/indisponibilité, redirection vers une session de secours. |

---

## 9. Contrôle de débit (throughput) et de congestion

### 9.1 Exigence

L'utilisateur définit un **débit cible en messages/seconde** (`throughput_tps`) par session (et éventuellement par campagne). Le moteur garantit que l'émission ne dépasse jamais ce débit, tout en maximisant l'utilisation de la fenêtre et en réagissant aux signaux de congestion.

### 9.2 Deux mécanismes complémentaires

Le débit effectif est borné par **deux** contraintes appliquées conjointement :

1. **Limiteur de débit (rate limiting)** — un *token bucket* (crate `governor`, algorithme GCRA) délivre au plus `throughput_tps` jetons par seconde. Chaque `submit_sm` consomme un jeton ; en l'absence de jeton, le writer attend (back-pressure), sans blocage du reste de l'application.
2. **Fenêtrage (windowing)** — au plus `window_size` PDU peuvent être en attente de réponse. Chaque `submit_sm` incrémente un compteur ; chaque `submit_sm_resp` le décrémente. Le writer se met en pause lorsque la fenêtre est pleine.

Le débit réel = min(limite de tokens, débit permis par la fenêtre et la latence RTT du SMSC).

### 9.3 Pseudocode du writer régulé

```rust
loop {
    let msg = queue.recv().await?;                 // file d'émission de la session
    rate_limiter.until_ready().await;              // token bucket: respecte TPS cible
    window.acquire().await;                        // attend un slot de fenêtre libre
    let seq = next_sequence();
    let (tx, rx) = oneshot::channel();
    pending.insert(seq, tx);                        // corrélation seq -> attente réponse
    framed.send(build_submit_sm(&msg, seq)).await?;
    persist_state(&msg, State::Sent);
    tokio::spawn(async move {
        match timeout(resp_timeout, rx).await {
            Ok(Ok(resp)) => handle_resp(resp),     // MAJ état, libère fenêtre
            _            => on_timeout(seq, &msg),  // timeout: rejeu/échec, libère fenêtre
        }
    });
}
```

### 9.4 Adaptation dynamique (congestion & throttling)

Le débit cible est ajusté automatiquement selon deux signaux :

- **`congestion_state` (v5.0)** : lu dans les PDU de réponse (0 = inactif, 100 = congestionné). Politique : maintenir la zone **80–90** ; si `congestion_state > 90`, réduire le débit (p. ex. −20 %) ; si `< 70` durablement, remonter progressivement vers la cible utilisateur (AIMD — *Additive Increase, Multiplicative Decrease*).
- **`ESME_RTHROTTLED` / `ESME_RMSGQFUL`** : en cas de réception, appliquer une **réduction multiplicative** immédiate du débit et un back-off, puis remontée additive. Ce mécanisme fonctionne aussi bien en v3.4 (qui n'a pas `congestion_state`) qu'en v5.0.

```
débit_effectif = clamp(débit_cible_utilisateur × facteur_adaptatif, min_tps, cible)
facteur_adaptatif ← ×0.8   si throttled/congestion>90
facteur_adaptatif ← +0.05  par intervalle stable   (jusqu'à 1.0)
```

### 9.5 Paramètres exposés

`throughput_tps`, `window_size`, `response_timeout_s`, activation de l'adaptation dynamique, bornes `min_tps`/`max_tps`, coefficients AIMD. Des valeurs par défaut prudentes sont fournies (TPS 100, window 50, timeout 10 s).

### 9.6 Mesure et affichage

Le débit instantané (moyenne glissante 1 s / 10 s), le taux d'occupation de la fenêtre, le RTT moyen des réponses et le `congestion_state` courant sont exposés en temps réel dans l'IHM (jauges + courbes).

---

## 10. Envoi simple et envoi en masse (batch)

### 10.1 Envoi simple

Formulaire unique : session, expéditeur (source_addr + TON/NPI), destinataire, message (avec compteur de caractères/segments et détection d'encodage en direct), options avancées (registered_delivery, validity_period, priority, TLV). À la validation : normalisation, encodage, segmentation, persistance, mise en file, émission. Le résultat (message_id, statut, DLR) s'affiche en direct.

### 10.2 Envoi en masse (batch/campagne)

Une **campagne** est une entité persistée regroupant :

- Une **source de destinataires** : liste de contacts importée (§11), génération automatique (§12), ou coller/saisie manuelle.
- Un **modèle de message** avec variables (`{{prenom}}`, `{{ville}}`…) résolues par destinataire à partir des colonnes de contact.
- Une **configuration d'envoi** : session(s) cible(s) et stratégie de routage (§8.6), débit, fenêtre, options SMPP communes.
- Un **planning** optionnel (démarrage différé, plage horaire autorisée).

### 10.3 Cycle de vie d'une campagne

```
CREATED → VALIDATED → RUNNING → (PAUSED ⇄ RUNNING) → COMPLETED
                          │
                          └──► CANCELLED / FAILED
```

Contrôles disponibles : **démarrer, mettre en pause, reprendre, annuler**. La pause interrompt l'alimentation de la file sans casser la session ; les messages déjà dans la fenêtre se terminent proprement.

### 10.4 Traitement par lots et back-pressure

Les destinataires sont lus en flux (streaming) depuis la base — jamais chargés intégralement en mémoire — et poussés dans la file d'émission bornée. La file applique une **back-pressure** : si le SMSC ralentit, la lecture ralentit d'autant, évitant toute explosion mémoire même pour des campagnes de plusieurs millions de destinataires.

### 10.5 Reprise et idempotence

Chaque message porte un `client_message_id` (UUID) et un `campaign_id`. En cas d'arrêt, la reprise repart des messages en état `QUEUED`/`SENT` non confirmés. Un garde-fou empêche le double envoi (vérification d'état avant émission).

### 10.6 `submit_multi`

Pour des envois à destinataires multiples partageant le même contenu, l'application peut utiliser `submit_multi` (jusqu'à ~254 destinataires par PDU) afin de réduire le nombre de PDU, lorsque le SMSC le supporte. Sinon, repli automatique sur des `submit_sm` individuels.

### 10.7 Gestion des retours et rejeu

Politique de rejeu configurable : nombre maximal de tentatives, délai entre tentatives, filtrage par code d'erreur (rejouer sur `ESME_RMSGQFUL`/`RTHROTTLED`/timeout ; ne pas rejouer sur `RINVDSTADR`). Les échecs définitifs sont marqués et exportables.

---

## 11. Gestion des contacts et import XLSX/CSV

### 11.1 Modèle de contact

```jsonc
{
  "contact_id": "uuid",
  "msisdn": "+2250700000000",   // E.164 normalisé
  "country": "CI",               // ISO 3166-1 alpha-2 (déduit)
  "valid": true,
  "attributes": { "prenom": "Awa", "ville": "Abidjan", "segment": "VIP" },
  "lists": ["campagne_juillet"],
  "source": "import_xlsx",
  "created_at": "…"
}
```

### 11.2 Import CSV

- Détection automatique du séparateur (`,`, `;`, tabulation) et de l'encodage (UTF-8, UTF-8 BOM, Latin-1).
- Gestion des guillemets, des retours à la ligne échappés, des en-têtes.
- Bibliothèque : crate `csv` (lecture en streaming).

### 11.3 Import XLSX

- Lecture via la crate **calamine** (sans dépendance à Excel/LibreOffice).
- Prise en charge de plusieurs feuilles (choix de la feuille).
- Lecture en streaming ligne par ligne pour les gros fichiers.

### 11.4 Mapping des colonnes

Après lecture de l'en-tête, l'utilisateur associe chaque colonne du fichier à un champ cible : **numéro (obligatoire)**, pays (optionnel, sinon déduit), et attributs libres (prénom, ville, etc.) réutilisables comme variables de modèle. Le mapping est mémorisable comme profil d'import réutilisable.

### 11.5 Validation et normalisation

Chaque numéro passe par la crate **phonenumber** :

1. Parsing avec pays par défaut (colonne pays, indicatif détecté, ou pays global de l'import).
2. Vérification de validité (`is_valid_number`) et du type de ligne (mobile vs fixe) — option pour ne conserver que les mobiles.
3. Normalisation au format **E.164** (`+<indicatif><numéro>`).
4. Marquage des numéros **invalides** avec la raison (trop court, indicatif inconnu, format incorrect…).

### 11.6 Déduplication et rapport d'import

- Déduplication sur le MSISDN normalisé (option : conserver la première occurrence / fusionner les attributs).
- **Rapport d'import** : nombre total de lignes, valides, invalides (avec motifs), doublons, mobiles/fixes. Les lignes rejetées sont exportables pour correction.

### 11.7 Listes et groupes

Les contacts peuvent être organisés en listes nommées, filtrables et combinables (union/intersection), servant de source aux campagnes.

---

## 12. Génération automatique de numéros valides par pays

### 12.1 Objectif

Produire, pour un **pays donné**, un ensemble de numéros de téléphone **structurellement valides** (conformes au plan de numérotation national), utiles pour les tests de charge, la QA et la validation de plateformes — **sans prétendre que les numéros soient attribués à un abonné réel**.

> ⚠️ **Avertissement d'usage :** cette fonction génère des numéros **syntaxiquement valides** au sens du plan de numérotation, ce qui ne garantit pas qu'ils soient actifs. Elle est destinée aux tests et à la validation techniques. L'envoi de messages non sollicités vers des numéros générés peut être illégal ; voir les garde-fous §17.6.

### 12.2 Sources de règles

La génération s'appuie sur les métadonnées de **libphonenumber** (via la crate `phonenumber`), qui fournit pour chaque pays :

- L'**indicatif pays** (country calling code).
- Les **préfixes/plages mobiles** valides (patterns d'opérateurs).
- Les **longueurs** de numéro nationales autorisées.
- Les motifs de validation (expressions régulières de plage).

### 12.3 Algorithme de génération

```
Entrées : pays (ISO alpha-2), quantité N, type (mobile/fixe/tous),
          option opérateur (préfixe imposé), unicité (bool), graine (option)

1. Charger les métadonnées du pays (indicatif, patterns mobiles, longueurs).
2. Sélectionner un pattern de préfixe (aléatoire pondéré ou imposé par l'utilisateur).
3. Pour chaque numéro à générer :
   a. Choisir un préfixe mobile valide.
   b. Compléter les chiffres restants aléatoirement jusqu'à la longueur nationale.
   c. Assembler en E.164 (+indicatif + national).
   d. VALIDER via phonenumber::is_valid_number ; rejeter et régénérer si invalide.
   e. Si unicité demandée : rejeter les doublons (ensemble de hachage).
4. Retourner la liste (+ statistiques : taux de validité, répartition par préfixe).
```

- Une **graine (seed)** optionnelle rend la génération **reproductible** (RNG déterministe) pour les tests.
- La validation systématique par `is_valid_number` garantit que seuls des numéros conformes au plan national sont émis.
- Génération en **streaming** vers la base pour de gros volumes (millions), sans saturation mémoire.

### 12.4 Paramètres exposés dans l'IHM

Pays (sélecteur avec drapeau + indicatif), quantité, type de ligne, préfixe/opérateur optionnel, unicité, graine, et destination (nouvelle liste de contacts ou alimentation directe d'une campagne). Un aperçu de quelques exemples est affiché avant génération en masse.

### 12.5 Exemple

Pour la Côte d'Ivoire (`CI`, indicatif +225, numéros mobiles à 10 chiffres nationaux) : génération de `+2250700000000`-style respectant les préfixes opérateurs valides, chaque candidat étant revalidé avant insertion.

---

## 13. Journalisation des messages et export

### 13.1 Niveaux de journalisation

L'application distingue deux plans :

1. **Journal métier des messages** (persisté en base) — l'historique complet de chaque message et de son cycle de vie. C'est la source des exports et des statistiques.
2. **Traces techniques** (`tracing`) — diagnostic applicatif (connexions, erreurs, décodage PDU) écrit dans des fichiers de log rotatifs, avec niveaux (`ERROR/WARN/INFO/DEBUG/TRACE`) et option de trace **hexadécimale des PDU** pour le débogage protocolaire.

### 13.2 Contenu du journal des messages

Chaque enregistrement contient au minimum : horodatages (création, envoi, réponse, DLR), `session_id`, `campaign_id`, `client_message_id`, `smsc_message_id`, expéditeur, destinataire, pays, encodage, nombre de segments, `command_status` et libellé, état courant, code DLR (`stat`/`err`), texte (option de masquage/tronquage pour la confidentialité), et coût estimé si fourni.

### 13.3 Vue temps réel

Une table virtualisée (haute performance, centaines de milliers de lignes) affiche le flux en direct avec :

- **Filtres** : session, campagne, statut/état, plage de dates, destinataire/préfixe, code d'erreur.
- **Recherche** plein texte.
- **Codes couleur** par état (envoyé, accepté, livré, échec).
- **Détail PDU** au clic (en-tête + corps décodé + TLV + hex brut).

### 13.4 Statistiques et tableaux de bord

Agrégats par campagne/session : totaux envoyés/acceptés/livrés/échoués, taux de livraison, débit moyen/pic, latence moyenne, répartition des codes d'erreur, courbes temporelles. Export possible des agrégats.

### 13.5 Export

| Format | Portée | Détails |
|--------|--------|---------|
| **CSV** | Messages filtrés / campagne / tout | Séparateur et encodage configurables, en-têtes localisés (crate `csv`). |
| **XLSX** | Messages + feuille de statistiques | Génération via `rust_xlsxwriter`, mise en forme, colonnes typées, feuilles multiples. |
| **JSON / JSONL** | Messages (intégration/automatisation) | JSONL pour le streaming ligne à ligne de gros volumes. |
| **Traces PDU (hex)** | Débogage | Export du dump hexadécimal d'une sélection de PDU. |

L'export s'effectue en **streaming** depuis la base (aucune limite mémoire), avec barre de progression, et écrit un fichier livré à l'utilisateur (sélecteur d'emplacement natif).

### 13.6 Rétention et purge

Politique configurable : durée de rétention, purge manuelle ou automatique, archivage compressé (export puis suppression). Le mode WAL de SQLite et un `VACUUM` planifié maintiennent la taille de la base sous contrôle.

---

## 14. Modèle de données et persistance

### 14.1 Choix du moteur

**SQLite** en mode **WAL** (Write-Ahead Logging) : embarqué, sans serveur, transactionnel, adapté à un poste unique avec forte concurrence lecture/écriture entre les tâches d'émission et l'UI. Accès via **SQLx** (async, requêtes vérifiées à la compilation) ou **rusqlite**. Migrations versionnées (crate `sqlx::migrate` ou `refinery`).

### 14.2 Schéma relationnel (extrait)

```sql
-- Profils de connexion / sessions
CREATE TABLE session_profiles (
  session_id      TEXT PRIMARY KEY,
  name            TEXT NOT NULL,
  host            TEXT NOT NULL,
  port            INTEGER NOT NULL,
  bind_type       TEXT NOT NULL,               -- transmitter|receiver|transceiver
  interface_version TEXT NOT NULL,             -- v3.4|v5.0
  system_id       TEXT NOT NULL,
  password_enc    BLOB NOT NULL,               -- chiffré (AES-GCM), voir §17
  system_type     TEXT DEFAULT '',
  tls_config      TEXT,                        -- JSON
  window_size     INTEGER DEFAULT 50,
  throughput_tps  INTEGER DEFAULT 100,
  enquire_link_s  INTEGER DEFAULT 30,
  response_timeout_s INTEGER DEFAULT 10,
  reconnect_config TEXT,                       -- JSON
  bind_count      INTEGER DEFAULT 1,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);

-- Contacts
CREATE TABLE contacts (
  contact_id  TEXT PRIMARY KEY,
  msisdn      TEXT NOT NULL,
  country     TEXT,
  valid       INTEGER NOT NULL DEFAULT 1,
  line_type   TEXT,                            -- mobile|fixed_line|...
  attributes  TEXT,                            -- JSON (variables de modèle)
  source      TEXT,
  created_at  TEXT NOT NULL
);
CREATE INDEX idx_contacts_msisdn ON contacts(msisdn);

CREATE TABLE contact_lists (
  list_id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE TABLE contact_list_members (
  list_id TEXT, contact_id TEXT,
  PRIMARY KEY (list_id, contact_id)
);

-- Campagnes
CREATE TABLE campaigns (
  campaign_id   TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  status        TEXT NOT NULL,                 -- CREATED|RUNNING|PAUSED|COMPLETED|...
  template      TEXT NOT NULL,                 -- modèle avec variables
  send_config   TEXT NOT NULL,                 -- JSON (sessions, routage, options SMPP)
  total_count   INTEGER DEFAULT 0,
  sent_count    INTEGER DEFAULT 0,
  delivered_count INTEGER DEFAULT 0,
  failed_count  INTEGER DEFAULT 0,
  created_at    TEXT NOT NULL,
  started_at    TEXT, completed_at TEXT
);

-- Messages (journal métier, write-ahead)
CREATE TABLE messages (
  client_message_id TEXT PRIMARY KEY,          -- UUID interne
  campaign_id     TEXT,
  session_id      TEXT,
  smsc_message_id TEXT,                         -- attribué par le SMSC
  source_addr     TEXT, source_ton INTEGER, source_npi INTEGER,
  dest_addr       TEXT, dest_ton INTEGER, dest_npi INTEGER,
  data_coding     INTEGER,
  segments        INTEGER DEFAULT 1,
  text            TEXT,
  state           TEXT NOT NULL,               -- QUEUED|SENT|ACCEPTED|DELIVERED|FAILED|EXPIRED
  command_status  INTEGER,
  dlr_stat        TEXT, dlr_err TEXT,
  attempts        INTEGER DEFAULT 0,
  created_at      TEXT NOT NULL,
  sent_at         TEXT, resp_at TEXT, dlr_at TEXT
);
CREATE INDEX idx_messages_campaign ON messages(campaign_id);
CREATE INDEX idx_messages_state    ON messages(state);
CREATE INDEX idx_messages_smscid   ON messages(smsc_message_id);

-- Journal PDU (optionnel, débogage)
CREATE TABLE pdu_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT, direction TEXT,             -- in|out
  command_id INTEGER, command_status INTEGER, sequence_number INTEGER,
  raw_hex TEXT, decoded TEXT, ts TEXT NOT NULL
);
```

### 14.3 Cycle de vie d'un message (états)

`QUEUED` → `SENT` (submit_sm émis) → `ACCEPTED` (submit_sm_resp OK, message_id reçu) → `DELIVERED` | `FAILED` | `EXPIRED` (selon DLR). Un `submit_sm_resp` en erreur mène directement à `FAILED` (avec `command_status`). Les transitions sont journalisées et déclenchent des événements UI.

### 14.4 Emplacement des données

Base et fichiers dans le répertoire de données applicatif standard par OS (via l'API Tauri `path`) : `%APPDATA%` (Windows), `~/Library/Application Support` (macOS), `~/.local/share` (Linux). Aucune donnée sensible n'est écrite hors de ce répertoire.

---

## 15. Interface applicative interne (commandes Tauri / IPC)

### 15.1 Principe

Le frontend n'accède à aucune ressource système directement ; il appelle des **commandes Tauri** (`invoke`) exposées par le backend Rust, et s'abonne à des **événements** (`listen`) pour les mises à jour temps réel. Toutes les entrées sont validées côté Rust.

### 15.2 Commandes (extrait de l'API IPC)

| Commande | Entrée | Sortie | Rôle |
|----------|--------|--------|------|
| `session_create` | Profil | `session_id` | Créer/enregistrer un profil |
| `session_update` / `session_delete` | id + profil | ok | Modifier/supprimer |
| `session_bind` | `session_id` | état | Ouvrir la session (connect+bind) |
| `session_unbind` | `session_id` | ok | Fermer proprement |
| `session_list` / `session_status` | — / id | profils / état+métriques | Lister / superviser |
| `message_send` | message unitaire | `client_message_id` | Envoi simple |
| `campaign_create` | modèle + source + config | `campaign_id` | Créer une campagne |
| `campaign_start` / `pause` / `resume` / `cancel` | `campaign_id` | ok | Contrôle de campagne |
| `contacts_import` | chemin + mapping | rapport d'import | Import CSV/XLSX |
| `contacts_query` | filtre + pagination | page de contacts | Lister/filtrer |
| `numbers_generate` | pays + N + options | liste / liste_id | Génération de numéros |
| `logs_query` | filtre + pagination | page de messages | Consulter les logs |
| `logs_export` | filtre + format + chemin | progression | Exporter (CSV/XLSX/JSON) |
| `stats_get` | portée | agrégats | Tableaux de bord |
| `config_get` / `config_set` | — / config | config | Réglages applicatifs |

### 15.3 Événements (backend → frontend)

| Événement | Charge utile | Usage UI |
|-----------|-------------|----------|
| `sessions:state` | états + métriques par session | Bandeau de sessions, jauges |
| `message:update` | id + nouvel état | Table de logs temps réel |
| `campaign:progress` | compteurs + débit | Barre de progression campagne |
| `metrics:tick` | TPS, fenêtre, RTT, congestion | Graphiques temps réel |
| `import:progress` | lignes traitées | Barre de progression import |
| `export:progress` | lignes exportées | Barre de progression export |
| `error:notify` | code + message | Notifications/toasts |

### 15.4 Contrats de données

Tous les DTO sont définis en Rust avec `serde` et générés/typé côté TypeScript (par exemple via `ts-rs` ou `tauri-specta`) pour garantir la cohérence des types entre backend et frontend.

---

## 16. Interface utilisateur (UI/UX)

### 16.1 Cartographie des écrans

1. **Tableau de bord** — vue d'ensemble : sessions actives, débit global, campagnes en cours, indicateurs clés.
2. **Sessions** — liste des profils, création/édition, état en direct, contrôle bind/unbind, métriques par session (TPS, fenêtre, RTT, congestion).
3. **Envoi** — onglet *Simple* (formulaire unitaire) et onglet *Campagne* (modèle, source, routage, options, planning).
4. **Contacts** — import (CSV/XLSX) avec assistant de mapping, listes/groupes, recherche, rapport d'import.
5. **Générateur de numéros** — sélection pays, quantité, options, aperçu, export/alimentation de liste.
6. **Journaux** — table temps réel filtrable, détail PDU, export.
7. **Statistiques** — tableaux de bord et courbes par campagne/session.
8. **Réglages** — thèmes, langue (FR/EN), rétention, sécurité, chemins.

### 16.2 Composants transverses

Éditeur de message avec **compteur de caractères/segments** et détection d'encodage en direct ; sélecteurs documentés TON/NPI/DCS ; éditeur de TLV clé/valeur ; jauges de débit et de fenêtre ; notifications (toasts) pour succès/erreurs.

### 16.3 Réactivité et volumétrie

Toutes les tables potentiellement volumineuses (contacts, logs) sont **virtualisées** et paginées côté backend. Aucune opération bloquante n'est exécutée dans le thread UI. Les longues opérations affichent une progression et restent annulables.

### 16.4 Accessibilité et i18n

Contraste conforme WCAG AA, navigation clavier, libellés ARIA, thèmes clair/sombre, internationalisation FR/EN extensible via fichiers de traduction.

---

## 17. Sécurité

### 17.1 Modèle de menaces (résumé)

Menaces principales : vol des identifiants SMSC stockés, interception du trafic SMPP en clair, exfiltration de données de contacts/messages, abus d'envoi (spam), exécution de code via des dépendances vulnérables, et surface d'attaque de la WebView.

### 17.2 Protection des secrets au repos

- Les mots de passe SMSC et autres secrets sont **chiffrés** (AES-256-GCM). La clé de chiffrement est stockée dans le **trousseau du système d'exploitation** via la crate `keyring` (Keychain macOS, Credential Manager Windows, Secret Service/libsecret Linux).
- Option de **mot de passe maître** : dérivation de clé par **Argon2id** si l'utilisateur souhaite un chiffrement indépendant du trousseau OS.
- Aucun secret n'est jamais journalisé en clair ni exporté en clair.

### 17.3 Sécurité du transport

- Support **TLS** sur la connexion TCP (SMPP sur TLS) via `tokio-rustls`, avec vérification du certificat serveur (option CA personnalisée, option de vérification stricte activée par défaut).
- Avertissement explicite dans l'IHM lorsqu'une session est configurée **sans TLS** (trafic et identifiants en clair).

### 17.4 Durcissement de Tauri

- **Capacités/permissions Tauri 2.x** limitées au strict nécessaire (pas d'accès shell, système de fichiers restreint aux répertoires applicatifs et aux fichiers explicitement choisis par l'utilisateur via les dialogues natifs).
- **CSP** stricte pour la WebView, pas de contenu distant chargé, pas d'`eval`.
- Toutes les entrées venant du frontend sont **validées et assainies** côté Rust (le frontend est considéré comme non fiable).

### 17.5 Chaîne d'approvisionnement (supply chain)

- `cargo audit` / `cargo deny` en CI pour détecter les dépendances vulnérables et vérifier les licences.
- Verrouillage des versions (`Cargo.lock`, `package-lock.json`), builds reproductibles.
- Signature des binaires (voir §20).

### 17.6 Garde-fous d'usage responsable

Compte tenu des capacités de génération de numéros et d'envoi en masse :

- **Avertissements** clairs sur la légalité de l'envoi de messages non sollicités et l'usage des numéros générés (destinés aux tests).
- **Liste noire / opt-out** : possibilité d'importer une liste d'exclusion appliquée avant tout envoi.
- **Limites de sécurité** configurables (plafond de débit, confirmation au-delà d'un certain volume).
- Journalisation d'audit des campagnes (qui, quoi, quand, combien).

### 17.7 Confidentialité des données

Les données restent **locales** au poste (aucune télémétrie externe par défaut). Option de masquage/tronquage du contenu des messages dans les journaux et exports.

---

## 18. Observabilité, métriques et supervision

### 18.1 Métriques exposées

Par session et global : TPS instantané et moyen, pic de débit, occupation de fenêtre, RTT des réponses, `congestion_state`, compteurs par état de message, taux d'erreur par `command_status`, nombre de reconnexions, uptime de session.

### 18.2 Traces techniques

`tracing` avec spans par session/campagne, corrélation par `sequence_number`, niveaux configurables, sortie fichier rotative (crate `tracing-appender`), et mode **debug PDU** (dump hexadécimal) activable à la demande.

### 18.3 Diagnostic intégré

Écran de diagnostic : test de connectivité (TCP + bind), envoi d'un message de test, inspection des derniers PDU échangés, export d'un **bundle de diagnostic** (logs techniques + configuration anonymisée) pour le support.

---

## 19. Stratégie de tests et qualité

### 19.1 Tests unitaires

- **Codec PDU** : encodage/décodage de chaque type de PDU (v3.4 et v5.0), round-trip, cas limites (longueurs, TLV, corps vides).
- **Encodage/segmentation** : GSM 7-bit vs UCS2, seuils de segmentation, UDH de concaténation.
- **Génération de numéros** : validité (100 % via `is_valid_number`), unicité, reproductibilité par graine.
- **Import** : parsing CSV/XLSX, mapping, normalisation E.164, détection des invalides.
- **Limiteur de débit** : respect du TPS cible, comportement de la fenêtre.

Objectif de couverture ≥ **80 %** sur le cœur.

### 19.2 Tests d'intégration

- Contre un **simulateur SMSC** (SMPPSim, ou un serveur `rusmpps` embarqué pour les tests) : scénarios bind/unbind, submit + resp, DLR, throttling, congestion, reconnexion.
- Tests de bout en bout d'une campagne (création → envoi → DLR → statistiques → export).

### 19.3 Tests de charge et performance

- Bancs de charge (`criterion` pour micro-benchmarks ; scénarios de débit soutenu contre simulateur) validant les objectifs §4.1 (≥ 1 000 TPS/session, back-pressure, stabilité mémoire sur campagnes massives).

### 19.4 Tests de robustesse

- Injection de fautes : coupures TCP, réponses malformées, timeouts, PDU inattendus, `generic_nack` — vérification de la reconnexion et de l'absence de perte/doublon.

### 19.5 Qualité de code et CI

- `cargo fmt`, `cargo clippy` (deny warnings), `cargo test`, `cargo audit`/`deny`.
- Frontend : `eslint`, `tsc`, tests de composants (Vitest) et E2E (Playwright/WebDriver via `tauri-driver`).
- **Intégration continue** multi-OS (Windows, macOS, Linux) exécutant lint + tests + build.

### 19.6 Recette (critères d'acceptation)

Chaque exigence fonctionnelle (§3) est tracée vers au moins un cas de test. La recette valide notamment : envoi simple/batch, sessions multiples simultanées, débit respecté, import XLSX/CSV, génération de numéros valides, journalisation et export CSV/XLSX, support v3.4 et v5.0.

---

## 20. Construction, packaging et déploiement multiplateforme

### 20.1 Cibles et formats

| OS | Formats de distribution |
|----|-------------------------|
| **Windows 10/11 (x64)** | `.msi` (WiX) et `.exe` (NSIS) |
| **macOS 12+ (Intel & Apple Silicon)** | `.dmg` / `.app` (build universel) |
| **Linux (x64)** | `.deb`, `.rpm`, **AppImage** |

Génération via le **bundler Tauri** (`tauri build`), qui produit les paquets natifs pour chaque plateforme.

### 20.2 Signature et notarisation

- **Windows** : signature Authenticode (certificat de signature de code).
- **macOS** : signature avec Developer ID + **notarisation** Apple (indispensable pour l'exécution sans avertissement Gatekeeper).
- **Linux** : signatures de paquets et checksums publiés.

### 20.3 Mises à jour

Intégration de l'**updater Tauri** (mises à jour signées, vérification de signature du manifeste), avec canal stable et flux de release. Alternative : distribution manuelle des paquets.

### 20.4 CI/CD

Pipeline (GitHub Actions ou équivalent) à matrice OS : build frontend → build Rust → `tauri build` → signature → publication des artefacts. Cache des dépendances (`cargo`, `node_modules`) pour accélérer les builds.

### 20.5 Prérequis d'exécution

- **Windows** : WebView2 Runtime (préinstallé sur Windows 11 ; installeur peut l'embarquer/bootstrapper).
- **macOS** : WKWebView (système).
- **Linux** : `webkit2gtk` (dépendance de paquet déclarée).

---

## 21. Structure du dépôt et organisation du code

```
shinobismpp/
├── Cargo.toml                    # workspace Rust
├── crates/
│   ├── smpp-core/                # codec PDU, machine à états (v3.4 & v5.0) — s'appuie sur rusmpp
│   ├── smpp-session/             # SessionManager, acteurs, fenêtrage, reconnexion
│   ├── rate-control/             # limiteur de débit + adaptation congestion (governor)
│   ├── messaging/                # encodage, segmentation, orchestrateur d'envoi/campagnes
│   ├── contacts/                 # import CSV/XLSX, validation E.164, listes
│   ├── numbers-gen/              # génération de numéros par pays (phonenumber)
│   ├── persistence/              # accès SQLite (SQLx), migrations, repositories
│   ├── logging-export/           # journal métier, export CSV/XLSX/JSON
│   └── security/                 # chiffrement des secrets, keyring, TLS
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json           # config Tauri, capacités, bundler
│   └── src/
│       ├── main.rs               # point d'entrée, état applicatif
│       ├── commands/             # commandes IPC (invoke)
│       └── events.rs             # émission d'événements vers l'UI
├── ui/                           # frontend (React/TS + Vite)
│   ├── package.json
│   └── src/
│       ├── views/                # Dashboard, Sessions, Send, Contacts, Numbers, Logs, Stats, Settings
│       ├── components/
│       ├── store/                # état global
│       ├── ipc/                  # wrappers invoke + types générés
│       └── i18n/
├── migrations/                   # migrations SQL versionnées
├── tests/                        # tests d'intégration (simulateur SMSC)
└── .github/workflows/            # CI multi-OS
```

Séparation stricte : le **cœur SMPP** et la **logique métier** sont des crates indépendantes de Tauri (testables sans UI) ; `src-tauri` ne fait que l'orchestration IPC ; `ui/` ne contient aucune logique protocolaire.

---

## 22. Feuille de route et jalons

| Jalon | Contenu | Livrable |
|-------|---------|----------|
| **M1 — Socle** | Workspace, session unique TRX v3.4, bind/enquire_link, submit_sm + resp, persistance minimale, envoi simple | Envoyer 1 SMS et voir sa réponse |
| **M2 — Fiabilité & débit** | Fenêtrage, limiteur de débit, reconnexion/back-off, DLR, journal des messages | Envoi régulé + DLR tracés |
| **M3 — Masse & données** | Campagnes, import CSV/XLSX, modèles à variables, back-pressure, reprise | Batch massif stable |
| **M4 — Multi-session & v5.0** | Sessions multiples, routage, congestion_state, broadcast_sm, TLV v5.0 | v3.4 + v5.0 complets |
| **M5 — Numéros & export** | Génération par pays, exports CSV/XLSX/JSON, statistiques/tableaux de bord | Fonctions demandées complètes |
| **M6 — Sécurité & packaging** | Chiffrement secrets, TLS, signature/notarisation, updater, CI multi-OS | Binaires signés Win/macOS/Linux |
| **M7 (option)** | Simulateur SMSC intégré (rusmpps), tests de charge automatisés | Banc de test complet |

---

## 23. Annexes

### 23.1 Références normatives et techniques

- **SMPP v3.4**, *Short Message Peer to Peer Protocol Specification v3.4, Issue 1.2* — smpp.org (`SMPP_v3_4_Issue1_2.pdf`).
- **SMPP v5.0**, *Short Message Peer-to-Peer Protocol Specification Version 5.0* — smpp.org (`SMPP_v5.pdf`).
- **SMPP v5 Overview** — smpp.org/smpp-v5.html (congestion_state, broadcast, TLV additionnels).
- **GSM 03.38** — alphabet 7-bit et table d'extension.
- **E.164** — plan de numérotation international de l'UIT-T.
- **libphonenumber** — métadonnées et validation des numéros (port Rust : crate `phonenumber`).

### 23.2 Bibliothèques Rust clés

- **rusmpp** / **rusmppc** — implémentation SMPP v5 (codec, PDU, client) — github.com/JadKHaddad/Rusmpp.
- **tokio**, **tokio-rustls** — runtime asynchrone et TLS.
- **governor** — limitation de débit (GCRA / token bucket).
- **calamine** (lecture XLSX), **rust_xlsxwriter** (écriture XLSX), **csv** — fichiers tabulaires.
- **phonenumber** — validation/génération de numéros.
- **sqlx** / **rusqlite** — persistance SQLite.
- **serde**, **tracing**, **keyring**, **aes-gcm**, **argon2** — sérialisation, traces, secrets.

### 23.3 Valeurs par défaut recommandées

| Paramètre | Défaut |
|-----------|--------|
| Port SMPP | 2775 (non TLS) / 3550 (TLS courant) |
| `window_size` | 50 |
| `throughput_tps` | 100 |
| `enquire_link_interval` | 30 s |
| `response_timeout` | 10 s |
| Back-off reconnexion | 1 s → 60 s (exponentiel + jitter) |
| Encodage par défaut | GSM 7-bit (bascule auto UCS2) |
| `registered_delivery` | 1 (DLR final) |
| Rétention logs | 90 jours (configurable) |

### 23.4 Matrice de compatibilité des versions

| Fonction | v3.4 | v5.0 |
|----------|:----:|:----:|
| bind TX/RX/TRX, submit_sm, deliver_sm, DLR | ✔ | ✔ |
| submit_multi, data_sm, query/cancel/replace | ✔ | ✔ |
| Concaténation (UDH / sar_* TLV) | ✔ | ✔ |
| `congestion_state` (contrôle de flux) | — | ✔ |
| `broadcast_sm` / query / cancel (diffusion cellulaire) | — | ✔ |
| TLV de portabilité & identification de nœud | — | ✔ |
| `ussd_service_op`, `billing_identification` | — | ✔ |

---

*Fin du document — Spécifications Techniques Client SMPP Multiplateforme v1.0.*
