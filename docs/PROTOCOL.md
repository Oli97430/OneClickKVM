# OneClick KVM — Protocole réseau

> Version protocole : **1** (champ `protocol_version` dans le handshake)
> Endianness : **Big Endian** sur le wire pour tous les entiers multi-octets,
> sauf champs Little Endian explicitement marqués `(LE)`.
> Encodage des structures : **bincode v2 fixed-int** sur les payloads applicatifs.

---

## 1. Couches

```
┌──────────────────────────────────────────────────────────┐
│  Couche 4 : MESSAGES applicatifs                         │  (opcodes par module)
│  (InputEvent, ClipboardSync, FileChunk, AudioFrame, ...) │
├──────────────────────────────────────────────────────────┤
│  Couche 3 : FRAMING multicanaux                          │  (header + canal)
│  [len: u32][channel: u8][ctr: u64][AEAD payload + tag]   │
├──────────────────────────────────────────────────────────┤
│  Couche 2 : SESSION chiffrée AES-256-GCM                 │  (per-direction nonce)
├──────────────────────────────────────────────────────────┤
│  Couche 1 : TRANSPORT (TCP keepalive + nodelay, UDP)     │
└──────────────────────────────────────────────────────────┘
```

---

## 2. Handshake (canal de contrôle, TCP)

Le handshake est **un échange de 4 messages non chiffrés** pour établir la
session, suivi du passage en mode chiffré. La structure de chaque message
est sérialisée en **bincode v2** avec un préfixe de longueur `u32 BE`.

### 2.1 Message 1 — ClientHello

```rust
struct ClientHello {
    magic:            [u8; 4],   // b"OCKV"
    protocol_version: u16,       // = 1
    flags:            u16,       // bit 0: supports_ipv6, bit 1: requires_pairing, bit 2-15 reserved
    nonce:            [u8; 32],  // random
    ephemeral_pub:    [u8; 32],  // X25519 public key (éphémère pour cette session)
    identity_pub:     [u8; 32],  // Ed25519 device long-term public key
    capabilities:     Capabilities,
    pairing_pin_hash: Option<[u8; 32]>, // SHA-256(pin || nonce) si premier appairage
}
```

### 2.2 Message 2 — ServerHello

```rust
struct ServerHello {
    magic:            [u8; 4],   // b"OCKV"
    protocol_version: u16,
    flags:            u16,
    nonce:            [u8; 32],
    ephemeral_pub:    [u8; 32],
    identity_pub:     [u8; 32],
    capabilities:     Capabilities,
    signature:        [u8; 64],  // Ed25519(identity_priv, transcript_hash)
    pairing_required: bool,
    pairing_pin_hash: Option<[u8; 32]>,
}
```

Où `transcript_hash = SHA-256(ClientHello bytes || ServerHello-without-signature bytes)`.

### 2.3 Message 3 — ClientFinished

Premier message **chiffré** (mode session établi). Contenu :

```rust
struct ClientFinished {
    transcript_signature: [u8; 64],  // Ed25519(client_identity_priv, transcript_hash_full)
    selected_channels:    Vec<ChannelDesc>,
}

struct ChannelDesc {
    id:        u8,        // 0..4
    transport: Transport, // Tcp | Udp
    udp_port:  Option<u16>, // si UDP : port d'écoute distant
}

enum Transport { Tcp = 0, Udp = 1 }
```

### 2.4 Message 4 — ServerFinished

```rust
struct ServerFinished {
    accepted: bool,
    reason:   Option<RejectReason>,  // None si accepted
    udp_ports: Vec<(u8, u16)>,       // (channel_id, port) pour les canaux UDP négociés
}

enum RejectReason {
    UnsupportedVersion,
    UnknownPeer,        // peer non appairé et pairing désactivé
    PairingFailed,      // PIN incorrect
    AclDenied,
    CapabilityMismatch,
    Internal,
}
```

### 2.5 Capabilities

