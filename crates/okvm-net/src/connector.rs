//! Connector TCP : ouvre une session sortante (cote client).

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpStream;

use okvm_core::{Capabilities, IdentityKeypair};
use okvm_protocol::messages::ChannelDesc;

use crate::handshake::{drive_client, DriverError};
use crate::session::Session;

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
        Ok(Session::start(
            stream,
            outcome,
            self.cfg.heartbeat_interval,
            self.cfg.heartbeat_timeout,
        ))
    }
}
