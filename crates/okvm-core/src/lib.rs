//! `okvm-core` — types et utilitaires partagés par toutes les crates `OneClick` KVM.
//!
//! Cette crate **ne dépend d'aucune autre crate du workspace**. Elle expose :
//!
//! - [`DeviceId`] : identifiant Ed25519 long-terme d'une installation.
//! - [`Fingerprint`] : empreinte SHA-256 publique tronquée, lisible humainement.
//! - [`PeerId`] : identifiant logique d'une session (UUID v7).
//! - [`Capabilities`], [`ScreenInfo`], etc. : descripteurs échangés au handshake.
//! - [`Error`], [`Result`] : type d'erreur commun à tout le projet.
//! - [`Edge`], [`MouseButton`], etc. : énumérations applicatives partagées.
//!
//! Voir `docs/ARCHITECTURE.md` et `docs/PROTOCOL.md` pour le contexte.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod caps;
pub mod error;
pub mod ids;
pub mod input;
pub mod time;

pub use caps::{
    AudioCodec, Capabilities, OsInfo, Permission, PermissionPolicy, ScreenInfo, VideoCodec,
};
pub use error::{Error, Result};
pub use ids::{DeviceId, Fingerprint, IdentityKeypair, PeerId};
pub use input::{ButtonState, ClipboardFormat, Edge, MouseButton, TouchPhase};
pub use time::Timestamp;
