# Changelog

Toutes les modifications notables de OneClick KVM sont documentées ici.

Format basé sur [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/),
versions sémantiques [SemVer](https://semver.org/lang/fr/).

## [Unreleased] — non publié sur GitHub Releases

### Ajouté — V3.3.1 : vrai hardware encoding NVENC/AMF/QSV

**Premier vrai GPU H.264 encoding** sur les MFTs async-mode (NVIDIA,
AMD, Intel récents). Avant : Microsoft AVC DX12 sync seulement, qui
est un fallback. Maintenant : si NVENC est dispo, c'est ce qui est
utilisé pour le partage écran.

- Nouveau module `okvm_video::mediafoundation_async` (~660 lignes) :
  wrapper async-mode MFT avec `IMFAsyncCallback` (via `#[implement]`
  macro de windows-core), worker thread dédié, event loop avec
  re-arm `BeginGetEvent` après chaque event.
- API publique stable : `MfH264AsyncEncoder::try_new` /
  `encode_rgb` / `force_keyframe` / `drain` — même contrat que
  `MfH264Encoder` sync.
- Wired dans `AnyH264Encoder` (`win32.rs`) : essai async en priorité,
  fallback transparent vers sync si pas de GPU support.

**Validation E2E** : 3 tests (`mediafoundation_async::tests::*`)
- `try_new_async_does_not_panic` — init OK (NVENC activé sur machine de dev)
- `encode_synthetic_frames_produces_nal` — 60 frames → ~125 KB de
  bitstream Annex-B avec start codes valides
- `encode_async_then_decode_with_openh264` — **decoder de référence
  indépendant openh264 décode le bitstream NVENC** → preuve absolue de
  conformité H.264. Width/height préservés (320×240).

**Régression évitée** : V3.3.1 step 1 avait un transmute hack pour
re-armer `BeginGetEvent` depuis l'intérieur de `Invoke` qui causait
STATUS_ACCESS_VIOLATION dès la 1ère frame. Fix : re-arm depuis le
worker thread qui détient `bridge: IMFAsyncCallback` + `event_gen:
IMFMediaEventGenerator` en upvars.

### Ajouté — Diagnostic & robustesse

- **File logger rotatif** : `tracing-appender` 0.2 ajouté, nouvelle
  fonction `okvm_logging::init_with_file()` qui écrit les events JSON
  dans `%LocalAppData%\OneClick\OneClickKVM\data\logs\app.log.<date>`
  (rotation quotidienne, non-bloquant via worker thread).
- **Bouton "Ouvrir les logs"** dans Settings → Dossiers, à côté des
  boutons existants "Open config" et "Open inbox". Permet de partager
  les logs JSON en cas de bug sans naviguer dans le filesystem.
- **Boot tracing détaillé** : 4 `tracing::info!` aux étapes critiques
  (démarrage, mf-boot-probe spawn, AppState init début/fin) — toutes
  visibles dans le fichier de log. La prochaine régression boot sera
  diagnosticable en 1 lecture.

### Corrigé — Anti-régression apartment COM

- `okvm_video::ensure_mf_init` a maintenant un `debug_assert!` qui
  fire si appelé depuis un thread sans nom (heuristique = main thread).
  Cargo tests ont des threads nommés → pas de faux positifs.
- Doc explicite que ce call empoisonne le thread courant en MTA et
  comment l'appeler depuis une app GUI (toujours `std::thread::spawn`).
- Référence historique au crash v0.1.2 dans la doc pour traçabilité.
- Le spawn dans `lib.rs::run()` est maintenant nommé `mf-boot-probe`
  (évite le debug_assert + facilite le grep dans les logs).

### CI infra

- Bump GitHub Actions vers versions Node.js 24 :
  `actions/checkout@v6`, `actions/setup-node@v6`, `actions/cache@v5`,
  `pnpm/action-setup@v6`, `softprops/action-gh-release@v3`.
  Supprime le warning Node.js 20 deprecation vu dans la build
  v0.1.2 release.

## [0.1.2] — 2026-05-23

**Bugfix release** : corrige un crash silencieux au boot introduit par
la V3.3 dans `main` post-v0.1.1. **Recommandée pour tous les utilisateurs**
qui ont essayé un build récent et ont vu l'app planter au lancement.

### Corrigé — CRASH au boot (régression V3.3)

- **Cause** : `okvm_video::log_hardware_h264_status()` appelait
  `ensure_mf_init()` qui fait `CoInitializeEx(MULTITHREADED)` **sur le
  main thread**. Empoisonnait l'apartment COM avant que `tao`
  (event loop de Tauri) ne tente `OleInitialize(STA)` → panic
  `RPC_E_CHANGED_MODE` ~3 secondes plus tard. Côté utilisateur : fenêtre
  s'ouvre brièvement puis l'app crash silencieusement (windows_subsystem
  = "windows" supprime stdout/stderr en release).
- **Fix** : `log_hardware_h264_status()` est appelé dans
  `std::thread::spawn` — le main thread reste libre pour Tauri/STA.
- **Bonus** : `install_panic_hook()` écrit désormais tout panic +
  backtrace dans `%LocalAppData%\Temp\oneclick-kvm-crash.log`. Plus de
  crash invisible. Active `RUST_BACKTRACE=full` automatiquement.

### Ajouté — depuis 0.1.1 (consolidation du backlog `main`)

> Validé par tests Rust + loopback, **non testé E2E sur 2 vrais PCs**.

#### V3.1 audio UDP+FEC bout-en-bout

- `okvm-udp` crate : Reed-Solomon FEC + AEAD + framing (13 tests).
- Négociation UDP au handshake (`ServerFinished.udp_ports` + HKDF
  epoch=1 séparé du TCP epoch=0).
- `okvm_net::UdpAudioPipe` : sender + receiver tasks qui bridgent
  `mpsc<AudioMessage>` ⇄ UDP+FEC chiffré.
- NAT pinning auto : `UdpFecReceiver::recv_frame` remonte la
  `SocketAddr`, le serveur pin l'endpoint UDP au 1er decrypt réussi.
- `Session::start_with_udp` + Listener + Connector auto-détection
  (fallback transparent TCP si bind UDP échoue).

#### V3.3 chemin MFT hardware (limité)

- `d3d11_helper.rs` : `D3D11Resources` + `IMFDXGIDeviceManager`.
- `MfH264Encoder::try_new_hardware` : itère `MFTEnumEx(HARDWARE)`,
  prend le 1er sync. `MFT_MESSAGE_SET_D3D_MANAGER` avant config.
- `MfH264Encoder::new_best` : HW puis fallback software.
- `probe_best_backend` : diagnostic non bloquant pour AboutView,
  cache `OnceLock` process-wide.
- `enumerate_h264_encoders.is_async_mode` exposé pour diagnostic.

> ⚠️ **Honnêteté technique** : sur la machine de dev, les 3 MFTs hardware
> trouvés (AMD, NVIDIA, Microsoft AVC DX12) sont tous en mode async. Seul
> Microsoft AVC DX12 est sync — c'est lui qui est sélectionné. NVENC/AMF
> réels nécessitent V3.3.1 (event loop `METransform*` events — pas livré).

#### Multi-instance test local

- `OKVM_INSTANCE` env var → `%APPDATA%\OneClick\OneClickKVM-{name}\config\`.
- Sanitisation `[a-zA-Z0-9_-]{,32}` + refus des noms DOS réservés
  (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`).
