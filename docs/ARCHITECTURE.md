# OneClick KVM — Architecture

> Version : 0.1 — 2026-05-20
> Cible : Windows 10/11 x64 uniquement (premier jalon)
> Stack : Rust (workspace Cargo) + Tauri 2.x (UI Web embarquée)

---

## 1. Vue d'ensemble

OneClick KVM est une application **peer-to-peer** permettant à un PC « maître » de
contrôler jusqu'à **10 PCs** (mode KM) ou **5 PCs avec affichage** (mode KVM)
via le réseau local, sans matériel dédié.

Chaque instance de l'application est **symétrique** : elle peut jouer le rôle
de *Host* (capture clavier/souris/audio, envoie aux pairs) ou de *Client*
(reçoit et injecte les événements localement), parfois les deux simultanément
selon la topologie configurée.

```
   ┌──────────┐         LAN (TCP/UDP, IPv4/IPv6, AES-256-GCM)
   │  PC #1   │◄──────────────────────────────────────────────┐
   │ (Host)   │                                               │
   │ K + M    │                                               │
   └────┬─────┘                                               │
        │ capture                                             │
        │ ▼                                                   │
   ┌────────────┐   ┌────────────┐   ┌────────────┐   ┌──────────────┐
   │  PC #2     │   │  PC #3     │   │  PC #4     │ … │   PC #10     │
   │ (Client)   │   │ (Client)   │   │ (Client)   │   │  (Client)    │
   │ inject K+M │   │ inject K+M │   │ inject K+M │   │  inject K+M  │
   └────────────┘   └────────────┘   └────────────┘   └──────────────┘
```

### 1.1 Modes de fonctionnement

| Mode      | Description                                                     | Cible    |
| --------- | --------------------------------------------------------------- | -------- |
| **KM**    | Partage clavier + souris uniquement                             | ≤ 10 PCs |
| **KVM**   | KM + streaming vidéo des écrans distants vers l'écran maître    | ≤ 5 PCs  |
| **Audio** | Capture loopback WASAPI d'un ou plusieurs PCs vers le maître    | ≤ 5 PCs  |
| **Mixte** | Combinaison à la carte (ex : KM sur 10, KVM sur 3, audio sur 2) | dépend   |

### 1.2 Topologies

- **Star** (par défaut) : un *Master* contrôle N *Clients*.
- **Mesh** : tous les pairs peuvent se contrôler mutuellement (selon ACL).
- **Cascade** : un *Client* peut relayer vers d'autres (réservé v2+).

---

## 2. Composants logiques

L'application est découpée en **crates Cargo indépendantes** dans un workspace.
Chaque crate a une responsabilité unique et expose un trait public testable
en isolation.

```
oneclick-kvm/
├── crates/
│   ├── okvm-core/           # Types partagés, erreurs, identifiants
│   ├── okvm-protocol/       # Sérialisation, opcodes, framing binaire
│   ├── okvm-crypto/         # AES-256-GCM, X25519, HKDF, Ed25519
│   ├── okvm-net/            # TCP/UDP, IPv6, transport chiffré, heartbeat
│   ├── okvm-discovery/      # mDNS-like, broadcast UDP, pairing manuel
│   ├── okvm-input-capture/  # Hooks Win32 WH_KEYBOARD_LL / WH_MOUSE_LL
│   ├── okvm-input-inject/   # SendInput, mouse_event, keybd_event
│   ├── okvm-switch/         # Edge detection, grille d'écrans, hotkeys
│   ├── okvm-clipboard/      # Sync clipboard (texte/RTF/HTML/image)
│   ├── okvm-files/          # Drag&drop seamless, transfert multi-thread
│   ├── okvm-audio/          # WASAPI loopback capture + playback
│   ├── okvm-video/          # DXGI Desktop Duplication + H.264/HEVC
│   ├── okvm-wol/            # Wake-on-LAN magic packet
│   ├── okvm-power/          # Lock/Unlock, sleep, reboot
│   ├── okvm-logging/        # tracing + Windows Event Log writer
│   ├── okvm-config/         # Settings, import/export, profils
│   ├── okvm-i18n/           # Catalogues de traductions (fluent)
│   └── okvm-ipc/            # Commands Tauri ↔ backend Rust
├── app/                     # Frontend Tauri (HTML/CSS/JS - framework TBD)
├── docs/
├── assets/
└── service/                 # (optionnel v2) service Windows pour démarrage auto
```

### 2.1 Dépendances entre crates

