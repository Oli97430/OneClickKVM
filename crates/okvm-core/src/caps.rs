//! Descripteurs de capacités et de permissions échangés au handshake.
//!
//! Voir `docs/PROTOCOL.md` §2.5 pour le wire format détaillé.

use serde::{Deserialize, Serialize};

/// Capacités annoncées par un pair lors du handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Version applicative (semver), p. ex. `"0.1.0"`.
    pub app_version: String,

    /// Informations OS.
    pub os: OsInfo,

    /// Écrans physiquement présents sur ce PC.
    pub screens: Vec<ScreenInfo>,

    /// Le pair peut **fournir** un flux audio loopback (sortie en capture).
    pub audio_capture: bool,

    /// Le pair peut **fournir** un flux vidéo (capture d'écran encodée).
    pub video_capture: bool,

    /// Codecs vidéo supportés (en *décodage* comme en *encodage*).
    pub video_codecs: Vec<VideoCodec>,

    /// Codecs audio supportés.
    pub audio_codecs: Vec<AudioCodec>,

    /// Le pair supporte les raccourcis/hotkeys globaux.
    pub hotkeys_supported: bool,

    /// Taille max d'un chunk de fichier acceptée en réception (en KiB).
    pub file_max_chunk_kib: u32,

    /// Langues UI exposées (codes BCP-47, ex. `"fr"`, `"en-US"`).
    pub languages: Vec<String>,
}

impl Capabilities {
    /// Capacités par défaut d'un PC Windows fraîchement installé.
    /// Utilisé pour les tests et comme socle modifiable.
    #[must_use]
    pub fn default_windows() -> Self {
        Self {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            os: OsInfo::current_windows_stub(),
            screens: Vec::new(),
            audio_capture: true,
            video_capture: true,
            video_codecs: vec![VideoCodec::H264, VideoCodec::Mjpeg],
            audio_codecs: vec![AudioCodec::Opus, AudioCodec::Pcm16],
            hotkeys_supported: true,
            file_max_chunk_kib: 256,
            languages: vec!["fr".into(), "en".into()],
        }
    }
}

/// Informations système courantes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsInfo {
    /// Famille OS : `"windows"`, `"linux"`, `"macos"`.
    pub family: String,
    /// Version brute, p. ex. `"10.0.26100"`.
    pub version: String,
    /// Architecture CPU : `"x86_64"`, `"aarch64"`.
    pub arch: String,
    /// Nom d'hôte `NetBIOS` / hostname.
    pub hostname: String,
}

impl OsInfo {
    /// Construit un `OsInfo` Windows placeholder pour tests/bootstrap.
    /// La vraie détection vit dans `okvm-config`.
    #[must_use]
    pub fn current_windows_stub() -> Self {
        Self {
            family: "windows".into(),
            version: "10.0.0".into(),
            arch: std::env::consts::ARCH.into(),
            hostname: "PC".into(),
        }
    }
}

/// Description d'un écran physique côté pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenInfo {
    /// Index logique côté pair (0..N).
    pub index: u32,
    /// `true` si c'est l'écran principal de l'OS.
    pub is_primary: bool,
    /// Largeur en pixels physiques.
    pub width_px: u32,
    /// Hauteur en pixels physiques.
    pub height_px: u32,
    /// DPI logique (96 = 100 %, 144 = 150 %, etc.).
    pub dpi: u32,
    /// Position X de l'origine dans le bureau virtuel local.
    pub origin_x: i32,
    /// Position Y de l'origine dans le bureau virtuel local.
    pub origin_y: i32,
}

/// Codecs vidéo supportés pour le streaming KVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum VideoCodec {
    /// H.264 / AVC — meilleure compatibilité matérielle.
    H264 = 0,
    /// H.265 / HEVC — meilleur ratio compression.
    H265 = 1,
    /// AV1 — futur, encodeurs hardware émergents.
    Av1 = 2,
    /// Motion JPEG — fallback CPU sans accélération.
    Mjpeg = 100,
}

/// Codecs audio supportés.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AudioCodec {
    /// Opus — par défaut, faible latence.
    Opus = 0,
    /// PCM 16-bit brut.
    Pcm16 = 1,
    /// AAC.
    Aac = 2,
}

/// Politique de permission attribuée à un pair pour une capacité donnée.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PermissionPolicy {
    /// Autorisé sans confirmation.
    Allow,
    /// Refusé d'emblée.
    Deny,
    /// Affiche une demande à l'utilisateur à chaque tentative.
    #[default]
    Prompt,
}

/// Permissions configurables pour chaque pair appairé.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permission {
    /// Injection clavier/souris.
    pub input: PermissionPolicy,
    /// Synchronisation clipboard texte/RTF/HTML.
    pub clipboard_text: PermissionPolicy,
    /// Synchronisation clipboard image.
    pub clipboard_image: PermissionPolicy,
    /// Réception de fichiers entrants.
    pub files_inbound: PermissionPolicy,
    /// Envoi de fichiers sortants.
    pub files_outbound: PermissionPolicy,
    /// Capture audio loopback partagée.
    pub audio_capture: PermissionPolicy,
    /// Capture vidéo (KVM) partagée.
    pub video_capture: PermissionPolicy,
    /// Wake-on-LAN sortant vers le pair.
    pub wol: PermissionPolicy,
    /// Verrouillage / déverrouillage à distance.
    pub lock_unlock: PermissionPolicy,
}

impl Default for Permission {
    fn default() -> Self {
        Self {
            input: PermissionPolicy::Allow,
            clipboard_text: PermissionPolicy::Allow,
            clipboard_image: PermissionPolicy::Prompt,
            files_inbound: PermissionPolicy::Prompt,
            files_outbound: PermissionPolicy::Allow,
            audio_capture: PermissionPolicy::Deny,
            video_capture: PermissionPolicy::Deny,
            wol: PermissionPolicy::Allow,
            lock_unlock: PermissionPolicy::Allow,
        }
    }
}
