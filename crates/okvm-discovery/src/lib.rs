//! `okvm-discovery` — decouverte de pairs `OneClick` KVM sur le LAN.
//!
//! Deux mecanismes complementaires (cf. `docs/PROTOCOL.md` §5) :
//!
//! - **mDNS** : service `_oneclick-kvm._tcp.local.` annonce via `mdns-sd`.
//! - **Broadcast UDP** : fallback sur `255.255.255.255:47100` quand mDNS
//!   est filtre par un routeur ou un pare-feu d'entreprise.
//!
//! L'app instancie un [`DiscoveryService`] qui :
//!
//! - annonce **ce** PC en parallele via les deux mecanismes,
//! - decouvre les pairs distants et envoie des [`DiscoveredPeer`] sur un
//!   `mpsc::Sender` fourni par l'app.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

pub mod mdns;
pub mod udp;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use okvm_core::{DeviceId, Result};

pub use mdns::MdnsService;
pub use udp::UdpDiscovery;

/// Identifiant logique d'un pair decouvert (= `DeviceId` long-terme).
pub type PeerKey = DeviceId;

/// Une decouverte de pair distant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPeer {
    /// Identite long-terme.
    pub device_id: DeviceId,
    /// Nom convivial (hostname ou alias).
    pub name: String,
    /// Adresse a laquelle initier le handshake.
    pub addr: SocketAddr,
    /// Bitmask de capacites annoncees.
    pub capabilities_short: u32,
    /// Source de la decouverte (utile pour debug et UI).
    pub source: DiscoverySource,
}

/// Source d'une decouverte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySource {
    /// Service mDNS `_oneclick-kvm._tcp.local.`.
    Mdns,
    /// Broadcast UDP `255.255.255.255:47100`.
    UdpBroadcast,
}

/// Configuration d'annonce de soi-meme.
#[derive(Debug, Clone)]
pub struct SelfAnnounce {
    /// Identite publique de ce PC.
    pub device_id: DeviceId,
    /// Nom convivial expose.
    pub name: String,
    /// Port TCP du serveur de handshake.
    pub tcp_port: u16,
    /// Bitmask de capacites.
    pub capabilities_short: u32,
}

/// Bits du `capabilities_short`. A garder en phase avec `okvm-protocol`.
pub mod caps_bits {
    /// KM (clavier/souris) supporte.
    pub const KM: u32 = 1 << 0;
    /// KVM (KM + video) supporte.
    pub const KVM: u32 = 1 << 1;
    /// Audio streaming supporte.
    pub const AUDIO: u32 = 1 << 2;
    /// Video streaming supporte.
    pub const VIDEO: u32 = 1 << 3;
    /// Wake-on-LAN supporte.
    pub const WOL: u32 = 1 << 4;
    /// Verrouillage distant supporte.
    pub const LOCK: u32 = 1 << 5;
    /// Transfert de fichiers supporte.
    pub const FILES: u32 = 1 << 6;
}

/// Service global de decouverte : annonce + reception, double pile (mDNS + UDP).
pub struct DiscoveryService {
    /// Pairs vus recemment, indexes par identite.
    pub known: Arc<Mutex<HashMap<PeerKey, DiscoveredPeer>>>,
    handles: Vec<JoinHandle<()>>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl DiscoveryService {
    /// Demarre le service avec annonce de soi et envoi des pairs decouverts sur `tx`.
    ///
    /// - `enable_mdns` : active l'annonce + decouverte mDNS.
    /// - `enable_broadcast` : active l'annonce + decouverte UDP broadcast.
    pub fn start(
        announce: SelfAnnounce,
        tx: mpsc::Sender<DiscoveredPeer>,
        enable_mdns: bool,
        enable_broadcast: bool,
    ) -> Result<Self> {
        let known: Arc<Mutex<HashMap<PeerKey, DiscoveredPeer>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let mut handles = Vec::new();

        if enable_mdns {
            let mdns = MdnsService::new(announce.clone())?;
            let known_c = known.clone();
            let tx_c = tx.clone();
            let mut sd_rx = shutdown_rx.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = mdns.run(known_c, tx_c, &mut sd_rx).await {
                    tracing::warn!(error = %e, "mdns service stop");
                }
            }));
        }

        if enable_broadcast {
            let udp = UdpDiscovery::new(announce);
            let known_c = known.clone();
            let tx_c = tx;
            let mut sd_rx = shutdown_rx;
            handles.push(tokio::spawn(async move {
                if let Err(e) = udp.run(known_c, tx_c, &mut sd_rx).await {
                    tracing::warn!(error = %e, "udp discovery stop");
                }
            }));
        }

        Ok(Self {
            known,
            handles,
            shutdown: shutdown_tx,
        })
    }

    /// Arrete le service et attend la fin des tasks.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        for h in self.handles {
            let _ = h.await;
        }
    }
}
