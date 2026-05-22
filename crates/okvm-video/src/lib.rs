//! `okvm-video` — capture d'écran + MJPEG (V1).
//!
//! Architecture V1 (simple, fonctionne) :
//!
//! - **Capture** via `windows-capture` (Windows Graphics Capture API, plus
//!   simple et plus permissif que DXGI Desktop Duplication direct).
//! - Le callback recoit un buffer **BGRA 8-bit** ; on convertit en RGB, on
//!   downscale a une cible (defaut 1280x720), on encode en **JPEG** via le
//!   crate `image`, et on emet une [`okvm_protocol::VideoMessage::StreamFrame`]
//!   sur un `tokio::sync::mpsc`.
//! - Cadence cible : 15 fps (66 ms entre frames). Throttle simple cote callback.
//!
//! Limitations V1 :
//! - MJPEG = ~10 Mbps a 1280x720@15 (acceptable LAN, lourd Wi-Fi)
//! - Pas de keyframe ou de FEC : chaque frame est complete (MJPEG = pas d'inter-frame)
//! - Encodage CPU : pas hardware-accelere → utilise un thread dedie pour ne pas bloquer
//! - **Rendu cote receveur** : on rend les blobs JPEG bruts ; c'est le frontend
//!   (Tauri / Svelte) qui les affiche dans une `<img src="data:image/jpeg;base64,...">`.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

use async_trait::async_trait;
use tokio::sync::mpsc;

use okvm_core::Result;
use okvm_protocol::VideoMessage;

/// Backend H.264 disponible pour l'encodage côté capture.
///
/// - `Openh264` : implémentation Cisco référence pure software, portable.
/// - `MediaFoundation` : MFT Microsoft software (Windows-only). Souvent plus
///   rapide qu'openh264 grâce aux optimisations SSE/AVX. Le wrapper futur
///   D3D11Manager (V3.3) basculera ce backend sur NVENC / Quick Sync / AMF
///   sans changer l'API publique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum H264Backend {
    /// Cisco openh264 (par défaut, portable).
    Openh264,
    /// Microsoft Media Foundation H.264 MFT (Windows-only).
    MediaFoundation,
}

impl Default for H264Backend {
    fn default() -> Self {
        Self::Openh264
    }
}

/// Trait pour la capture vidéo.
#[async_trait]
pub trait VideoCapture: Send + Sync {
    /// Démarre la capture de l'écran d'index `screen_idx`.
    /// Émet des `VideoMessage::StreamFrame` sur `tx`.
    async fn start(
        &self,
        screen_idx: u32,
        tx: mpsc::Sender<VideoMessage>,
    ) -> Result<VideoHandle>;
}

/// Trait pour le rendu vidéo (côté master qui reçoit).
///
/// Pour cette V1, le "rendu" se fait cote frontend Svelte ; ce trait n'est
/// implemente que par un decoder de validation (verifie que les bytes sont
/// bien un JPEG decodable).
#[async_trait]
pub trait VideoRenderer: Send + Sync {
    /// Decode et pousse une frame dans la file de rendu.
    async fn push(&self, msg: VideoMessage) -> Result<()>;
}

/// Handle pour arrêter une capture.
pub struct VideoHandle {
    /// Signal d'arrêt.
    pub stop: tokio::sync::oneshot::Sender<()>,
    /// JoinHandle de la task bridge.
    pub bridge: tokio::task::JoinHandle<()>,
}

pub mod h264;

#[cfg(windows)]
pub mod mediafoundation;

#[cfg(windows)]
pub mod mediafoundation_encoder;

#[cfg(windows)]
pub mod win32;

#[cfg(windows)]
pub use win32::WindowsCaptureSource;

pub use h264::{H264Config, H264Decoder, H264Encoder};

#[cfg(windows)]
pub use mediafoundation::{
    enumerate_h264_encoders, has_hardware_h264, log_hardware_h264_status, H264EncoderInfo,
};

#[cfg(windows)]
pub use mediafoundation_encoder::{rgb_to_nv12, MfH264Encoder};
