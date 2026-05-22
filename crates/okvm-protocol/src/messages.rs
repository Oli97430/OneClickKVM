//! Messages applicatifs par canal — `enum` sérialisables en bincode v2.
//!
//! Toutes les évolutions rétrocompatibles **doivent** se faire en ajoutant un
//! nouveau variant à la fin (avec `#[serde(other)]` côté décodeur), ou un
//! nouveau champ optionnel (`Option<T>` ou `#[serde(default)]`).
//!
//! Pour les changements **breaking**, incrémenter [`crate::PROTOCOL_VERSION`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use okvm_core::{
    ButtonState, Capabilities, ClipboardFormat, DeviceId, Edge, MouseButton, ScreenInfo, TouchPhase,
};

// ===========================================================================
// Canal #0 — Control
// ===========================================================================

/// Messages échangés sur le canal de contrôle.
///
/// Chaque variant est documenté inline ; les champs internes (timestamps,
/// stats, codes d'erreur) ont des noms self-explanatory dans le contexte
/// de leur variant, donc on supprime ici la règle `missing_docs` au niveau
/// type pour éviter une duplication de documentation.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CtrlMessage {
    /// Demande de pong, contient l'horodatage local de l'envoi (ms unix).
    Ping { ts_ms: u64 },
    /// Réponse à un `Ping`. `peer_ts_ms` recopie le `ts_ms` reçu.
    Pong { ts_ms: u64, peer_ts_ms: u64 },
    /// Heartbeat périodique avec stats légères.
    Heartbeat {
        ts_ms: u64,
        cpu_pct: u8,
        rss_mb: u32,
    },
    /// Mise à jour à chaud des capacités (ex : un écran a été branché).
    CapabilitiesUpdate(Capabilities),
    /// Demande de rotation de clé : on incrémente l'epoch côté demandeur.
    KeyRotationRequest { new_epoch: u32 },
    /// Acquittement de rotation : confirme qu'on a drainé les messages en attente.
    KeyRotationAck { new_epoch: u32 },
    /// Le pair signale qu'il va se déconnecter proprement.
    GoodBye { reason: String },
    /// Erreur applicative (le code suit la table de `PROTOCOL.md` §7).
    Error { code: u16, msg: String },
}

/// Raisons possibles d'un refus de session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RejectReason {
    /// `protocol_version` annoncé est trop bas ou trop haut.
    UnsupportedVersion,
    /// Pair inconnu et le pairing est désactivé.
    UnknownPeer,
    /// PIN d'appairage erroné.
    PairingFailed,
    /// L'ACL refuse la connexion.
    AclDenied,
    /// Capacités requises non disponibles.
    CapabilityMismatch,
    /// Erreur interne du serveur.
    Internal,
}

// ===========================================================================
// Canal #1 — Input + Clipboard + UI events
// ===========================================================================

/// Messages temps-réel circulant sur le canal input.
///
/// Variants documentés inline. Les champs `x/y/dx/dy/state/...` ont des
/// noms standards dans le contexte input ; on dispense de docs par champ
/// (cf. note sur `CtrlMessage`).
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum InputMessage {
    // --- Souris -------------------------------------------------------------
    /// Position absolue + delta brut + index d'écran cible.
    MouseMove {
        /// X dans le bureau virtuel **distant** (déjà reprojeté côté envoi).
        x_global: i32,
        /// Y dans le bureau virtuel distant.
        y_global: i32,
        /// Delta brut depuis le précédent (utile aux jeux qui lisent `dx/dy`).
        dx: i16,
        dy: i16,
        /// Index d'écran cible chez le pair.
        screen_idx: u32,
    },
    /// Clic ou relâchement.
    MouseButton {
        button: MouseButton,
        state: ButtonState,
        x: i32,
        y: i32,
    },
    /// Défilement molette (en wheel-deltas Win32 ; 120 = 1 cran natif).
    MouseWheel {
        delta_x: i32,
        delta_y: i32,
        x: i32,
        y: i32,
    },

    // --- Clavier ------------------------------------------------------------
    /// Événement clavier brut.
    KeyEvent {
        /// Virtual-Key code Windows.
        vk: u16,
        /// Scancode matériel.
        scancode: u16,
        /// Down/Up.
        state: ButtonState,
        /// Bit étendu (touches du pavé numérique, `AltGr`...).
        extended: bool,
        /// Bitfield modificateurs (Shift=1, Ctrl=2, Alt=4, Win=8, CapsLock=16, NumLock=32).
        modifiers: u16,
    },
    /// Texte composé (IME, dictée) à injecter directement.
    KeyText { text: String },

    // --- Switch -------------------------------------------------------------
    /// Le curseur **entre** sur ce pair par tel bord.
    SwitchEnter {
        from_peer: Uuid,
        enter_x: i32,
        enter_y: i32,
        edge: Edge,
    },
    /// Le curseur **quitte** ce pair vers tel pair par tel bord.
    SwitchLeave {
        to_peer: Uuid,
        leave_x: i32,
        leave_y: i32,
        edge: Edge,
    },

    // --- Clipboard ----------------------------------------------------------
    /// Annonce d'un nouveau contenu clipboard avec ses formats disponibles.
    ClipboardUpdate {
        /// Numéro de séquence monotone (anti-rebond).
        seq: u64,
        /// Formats inclus.
        formats: Vec<ClipboardItem>,
    },
    /// Demande explicite d'un format spécifique (pour les contenus volumineux,
    /// le sender peut n'annoncer que les formats légers et fournir les lourds
    /// sur demande).
    ClipboardRequest { seq: u64, format: ClipboardFormat },

    // --- Tactile ------------------------------------------------------------
    /// Événement tactile (un par doigt, identifié par `id`).
    TouchEvent {
        id: u32,
        phase: TouchPhase,
        x: i32,
        y: i32,
        /// Pression 0..1. Si non disponible : 0.5.
        pressure: f32,
    },

    // --- Power --------------------------------------------------------------
    /// Demande à verrouiller la session Windows distante.
    LockWorkstation,
    /// Challenge pour un déverrouillage assisté (rappel : Windows ne permet
    /// pas l'unlock direct depuis user-mode, ce message sert d'amorce à un
    /// flow Hello biométrie / PIN local côté distant).
    UnlockHint { challenge: [u8; 16] },
    /// Met le PC distant en veille.
    SleepRequest,
    /// Acquittement post-WoL.
    WakeAck,
}

