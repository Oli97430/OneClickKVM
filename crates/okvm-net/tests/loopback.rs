//! Test d'integration loopback : un Listener + un Connector echangent un
//! Ping/Pong chiffre apres handshake complet.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use okvm_core::Capabilities;
use okvm_crypto::generate_identity;
use okvm_net::{Connector, ConnectorConfig, Listener, ListenerConfig};
use okvm_protocol::CtrlMessage;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handshake_and_ping_pong() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("okvm_net=debug,okvm_crypto=info")
        .try_init();

    let server_id = generate_identity().unwrap();
    let client_id = generate_identity().unwrap();
    let caps = Capabilities::default_windows();

    // 1. Bind sur port aleatoire pour le test.
    let bind: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let tcp = tokio::net::TcpListener::bind(bind).await.unwrap();
    let local = tcp.local_addr().unwrap();
    drop(tcp); // libere : on rebind via Listener

    let listener_cfg = ListenerConfig {
        bind: local,
        handshake_timeout: Duration::from_secs(2),
        heartbeat_interval: Duration::from_millis(500),
        heartbeat_timeout: Duration::from_secs(2),
    };
    let acl: okvm_net::listener::AclHook = Arc::new(|_ch| Ok(()));
    let listener = Listener::new(listener_cfg.clone(), server_id, caps.clone(), acl);

    let (sess_tx, mut sess_rx) = mpsc::channel(1);
    let server_task = tokio::spawn(async move {
        let _ = listener.run(sess_tx).await;
    });

    // Petit delai pour laisser le bind se faire avant le connect.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 2. Connect cote client.
    let connector_cfg = ConnectorConfig {
        remote: local,
        connect_timeout: Duration::from_secs(2),
        handshake_timeout: Duration::from_secs(2),
        heartbeat_interval: Duration::from_millis(500),
        heartbeat_timeout: Duration::from_secs(2),
        desired_channels: ConnectorConfig::default().desired_channels,
        pairing_pin: None,
    };
    let connector = Connector::new(connector_cfg, client_id, caps);
    let mut client_session = connector.connect().await.expect("client handshake");
    let mut server_session = tokio::time::timeout(Duration::from_secs(2), sess_rx.recv())
        .await
        .expect("accept timeout")
        .expect("listener ferme");

    // 3. Le client envoie Ping, le serveur recoit Ping et repond Pong.
    let ping = CtrlMessage::Ping { ts_ms: 12345 };
    client_session.ctrl_tx.send(ping).await.unwrap();

    let received_at_server = tokio::time::timeout(Duration::from_secs(2), async {
        // On filtre les heartbeats potentiels (cadence 500ms).
        loop {
            let msg = server_session.ctrl_rx.recv().await.unwrap();
            if matches!(msg, CtrlMessage::Heartbeat { .. }) {
                continue;
            }
            return msg;
        }
    })
    .await
    .expect("server ping recv timeout");

    let peer_ts = match received_at_server {
        CtrlMessage::Ping { ts_ms } => ts_ms,
        other => panic!("attendu Ping, recu {other:?}"),
    };
    assert_eq!(peer_ts, 12345);

    server_session
        .ctrl_tx
        .send(CtrlMessage::Pong {
            ts_ms: 67890,
            peer_ts_ms: peer_ts,
        })
        .await
        .unwrap();

    let pong = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let m = client_session.ctrl_rx.recv().await.unwrap();
            if matches!(m, CtrlMessage::Heartbeat { .. }) {
                continue;
            }
            return m;
        }
    })
    .await
    .expect("client pong timeout");
    match pong {
        CtrlMessage::Pong { ts_ms, peer_ts_ms } => {
            assert_eq!(ts_ms, 67890);
            assert_eq!(peer_ts_ms, 12345);
        }
        other => panic!("attendu Pong, recu {other:?}"),
    }

    // 4. Cleanup propre.
    client_session.shutdown_and_wait().await;
    server_session.shutdown_and_wait().await;
    server_task.abort();
}