- Recettes `just dev-alice`, `dev-bob`, `run-2`, `clean-test-configs`.
- Guide complet : `docs/TESTING.md` (7 scénarios E2E).

#### Polish UI

- Dropdown sélecteur de moniteur dans Settings vidéo
  (`video_screen_idx` persisté).
- Hot-rejoin : sessions mortes nettoyées après 5 s (vs 15 s avant).

### Corrigé — Review code post V3.1

- #1 UDP audio dead-lock (lazy-init `UdpFecSender` + re-check pin chaque
  frame — plus de drain-and-drop permanent si pin tardif).
- #2 TCP audio skew warn-once.
- #4 `UdpAudioError::MissingUdpKeys` (erreur claire vs `BadParams{0,0}`).
- #6 Refuse les noms DOS réservés dans `OKVM_INSTANCE`.
- #9 Cache `OnceLock` pour `probe_best_backend` (~10-50 ms économisés).
- #10 Doc explicite du timing read-at-call de `OKVM_INSTANCE`.

### Nettoyé — dead code

- Supprime `PIN_TIMEOUT` (obsolète depuis fix #1), module `bytes16`
  (0 callers), `Arc<Notify> pin_notify*` (sender re-check direct).

### Tests
- Workspace : **91 passing**, 2 ignored, 0 failed, `RUSTFLAGS=-D warnings`.
- 1 test d'intégration UDP audio bout-en-bout en loopback (régression #1).
- Aucun test **E2E entre 2 vrais PCs** — env de dev mono-machine.

### À venir
- **V3.3.1** : event loop async-mode MFT (vrai NVENC/AMF/QSV).
- **Auto-update Tauri** : `tauri-plugin-updater` non câblé (différé,
  cohérent avec "pas de cert" choisi).

### Ajouté — V3.1 audio UDP+FEC bout-en-bout

- **`okvm-udp` crate** (V3 step 0) : Reed-Solomon FEC + AEAD + framing.
  13 tests dont 5 d'intégration (loopback, packet loss recovery K=4/M=2
  avec 2 paquets perdus, spray-attack DoS protection, bidirectionnel
  `Arc<UdpSocket>` partagé).
