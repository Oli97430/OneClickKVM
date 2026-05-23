//! Connector TCP : ouvre une session sortante (cote client).

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::{TcpStream, UdpSocket};

use okvm_core::{Capabilities, IdentityKeypair};
use okvm_protocol::{messages::ChannelDesc, Channel};

use crate::handshake::{drive_client, DriverError};
use crate::session::Session;

/// Identifiant du canal audio dans les `udp_ports` annoncés par le serveur.
const UDP_CHANNEL_AUDIO: u8 = Channel::Audio as u8;

/// Configuration d'un connector.
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    /// Adresse du pair distant.
    pub remote: SocketAddr,
    /// Timeout de connexion TCP.
    pub connect_timeout: Duration,
    /// Timeout du handshake.
    pub handshake_timeout: Duration,
    /// Intervalle heartbeat.
    pub heartbeat_interval: Duration,
    /// Timeout heartbeat.
    pub heartbeat_timeout: Duration,
    /// Canaux demandes a la negociation.
    pub desired_channels: Vec<ChannelDesc>,
    /// PIN d'appairage si premiere connexion.
    pub pairing_pin: Option<String>,
}

impl Default for ConnectorConfig {
    fn default() -> Self {
        use okvm_protocol::messages::Transport;
        Self {
            remote: "127.0.0.1:47101".parse().expect("addr litterale valide"),
            connect_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_millis(okvm_protocol::consts::HANDSHAKE_TIMEOUT_MS),
            heartbeat_interval: Duration::from_millis(okvm_protocol::consts::HEARTBEAT_INTERVAL_MS),
            heartbeat_timeout: Duration::from_millis(okvm_protocol::consts::HEARTBEAT_TIMEOUT_MS),
            desired_channels: vec![
                ChannelDesc {
                    id: 0,
                    transport: Transport::Tcp,
                    udp_port: None,
                },
                ChannelDesc {
                    id: 1,
                    transport: Transport::Tcp,
                    udp_port: None,
                },
                ChannelDesc {
                    id: 2,
                    transport: Transport::Tcp,
                    udp_port: None,
                },
            ],
            pairing_pin: None,
        }
    }
}

/// Ouvre une session sortante.
pub struct Connector {
    cfg: ConnectorConfig,
    identity: IdentityKeypair,
    capabilities: Capabilities,
}

impl Connector {
    /// Construit un connector.
    #[must_use]
    pub fn new(
        cfg: ConnectorConfig,
        identity: IdentityKeypair,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            cfg,
            identity,
            capabilities,
        }
    }

    /// Connecte et execute le handshake.
    pub async fn connect(self) -> Result<Session, DriverError> {
        let mut stream = tokio::time::timeout(
            self.cfg.connect_timeout,
            TcpStream::connect(self.cfg.remote),
        )
        .await
        .map_err(|_| DriverError::Timeout)??;
        let _ = stream.set_nodelay(true);

        let outcome = drive_client(
            &mut stream,
            self.identity,
            self.capabilities,
            self.cfg.desired_channels,
            self.cfg.pairing_pin.as_deref(),
            self.cfg.handshake_timeout,
        )
        .await?;

        // V3.1 step 5+6 : si le serveur a annoncé un port UDP audio, on bind
        // un UDP socket local et on appelle start_with_udp(). Sinon fallback
        // sur Session::start() (audio TCP comme avant).
        let server_udp = outcome
            .udp_ports
            .iter()
            .find(|(ch, _)| *ch == UDP_CHANNEL_AUDIO)
            .copied();
        let session = if let Some((_chan, server_udp_port)) = server_udp {
            let server_udp_addr = SocketAddr::new(self.cfg.remote.ip(), server_udp_port);
            let bind_local_ip = match self.cfg.remote {
                SocketAddr::V4(_) => "0.0.0.0".parse().unwrap(),
                SocketAddr::V6(_) => "::".parse().unwrap(),
            };
            match UdpSocket::bind(SocketAddr::new(bind_local_ip, 0)).await {
                Ok(socket) => {
                    tracing::info!(
                        local = ?socket.local_addr().ok(),
                        remote = %server_udp_addr,
                        "session client avec audio UDP+FEC"
                    );
                    match Session::start_with_udp(
                        stream,
                        outcome,
                        std::sync::Arc::new(socket),
                        Some(server_udp_addr),
                        self.cfg.heartbeat_interval,
                        self.cfg.heartbeat_timeout,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                error = ?e,
                                "start_with_udp echec, fallback TCP audio"
                            );
                            // Pas de fallback possible sans re-construire la session ;
                            // on propage l'échec.
                            return Err(DriverError::Codec(format!("UDP audio init: {e}")));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "client UDP bind echec — fallback TCP audio");
                    Session::start(
                        stream,
                        outcome,
                        self.cfg.heartbeat_interval,
                        self.cfg.heartbeat_timeout,
                    )
                }
            }
        } else {
            Session::start(
                stream,
                outcome,
                self.cfg.heartbeat_interval,
                self.cfg.heartbeat_timeout,
            )
        };
        Ok(session)
    }
}
