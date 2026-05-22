//! Structures sérialisables des 4 messages du handshake.
//!
//! Sérialisation via `bincode` v2 en mode `standard` (varint pour les
//! collections — sécurisé tant qu'on plafonne les tailles via `with_limit`
//! côté décodeur). Voir `docs/PROTOCOL.md` §2.

use serde::{Deserialize, Serialize};

use okvm_core::Capabilities;

use crate::serde_helpers::{
    bytes32, bytes4, bytes64, opt_bytes32,
};

/// Magic constant en tête des Hello : permet de rejeter rapidement les paquets erronés.
pub const HANDSHAKE_MAGIC: [u8; 4] = *b"OCKV";

/// Premier message envoyé par le client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    /// `HANDSHAKE_MAGIC`.
    #[serde(with = "bytes4")]
    pub magic: [u8; 4],
    /// Version du protocole supportée.
    pub protocol_version: u16,
    /// Bitfield (bit 0 = supports IPv6, bit 1 = pairing required, etc.).
    pub flags: u16,
    /// Nonce aléatoire 32 octets (anti-rejeu de session).
    #[serde(with = "bytes32")]
    pub nonce: [u8; 32],
    /// Clé publique X25519 éphémère pour cette session.
    #[serde(with = "bytes32")]
    pub ephemeral_pub: [u8; 32],
    /// Clé publique Ed25519 long-terme du PC client.
    #[serde(with = "bytes32")]
    pub identity_pub: [u8; 32],
    /// Capacités annoncées.
    pub capabilities: Capabilities,
    /// Hash du PIN d'appairage (`SHA-256(pin_utf8 || nonce)`) si pairing initial.
    #[serde(with = "opt_bytes32")]
    pub pairing_pin_hash: Option<[u8; 32]>,
}

/// Réponse du serveur.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    /// `HANDSHAKE_MAGIC`.
    #[serde(with = "bytes4")]
    pub magic: [u8; 4],
    /// Version du protocole choisie (typiquement min des deux côtés).
    pub protocol_version: u16,
    /// Flags miroir / réponse.
    pub flags: u16,
    /// Nonce serveur 32 octets.
    #[serde(with = "bytes32")]
    pub nonce: [u8; 32],
    /// Clé publique X25519 éphémère serveur.
    #[serde(with = "bytes32")]
    pub ephemeral_pub: [u8; 32],
    /// Clé publique Ed25519 du PC serveur.
    #[serde(with = "bytes32")]
    pub identity_pub: [u8; 32],
    /// Capacités annoncées par le serveur.
    pub capabilities: Capabilities,
    /// Indique au client si un appairage est requis pour finaliser.
    pub pairing_required: bool,
    /// Hash du PIN d'appairage côté serveur (challenge).
    #[serde(with = "opt_bytes32")]
    pub pairing_pin_hash: Option<[u8; 32]>,
    /// Signature Ed25519 du transcript `ClientHello || ServerHello(sans signature)`.
    #[serde(with = "bytes64")]
    pub signature: [u8; 64],
}

/// Premier message **chiffré** envoyé par le client après dérivation des clés.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFinished {
    /// Signature Ed25519 du transcript courant — authentifie le client.
    #[serde(with = "bytes64")]
    pub transcript_signature: [u8; 64],
    /// Description des canaux que le client demande à activer.
    pub selected_channels: Vec<ChannelDesc>,
}

/// Réponse finale du serveur, **chiffrée**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerFinished {
    /// `true` si la session est acceptée.
    pub accepted: bool,
    /// Raison de refus si `accepted == false`.
    pub reason: Option<crate::messages::RejectReason>,
    /// Ports UDP négociés `(channel_id, port)`.
    pub udp_ports: Vec<(u8, u16)>,
}

/// Réutilise les types `Transport` / `ChannelDesc` du module `messages` pour cohérence.
pub use crate::messages::{ChannelDesc, Transport};

#[cfg(test)]
mod tests {
    use super::*;
    use bincode::config::{standard, Configuration};

    fn cfg() -> Configuration {
        standard()
    }

    #[test]
    fn client_hello_round_trip() {
        let ch = ClientHello {
            magic: HANDSHAKE_MAGIC,
            protocol_version: 1,
            flags: 0,
            nonce: [1u8; 32],
            ephemeral_pub: [2u8; 32],
            identity_pub: [3u8; 32],
            capabilities: Capabilities::default_windows(),
            pairing_pin_hash: None,
        };
        let bytes = bincode::serde::encode_to_vec(&ch, cfg()).unwrap();
        let (decoded, _): (ClientHello, _) =
            bincode::serde::decode_from_slice(&bytes, cfg()).unwrap();
        assert_eq!(decoded, ch);
    }

    #[test]
    fn server_hello_round_trip_with_signature() {
        let sh = ServerHello {
            magic: HANDSHAKE_MAGIC,
            protocol_version: 1,
            flags: 0,
            nonce: [9u8; 32],
            ephemeral_pub: [8u8; 32],
            identity_pub: [7u8; 32],
            capabilities: Capabilities::default_windows(),
            pairing_required: false,
            pairing_pin_hash: None,
            signature: [0u8; 64],
        };
        let bytes = bincode::serde::encode_to_vec(&sh, cfg()).unwrap();
        let (decoded, _): (ServerHello, _) =
            bincode::serde::decode_from_slice(&bytes, cfg()).unwrap();
        assert_eq!(decoded, sh);
    }
}
