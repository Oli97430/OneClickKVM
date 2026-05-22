# OneClick KVM — Modèle de sécurité

> Version : 0.1 — 2026-05-20
> Statut : *draft*, à challenger avant production.

---

## 1. Cadre

OneClick KVM transmet **clavier, souris, presse-papier, fichiers, audio, vidéo**
entre PCs sur un réseau local. C'est par construction un **vecteur d'attaque
high-value** : qui contrôle le canal contrôle les machines.

Le présent document décrit :

- les **biens à protéger** (assets) ;
- le **modèle de menace** (qui peut attaquer, comment) ;
- les **garanties** offertes par le design cryptographique ;
- les **non-garanties** (ce que l'app ne protège pas) ;
- la **gestion des secrets** sur disque ;
- les **règles de durcissement** côté code.

---

## 2. Assets

| # | Asset                                    | Sensibilité | Notes                                         |
| - | ---------------------------------------- | ----------- | --------------------------------------------- |
| 1 | Clés d'identité Ed25519                  | **Critique**| Stockées DPAPI-encrypted, jamais en logs      |
| 2 | Clés de session AES-256 dérivées         | **Critique**| In-memory uniquement, zeroized au teardown    |
| 3 | Frappes clavier en transit               | **Critique**| Inclut potentiellement mots de passe         |
| 4 | Presse-papier (texte/image)              | **Élevé**   | Peut contenir secrets, tokens                 |
| 5 | Fichiers transférés                      | **Élevé**   | Confidentialité et intégrité                  |
| 6 | Captures écran/audio                     | **Élevé**   | Vie privée                                    |
| 7 | Liste des pairs et leurs empreintes      | **Modéré**  | Métadonnée de réseau                          |
| 8 | Configuration application                | **Modéré**  | Stockée en clair (pas de secret dedans)       |
| 9 | Logs (Event Viewer + fichier rotatif)    | **Modéré**  | Pas de contenu sensible loggé (cf. §7)        |

---

## 3. Modèle de menace

### 3.1 Attaquants considérés

| Attaquant                          | Capacité                                              | En scope ? |
| ---------------------------------- | ----------------------------------------------------- | ---------- |
| **Réseau passif** (sniffer LAN)    | Écoute tout le trafic                                 | ✅ Oui     |
| **Réseau actif** (MITM)            | Modifie, rejoue, supprime des paquets                 | ✅ Oui     |
| **Pair malveillant déjà appairé**  | A déjà été accepté, abuse de l'ACL                    | ✅ Oui     |
| **Voisin réseau non appairé**      | Tente de se connecter                                 | ✅ Oui     |
| **Malware sur le PC distant**      | Tente d'exfiltrer via OneClickKVM                     | ⚠️ Partiel |
| **Attaquant local non-admin**      | Lit fichiers user du PC local                         | ✅ Oui     |
| **Attaquant local admin / SYSTEM** | Contrôle total du PC                                  | ❌ Hors scope (game over) |
| **Évil-maid hors-ligne**           | Accès physique disque                                 | ❌ Hors scope sans BitLocker |
| **Side-channels matériels**        | Timing AES, Spectre, etc.                             | ❌ Hors scope |
| **Compromis chaîne d'approvisionnement crate** | crates.io supply chain               | ⚠️ Mitigé via `cargo deny` |

### 3.2 Surfaces d'attaque

1. **Sockets TCP/UDP exposés** sur le LAN (ports 47100, 47101, range UDP).
2. **mDNS responder** qui répond à toute requête de service correspondant.
3. **WebView Tauri** : XSS dans le HTML local → exécution dans le contexte UI.
4. **Commands IPC Tauri** : tout argument venant du frontend doit être validé.
5. **Parsing de protocole** : binaire mal formé → panic, OOM, RCE potentielle.
6. **Drivers d'input** : `SendInput` peut être détourné par malware local.
7. **Fichiers reçus** : path traversal possible si on respecte aveuglément `rel_path`.

---

## 4. Cryptographie — choix et justifications

| Primitive             | Choix                       | Raison                                                 |
| --------------------- | --------------------------- | ------------------------------------------------------ |
| Chiffrement AEAD      | **AES-256-GCM**             | NIST-approuvé, accéléré matériel (AES-NI), nonce 96b   |
| Échange de clés       | **X25519 (ECDH)**           | Performant, courbe sans backdoor, tooling mature       |
| Signatures            | **Ed25519**                 | Pareil, déterministe, pas de RNG critique en signature |
| KDF                   | **HKDF-SHA256**             | Standard RFC 5869, séparation de domaines              |
| Hash                  | **SHA-256**                 | Standard pour signatures et empreintes                 |
| Hash de fichier       | **BLAKE3**                  | Beaucoup plus rapide que SHA-256, sécurité équivalente |
| PRNG                  | OS (`getrandom`)            | BCryptGenRandom sur Windows                            |
| MAC explicite         | (inclus dans GCM)           | -                                                      |
| Stockage secrets disk | **Windows DPAPI** (user)    | Bénéficie du login Windows pour la dérivation          |

### 4.1 Pourquoi AES-256-GCM et pas ChaCha20-Poly1305 ?

- AES-NI est universel sur les CPUs Windows x64 cibles → perfs ≥ 3 GB/s par cœur.
- Conformité PCI/CC : AES-256 est le plus largement reconnu.
- ChaCha20-Poly1305 reste un fallback envisageable si on portait à des CPUs
  sans AES-NI (ARM low-end), pas notre cible.

### 4.2 Construction du nonce GCM

```
nonce[12] = epoch[4] BE  ||  counter[8] BE
```

- `epoch` est négocié à chaque rotation de clé (cf. PROTOCOL §6), 0 à l'init.
- `counter` est **incrémenté par 1 à chaque frame envoyée**, par canal et par direction.
- L'unicité (nonce, key) est **mécaniquement garantie** tant qu'on rote avant
  d'épuiser le compteur (2⁶⁴ frames — inatteignable en pratique).

### 4.3 AAD

L'AAD de chaque frame inclut `channel || counter` afin que :

- le déchiffrement échoue si un attaquant **mélange les canaux** ;
- toute **réordonnancement** ou **rejeu** est détecté (counter doit strictement croître).

### 4.4 Anti-replay sur UDP

Sur les canaux UDP (audio/vidéo), on tolère perte et désordre **modérés** mais
on rejette tout `counter` ≤ `max_received - window` (window = 1024). On
maintient un **bitmap glissant** pour rejeter les vrais rejeus dans la fenêtre.

---

## 5. Handshake — propriétés visées

| Propriété                             | Comment c'est garanti                                                                                                    |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Authentification mutuelle             | Signature Ed25519 du `transcript_hash` côté serveur (msg 2) + côté client (msg 3)                                        |
| Perfect Forward Secrecy               | Clés AES dérivées d'une éphémère X25519 jetée après dérivation                                                           |
| Protection contre MITM passif         | AEAD scelle tout le trafic                                                                                               |
| Protection contre MITM actif appairé  | Toute altération du transcript fait échouer la signature                                                                 |
| Protection contre MITM actif TOFU     | PIN à 6 chiffres dans le hash signé du msg 1+2 lors du premier appairage                                                 |
| Détection clé compromise              | Empreinte changée détectée par `peers.json`, warning critique UI (comme SSH `WARNING: REMOTE HOST IDENTIFICATION CHANGED`) |
| Replay de session entière             | Nonces aléatoires 32 octets dans les Hello, intégrés au transcript                                                       |
| Downgrade de version                  | `protocol_version` est dans le transcript signé                                                                          |

---

## 6. Stockage des secrets

### 6.1 Identité Ed25519

- Génération à la **première exécution** via `BCryptGenRandom` (`getrandom`).
- La clé privée brute (32 octets) est encapsulée dans un blob **DPAPI user-scope** :
  `CryptProtectData(seed, CRYPTPROTECT_LOCAL_MACHINE = 0)`.
- Stockée dans `%APPDATA%\OneClickKVM\identity.dpapi`.
- Lue en RAM uniquement quand nécessaire ; le `Drop` zeroize.

### 6.2 Liste des pairs

- Fichier `%APPDATA%\OneClickKVM\peers.json` en clair (contient empreintes publiques et ACL — pas de secret).

### 6.3 Configuration

- `%APPDATA%\OneClickKVM\config.json` en clair.
- Toute valeur sensible future (ex : password proxy SOCKS) sera DPAPI-encryptée individuellement.

### 6.4 Pas de mot de passe utilisateur

Le design n'expose **aucun mot de passe maître** à l'utilisateur. L'auth repose
sur (a) la session Windows pour DPAPI et (b) le PIN à usage unique pour l'appairage.

---

## 7. Logging — règles

Logging is potentially a sidechannel. Donc :

- **Jamais** logger : payloads (clipboard, fichiers, frames audio/vidéo), frappes clavier, clés cryptographiques, nonces complets.
- **Toujours** logger (niveau INFO+) : connexions, déconnexions, échecs handshake, erreurs ACL, rotations de clé, transferts fichiers (nom + taille, pas contenu).
- **Niveau DEBUG** uniquement en build debug ; en release, max INFO.
- Format : **JSON structuré** via `tracing-subscriber::fmt::json()`.
- Destination : (a) fichier rotatif `%LOCALAPPDATA%\OneClickKVM\logs\app.log` 7 jours, et (b) Windows Event Log source `OneClickKVM` pour les événements de niveau Warn+.

---

## 8. Durcissement code

### 8.1 Règles Rust

- `#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]` dans tous les crates métier ;
  les `unsafe` autorisés uniquement dans `okvm-input-*`, `okvm-audio`, `okvm-video`
  (FFI Win32) et chaque bloc unsafe doit avoir un commentaire `// SAFETY:`.
- `#[forbid(unused_must_use)]` pour éviter les `Result` ignorés.
- `panic = "abort"` en release pour éviter les double-frees post-unwind.
- `cargo deny` en CI pour bloquer les crates non audités / GPL / yanked.
- `cargo audit` en CI pour les CVEs connues.
- `cargo fuzz` ciblant `okvm-protocol::decode_frame`.
- `clippy::pedantic` activé, exceptions documentées.

### 8.2 Parsing défensif

- **Plafond** sur toute longueur (`total_len ≤ 16 MiB`, vec elements ≤ 65536, etc.).
- **bincode v2** en mode `with_limit` pour rejeter les structures explosives.
- Refus systématique des UTF-8 invalides (`String::from_utf8` jamais `unchecked`).

### 8.3 Path traversal sur fichiers reçus

Tout `rel_path` reçu via `FileMessage::TransferStart` est :

1. Décomposé via `std::path::Path::components`.
2. Rejeté si contient `Component::ParentDir`, `Component::RootDir`, `Component::Prefix`, ou un nom contenant `:` (ADS Windows).
3. Joint au répertoire cible via `join`, puis `canonicalize` sur le parent : le chemin résolu doit être **descendant strict** du dossier de destination.

### 8.4 Rate limiting

- **InputMessage** : max **10 000 events/s** par session. Au-delà : drop + warn.
- **Connexions handshake** : max **5/min/IP**. Au-delà : tarpit 30 s.
- **ClipboardUpdate** : déduplication par `seq`, ignorer si `seq ≤ last_seq` ou si payload identique au précédent.

### 8.5 Surface Tauri

- `tauri.conf.json` : `allowlist` vide. Toute capacité passe par une command Rust explicite.
- CSP stricte : `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'` (inline style accepté pour Svelte, à revoir).
- Toute command IPC :
  - Validation arguments (`serde` + bornes type fortes, ex : `BoundedString<256>`).
  - Pas d'`unwrap` qui panique sur input frontend → ferait crasher le backend.
  - Erreurs renvoyées en `Result<T, AppError>` typé.

---

## 9. Permissions Windows

L'application tourne par défaut **sans élévation** (manifest sans `requestedExecutionLevel="requireAdministrator"`).

Conséquences :
- ❌ Pas de hook clavier au niveau driver — on utilise `WH_KEYBOARD_LL` / `WH_MOUSE_LL` user-mode.
  Limitation : Ctrl+Alt+Suppr et certains hotkeys protégés ne peuvent pas être interceptés (by design Windows).
- ❌ Pas de `LockWorkstation()` pour les **autres** sessions — uniquement la session courante.
- ✅ Wake-on-LAN sortant OK (juste un broadcast UDP).
- ✅ `SendInput` OK pour injecter dans la session courante non-élevée.
- ⚠️ Si Windows est dans une **session UAC élevée** (dialog UAC affiché), l'app non-élevée **ne peut pas y envoyer d'input** (Mandatory Integrity Control). C'est une protection OS, on n'essaie pas de la contourner.

Une option « **lancer en tant qu'administrateur** » sera proposée dans l'UI
pour débloquer les hotkeys protégés et l'input vers les fenêtres élevées,
en documentant clairement le compromis.

---

## 10. Bug bounty / divulgation responsable

À définir si distribution commerciale ou open source publique. Squelette :

- Email dédié `security@<domain>` (à provisionner).
- 90 jours de coordination avant disclosure publique.
- Hall of fame.

---

## 11. Checklist avant release

- [ ] `cargo audit` clean.
- [ ] `cargo deny` clean.
- [ ] `cargo fuzz run protocol -- -max_total_time=600` clean.
- [ ] Tests d'intégration avec injection paquets corrompus passent.
- [ ] Tous les `unsafe` ont un commentaire `// SAFETY:`.
- [ ] `tauri.conf.json` revu (allowlist, CSP).
- [ ] Aucun secret loggé (revue grep `tracing::`).
- [ ] DPAPI scope correct (user, pas LocalMachine).
- [ ] Binaire signé Authenticode (si distribution publique).
- [ ] Document changelog/CVE policy publié.