```rust
struct Capabilities {
    app_version:        String,        // ex: "0.1.0"
    os:                 OsInfo,
    screens:            Vec<ScreenInfo>,
    audio_capture:      bool,
    video_capture:      bool,
    video_codecs:       Vec<VideoCodec>,
    audio_codecs:       Vec<AudioCodec>,
    hotkeys_supported:  bool,
    file_max_chunk_kb:  u32,
    languages:          Vec<String>,   // ex: ["fr", "en"]
}

struct OsInfo {
    family: String,   // "windows"
    version: String,  // "10.0.26100"
    arch: String,     // "x86_64"
    hostname: String, // NetBIOS
}

struct ScreenInfo {
    index:    u32,
    is_primary: bool,
    width_px: u32,
    height_px: u32,
    dpi:      u32,
    origin_x: i32,    // dans le bureau virtuel local
    origin_y: i32,
}

enum VideoCodec { H264 = 0, H265 = 1, Av1 = 2, Mjpeg = 100 }
enum AudioCodec { Opus = 0, Pcm16 = 1, Aac = 2 }
```

---

## 3. Framing post-handshake (TCP)

Chaque frame transportée sur le canal TCP de session a la structure :

```
 0               1               2               3
 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          total_len (u32 BE)                   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|    channel    |                                               |
+-+-+-+-+-+-+-+-+                                               |
|                       nonce_counter (u64 BE)                  |
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
|                  AEAD ciphertext  (total_len - 13)            |
|                  inclut tag GCM 16 octets en fin               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

- `total_len` : taille de toute la frame **sans** ces 4 octets de longueur.
- `channel` : 0=ctrl, 1=input+clipboard, 2=files, 3=audio (UDP), 4=video (UDP).
- `nonce_counter` : compteur monotone par canal **par direction**.
- Le **nonce AES-GCM** (96 bits) = `epoch (32 bits, fixé au handshake) || nonce_counter (64 bits)`.
- L'**AAD** AEAD = `channel || nonce_counter` (les 9 octets de l'en-tête après `total_len`).
- **Plafond** : `total_len ≤ 16 MiB`. Au-delà : reject + close.

### 3.1 Frame UDP

Sur UDP, l'en-tête est identique mais on perd `total_len` (la taille du
datagramme la donne implicitement). On préfixe à la place 4 octets de
**magic + version** pour rejeter rapidement les paquets erronés :

```
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|  magic (u16)  | version (u16) |   magic = 0x4F4B ("OK")
+-+-+-+-+-+-+-+-+               |
|    channel    |               |
+-+-+-+-+-+-+-+-+               |
|                nonce_counter (u64 BE)
...
|             AEAD ciphertext + tag
```

---

## 4. Messages applicatifs

Les payloads chiffrés sont des `enum Message` sérialisés en bincode v2.

### 4.1 Canal #0 — Control

```rust
enum CtrlMessage {
    Ping { ts_ms: u64 },
    Pong { ts_ms: u64, peer_ts_ms: u64 },
    Heartbeat { ts_ms: u64, cpu_pct: u8, rss_mb: u32 },
    CapabilitiesUpdate(Capabilities),
    KeyRotationRequest { new_epoch: u32 },
    KeyRotationAck { new_epoch: u32 },
    GoodBye { reason: String },
    Error { code: u16, msg: String },
}
```

### 4.2 Canal #1 — Input + Clipboard + UI events

```rust
enum InputMessage {
    // Souris ----------------------------------------------------------
    MouseMove { x_global: i32, y_global: i32, dx: i16, dy: i16, screen_idx: u32 },
    MouseButton { button: MouseButton, state: ButtonState, x: i32, y: i32 },
    MouseWheel { delta_x: i32, delta_y: i32, x: i32, y: i32 },

    // Clavier ---------------------------------------------------------
    KeyEvent { vk: u16, scancode: u16, state: ButtonState, extended: bool, modifiers: u16 },
    KeyText  { text: String },  // pour les IME / saisie composée

    // Switch ----------------------------------------------------------
    SwitchEnter { from_peer: Uuid, enter_x: i32, enter_y: i32, edge: Edge },
    SwitchLeave { to_peer: Uuid, leave_x: i32, leave_y: i32, edge: Edge },

