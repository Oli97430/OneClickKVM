# OneClick KVM

[![CI](https://github.com/Oli97430/OneClickKVM/actions/workflows/ci.yml/badge.svg)](https://github.com/Oli97430/OneClickKVM/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Oli97430/OneClickKVM?include_prereleases)](https://github.com/Oli97430/OneClickKVM/releases/latest)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#licence)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange?logo=rust)](https://rustup.rs)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-blue?logo=windows)](https://github.com/Oli97430/OneClickKVM/releases)

> Contrôle multi-PC chiffré pour Windows — partage clavier, souris, presse-papier,
> fichiers, audio et écran entre plusieurs ordinateurs sur le réseau local.

OneClick KVM est une application **peer-to-peer** qui permet à un PC « maître »
de contrôler jusqu'à 10 PCs avec un seul clavier et une seule souris (mode KM),
ou jusqu'à 5 PCs avec affichage vidéo de leur écran (mode KVM). Tout le trafic
est chiffré **AES-256-GCM** avec échange de clés **X25519 ECDH** (Perfect Forward
Secrecy) et signatures **Ed25519**.

## Caractéristiques principales

| Domaine | Fonctionnalité |
|---|---|
| **KM** | Clavier/souris partagés via bord d'écran ou hotkey `Ctrl+Alt+Win+1..9` |
| **Vidéo** | Capture écran via Windows Graphics Capture, encodage H.264 (openh264) |
| **Audio** | Capture WASAPI loopback, encodage **Opus 64 kbps** (~25× moins que PCM) |
| **Fichiers** | Drag & drop multi-thread + vérification BLAKE3 + sandbox path traversal |
| **Presse-papier** | Sync multi-format (texte UTF-8, RTF, HTML, PNG, fichiers) |
| **Découverte** | mDNS `_oneclick-kvm._tcp.local.` + broadcast UDP fallback |
| **Identité** | Ed25519 long-terme persistée via Windows **DPAPI** (user scope) |
| **UI** | Tauri 2 + Svelte 5, FR/EN complet + DE/ES/IT/PT/NL/JA/ZH partiel (auto-détection locale Windows) |
| **Système** | System tray, démarrage minimisé, hotkeys, autostart Windows |
| **Distribution** | Installeur NSIS 4 MB, install per-user sans admin |

## Architecture en un coup d'œil

```
oneclick-kvm/
├── crates/                       # Workspace Cargo (20 crates Rust)
│   ├── okvm-core/                # Types partagés, erreurs, identités
│   ├── okvm-crypto/              # AES-256-GCM, X25519, Ed25519, HKDF
│   ├── okvm-protocol/            # Framing binaire, opcodes, bincode
│   ├── okvm-net/                 # Transport TCP chiffré + handshake
│   ├── okvm-discovery/           # mDNS + UDP broadcast
│   ├── okvm-input-capture/       # Hooks Win32 WH_KEYBOARD_LL / WH_MOUSE_LL
│   ├── okvm-input-inject/        # SendInput
│   ├── okvm-switch/              # Edge detection, grille, hotkeys
│   ├── okvm-clipboard/           # Win32 clipboard multi-format
│   ├── okvm-files/               # Transfert + BLAKE3 + sandbox
│   ├── okvm-audio/               # cpal WASAPI loopback + Opus
│   ├── okvm-video/               # Windows Graphics Capture + H.264
│   ├── okvm-wol/                 # Wake-on-LAN magic packets
│   ├── okvm-power/               # LockWorkStation, Sleep, Shutdown
│   ├── okvm-logging/             # tracing + Windows Event Log
│   ├── okvm-config/              # Settings, peers, identity DPAPI
│   ├── okvm-i18n/                # Catalogues backend (Fluent-ready)
│   └── okvm-ipc/                 # DTOs Tauri ↔ Svelte
├── app/                          # Application Tauri
│   ├── src/                      # Frontend Svelte 5 (12 composants)
│   └── src-tauri/                # Backend Rust qui orchestre les crates
├── docs/
│   ├── ARCHITECTURE.md           # Vue d'ensemble système
│   ├── PROTOCOL.md               # Format binaire wire complet
│   └── SECURITY.md               # Modèle de menace + choix crypto
└── README.md                     # Ce fichier
```

## Installation

### Pour utiliser (Windows 10/11 x64)

Télécharge la dernière release depuis
[github.com/Oli97430/OneClickKVM/releases](https://github.com/Oli97430/OneClickKVM/releases)
(fichier `OneClick KVM_0.1.0_x64-setup.exe`, ~4 MB), double-clic, install
en mode user (pas d'admin requis). L'app se lance via le menu Démarrer.

> À la première ouverture, Windows demande l'autorisation pour les **réseaux
> privés** — coche-la pour permettre la découverte mDNS sur le LAN.

### Pour développer

Prérequis :

- **Rust** stable 1.80+ (toolchain MSVC sur Windows : `rustup default stable-x86_64-pc-windows-msvc`)
- **Node.js** 22+ et **pnpm** 10+
- **WebView2 Runtime** (préinstallé sur Windows 11, sinon télécharger depuis Microsoft)

Clone et démarre en mode dev (Vite + Tauri avec hot reload) :

```bash
git clone https://github.com/Oli97430/OneClickKVM.git
cd OneClickKVM/app
pnpm install
pnpm tauri:dev
```

Build release + installeur NSIS :

```bash
cd oneclick-kvm/app
pnpm tauri:build
# Produit : src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/OneClick KVM_X.Y.Z_x64-setup.exe
```

Tests :

```bash
# Tous les crates Rust (86 tests, dont tests d'intégration loopback handshake + UDP+FEC)
cd oneclick-kvm
cargo test --workspace

# Vérification TypeScript / Svelte
cd app
pnpm exec svelte-check --tsconfig ./tsconfig.json
```

## Utilisation

### Cas typique : 2 PCs sur le même LAN

1. Lancer OneClick KVM sur les 2 PCs.
2. Sur chacun : clic **"En écoute"** → mDNS commence à annoncer.
3. Après quelques secondes, les pairs apparaissent dans la section
   "Pairs détectés" avec un badge "Visible LAN".
4. Sur le PC qui veut être maître : clic **"Appairer"** sur la carte de l'autre →
   handshake AES-256-GCM → la session apparaît.
5. Clic **"Activer master"** → les hooks Win32 capturent clavier/souris.
6. Glisser la souris au bord droit de l'écran → le curseur traverse vers
   l'autre PC. Bouger/taper sur PC1 → action sur PC2.
7. `Ctrl+Alt+Win+0` → retour curseur sur PC1.

### Drag & drop fichier

1. Glisser un fichier ou dossier sur la fenêtre OneClick KVM.
2. Sélectionner le pair cible dans le dropdown.
3. Lâcher → transfert avec barre de progression, BLAKE3 vérifié.
4. Réception dans `Documents/OneClickKVM/Inbox/`.

### Partage audio / écran

Clic **"Partager audio"** ou **"Partager écran"** dans le StatusBar.
L'autre pair entend le son / voit l'écran en direct dans son panneau
"Écrans partagés".

## Sécurité

Voir [docs/SECURITY.md](docs/SECURITY.md) pour le modèle de menace complet.
Résumé :

- **Chiffrement** : AES-256-GCM pour toutes les données en transit.
- **Échange de clés** : X25519 ECDH par session (Perfect Forward Secrecy).
- **Authentification** : Ed25519 long-terme + transcript hash signé.
- **Anti-replay** : compteur nonce monotone + bitmap glissant pour UDP.
- **Identité** : seed Ed25519 stockée via DPAPI (user scope) — un autre user
  Windows ou une autre machine ne peut pas la lire.
- **Pas d'admin requis** : l'app tourne en user-mode, les hooks Win32
  globaux fonctionnent sans élévation (limitation : pas d'interception
  pour fenêtres élevées par UAC).

## Dépannage

Problèmes courants → [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md)
(découverte LAN, pairing, capture clavier, audio crépitement, SmartScreen).

## Tester soi-même (2 instances locales)

Tu peux lancer **2 instances OneClick KVM sur la même machine** pour
valider le scénario complet appairage + KM + audio + écran + fichiers
sans 2 PC physiques. Variable d'env `OKVM_INSTANCE` isole les configs.

Le plus rapide (avec `just` installé) :

```powershell
just dev-alice   # terminal 1
just dev-bob     # terminal 2
# OU avec les EXE release post-build
just run-2       # ouvre 2 fenêtres alice+bob
```

Guide pas-à-pas + 7 scénarios E2E : [docs/TESTING.md](docs/TESTING.md).

## Statut

Version actuelle : **0.1.1** (alpha, prêt pour usage personnel sur LAN de confiance).

- ✅ 30+ fonctionnalités implémentées (KM, audio, vidéo, fichiers, clipboard, WoL, etc.)
- ✅ 91 tests unitaires + intégration passants (`RUSTFLAGS=-D warnings` strict)
- ✅ Compile cleanly, build release 17 MB, installeur NSIS 4 MB
- ✅ Audio routé sur **UDP+FEC** sur git HEAD (V3.1 livré dans le code,
  pas encore inclus dans la release publiée v0.1.1)
- ⚠️ **Pas testé E2E sur 2 vrais PCs** — l'app a tourné côté UI mais
  jamais en condition d'appairage / partage réels. Tous les tests sont
  loopback (un process talking to itself sur 127.0.0.1).
- ⚠️ Pas de signature Authenticode (cf. note ci-dessus) — SmartScreen avertit,
  vérifier le SHA-256 publié sur la release
- ⚠️ Vidéo en software H.264 (CPU) — V3.3 ajoutera Media Foundation hardware
- ⚠️ Release publiée v0.1.1 = audio TCP. UDP+FEC sera dans v0.1.2.

## Roadmap V3

- [ ] Hardware H.264 / NVENC / AMF / QuickSync via Media Foundation
- [ ] UDP + Reed-Solomon FEC pour audio/vidéo (latence p99 < 30ms cible)
- [ ] Auto-update Tauri (delta updates depuis GitHub releases)

> **Scope** : OneClick KVM est **Windows-only par design**. Le code s'appuie
> en profondeur sur Win32 (DPAPI, WH_KEYBOARD_LL, SendInput, WASAPI loopback,
> Windows Graphics Capture, Media Foundation). Un portage macOS/Linux n'est
> **pas** prévu — les besoins KVM cross-OS sont déjà très bien couverts par
> [Barrier](https://github.com/debauchee/barrier) ou
> [Input Leap](https://github.com/input-leap/input-leap).
>
> **Code signing Authenticode** n'est **pas** prévu (~300 €/an de cert
> récurrent pour un projet personnel ne vaut pas le coût). Conséquence
> assumée : SmartScreen affiche "Application non reconnue" au premier
> lancement. Les utilisateurs vérifient l'intégrité via le **SHA-256**
> publié à chaque release (cf. `sha256.txt`).

## Licence

`MIT OR Apache-2.0` — contributions bienvenues.

Le code embarque :

- **openh264** (Cisco) — BSD-2-Clause
- **windows-rs** — MIT/Apache-2.0
- **Tauri 2** — MIT/Apache-2.0
- **Svelte 5** — MIT
- **audiopus** + **libopus** (Xiph) — BSD-3-Clause