/// Items que peut contenir un `ClipboardUpdate`.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ClipboardItem {
    /// Texte UTF-8.
    Text(String),
    /// Texte enrichi RTF (ASCII / 7-bit).
    Rtf(String),
    /// Fragment HTML, avec optionnellement une version texte brut alternative.
    Html {
        html: String,
        plaintext: Option<String>,
    },
    /// Image PNG (octets bruts).
    Png(Vec<u8>),
    /// Liste de chemins (drag&drop fichier classique). Le transfert effectif
    /// passe par le canal #2.
    FileList(Vec<String>),
}

// ===========================================================================
// Canal #2 — Files
// ===========================================================================

/// Messages du canal transfert de fichiers.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FileMessage {
    /// Démarre un transfert : annonce les fichiers et négocie multi-thread.
    TransferStart {
        transfer_id: Uuid,
        files: Vec<FileEntry>,
        total_bytes: u64,
        compression: Compression,
        /// Nombre de threads parallèles souhaités (hint). Le receveur peut
        /// répondre `TransferAccept` avec moins.
        threads: u8,
    },
    /// Acceptation, éventuellement partielle (`accepted` = indexes acceptés).
    TransferAccept {
        transfer_id: Uuid,
        accepted: Vec<u32>,
    },
    /// Refus complet.
    TransferReject { transfer_id: Uuid, reason: String },
    /// Un chunk de données.
    Chunk {
        transfer_id: Uuid,
        file_idx: u32,
        /// Sous-stream pour le multi-thread (0..threads).
        thread_idx: u8,
        /// Offset dans le fichier.
        offset: u64,
        /// Données. Taille ≤ `file_max_chunk_kib`.
        data: Vec<u8>,
        /// `true` si dernier chunk du fichier.
        is_last: bool,
        /// CRC32 du chunk (rapide, ne remplace pas l'AEAD ; utile pour debug).
        crc32: u32,
    },
    /// ACK chunk reçu (window-based flow control optionnel).
    ChunkAck {
        transfer_id: Uuid,
        file_idx: u32,
        offset: u64,
    },
    /// Fin d'un fichier individuel (envoie le BLAKE3 pour vérif intégrité).
    TransferComplete {
        transfer_id: Uuid,
        file_idx: u32,
        blake3: [u8; 32],
    },
    /// Annulation explicite par l'un des deux côtés.
    TransferCancel { transfer_id: Uuid, reason: String },
}

/// Entrée d'un transfert (fichier ou dossier).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Index 0..N (référence par `Chunk.file_idx`).
    pub idx: u32,
    /// Chemin relatif POSIX (`/` séparateur, jamais `..`).
    pub rel_path: String,
    /// Taille en octets ; 0 pour les dossiers.
    pub size_bytes: u64,
    /// `true` pour un dossier (créer puis sauter).
    pub is_dir: bool,
    /// Mtime en ms unix (peut être négatif).
    pub mtime_ms: i64,
    /// Permissions POSIX-style (sur Windows : surtout 0o755 ou 0o644).
    pub permissions: u16,
}

/// Compression appliquée par chunk.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Compression {
    /// Pas de compression.
    None,
    /// zstd avec niveau.
    Zstd { level: i32 },
}

// ===========================================================================
// Canal #3 — Audio (UDP)
// ===========================================================================

