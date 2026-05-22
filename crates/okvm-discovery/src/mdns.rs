//! Service mDNS via la crate `mdns-sd`.
//!
//! Annonce un service `_oneclick-kvm._tcp.local.` et browse les pairs
//! correspondants. Les TXT records portent :
//!
//! - `v=1`
//! - `id=<base64url(device_id_pub, 43 chars)>`
//! - `name=<hostname>`
//! - `caps=<u32 hexadecimal du bitmask>`

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::interval;

use okvm_core::{DeviceId, Result};

use crate::{DiscoveredPeer, DiscoverySource, PeerKey, SelfAnnounce};

const SERVICE_TYPE: &str = "_oneclick-kvm._tcp.local.";
const MDNS_VERSION: &str = "1";

/// Wrapper autour du daemon mDNS.
pub struct MdnsService {
    announce: SelfAnnounce,
    daemon: ServiceDaemon,
    /// Nom complet du service ajoute (pour pouvoir le retirer au shutdown).
    full_service_name: String,
}

impl MdnsService {
    /// Cree et demarre le daemon, et enregistre le service local.
    pub fn new(announce: SelfAnnounce) -> Result<Self> {
        let daemon =
            ServiceDaemon::new().map_err(|e| okvm_core::Error::Net(format!("mdns daemon: {e}")))?;

        let instance = unique_instance_name(&announce);
        let id_b64 = URL_SAFE_NO_PAD.encode(announce.device_id.0);
        let caps_hex = format!("{:08x}", announce.capabilities_short);

        // Construit le SuiviceInfo avec les TXT records.
        let host_ips: Vec<IpAddr> = match local_ips() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "impossible de lister les IP locales, mDNS sans IP");
                Vec::new()
            }
        };

        let mut props: HashMap<String, String> = HashMap::new();
        props.insert("v".into(), MDNS_VERSION.into());
        props.insert("id".into(), id_b64);
        props.insert("name".into(), announce.name.clone());
        props.insert("caps".into(), caps_hex);

        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &format!("{instance}.local."),
            host_ips.as_slice(),
            announce.tcp_port,
            Some(props),
        )
        .map_err(|e| okvm_core::Error::Net(format!("mdns service info: {e}")))?;

        let full_name = info.get_fullname().to_string();
        daemon
            .register(info)
            .map_err(|e| okvm_core::Error::Net(format!("mdns register: {e}")))?;

        tracing::info!(service = %full_name, "mDNS service registered");

        Ok(Self {
            announce,
            daemon,
            full_service_name: full_name,
        })
    }

    /// Boucle de browsing : recoit les `ServiceEvent` et les forwarde.
    pub async fn run(
        self,
        known: Arc<Mutex<HashMap<PeerKey, DiscoveredPeer>>>,
        tx: mpsc::Sender<DiscoveredPeer>,
        shutdown_rx: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        let receiver = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| okvm_core::Error::Net(format!("mdns browse: {e}")))?;

        // Conversion mpsc-crossbeam vers tokio : on consomme dans une task blocking.
        let (event_tx, mut event_rx) = mpsc::channel::<ServiceEvent>(64);
        let _bridge: std::thread::JoinHandle<()> = std::thread::Builder::new()
            .name("okvm-mdns-bridge".into())
            .spawn(move || {
                while let Ok(ev) = receiver.recv() {
                    if event_tx.blocking_send(ev).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| okvm_core::Error::Os(format!("mdns bridge thread: {e}")))?;

        // Periodic refresh : on declenche un browse a intervalle pour rafraichir.
        let mut tick = interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::debug!("mdns shutdown signal");
                        break;
                    }
                }
                _ = tick.tick() => {
                    // Le daemon mdns-sd rafraichit en interne ; tick sert juste
                    // de heartbeat pour la log eventuelle.
                }
                maybe_ev = event_rx.recv() => {
                    let Some(ev) = maybe_ev else { break; };
                    if let Some(peer) = handle_event(ev, &self.announce.device_id) {
                        let is_new = {
                            let mut g = known.lock();
                            let prev = g.insert(peer.device_id, peer.clone());
                            prev.as_ref() != Some(&peer)
                        };
                        if is_new && tx.send(peer).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }

        // Cleanup
        if let Err(e) = self.daemon.unregister(&self.full_service_name) {
            tracing::warn!(error = ?e, "mdns unregister failed");
        }
        let _ = self.daemon.shutdown();
        Ok(())
    }
}

fn handle_event(ev: ServiceEvent, self_id: &DeviceId) -> Option<DiscoveredPeer> {
    match ev {
        ServiceEvent::ServiceResolved(info) => {
            let props = info.get_properties();
            let id_b64 = props.get_property_val_str("id")?;
            let device_id_bytes = URL_SAFE_NO_PAD.decode(id_b64).ok()?;
            let device_id = DeviceId::from_slice(&device_id_bytes).ok()?;
            // Skip ourselves
            if device_id == *self_id {
                return None;
            }
            let name = props
                .get_property_val_str("name")
                .map_or_else(|| info.get_hostname().to_string(), str::to_string);
            let caps_hex = props.get_property_val_str("caps").unwrap_or("0");
            let capabilities_short = u32::from_str_radix(caps_hex, 16).unwrap_or(0);

            let port = info.get_port();
            let addr: SocketAddr = info
                .get_addresses()
                .iter()
                .next()
                .copied()
                .map(|ip| SocketAddr::new(ip, port))?;

            Some(DiscoveredPeer {
                device_id,
                name,
                addr,
                capabilities_short,
                source: DiscoverySource::Mdns,
            })
        }
        ServiceEvent::ServiceRemoved(_ty, name) => {
            tracing::debug!(service = %name, "mDNS service removed");
            None
        }
        _ => None,
    }
}

fn unique_instance_name(a: &SelfAnnounce) -> String {
    // Tronc base64url de l'id, prefixe lisible.
    let id_short = URL_SAFE_NO_PAD.encode(&a.device_id.0[..6]);
    format!("{}-{id_short}", sanitize(&a.name))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect()
}

fn local_ips() -> Result<Vec<IpAddr>> {
    // Implementation legere : on demande au socket UDP de "se connecter" a une
    // adresse externe (sans paquet emis) et on lit son local_addr.
    use std::net::UdpSocket;
    let mut out = Vec::new();
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("1.1.1.1:80").is_ok() {
            if let Ok(a) = s.local_addr() {
                out.push(a.ip());
            }
        }
    }
    if let Ok(s) = UdpSocket::bind("[::]:0") {
        if s.connect("[2606:4700:4700::1111]:80").is_ok() {
            if let Ok(a) = s.local_addr() {
                out.push(a.ip());
            }
        }
    }
    Ok(out)
}