```
                       ┌─────────────┐
                       │  okvm-core  │  (zéro dépendance interne)
                       └──────┬──────┘
                              │
        ┌──────────┬──────────┼──────────┬─────────────┐
        ▼          ▼          ▼          ▼             ▼
   okvm-crypto  okvm-i18n  okvm-config  okvm-logging  okvm-protocol
                                                          │
                                                          ▼
                                                     okvm-net
                                                          │
                ┌────────────┬──────────┬─────────────────┼──────────────┐
                ▼            ▼          ▼                 ▼              ▼
        okvm-input-      okvm-      okvm-           okvm-discovery   okvm-files
        capture/inject   clipboard  audio/video                      okvm-wol/power
                            │
                            ▼
                       okvm-switch
                            │
                            ▼
                       okvm-ipc  ◄──── Tauri commands ────►  app/ (UI)
```

### 2.2 Threading model

- **Tokio multi-thread runtime** pour tout le réseau, l'IPC et les tâches I/O.
- **Threads OS dédiés** (pas Tokio) pour :
  - Le hook clavier/souris global (`WH_*_LL` exigent un `MessageLoop` natif).
  - La capture DXGI (boucle de présentation 60+ FPS, hors runtime async).
  - Le WASAPI loopback (callbacks temps réel à faible latence).
- **Channels `tokio::sync::mpsc`** entre threads OS et runtime async.

---

## 3. Cycle de vie d'une session

```
┌──────────────┐   1. Discovery (mDNS + UDP broadcast)
│   Host PC    │ ───────────────────────────────────────►
└──────┬───────┘                                          ┌──────────────┐
       │   2. TCP connect + TLS-like handshake            │  Client PC   │
       │ ◄───────────────────────────────────────────────►│              │
       │      a. ClientHello (random, X25519 pub)         │              │
       │      b. ServerHello (random, X25519 pub, sig)    │              │
       │      c. Pairing PIN (premier appairage seulement)│              │
       │      d. Derive AES-256 key via HKDF              │              │
       │                                                  │              │
       │   3. Capabilities exchange                       │              │
       │      (modes supportés, écrans, audio, version)   │              │
       │                                                  │              │
       │   4. Session établie                             │              │
       │      ↓ INPUT events (KM/KVM)                     │              │
       │      ↓ CLIPBOARD events                          │              │
       │      ↓ FILE chunks (canal #2)                    │              │
       │      ↓ AUDIO frames (canal UDP)                  │              │
       │      ↓ VIDEO frames (canal UDP, FEC)             │              │
       │      ↑ HEARTBEAT (toutes les 2s)                 │              │
       │                                                  │              │
       │   5. Teardown                                    │              │
       └──────────────────────────────────────────────────┘
```

### 3.1 Canaux

Une session utilise **plusieurs canaux logiques** multiplexés :

| Canal | Transport | Usage                                    | Garantie         |
| ----- | --------- | ---------------------------------------- | ---------------- |
| #0    | TCP       | Contrôle (handshake, capabilities, ping) | Fiable, ordonné  |
| #1    | TCP       | Événements input + clipboard             | Fiable, ordonné  |
| #2    | TCP       | Transfert fichiers (parallèle #1)        | Fiable, ordonné  |
| #3    | UDP       | Audio frames                             | Best-effort      |
| #4    | UDP       | Vidéo frames + FEC                       | Best-effort, FEC |

Le multiplexage TCP se fait au niveau **frame** (voir `PROTOCOL.md` §3).
Les canaux UDP sont des sockets séparés avec leur propre nonce AES.

---

## 4. Modèle d'identité et d'appairage

### 4.1 Identité d'un PC

Chaque installation génère à l'initialisation :

- Une paire **Ed25519** d'identité long-terme (`device_id_priv`, `device_id_pub`).
- Un nom convivial (par défaut : nom NetBIOS du PC).
- Un UUID v7 stocké dans `%APPDATA%\OneClickKVM\identity.json`.

L'empreinte affichée à l'utilisateur est : `SHA-256(device_id_pub)` tronquée
sur 8 mots de 4 hex (style WireGuard / SSH).

### 4.2 Appairage

Première connexion = **TOFU + PIN à 6 chiffres** affiché côté Host, à saisir
côté Client (ou vice versa selon qui initie). Le PIN sert à dériver une clé
de session **éphémère** qui signe les clés publiques échangées. Une fois
appairés, les pairs se reconnaissent par leur empreinte Ed25519 sans re-PIN.