/// Messages audio.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AudioMessage {
    /// Démarre un stream.
    StreamStart {
        stream_id: Uuid,
        codec: okvm_core::AudioCodec,
        sample_rate_hz: u32,
        channels: u8,
        /// Nombre d'échantillons par frame (Opus 20 ms à 48 kHz → 960).
        frame_size_samples: u32,
        /// Étiquette source pour l'UI (« Speakers (Realtek) »).
        source_name: String,
    },
    /// Une frame encodée.
    StreamFrame {
        stream_id: Uuid,
        /// Séquence monotone (wrap u32 autorisé).
        seq: u32,
        /// Timestamp capture en µs.
        ts_us: u64,
        /// Payload codec.
        payload: Vec<u8>,
    },
    /// Arrêt du stream.
    StreamStop { stream_id: Uuid },
}

// ===========================================================================
// Canal #4 — Video (UDP)
// ===========================================================================

/// Messages vidéo.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum VideoMessage {
    /// Démarre un stream vidéo de l'écran `screen_idx` du pair.
    StreamStart {
        stream_id: Uuid,
        screen_idx: u32,
        codec: okvm_core::VideoCodec,
        width_px: u32,
        height_px: u32,
        target_fps: u32,
        bitrate_kbps: u32,
    },
    /// Une frame (potentiellement un shard Reed-Solomon).
    StreamFrame {
        stream_id: Uuid,
        seq: u32,
        ts_us: u64,
        is_keyframe: bool,
        /// Identifiant du groupe FEC (toutes les frames du même groupe
        /// peuvent être reconstruites si on en a au moins `fec_k` parmi `fec_n`).
        fec_group: u16,
        /// Index dans le groupe `0..fec_n`.
        fec_index: u16,
        /// Nb shards de données.
        fec_k: u16,
        /// Nb shards totaux (k + parité).
        fec_n: u16,
        payload: Vec<u8>,
    },
    /// Demande explicite d'un keyframe (perte, resize, join initial).
    KeyframeRequest {
        stream_id: Uuid,
        reason: KeyframeReason,
    },
    /// Arrête le stream.
    StreamStop { stream_id: Uuid },
    /// Ajuste à chaud le débit (contrôle de congestion).
    BitrateAdjust { stream_id: Uuid, new_kbps: u32 },
}

/// Raisons d'une demande de keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum KeyframeReason {
    /// Perte détectée (UDP).
    Loss,
    /// Redimensionnement de la surface destination.
    Resize,
    /// Nouveau spectateur qui vient de rejoindre.
    InitialJoin,
}

// ===========================================================================
// Description de canal (utilisée dans le handshake et ailleurs)
// ===========================================================================

/// Demande d'activation d'un canal logique.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelDesc {
    /// Identifiant `Channel::as_u8()`.
    pub id: u8,
    /// Transport demandé.
    pub transport: Transport,
    /// Port d'écoute distant si UDP (None en TCP).
    pub udp_port: Option<u16>,
}

/// Transport réseau d'un canal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Transport {
    /// TCP fiable (Ctrl/Input/Files).
    Tcp = 0,
    /// UDP best-effort (Audio/Video).
    Udp = 1,
}

// ===========================================================================
// Discovery beacon (UDP broadcast)
// ===========================================================================

/// Beacon émis périodiquement en broadcast UDP pour la découverte de fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryBeacon {
    /// `*b"OCKB"`.
    pub magic: [u8; 4],
    /// Version (= 1).
    pub version: u16,
    /// Identité publique (Ed25519).
    pub device_id_pub: DeviceId,
    /// Nom convivial pour affichage.
    pub name: String,
    /// Bitmask : 1=km, 2=kvm, 4=audio, 8=video, 16=wol, ...
    pub capabilities_short: u32,
    /// Port TCP du serveur de handshake.
    pub tcp_port: u16,
    /// `true` si IPv6 supporté.
    pub ipv6_ok: bool,
    /// Écrans (résumé pour aider la grille côté découvreur).
    pub screens_short: Vec<ScreenInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bincode::config::standard;

    #[test]
    fn ctrl_round_trip() {
        let m = CtrlMessage::Ping { ts_ms: 42 };
        let bytes = bincode::serde::encode_to_vec(&m, standard()).unwrap();
        let (d, _): (CtrlMessage, _) =
            bincode::serde::decode_from_slice(&bytes, standard()).unwrap();
        assert_eq!(d, m);
    }

    #[test]
    fn input_mouse_move_round_trip() {
        let m = InputMessage::MouseMove {
            x_global: 100,
            y_global: 200,
            dx: 1,
            dy: -1,
            screen_idx: 0,
        };
        let bytes = bincode::serde::encode_to_vec(&m, standard()).unwrap();
        let (d, _): (InputMessage, _) =
            bincode::serde::decode_from_slice(&bytes, standard()).unwrap();
        assert_eq!(d, m);
    }

    #[test]
    fn file_chunk_with_data() {
        let m = FileMessage::Chunk {
            transfer_id: Uuid::nil(),
            file_idx: 0,
            thread_idx: 0,
            offset: 0,
            data: vec![1, 2, 3, 4, 5],
            is_last: true,
            crc32: 0xDEADBEEF,
        };
        let bytes = bincode::serde::encode_to_vec(&m, standard()).unwrap();
        let (d, _): (FileMessage, _) =
            bincode::serde::decode_from_slice(&bytes, standard()).unwrap();
        assert_eq!(d, m);
    }
}
