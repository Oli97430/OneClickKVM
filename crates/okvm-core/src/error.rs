//! Type d'erreur racine du projet.
//!
//! Chaque crate métier expose son propre `enum` d'erreur typé (`thiserror`),
//! mais toutes les variantes sont convertibles en [`Error`] via `From`. Cela
//! permet aux couches hautes (`okvm-ipc`, Tauri commands) de manipuler une
//! erreur unique sans dépendre de toutes les crates.

use std::io;

use thiserror::Error;

/// Alias `Result<T, Error>` utilisé par défaut dans tout le workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// Erreur racine englobant toutes les défaillances applicatives.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Erreur d'entrée/sortie système.
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// Erreur de sérialisation / désérialisation.
    #[error("serde: {0}")]
    Serde(String),

    /// Erreur cryptographique (handshake, AEAD, signature).
    #[error("crypto: {0}")]
    Crypto(String),

    /// Erreur de protocole (frame mal formée, version inconnue...).
    #[error("protocol: {0}")]
    Protocol(String),

    /// Erreur de transport (TCP/UDP).
    #[error("net: {0}")]
    Net(String),

    /// L'opération a été refusée par l'ACL.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// Timeout (handshake, heartbeat, ack...).
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),

    /// Le pair distant a fermé proprement la session.
    #[error("peer closed: {0}")]
    PeerClosed(String),

    /// Erreur Win32 ou autre API système.
    #[error("os: {0}")]
    Os(String),

    /// Configuration invalide ou manquante.
    #[error("config: {0}")]
    Config(String),

    /// Erreur générique avec contexte libre.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Construit une erreur générique à partir d'un message.
    pub fn other<S: Into<String>>(msg: S) -> Self {
        Self::Other(msg.into())
    }
}