    // Clipboard -------------------------------------------------------
    ClipboardUpdate {
        seq: u64,                       // monotone, anti-rebond
        formats: Vec<ClipboardItem>,
    },
    ClipboardRequest { seq: u64, format: ClipboardFormat },

    // Tactile (pour les écrans tactiles distants) --------------------
    TouchEvent { id: u32, phase: TouchPhase, x: i32, y: i32, pressure: f32 },

    // Power -----------------------------------------------------------
    LockWorkstation,
    UnlockHint { challenge: [u8; 16] },  // l'OS Windows ne permet pas l'unlock direct
    SleepRequest,
    WakeAck,
}

enum MouseButton { Left, Right, Middle, X1, X2 }
enum ButtonState { Down, Up }
enum Edge { Left, Right, Top, Bottom }
enum TouchPhase { Began, Moved, Ended, Cancelled }

enum ClipboardItem {
    Text(String),
    Rtf(String),
    Html { html: String, plaintext: Option<String> },
    Png(Vec<u8>),
    FileList(Vec<String>),  // chemins (le drag&drop fichier passe par #2)
}

enum ClipboardFormat { Text, Rtf, Html, Png, FileList }
```

### 4.3 Canal #2 — Files

```rust
enum FileMessage {
    TransferStart {
        transfer_id: Uuid,
        files: Vec<FileEntry>,
        total_bytes: u64,
        compression: Compression,
        threads: u8,           // hint nombre de threads (négocié)
    },
    TransferAccept { transfer_id: Uuid, accepted: Vec<u32> /* indexes */ },
    TransferReject { transfer_id: Uuid, reason: String },
    Chunk {
        transfer_id: Uuid,
        file_idx: u32,
        thread_idx: u8,         // pour multi-thread, identifie le sous-stream
        offset: u64,
        data: Vec<u8>,          // ≤ file_max_chunk_kb (négocié, défaut 256 KiB)
        is_last: bool,
        crc32: u32,
    },
    ChunkAck { transfer_id: Uuid, file_idx: u32, offset: u64 },
    TransferComplete { transfer_id: Uuid, file_idx: u32, blake3: [u8; 32] },
    TransferCancel { transfer_id: Uuid, reason: String },
}

struct FileEntry {
    idx: u32,
    rel_path: String,       // POSIX-style séparateurs
    size_bytes: u64,
    is_dir: bool,
    mtime_ms: i64,
    permissions: u16,
}

enum Compression { None, Zstd { level: i32 } }
```

### 4.4 Canal #3 — Audio (UDP)

```rust
enum AudioMessage {
    StreamStart {
        stream_id: Uuid,
        codec: AudioCodec,
        sample_rate_hz: u32,
        channels: u8,
        frame_size_samples: u32,
        source_name: String,    // ex: "Speakers (Realtek)"
    },
    StreamFrame {
        stream_id: Uuid,
        seq: u32,               // monotone par stream, wrap autorisé
        ts_us: u64,             // timestamp capture
        payload: Vec<u8>,       // frame encodée (Opus typique : 20 ms)
    },
    StreamStop { stream_id: Uuid },
}
```

### 4.5 Canal #4 — Video (UDP)

```rust
enum VideoMessage {
    StreamStart {
        stream_id: Uuid,
        screen_idx: u32,
        codec: VideoCodec,
        width_px: u32,
        height_px: u32,
        target_fps: u32,
        bitrate_kbps: u32,
    },
    StreamFrame {
        stream_id: Uuid,
        seq: u32,
        ts_us: u64,
        is_keyframe: bool,
        fec_group: u16,         // identifiant de groupe Reed-Solomon
        fec_index: u16,         // index dans le groupe
        fec_k: u16,             // nb shards données
        fec_n: u16,             // nb shards totaux (k + parité)
        payload: Vec<u8>,       // shard encodé
    },
    KeyframeRequest { stream_id: Uuid, reason: KeyframeReason },
    StreamStop { stream_id: Uuid },
    BitrateAdjust { stream_id: Uuid, new_kbps: u32 },
}

