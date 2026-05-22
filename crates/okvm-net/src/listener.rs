//! Listener TCP dual-stack pour les connexions entrantes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;

use okvm_core::{Capabilities, IdentityKeypair};
use okvm_protocol::messages::RejectReason;

use crate::handshake::{drive_server, DriverError};
use crate::session::Session;

/// Configuration du listener.
#[derive(Debug, Clone)]
pub struct ListenerConfig {
    /// Adresse d'ecoute. `[::]:port` recommande pour dual-stack IPv6/IPv4.
    pub bind: SocketAddr,
    /// Timeout du handshake.
    pub handshake_timeout: Duration,
    /// Intervalle des heartbeats.
    pub heartbeat_interval: Duration,
    /// Timeout heartbeat.
    pub heartbeat_timeout: Duration,
}

impl Default for ListenerConfig {
    fn default() -> Self {
        Self {
            bind: "[::]:47101".parse().expect("addr litterale valide"),
            handshake_timeout: Duration::from_millis(okvm_protocol::consts::HANDSHAKE_TIMEOUT_MS),
            heartbeat_interval: Duration::from_millis(okvm_protocol::consts::HEARTBEAT_INTERVAL_MS),
            heartbeat_timeout: Duration::from_millis(okvm_protocol::consts::HEARTBEAT_TIMEOUT_MS),
        }
    }
}

/// Hook ACL applicatif appelé **avant** d'accepter chaque connexion entrante.
///
/// Le hook reçoit le [`ClientHello`] complet et doit décider d'accepter
/// (`Ok(())`) ou de rejeter (`Err(RejectReason)`). Le serveur appelle ce hook
/// **après** le parse + check de magic/version mais **avant** la dérivation
/// des clés AEAD, ce qui permet de rejeter rapidement les pairs non
/// autorisés sans surcoût crypto.
///
/// ## Responsabilités déléguées au hook
///
/// `okvm-net` n'a **aucune notion** d'identités autorisées, de PIN, ou
/// d'expiration : ces concepts sont applicatifs et restent dans le hook.
/// Concrètement le hook est responsable de :
///
/// - **Filtrage par identité** : vérifier `ch.identity_pub` contre la liste
///   des pairs déjà appairés.
/// - **Validation du PIN** : si `ch.pairing_pin_hash` est `Some`, recomputer
///   `SHA-256(pin_attendu || ch.nonce)` et comparer en **temps constant**
///   (cf. `subtle::ConstantTimeEq`).
/// - **Rate-limit / anti-brute-force** : compter les tentatives ratées et
///   couper le pairing après N essais (cf. `MAX_PIN_ATTEMPTS` côté shell).
/// - **Politique générale** : accepter / refuser selon le mode courant
///   (pairing activé / désactivé, plafond connexions atteint, etc.).
///
/// Un hook permissif `|_ch| Ok(())` accepte **tout le monde**, ce qui n'est
/// jamais ce que vous voulez en prod.
///
/// [`ClientHello`]: okvm_protocol::handshake_msg::ClientHello
pub type AclHook = Arc<
    dyn Fn(&okvm_protocol::handshake_msg::ClientHello) -> Result<(), RejectReason> + Send + Sync,
>;

/// Listener TCP de `OneClick` KVM.
pub struct Listener {
    cfg: ListenerConfig,
    identity: IdentityKeypair,
    capabilities: Capabilities,
    acl: AclHook,
}

impl Listener {
    /// Construit un nouveau listener.
    pub fn new(
        cfg: ListenerConfig,
        identity: IdentityKeypair,
        capabilities: Capabilities,
        acl: AclHook,
    ) -> Self {
        Self {
            cfg,
            identity,
            capabilities,
            acl,
        }
    }

    /// Demarre l'ecoute et envoie chaque [`Session`] etablie sur `tx`.
    ///
    /// La boucle se termine quand `tx` est ferme cote receveur, ou si le bind
    /// initial echoue.
    pub async fn run(self, tx: mpsc::Sender<Session>) -> Result<(), DriverError> {
        let tcp = TcpListener::bind(self.cfg.bind).await?;
        let local_addr = tcp.local_addr()?;
        tracing::info!(addr = %local_addr, "okvm-net listener: bound");
        loop {
            let (mut stream, peer_addr) = match tcp.accept().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "accept echec — boucle continue");
                    continue;
                }
            };
            // TCP no-delay : on veut la latence input minimale.
            let _ = stream.set_nodelay(true);
            tracing::debug!(peer = %peer_addr, "accept");

            let identity = self.identity.clone();
            let caps = self.capabilities.clone();
            let acl = self.acl.clone();
            let cfg = self.cfg.clone();
            let tx2 = tx.clone();

            tokio::spawn(async move {
                let outcome = match drive_server(
                    &mut stream,
                    identity,
                    caps,
                    move |ch| (acl)(ch),
                    cfg.handshake_timeout,
                )
                .await
                {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::info!(peer = %peer_addr, error = %e, "handshake serveur echec");
                        return;
                    }
                };
                let session = Session::start(
                    stream,
                    outcome,
                    cfg.heartbeat_interval,
                    cfg.heartbeat_timeout,
                );
                if tx2.send(session).await.is_err() {
                    tracing::debug!("receiver de sessions ferme — abandon");
                }
            });

            // Si le receiver est ferme, on sort.
            if tx.is_closed() {
                tracing::info!("listener: receiver ferme, sortie");
                return Ok(());
            }
        }
    }
}
