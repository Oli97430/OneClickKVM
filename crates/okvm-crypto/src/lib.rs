//! `okvm-crypto` — primitives cryptographiques de `OneClick` KVM.
//!
//! Cette crate fournit :
//!
//! - [`identity`] : génération et persistance (in-memory) d'une paire Ed25519.
//! - [`handshake`] : machine d'état Noise-like (X25519 ECDH + Ed25519 sig).
//! - [`aead`] : chiffrement/déchiffrement AES-256-GCM avec nonce déterministe.
//! - [`kdf`] : dérivation de clés HKDF-SHA256.
//!
//! Voir `docs/SECURITY.md` pour les choix et leur justification.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod aead;
pub mod handshake;
pub mod identity;
pub mod kdf;

pub use aead::{AeadKey, AeadSession, Direction, Nonce, AEAD_TAG_SIZE, NONCE_SIZE};
pub use handshake::{
    HandshakeError, HandshakeRole, HandshakeState, SessionSecrets, TRANSCRIPT_HASH_SIZE,
};
pub use identity::{generate_identity, IdentityError};
pub use kdf::derive_session_keys;
