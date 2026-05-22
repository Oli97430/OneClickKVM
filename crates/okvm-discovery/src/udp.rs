//! Decouverte par broadcast UDP — fallback quand mDNS est filtre.
//!
//! Un beacon `DiscoveryBeacon` est emis toutes les 5 s en broadcast sur le
//! port [`okvm_protocol::UDP_DISCOVERY_PORT`]. En reception, on parse le
//! datagramme, on filtre soi-meme, et on emet un [`DiscoveredPeer`].

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::interval;

use okvm_core::{DeviceId, Result};
use okvm_protocol::{DiscoveryBeacon, UDP_DISCOVERY_PORT};

use crate::{DiscoveredPeer, DiscoverySource, PeerKey, SelfAnnounce};

const BEACON_MAGIC: [u8; 4] = *b"OCKB";
const BEACON_VERSION: u16 = 1;

/// Service de decouverte UDP broadcast.
pub struct UdpDiscovery {
    announce: SelfAnnounce,
}

impl UdpDiscovery {
    /// Cree le service (sans le demarrer).
    #[must_use]
    pub fn new(announce: SelfAnnounce) -> Self {
        Self { announce }
    }

    /// Boucle d'envoi + reception jusqu'au signal de shutdown.
    pub async fn run(
        self,
        known: Arc<Mutex<HashMap<PeerKey, DiscoveredPeer>>>,
        tx: mpsc::Sender<DiscoveredPeer>,
        shutdown_rx: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, UDP_DISCOVERY_PORT))
            .await
            .map_err(|e| okvm_core::Error::Net(format!("udp bind {UDP_DISCOVERY_PORT}: {e}")))?;
        sock.set_broadcast(true)
            .map_err(|e| okvm_core::Error::Net(format!("udp set_broadcast: {e}")))?;
        tracing::info!(port = UDP_DISCOVERY_PORT, "udp discovery: bound");

        let beacon = build_beacon(&self.announce);
        let beacon_bytes = match bincode::serde::encode_to_vec(&beacon, bincode::config::standard()) {
            Ok(b) => b,
            Err(e) => return Err(okvm_core::Error::Serde(e.to_string())),
        };

        let broadcast_addr = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::BROADCAST,
            UDP_DISCOVERY_PORT,
        ));

        let mut emit_tick = interval(Duration::from_secs(5));
        let mut buf = vec![0u8; 4096];

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::debug!("udp discovery shutdown");
                        return Ok(());
                    }
                }
                _ = emit_tick.tick() => {
                    if let Err(e) = sock.send_to(&beacon_bytes, broadcast_addr).await {
                        tracing::warn!(error = %e, "udp beacon send echec");
                    }
                }
                recv = sock.recv_from(&mut buf) => {
                    match recv {
                        Ok((n, src)) => {
                            if let Some(peer) = parse_and_filter(&buf[..n], src, &self.announce.device_id) {
                                let is_new = {
                                    let mut g = known.lock();
                                    let prev = g.insert(peer.device_id, peer.clone());
                                    prev.as_ref() != Some(&peer)
                                };
                                if is_new && tx.send(peer).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "udp recv echec");
                        }
                    }
                }
            }
        }
    }
}

fn build_beacon(a: &SelfAnnounce) -> DiscoveryBeacon {
    DiscoveryBeacon {
        magic: BEACON_MAGIC,
        version: BEACON_VERSION,
        device_id_pub: a.device_id,
        name: a.name.clone(),
        capabilities_short: a.capabilities_short,
        tcp_port: a.tcp_port,
        ipv6_ok: true,
        screens_short: Vec::new(),
    }
}

fn parse_and_filter(bytes: &[u8], src: SocketAddr, self_id: &DeviceId) -> Option<DiscoveredPeer> {
    // Filtre magic avant le decode complet.
    // bincode encode `[u8; 4]` via l'impl tuple = 4 octets bruts sans prefixe.
    if bytes.len() < 4 || &bytes[..4] != &BEACON_MAGIC[..] {
        return None;
    }

    let (beacon, _) =
        bincode::serde::decode_from_slice::<DiscoveryBeacon, _>(bytes, bincode::config::standard())
            .ok()?;
    if beacon.magic != BEACON_MAGIC {
        return None;
    }
    if beacon.version != BEACON_VERSION {
        return None;
    }
    if beacon.device_id_pub == *self_id {
        return None;
    }

    let addr = SocketAddr::new(src.ip(), beacon.tcp_port);
    Some(DiscoveredPeer {
        device_id: beacon.device_id_pub,
        name: beacon.name,
        addr,
        capabilities_short: beacon.capabilities_short,
        source: DiscoverySource::UdpBroadcast,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_garbage() {
        let id = DeviceId([0u8; 32]);
        let src: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(parse_and_filter(b"", src, &id).is_none());
        assert!(parse_and_filter(b"hello world", src, &id).is_none());
    }

    #[test]
    fn parse_round_trip() {
        let me = DeviceId([1u8; 32]);
        let peer = DeviceId([2u8; 32]);
        let ann = SelfAnnounce {
            device_id: peer,
            name: "PeerName".into(),
            tcp_port: 47101,
            capabilities_short: 0xDEAD_BEEF,
        };
        let beacon = build_beacon(&ann);
        let bytes = bincode::serde::encode_to_vec(&beacon, bincode::config::standard()).unwrap();
        let src: SocketAddr = "10.0.0.5:55555".parse().unwrap();
        let out = parse_and_filter(&bytes, src, &me).expect("doit parser");
        assert_eq!(out.device_id, peer);
        assert_eq!(out.name, "PeerName");
        assert_eq!(out.addr.port(), 47101);
        assert_eq!(out.capabilities_short, 0xDEAD_BEEF);
    }

    #[test]
    fn parse_filters_self() {
        let me = DeviceId([7u8; 32]);
        let ann = SelfAnnounce {
            device_id: me,
            name: "Me".into(),
            tcp_port: 1,
            capabilities_short: 0,
        };
        let beacon = build_beacon(&ann);
        let bytes = bincode::serde::encode_to_vec(&beacon, bincode::config::standard()).unwrap();
        let src: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(parse_and_filter(&bytes, src, &me).is_none());
    }
}
