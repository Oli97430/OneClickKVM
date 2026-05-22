//! `okvm-audio` — capture WASAPI loopback + lecture audio via cpal.
//!
//! Architecture V1 :
//!
//! - **Capture** (`AudioCapture`) : ouvre le device de sortie par defaut en
//!   mode loopback via cpal (sur Windows, cpal active `AUDCLNT_STREAMFLAGS_LOOPBACK`
//!   automatiquement quand on appelle `build_input_stream` sur un output device).
//! - Le callback cpal (thread temps reel) accumule les samples PCM f32 dans
//!   un buffer, et toutes les ~20 ms convertit en s16le et emet un
//!   [`okvm_protocol::AudioMessage::StreamFrame`] sur le `tokio::sync::mpsc`.
//! - **Playback** (`AudioPlayback`) : ouvre le device de sortie par defaut,
//!   maintient un ring buffer de samples PCM, et le callback cpal le lit.
//!
//! V1 limitations :
//! - PCM s16le brut, pas d'Opus → bande passante ~1.5 Mbps a 48 kHz stereo
//! - Pas de jitter buffer evolue : juste une file simple
//! - Pas de gestion de re-echantillonnage entre source et destination

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
use okvm_protocol::AudioMessage;

/// Trait pour la capture audio loopback.
#[async_trait]
pub trait AudioCapture: Send + Sync {
    /// Demarre la capture du peripherique de sortie par defaut.
    /// Emet des `AudioMessage::StreamFrame` sur `tx`.
    async fn start(&self, tx: mpsc::Sender<AudioMessage>) -> Result<AudioHandle>;
}

/// Trait pour la lecture audio recue d'un pair.
#[async_trait]
pub trait AudioPlayback: Send + Sync {
    /// Pousse une frame recue dans la file de lecture.
    async fn push(&self, msg: AudioMessage) -> Result<()>;
}

/// Handle pour arreter une capture audio.
pub struct AudioHandle {
    /// Signal d'arret (drop pour arreter).
    pub stop: tokio::sync::oneshot::Sender<()>,
    /// `JoinHandle` de la task qui sert de pont vers tokio.
    pub bridge: tokio::task::JoinHandle<()>,
}

pub mod cpal_impl;

pub use cpal_impl::{CpalCapture, CpalPlayback};
