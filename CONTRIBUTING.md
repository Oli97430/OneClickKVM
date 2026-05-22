# Contribuer à OneClick KVM

Merci de votre intérêt pour le projet ! Ce guide couvre l'essentiel.

## Pré-requis dev

- **Rust stable 1.80+** avec toolchain MSVC :
  `rustup default stable-x86_64-pc-windows-msvc`
- **Node.js 22+** et **pnpm 10+** (`npm i -g pnpm@10`)
- **Windows 10/11 x64** (build natif uniquement pour l'instant — cf. roadmap
  pour le support macOS/Linux)
- **Windows SDK** (pour `signtool.exe` lors des releases)
- (Optionnel) **just** task runner — `winget install Casey.Just` ou
  `cargo install just`. Donne accès aux commandes du `justfile`
  (`just dev`, `just test`, `just ci-local`, …).

## Premier setup

```powershell
git clone https://github.com/Oli97430/OneClickKVM.git
cd OneClickKVM
cd app
pnpm install
pnpm tauri:dev    # lance Vite + Tauri en hot-reload
```

Pour un build release standalone :

```powershell
cd app
pnpm tauri build
# Installeur produit dans src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/
```

## Workflow PR

1. **Fork + branche** : `git checkout -b feat/ma-feature` (ou `fix/...`,
   `docs/...`, `refactor/...`)
2. **Code + tests** : ajoutez/modifiez les tests pour couvrir le changement
3. **Vérifs locales** **obligatoires** (le `justfile` à la racine raccourcit) :
   ```powershell
   just ci-local         # fait tout en une commande
   # OU manuellement :
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cd app && pnpm exec svelte-check --tsconfig ./tsconfig.json
   ```
4. **Commit** avec un message structuré (cf. ci-dessous)
5. **PR** vers `main` — la CI Windows lance fmt/clippy/test automatiquement

## Conventions de commit

Format : `type: description courte (max 72 char)`

Types acceptés :
- `feat` — nouvelle fonctionnalité utilisateur
- `fix` — correction de bug
- `perf` — amélioration de performance sans changement de comportement
- `refactor` — réorganisation sans changement fonctionnel
- `docs` — README, comments, RST
- `test` — ajout/modif de tests
- `ci` — GitHub Actions, Dependabot
- `chore` — bumps de deps, cleanup
- `security` — patch sécurité (préférer le canal privé pour les vulns)

Body et footer optionnels (suivant la convention git classique). Ajouter
`Closes #N` ou `Fixes #N` pour lier une issue.

## Architecture du repo

```
crates/        # 21 crates Rust (workspace Cargo)
  okvm-core/       # types partagés
  okvm-crypto/     # AES-256-GCM, X25519, Ed25519, HKDF
  okvm-protocol/   # framing binaire, opcodes, bincode
  okvm-net/        # transport TCP chiffré + handshake
  okvm-udp/        # transport UDP + Reed-Solomon FEC
  okvm-discovery/  # mDNS + UDP broadcast
  okvm-input-*     # hooks Win32 capture + injection SendInput
  okvm-switch/     # edge detection, grille, hotkeys
  okvm-clipboard/  # Win32 clipboard multi-format
  okvm-files/      # transfert + BLAKE3 + sandbox
  okvm-audio/      # cpal WASAPI loopback + Opus
  okvm-video/      # Windows Graphics Capture + H.264 (openh264 + MF)
  okvm-wol/        # Wake-on-LAN
  okvm-power/      # LockWorkStation, Sleep, Shutdown
  okvm-logging/    # tracing + Windows Event Log
  okvm-config/     # settings, peers, identity DPAPI
  okvm-i18n/       # catalogues backend
  okvm-ipc/        # DTOs Tauri ↔ Svelte
app/           # application Tauri 2
  src/             # frontend Svelte 5 (12 composants)
  src-tauri/       # backend Rust qui orchestre les crates
docs/          # design docs (ARCHITECTURE, PROTOCOL, SECURITY, RELEASE)
scripts/       # scripts release.ps1 + outils dev
```

Plus de détails dans [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Tests

- **Unit** : à côté du code (`#[cfg(test)] mod tests`)
- **Intégration** : dans `crates/<crate>/tests/`
- **Loopback** : `okvm-net/tests/loopback.rs` (handshake AES + Ping/Pong),
  `okvm-udp/tests/loopback.rs` (FEC reconstitution + anti-spray)
- **Frontend** : `svelte-check` (typecheck + a11y) + tests Vitest si pertinent

Cible : tout nouveau code mérite au moins un test qui démontre le
comportement attendu (au lieu d'un "ça compile"). Voir les exemples
existants pour le style — petits, focalisés, sans setup global.

## Style

- **Rust** : `cargo fmt --all` (config par défaut), `clippy::pedantic` activé
- **TypeScript/Svelte** : pas de Prettier formel, mais respect des conventions
  Svelte 5 (runes `$state/$derived/$effect/$props`, pas de stores `writable`)
- **i18n** : toute nouvelle string visible utilisateur doit être ajoutée à
  `app/src/i18n.svelte.ts` (au moins FR + EN, idéalement les 9 langues)
- **Comments** : français pour les docs internes, anglais pour les commits
  (audience plus large via GitHub)
- **unsafe** : chaque bloc DOIT avoir un commentaire `// SAFETY:` qui
  articule l'invariant réel (pas juste "c'est OK")

## Sécurité

**Ne reportez PAS de vulns dans une issue publique.** Voir
[`SECURITY.md`](SECURITY.md) pour les canaux privés.

## Roadmap & priorités

La roadmap V3+ est dans le [README](README.md#roadmap-v3). Les chantiers
ouverts marqués V3.1, V3.2, V3.3 dans le code (tasks GitHub Issues) sont
de bons points d'entrée — chacun est focalisé et bien documenté en amont.

## Licence

En soumettant une contribution, vous acceptez qu'elle soit publiée sous le
double licensing du projet : **MIT OU Apache-2.0** (cf. `LICENSE-MIT` et
`LICENSE-APACHE`).