Les empreintes connues sont stockées dans `%APPDATA%\OneClickKVM\peers.json`.
Tout changement de clé publique déclenche un **warning critique** côté UI
(comme `known_hosts` SSH).

### 4.3 ACL (Access Control List)

Par pair appairé, l'utilisateur configure :

```
{
  "peer_fingerprint": "abc1 23de f456 ...",
  "name": "PC du salon",
  "permissions": {
    "input": "allow",            // allow | deny | prompt
    "clipboard_text": "allow",
    "clipboard_image": "prompt",
    "files_inbound": "prompt",
    "files_outbound": "allow",
    "audio_capture": "deny",
    "video_capture": "deny",
    "wol": "allow",
    "lock_unlock": "allow"
  }
}
```

---

## 5. Choix techniques majeurs

### 5.1 Frontend (Tauri)

- **Tauri 2.x** : webview système (WebView2 sur Windows), binaire final < 15 MB.
- Framework UI : **à choisir** entre :
  - **Svelte** (recommandé, taille minimale, ergonomie)
  - **SolidJS** (alternative ultra-rapide)
  - **HTML+TypeScript vanilla** (zéro dépendance, plus lent à coder)
- Communication backend ↔ frontend : **Tauri commands** + **events**.

### 5.2 Crates Rust principales (dépendances externes)

| Besoin                | Crate                    | Raison                                                |
| --------------------- | ------------------------ | ----------------------------------------------------- |
| Async runtime         | `tokio`                  | Standard de facto                                     |
| Sérialisation         | `serde`, `bincode` v2    | Compact, rapide, schéma versionnable                  |
| Crypto AEAD           | `aes-gcm`                | AES-256-GCM, audité, pure-Rust                        |
| Crypto asymétrique    | `x25519-dalek`, `ed25519-dalek` | Standard, audité                              |
| KDF                   | `hkdf`                   | HKDF-SHA256                                           |
| Hash                  | `sha2`, `blake3`         | SHA-256 pour empreintes, BLAKE3 pour fichiers         |
| Windows API           | `windows` (officiel MS)  | Bindings générés, à jour                              |
| mDNS                  | `mdns-sd`                | Pure Rust, sans dépendance Avahi                      |
| Audio                 | `cpal` + `windows`       | CPAL pour playback ; loopback WASAPI direct via windows-rs |
| Vidéo capture         | `windows-capture`        | Wrapper DXGI Desktop Duplication                      |
| Vidéo encoding        | `mfx_dispatch` / `nvenc` / `mediafoundation-rs` | Hardware-accelerated H.264/HEVC      |
| Clipboard             | `arboard` + Win32 direct | Arboard ne couvre pas tout (RTF, HTML, multi-format)  |
| Logs                  | `tracing` + `tracing-subscriber` | Structured logs                               |
| Event Log Windows     | `eventlog`               | Écriture dans `Application` / source custom           |
| Erreurs               | `thiserror`              | Erreurs strongly-typed                                |
| Configuration         | `directories` + `serde_json` | XDG-like sur Windows (`%APPDATA%`)                |
| i18n                  | `fluent` + `fluent-bundle` | Système Mozilla, supérieur à gettext              |

### 5.3 Choix de chiffrement

Voir `SECURITY.md` pour le détail. Résumé :

- **AES-256-GCM** pour toutes les données en transit (data plane).
- **X25519-ECDH** pour l'échange de clés par session.
- **Ed25519** pour l'identité long-terme et la signature du handshake.
- **HKDF-SHA256** pour dériver les clés AES depuis le secret partagé.
- **Nonce** déterministe par direction (96 bits : 32-bit `epoch` + 64-bit counter).
- Rotation de clé toutes les 4 GB ou toutes les 24 h (la borne la plus tôt).

### 5.4 Protocole réseau

Voir `PROTOCOL.md`. Résumé :

- **Framing binaire** : `[len: u32 BE][channel: u8][nonce_counter: u64][payload + AEAD tag]`
- **Versionnable** : numéro de version dans le handshake, négociation min/max.
- **MTU-friendly UDP** : payload utile ≤ 1200 octets pour passer la plupart des MTU.
- **Backpressure** : window basée sur `tokio::sync::Semaphore` côté envoi.

---

## 6. Sécurité applicative

Voir `SECURITY.md` pour le modèle de menace complet. Points clefs :

