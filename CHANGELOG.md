# Changelog

Toutes les modifications notables de OneClick KVM sont documentées ici.

Format basé sur [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/),
versions sémantiques [SemVer](https://semver.org/lang/fr/).

## [0.1.0] — 2026-05-21

Première release publique (alpha). Prêt pour usage personnel sur LAN de confiance.

### Ajouté — Clavier / souris partagés (KM)

- Capture Win32 globale (`WH_KEYBOARD_LL`, `WH_MOUSE_LL`) sur thread dédié
  avec pompe `GetMessageW`.
- Injection via `SendInput` avec `MOUSEEVENTF_VIRTUALDESK` (multi-écrans).
- Basculement transparent par bord d'écran configurable.
- Hotkeys `Ctrl+Alt+Win+1..9` pour cibler un pair, `Ctrl+Alt+Win+0` pour
  revenir sur la machine maître.
- Grille spatiale modifiable (drag & drop visuel).

### Ajouté — Vidéo (KVM)

- Capture par Windows Graphics Capture API (`windows-capture` crate).
- Encodage **H.264** software via openh264 0.8 (Cisco BSD-2-Clause),
  fallback MJPEG si l'encodeur échoue à l'init.
- Keyframe forcée toutes les 2 secondes.
- Décodage côté pair + ré-encodage JPEG pour affichage WebView2.

### Ajouté — Audio

- Capture WASAPI **loopback** via `cpal` (audio du PC entier).
- Encodage **Opus 64 kbps** (~25× moins que PCM brut), fallback PCM si
  fréquence d'échantillonnage non standard.
- Playback sur device output par défaut, ring buffer 2 secondes.

### Ajouté — Fichiers

- Transfert multi-thread avec sémaphore (4 connexions parallèles par défaut).
- Vérification **BLAKE3** sur chaque fichier reçu.
- Sandbox path traversal : refuse `..`, chemins absolus, symlinks externes.
- Progression temps réel (throttle 4Hz vers l'UI).
- Drag & drop directement dans la fenêtre OneClick KVM.
- Réception dans `Documents/OneClickKVM/Inbox/`.

### Ajouté — Presse-papier

- Sync multi-format : texte UTF-8, RTF, HTML, image PNG, listes de fichiers.
- Poll polling 10Hz sur clipboard owner change.

### Ajouté — Découverte LAN

- mDNS sur `_oneclick-kvm._tcp.local.` (compatible Bonjour / Avahi).
- Broadcast UDP fallback sur port 47100.
- Auto-reconnexion aux pairs connus dès leur réapparition.

### Ajouté — Sécurité

- Chiffrement transport **AES-256-GCM** (nonce `epoch||counter`, monotone).
- Échange de clés **X25519 ECDH** par session (Perfect Forward Secrecy).
- Identité long-terme **Ed25519** signant le transcript hash.
- Anti-replay : compteur nonce + bitmap glissant (UDP).
- Identité **Ed25519** persistée chiffrée via Windows **DPAPI** (user scope) —
  migration automatique de l'ancien format clair `identity.seed`.

### Ajouté — UI

- Tauri 2 + Svelte 5 + Vite 6.
- Traduction complète FR/EN, partielle DE/ES/IT/PT/NL/JA/ZH.
- Auto-détection de la langue Windows via `GetUserDefaultLocaleName`.
- Thème **System / Light / Dark** avec preview live dans Settings.
- Persistance position + taille fenêtre entre sessions (avec fallback si
  hors écran).
- System tray avec menu Ouvrir / Quitter, fermeture = hide (l'app reste
  dans le tray).
- Carte de bienvenue first-run.
- Panneau À propos avec empreinte cryptographique copiable.
- Notifications toast (info / success / warn / error).

### Ajouté — Système

- Démarrage automatique Windows (registry `Run` key, per-user).
- Démarrage minimisé.
- Logs `tracing` JSON + sink Windows **Event Log** (source `OneClickKVM`).
- Option "redact_logs" : masque les payloads sensibles si jamais loggés.

### Ajouté — Réseau

- Transport TCP chiffré bidirectionnel, framing binaire bincode v2.
- Dual-stack IPv6/IPv4 par défaut (`[::]:47101`).
- 9 tâches par session (encoders, writer, reader, heartbeat, shutdown).
- Heartbeat 5 secondes, timeout 15 secondes.

### Ajouté — Bonus

- **Wake-on-LAN** : envoi de magic packet à un pair endormi.
- **Lock workstation / Sleep / Shutdown** (commandes Win32) déclenchables
  depuis un pair autorisé.

### Distribution

- Installeur **NSIS** 4 MB, install per-user (pas d'admin requis).
- Compatible Windows 10/11 x64.

### Tests

- 62 tests unitaires passants.
- 1 test d'intégration loopback (handshake AES + Ping/Pong via TCP).

### Limitations connues

- Pas de signature Authenticode (SmartScreen avertit au premier lancement).
- Vidéo software (CPU) — V3 ajoutera Media Foundation hardware.
- Audio en TCP — V3 → UDP + FEC pour basse latence.
- PIN flow d'appairage côté serveur en attente d'implémentation stricte.

## [0.1.1] — 2026-05-22

Release CI-built reproductible. Aucun changement de comportement utilisateur
par rapport à v0.1.0 ; valide le pipeline GitHub Actions
(test + fmt + clippy informational + auto-bundle NSIS).

### Modifié

- **Build reproductible** : installeur produit par GitHub Actions (runner
  Windows public) au lieu d'un build local. Garantit la même chaîne d'outils
  et permet l'audit.
- **Workspace lints** : `[workspace.lints]` centralisé dans `Cargo.toml`
  racine, hérité par tous les crates via `[lints] workspace = true`.
- **`.gitattributes`** : force LF cross-plateforme (évite que
  `cargo fmt --check` casse sur CI Windows à cause d'autocrlf).
- **`cargo fmt --all`** appliqué : 33 fichiers re-formattés selon
  `rustfmt.toml`.

### Infra OSS

- CI GitHub Actions Windows (`cargo fmt --check`, `cargo test --workspace`,
  `svelte-check --fail-on-warnings`).
- Release auto sur tag `v*.*.*` (build NSIS + SHA-256 + sig Ed25519
  optionnelle + manifest `latest.json` pour auto-updater).
- Dependabot hebdo (semver-major ignoré, groupes crypto/tokio/windows/...).
- Templates issues (bug, feature, config) + PR.
- `CONTRIBUTING.md`, `SECURITY.md` racine, `.editorconfig`, `rustfmt.toml`.

## [Unreleased] — V3 en cours

### Ajouté

- **PIN flow strict côté serveur** : nouveau mode d'appairage à activer
  explicitement, génère un PIN à 6 chiffres valide 60 secondes. Toute
  identité inconnue sans PIN valide est rejetée avec `PairingFailed`.
  Bannière dédiée dans l'UI avec compte à rebours.
- **Nouveau crate `okvm-udp`** : transport UDP chiffré (AES-256-GCM) avec
  Reed-Solomon FEC (codec configurable K + M). Tests d'intégration loopback
  couvrant duplication K=1/M=1, reconstitution K=4/M=2 avec 2 paquets perdus,
  et drop gracieux quand trop de paquets sont perdus. Pas encore câblé dans
  les pipelines audio/vidéo (V3.1).
- **Détection Media Foundation H.264** : énumération des encodeurs MFT au
  démarrage, distinction HW/SW. Sur les machines avec NVENC / QuickSync /
  AMF, c'est loggé et exposé dans AboutView.
- **Wrapper MFT H.264 encoder** (`MfH264Encoder`) : COM init via OnceLock,
  pipeline complet `CoCreateInstance(CLSID_CMSH264EncoderMFT)` →
  `SetOutputType(H264)` → `SetInputType(NV12)` → `NOTIFY_BEGIN_STREAMING` +
  `NOTIFY_START_OF_STREAM` → `ProcessInput` / `ProcessOutput` loop avec
  gestion `MF_E_TRANSFORM_NEED_MORE_INPUT`, plus une méthode `drain()` qui
  émet `COMMAND_DRAIN` pour récupérer les NAL restants. Conversion
  RGB → NV12 BT.601 limited-range pure Rust incluse. Tests : init, drain
  d'un keyframe IDR avec start code Annex-B vérifié. L'API publique restera
  inchangée quand on basculera sur les MFT hardware via D3D11Manager (V3.3).
- **Scripts release + doc signature** : `scripts/release.ps1` automatise
  build → signtool Authenticode → SHA-256 → signature Ed25519
  (`tauri-plugin-updater`) → manifeste `latest.json`. Tout est paramétré par
  variables d'environnement, et est no-op si rien n'est défini. Procédure
  complète documentée dans `docs/RELEASE.md`.

### Modifié

- README mentionne maintenant l'auto-détection de la langue Windows.
- Catalogues i18n élargis : nouvelles clés `pairing.*` pour la bannière
  d'appairage strict.

### Sécurité (durcissements post code-review)

- **Anti-brute-force PIN** : compteur `failed_attempts` sur `PairingMode` ;
  le mode d'appairage est désactivé automatiquement après 5 tentatives
  ratées. Une demande sans PIN compte aussi comme tentative. Le lock est
  conservé pendant tout le check + l'incrément pour éviter les attaques
  en parallèle.
- **Zeroize PIN** : `PairingMode.pin` est wrappé dans `zeroize::Zeroizing<String>`
  pour effacer la mémoire à la destruction (defense in depth).
- **Cap pending shards** : `UdpFecReceiver.pending` est plafonné à 256
  entrées avec éviction FIFO — protège contre une attaque "spray-orphan-shards"
  qui ferait grossir la map indéfiniment. Mémoire bornée ≈ 8.6 MB pire cas.
- **MF init unifié** : `mediafoundation::ensure_mf_init()` (OnceLock partagé)
  remplace les `MFStartup` répétés qui accumulaient des ref-counts internes.
- **Stabilité du wire-format config** : `H264BackendChoice` utilise
  `#[serde(rename, alias)]` pour les noms kebab-case + alias PascalCase
  legacy, permettant de renommer les variantes Rust sans casser les
  `config.json` existants.

## Roadmap

Voir [README.md#roadmap-v3](README.md#roadmap-v3).