- **Négociation UDP au handshake** (step 1+2) : `ServerFinished.udp_ports`
  populé, `HandshakeOutcome.udp_keys` dérivé via HKDF epoch=1 (séparé du
  nonce space TCP epoch=0).
- **`okvm_net::UdpAudioPipe`** (step 4) : sender + receiver tasks qui
  bridgent `mpsc<AudioMessage>` ⇄ UDP+FEC chiffré.
- **NAT pinning auto** (step 7) : `UdpFecReceiver::recv_frame` remonte
  la `SocketAddr` source, permettant au serveur de découvrir l'endpoint
  UDP du client sur sa 1ère frame reçue puis renvoyer vers lui.
- **`Session::start_with_udp`** (step 5+6) : variante qui substitue les
  channels audio TCP par UDP+FEC. L'API `session.audio_tx/audio_rx` est
  inchangée → AppState n'a aucune modification.
- **Listener + Connector** détectent automatiquement la négociation UDP
  et appellent `start_with_udp` au lieu de `start`. Fallback transparent
  TCP si le bind UDP échoue.

### Ajouté — V3.3 chemin MFT hardware (limité)

- **`d3d11_helper.rs`** (step 1) : `D3D11Resources` + `IMFDXGIDeviceManager`,
  reset_token via `MFCreateDXGIDeviceManager`. Test smoke d'init.
- **`MfH264Encoder::try_new_hardware`** (step 2) : itère les MFTs via
  `MFTEnumEx(HARDWARE)`, prend le 1er sync. Set `MFT_MESSAGE_SET_D3D_MANAGER`
  avant configuration des media types.
- **`MfH264Encoder::new_best`** : tente HW puis fallback software
  (`CLSID_CMSH264EncoderMFT`) sans bruit.
- **`MfH264Encoder::probe_best_backend`** : diagnostic non bloquant pour
  AboutView, cache `OnceLock` process-wide pour éviter l'init répétée.
- **`enumerate_h264_encoders`** : expose `is_async_mode` pour diagnostic.

> ⚠️ **Honnêteté technique** : sur la machine de dev, les 3 MFTs hardware
> trouvés (AMD, NVIDIA, Microsoft AVC DX12) sont tous en mode async. Seul
> Microsoft AVC DX12 est sync — c'est lui qui est sélectionné. NVENC/AMF
> réels nécessitent **V3.3.1** (event loop `METransformNeedInput` /
> `METransformHaveOutput` — pas encore livré).

### Ajouté — Multi-instance test local

- **Variable d'env `OKVM_INSTANCE`** : si définie (et non vide), le
  répertoire de config bascule vers `%APPDATA%\OneClickKVM-{instance}\`.
  Permet de lancer 2 instances locales (alice/bob) pour valider le
  scénario E2E complet sans 2 PCs.
- Sanitisation stricte : `[a-zA-Z0-9_-]{,32}`, refuse les noms DOS
  réservés (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`).
- **Recettes `just`** : `dev-alice`, `dev-bob`, `run-2`, `clean-test-configs`
  (cf. `docs/TESTING.md`).

### Ajouté — Sélecteur de moniteur + hot-rejoin

- **Dropdown moniteur** dans Settings vidéo : `video_screen_idx` persisté,
  fallback silencieux sur écran 0 si l'index n'existe plus.
- **Hot-rejoin** : sessions mortes nettoyées après 5s (vs 15s avant), un
  pair réapparu retrouve un slot propre instantanément.

### Corrigé — Review code post V3.1