enum KeyframeReason { Loss, Resize, InitialJoin }
```

---

## 5. Découverte (UDP broadcast/multicast)

### 5.1 mDNS

Service : `_oneclick-kvm._tcp.local.`
TXT records :

```
v=1
device_id=<base64url(SHA-256(identity_pub)) tronqué 16 octets>
name=<hostname>
caps=km|kvm|audio|video
```

Port annoncé : port TCP du serveur de handshake (par défaut **47101**).

### 5.2 Broadcast UDP de secours

Pour les réseaux où mDNS est filtré, broadcast UDP périodique (toutes les 5 s)
sur le port **47100** avec un payload bincode :

```rust
struct DiscoveryBeacon {
    magic: [u8; 4],          // b"OCKB"
    version: u16,            // = 1
    device_id_pub: [u8; 32],
    name: String,
    capabilities_short: u32, // bitmask : km | kvm | audio | video | wol | ...
    tcp_port: u16,
    ipv6_ok: bool,
}
```

---

## 6. Rotation de clé

Demandée par l'un ou l'autre pair via `CtrlMessage::KeyRotationRequest { new_epoch }`.

1. L'initiateur arrête d'envoyer (drain de la file).
2. L'autre pair répond `KeyRotationAck` quand il a drainé ses pending.
3. Les deux côtés exécutent un **nouveau X25519** (échange des nouvelles
   éphémères dans deux messages CtrlMessage dédiés) et **rederivent** la clé
   AES via HKDF avec l'`epoch` actualisé.
4. Le `nonce_counter` repart à 0 pour le nouvel `epoch`.

Déclencheurs automatiques :
- volume total chiffré par direction ≥ **4 GiB**, ou
- temps écoulé depuis dernière rotation ≥ **24 h**, ou
- compteur de nonce > **2⁵⁰** (très loin de l'épuisement, prudence).

---

## 7. Erreurs et fermeture

Côté serveur ou client, en cas d'erreur fatale (signature invalide, frame
corrompue, dépassement de plafond, etc.) :

1. Envoyer un dernier `CtrlMessage::Error { code, msg }` si possible.
2. Envoyer un `TCP FIN` propre, **pas** de RST.
3. Journaliser dans Windows Event Log (source `OneClickKVM`).
4. L'UI affiche un toast avec la raison.

Codes d'erreur normalisés (extrait) :

| Code  | Signification                                  |
| ----- | ---------------------------------------------- |
| 1000  | OK / Close normal                              |
| 1001  | Going away                                     |
| 1100  | Protocol version unsupported                   |
| 1200  | Crypto handshake failure                       |
| 1201  | Signature verification failed                  |
| 1202  | AEAD decryption failure                        |
| 1203  | Replay detected (nonce_counter rewinds)        |
| 1300  | Frame too large                                |
| 1301  | Unknown channel                                |
| 1302  | Unknown opcode                                 |
| 1400  | ACL denied                                     |
| 1500  | Rate limit exceeded                            |
| 1600  | Internal error                                 |

---

## 8. Versionning et compatibilité

- `protocol_version` est un `u16` ; on n'incrémente qu'en cas de **breaking change** au framing ou aux opcodes.
- Les `enum` applicatifs (`InputMessage`, etc.) sont **non-exhaustifs** côté décodeur : un opcode inconnu est **journalisé et ignoré** sans fermer la session.
- Toute évolution **rétrocompatible** (nouveau champ optionnel, nouveau variant) **n'incrémente pas** `protocol_version`.

---

## 9. Constantes de référence

```
TCP_PORT_DEFAULT        = 47101
UDP_DISCOVERY_PORT      = 47100
UDP_AUDIO_PORT_RANGE    = 47200..47210
UDP_VIDEO_PORT_RANGE    = 47300..47310

MAX_FRAME_BYTES         = 16 * 1024 * 1024   // 16 MiB
MAX_INPUT_EVENTS_PER_S  = 10_000
DEFAULT_FILE_CHUNK_KIB  = 256
HEARTBEAT_INTERVAL_MS   = 2000
HEARTBEAT_TIMEOUT_MS    = 6000               // 3 intervals
HANDSHAKE_TIMEOUT_MS    = 5000
```