- **Aucun secret en clair** sur le disque (master key dérivée + Windows DPAPI).
- **Pas de bypass UAC** : l'application tourne en *user-mode standard*.
  Wake-on-LAN, lock global et certaines APIs nécessitent des privilèges
  élevés ; ces modules détectent l'absence et désactivent la feature avec
  un message clair, plutôt que de demander une élévation silencieuse.
- **Validation systématique** des messages entrants (taille, opcodes, ACL).
- **Rate limiting** sur les événements input (max 10 000 ev/s).
- **Sandbox** : la WebView Tauri n'a accès qu'aux commandes IPC explicitement
  enregistrées. Pas de `fs`, pas de `http`, pas de `shell` exposés au frontend.

---

## 7. Performance — budgets cibles

| Métrique                              | Cible           | Mesure                              |
| ------------------------------------- | --------------- | ----------------------------------- |
| Latence input bout-en-bout (LAN GbE)  | < 5 ms p99      | Timestamp capture → injection       |
| Latence vidéo bout-en-bout (1080p60)  | < 30 ms p99     | Timestamp DXGI → présentation       |
| Latence audio bout-en-bout            | < 20 ms p99     | WASAPI capture → playback distant   |
| Throughput transfert fichier          | ≥ 800 Mbit/s    | Sur lien GbE, AES-NI activé         |
| CPU au repos (1 client connecté)      | < 1 %           | i7-12700H, idle                     |
| RAM au repos (1 client connecté)      | < 80 Mo         | RSS                                 |
| Taille binaire installé               | < 50 Mo         | Hors codecs hardware                |
| Démarrage à froid                     | < 1 s           | Premier `paint` UI                  |

---

## 8. Stratégie de tests

- **Tests unitaires** dans chaque crate (Rust `#[cfg(test)]`).
- **Tests d'intégration** : `crates/okvm-integration-tests/` avec deux instances
  back-to-back sur loopback (`127.0.0.1` et `::1`).
- **Fuzzing** sur le parser de protocole (`cargo fuzz` ciblant `okvm-protocol`).
- **Tests E2E** : scénarios manuels sur 2 VM Windows + 1 host physique.
- **Bench** : `criterion` pour le chiffrement, la sérialisation, la capture.

---

## 9. Roadmap d'implémentation

### Phase 1 — Fondations (cette itération)
1. ✅ Documents `ARCHITECTURE.md`, `PROTOCOL.md`, `SECURITY.md`.
2. Workspace Cargo + squelette Tauri 2.
3. `okvm-core` (types + erreurs) complet.
4. `okvm-protocol` (encode/decode + tests).
5. `okvm-crypto` (handshake + AEAD + tests).
6. `okvm-net` (transport chiffré + heartbeat + tests sur loopback).

### Phase 2 — KM basique
7. `okvm-input-capture` + `okvm-input-inject` (Win32 hooks).
8. `okvm-switch` (edge detection, hotkey, grille).
9. Premier client → host fonctionnel : on déplace la souris d'un PC à l'autre.

### Phase 3 — Confort
10. `okvm-clipboard` (texte/RTF/HTML/image).
11. `okvm-files` (drag&drop + transfert multi-thread).
12. `okvm-discovery` (auto-find sur LAN).
13. `okvm-wol` + `okvm-power`.

### Phase 4 — KVM complet
14. `okvm-audio` (loopback + playback).
15. `okvm-video` (DXGI + encoder hardware + decoder + présentation).
16. UI grille multi-écrans.

### Phase 5 — Pro
17. `okvm-logging` → Windows Event Viewer.
18. `okvm-i18n` (FR, EN, DE, ES, IT, PT, NL, JP, ZH).
19. Sauvegarde/restauration paramètres + profils.
20. Signature de code, installeur MSI, auto-update.

---

## 10. Décisions ouvertes

À trancher avec l'utilisateur en cours de route :

- [ ] **Framework UI** front Tauri : Svelte vs SolidJS vs vanilla TS.
- [ ] **Codec vidéo** par défaut : H.264 (compat) vs HEVC (ratio) vs AV1 (futur).
- [ ] **Niveau d'élévation** : demander UAC au démarrage, ou jamais ?
- [ ] **Mode service Windows** : démarrage auto sans session interactive (v2 ?).
- [ ] **Auto-update** : in-app updater (Tauri) ou MSI seul ?
- [ ] **Télémétrie** : aucune par défaut (recommandé), ou opt-in anonyme ?
- [ ] **Licence du code** : open source (MIT/Apache-2.0) ou propriétaire ?