- **#1 UDP audio dead-lock** : le sender ne s'endort plus en attendant un
  `pin_notify` qui peut ne jamais arriver. Lazy-init du `UdpFecSender`
  + re-check du pin à chaque frame applicative. Frames droppées quand
  pas encore pinné sont comptées et logguées (puissances de 2).
- **#2 TCP audio skew warn-once** : si un pair V3.0 envoie de l'audio
  sur TCP alors que le canal local attend UDP, un warn est émis 1 seule
  fois (au lieu de spam silencieux).
- **#4 `UdpAudioError::MissingUdpKeys`** : erreur claire si on appelle
  `Session::start_with_udp` sans que le handshake ait dérivé de clés UDP
  (avant : `BadParams{0,0}` cryptique).
- **#6 DOS reserved names** : `sanitize_instance_name` rejette en plus
  `CON`/`PRN`/`AUX`/`NUL`/`COM[1-9]`/`LPT[1-9]` (casse-insensible).
- **#9 cache probe** : `probe_best_backend` ne ré-énumère plus les MFTs
  hardware à chaque ouverture d'AboutView (~10-50ms économisés par call).
- **#10 doc `OKVM_INSTANCE`** : timing read-at-call documenté, best
  practice = définir avant le démarrage process.

### Tests
- Workspace : **91 passing**, 2 ignored, 0 failed, `RUSTFLAGS=-D warnings`.
- 1 test d'intégration UDP audio bout-en-bout en loopback (pin tardif
  recovery → régression #1).
- Aucun test **E2E entre 2 vrais PCs** — env de dev mono-machine.

### À venir
- **V3.3.1** : event loop async-mode pour MFT NVENC/AMF/QSV — débloquerait
  le vrai hardware encoding sur la majorité des GPU modernes.
- **Auto-update Tauri** : `tauri-plugin-updater` non câblé. La task #46
  initiale couvrait scripts + doc, mais le plugin n'est pas dans
  `Cargo.toml` ni dans `tauri.conf.json`. Statut : différé, l'utilisateur
  doit checker GitHub Releases manuellement.
- **Release v0.1.2** quand V3.1 sera validé sur vrais 2 PCs.

## [0.1.1] — 2026-05-22

Release CI-built reproductible. Aucun changement de comportement
utilisateur par rapport à v0.1.0 ; valide le pipeline GitHub Actions
(test + fmt + clippy informational + auto-bundle NSIS).

### Modifié

- **Build reproductible** : installeur produit par GitHub Actions (runner
  Windows public) au lieu d'un build local. Garantit la même chaîne
  d'outils et permet l'audit.
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

### Ajouté — Sécurité (durcissements post code-review)

- **Anti-brute-force PIN** : compteur `failed_attempts` sur `PairingMode` ;
  le mode d'appairage est désactivé automatiquement après 5 tentatives
  ratées. Une demande sans PIN compte aussi comme tentative. Le lock est
  conservé pendant tout le check + l'incrément pour éviter les attaques
  en parallèle.
- **Zeroize PIN** : `PairingMode.pin` est wrappé dans
  `zeroize::Zeroizing<String>` pour effacer la mémoire à la destruction
  (defense in depth).
- **Cap pending shards** : `UdpFecReceiver.pending` est plafonné à 256
  entrées avec éviction FIFO — protège contre une attaque
  "spray-orphan-shards" qui ferait grossir la map indéfiniment. Mémoire
  bornée ≈ 8.6 MB pire cas.
- **MF init unifié** : `mediafoundation::ensure_mf_init()` (OnceLock
  partagé) remplace les `MFStartup` répétés qui accumulaient des
  ref-counts internes.
- **Stabilité du wire-format config** : `H264BackendChoice` utilise
  `#[serde(rename, alias)]` pour les noms kebab-case + alias PascalCase
  legacy, permettant de renommer les variantes Rust sans casser les
  `config.json` existants.

## [0.1.0] — 2026-05-21

Première release publique (alpha). Prêt pour usage personnel sur LAN
de confiance.

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

- Pas de signature Authenticode (SmartScreen avertit au premier lancement —
  décision projet, pas de certif prévue).
- Vidéo software (CPU) — V3 ajoutera Media Foundation hardware.
- Audio en TCP — V3 → UDP + FEC pour basse latence.
- PIN flow d'appairage côté serveur en attente d'implémentation stricte
  (livré en pre-V3.1, intégré dans 0.1.1).

## Roadmap

Voir [README.md#roadmap-v3](README.md#roadmap-v3).
