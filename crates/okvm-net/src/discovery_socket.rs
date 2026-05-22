//! Socket UDP pour broadcast/multicast de découverte.
//!
//! L'implémentation mDNS vit dans `okvm-discovery`. Cette crate fournit
//! uniquement l'abstraction réseau UDP indépendante du contenu.

use std::net::SocketAddr;

use async_trait::async_trait;

use okvm_core::Result;

/// Trait pour émettre/recevoir des datagrammes de découverte.
#[async_trait]
pub trait DiscoverySocket: Send + Sync {
    /// Envoie un payload à une adresse (broadcast ou unicast).
    async fn send_to(&self, payload: &[u8], target: SocketAddr) -> Result<()>;
    /// Reçoit un datagramme. Renvoie `(payload, src)`.
    async fn recv(&self) -> Result<(Vec<u8>, SocketAddr)>;
}
