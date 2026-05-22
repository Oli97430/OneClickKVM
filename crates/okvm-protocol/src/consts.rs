//! Constantes du protocole.

/// Version courante du protocole. Voir `PROTOCOL.md` §8.
pub const PROTOCOL_VERSION: u16 = 1;

/// Port TCP par défaut du serveur de handshake.
pub const TCP_PORT_DEFAULT: u16 = 47101;

/// Port UDP pour le broadcast de découverte.
pub const UDP_DISCOVERY_PORT: u16 = 47100;

/// Taille maximale d'une frame TCP (16 MiB). Au-delà : reject + close.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Plafond d'événements input par seconde et par session.
pub const MAX_INPUT_EVENTS_PER_S: u32 = 10_000;

/// Intervalle entre deux heartbeats (ms).
pub const HEARTBEAT_INTERVAL_MS: u64 = 2_000;

/// Délai au-delà duquel l'absence de heartbeat ferme la session (ms).
pub const HEARTBEAT_TIMEOUT_MS: u64 = 6_000;

/// Timeout global du handshake (ms).
pub const HANDSHAKE_TIMEOUT_MS: u64 = 5_000;

/// Taille de chunk par défaut pour les transferts de fichiers (KiB).
pub const DEFAULT_FILE_CHUNK_KIB: u32 = 256;

/// Identifiant logique d'un canal multiplexé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Channel {
    /// Canal de contrôle : handshake post-TLS, ping/pong, heartbeat, rotation de clé.
    Ctrl = 0,
    /// Canal d'événements input + clipboard.
    Input = 1,
    /// Canal de transfert de fichiers.
    Files = 2,
    /// Canal audio (UDP).
    Audio = 3,
    /// Canal vidéo (UDP).
    Video = 4,
}

impl Channel {
    /// Tentative de conversion depuis le `u8` brut du wire.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Ctrl),
            1 => Some(Self::Input),
            2 => Some(Self::Files),
            3 => Some(Self::Audio),
            4 => Some(Self::Video),
            _ => None,
        }
    }

    /// Valeur `u8` à écrire sur le wire.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}
