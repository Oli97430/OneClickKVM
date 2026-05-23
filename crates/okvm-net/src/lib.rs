//! `okvm-net` — transport TCP chiffre, handshake et multiplexage de canaux.
//!
//! Architecture :
//!
//! ```text
//!  ┌────────────────────┐       ┌────────────────────┐
//!  │  app code (Tauri)  │       │  app code (Tauri)  │
//!  └─────────┬──────────┘       └─────────┬──────────┘
//!            │ Session API                │ Session API
//!  ┌─────────▼──────────┐       ┌─────────▼──────────┐
//!  │     Session        │       │     Session        │
//!  │  - writer task     │       │  - writer task     │
//!  │  - reader task     │  TCP  │  - reader task     │
//!  │  - heartbeat task  │◄─────►│  - heartbeat task  │
//!  │  - mpsc channels   │       │  - mpsc channels   │
//!  └─────────┬──────────┘       └─────────┬──────────┘
//!            │                            │
//!  ┌─────────▼──────────┐       ┌─────────▼──────────┐
//!  │  FrameCodec        │       │  FrameCodec        │
//!  │  (length-prefixed) │       │  (length-prefixed) │
//!  └─────────┬──────────┘       └─────────┬──────────┘
//!            │                            │
//!  ┌─────────▼──────────────────────────────▼────────┐
//!  │                  TCP socket                      │
//!  └──────────────────────────────────────────────────┘
//! ```
//!
//! ## Couches
//!
//! - [`codec::FrameCodec`] : `tokio_util::codec` qui sait extraire des frames
//!   `[len: u32 BE][header + AEAD payload]` depuis un `AsyncRead`.
//! - [`handshake`] : pilote le handshake 4-messages defini dans
//!   `docs/PROTOCOL.md`, en utilisant `okvm-crypto::handshake` et
//!   `okvm-protocol::handshake_msg`.
//! - [`session::Session`] : API publique cote application. Une session offre
//!   un envoi/reception type par canal (`InputMessage`, `CtrlMessage`,
//!   `FileMessage`).
//! - [`listener::Listener`] : accepte les sessions entrantes (dual-stack
//!   IPv6 par defaut, IPv4 acceptees via les adresses mappees `::ffff:`).
//! - [`connector::Connector`] : ouvre une session sortante.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod codec;
pub mod connector;
pub mod handshake;
pub mod listener;
pub mod session;
pub mod udp_audio;

pub use codec::{FrameCodec, MAX_LENGTH_PREFIX};
pub use connector::{Connector, ConnectorConfig};
pub use handshake::{drive_client, drive_server, HandshakeOutcome};
pub use listener::{Listener, ListenerConfig};
pub use session::{Session, SessionHandle};
pub use udp_audio::{spawn_pipe as spawn_udp_audio_pipe, UdpAudioError, UdpAudioPipe};
