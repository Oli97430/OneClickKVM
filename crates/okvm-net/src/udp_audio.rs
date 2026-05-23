//! [`UdpAudioPipe`] — pont mpsc ⇄ UDP+FEC pour le canal audio d'une session.
//!
//! Spawn 2 tasks tokio :
//!
//! - **Sender** : lit `AudioMessage` depuis `app_audio_send_rx`, encode bincode,
//!   AEAD-seal + FEC, envoie via [`UdpFecSender`] à `remote_addr`. Si
//!   `remote_addr` est `None` au démarrage (cas serveur — on apprend l'addr
//!   du client via la 1re réception), buffer en interne et flushe dès qu'on
//!   pin l'addr.
//! - **Receiver** : lit via [`UdpFecReceiver`], décode bincode, push
//!   `AudioMessage` sur `app_audio_recv_tx`. Sur le 1er decrypt réussi, si
//!   `remote_addr` était `None`, pin l'addr (apprend l'endpoint du client).
//!
//! Le socket UDP est partagé entre les 2 tasks via `Arc<UdpSocket>`.
//!
//! ## Paramètres FEC par défaut
//!
//! K=1, M=1 (duplication simple) — sur LAN les pertes sont rares, et la
//! duplication double la bande passante mais reste sous 200% (≈ 130 kbps
//! pour Opus 64 kbps). Pour un WAN saturé, monter à K=4 M=2 (latence
//! accrue mais résistance à 33 % de perte).
//!
//! ## Pinning du remote_addr (NAT punching simple)
//!
//! Côté client : `remote_addr` est connu dès le handshake (`outcome.udp_ports`
//! + `tcp_remote.ip`). Pas de pinning nécessaire.
//!
//! Côté serveur : `remote_addr` est `None` au démarrage. Le client envoie
//! une frame audio dès que disponible ; le receiver pin l'addr source de la
//! 1re décryption réussie. Le sender attend ce pinning via une `Notify`
//! tokio. Si rien n'arrive pendant `pin_timeout` (5s par défaut), le sender
//! drop les frames sortantes (sera retenté la prochaine fois qu'il y a du
//! trafic inbound).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use okvm_crypto::{aead::AeadKey, AeadSession};
use okvm_protocol::AudioMessage;
use okvm_udp::{FecCodec, UdpFecReceiver, UdpFecSender};

/// Configuration FEC par défaut pour audio sur LAN : duplication 1+1.
pub const AUDIO_FEC_K: usize = 1;
/// Shards de parité par défaut.
pub const AUDIO_FEC_M: usize = 1;

/// (Non utilisé depuis REVIEW fix #1 — le sender ne bloque plus, il
/// re-check le pin à chaque frame.) Gardé pour compat docs.
#[allow(dead_code)]
const PIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Erreurs au démarrage du pipe.
#[derive(Debug, thiserror::Error)]
pub enum UdpAudioError {
    /// Paramètres FEC invalides.
    #[error("fec: {0}")]
    Fec(#[from] okvm_udp::FecError),
    /// `HandshakeOutcome.udp_keys` est `None` alors qu'on tente de wirer UDP.
    /// Le handshake n'a pas négocié de canal UDP (`udp_ports` était vide
    /// côté serveur, donc pas de KDF UDP côté client).
    #[error("UDP audio requested but handshake did not derive UDP keys")]
    MissingUdpKeys,
}

/// Handle de pilotage d'un [`UdpAudioPipe`].
pub struct UdpAudioPipe {
    /// Sender task : app_audio_send_rx → UDP.
    pub sender_task: JoinHandle<()>,
    /// Receiver task : UDP → app_audio_recv_tx.
    pub receiver_task: JoinHandle<()>,
    /// Addr distante (pinned ou pas) — accessible pour diagnostic.
    pub remote_addr: Arc<Mutex<Option<SocketAddr>>>,
}

impl UdpAudioPipe {
    /// Arrête les 2 tasks et attend leur fin.
    pub async fn shutdown(self) {
        self.sender_task.abort();
        self.receiver_task.abort();
        let _ = self.sender_task.await;
        let _ = self.receiver_task.await;
    }
}

/// Construit + démarre le pipe audio UDP.
///
/// - `socket` : socket UDP partagé (Arc). Côté serveur c'est la port qu'on
///   a annoncée dans `ServerFinished.udp_ports` ; côté client c'est un
///   socket éphémère bindé après le handshake.
/// - `send_key`, `recv_key` : extraites de `HandshakeOutcome.udp_keys`.
/// - `remote_addr_init` : `Some(addr)` côté client (on connaît le serveur),
///   `None` côté serveur (on apprend du 1er inbound).
/// - `app_audio_send_rx` : la queue applicative produisant les
///   `AudioMessage` à envoyer.
/// - `app_audio_recv_tx` : la queue applicative qui consomme les
///   `AudioMessage` reçus.
///
/// # Erreurs
/// [`UdpAudioError::Fec`] si K/M défaut sont rejetés par la lib.
pub fn spawn_pipe(
    socket: Arc<UdpSocket>,
    send_key: AeadKey,
    recv_key: AeadKey,
    remote_addr_init: Option<SocketAddr>,
    mut app_audio_send_rx: mpsc::Receiver<AudioMessage>,
    app_audio_recv_tx: mpsc::Sender<AudioMessage>,
) -> Result<UdpAudioPipe, UdpAudioError> {
    let remote_addr = Arc::new(Mutex::new(remote_addr_init));
    let pin_notify = Arc::new(Notify::new());

    // ----- Sender task -----
    let socket_send = socket.clone();
    let remote_send = remote_addr.clone();
    let pin_notify_send = pin_notify.clone();
    let sender_task = tokio::spawn(async move {
        // V3.1 step 7 + REVIEW fix #1 : on init FEC + AEAD une fois, mais on
        // diffère la création du `UdpFecSender` jusqu'à ce que `remote_send`
        // soit pinned. On re-check à chaque frame reçue → si le pin se fait
        // EN COURS de session (NAT punching tardif), le sender se "réveille"
        // automatiquement plutôt que de rester dans drain-and-drop permanent.
        let fec = match FecCodec::new(AUDIO_FEC_K, AUDIO_FEC_M) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = ?e, "FEC init échec, sender UDP audio arrêté");
                return;
            }
        };
        let mut sender: Option<UdpFecSender> = None;
        let mut frames_dropped_no_pin: u64 = 0;

        // Si l'addr est déjà connue (cas client), on instancie immédiatement
        // — pas besoin d'attendre une frame inbound.
        if let Some(addr) = *remote_send.lock() {
            sender = Some(UdpFecSender::new(
                socket_send.clone(),
                addr,
                AeadSession::new(&send_key, 1),
                fec,
            ));
        } else {
            // Cas serveur : on N'instancie PAS encore. Mais on log qu'on
            // attend. Le 1er frame applicatif déclenchera une nouvelle
            // tentative.
            tracing::debug!("UDP audio sender : attend que le receiver pin l'addr");
        }

        while let Some(msg) = app_audio_send_rx.recv().await {
            // Si on n'a pas encore de sender, tente de l'instancier (pin
            // peut avoir eu lieu entre 2 frames).
            if sender.is_none() {
                if let Some(addr) = *remote_send.lock() {
                    tracing::info!(?addr, "UDP audio sender activé (pin tardif détecté)");
                    // FecCodec et AeadSession non clonable → on les recrée
                    // à ce stade. Pour l'aead c'est OK car le seq counter
                    // de l'epoch 1 démarre à 0 dans tous les cas.
                    let fec_new = match FecCodec::new(AUDIO_FEC_K, AUDIO_FEC_M) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    sender = Some(UdpFecSender::new(
                        socket_send.clone(),
                        addr,
                        AeadSession::new(&send_key, 1),
                        fec_new,
                    ));
                } else {
                    // Toujours pas pinned → drop cette frame (compte pour
                    // visibilité). On évite d'attendre le pin_notify ici
                    // pour ne pas bloquer si l'app produit des frames audio
                    // en continu (cas réaliste).
                    frames_dropped_no_pin = frames_dropped_no_pin.saturating_add(1);
                    if frames_dropped_no_pin.is_power_of_two() {
                        tracing::debug!(
                            count = frames_dropped_no_pin,
                            "UDP audio frame droppée (remote addr pas encore pinned)"
                        );
                    }
                    continue;
                }
            }
            let Some(s) = sender.as_mut() else { continue };
            let bytes = match bincode::serde::encode_to_vec(&msg, bincode::config::standard()) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e, "bincode encode AudioMessage échec");
                    continue;
                }
            };
            if let Err(e) = s.send_frame(&bytes).await {
                tracing::debug!(error = ?e, "UDP audio send échec (frame droppée)");
            }
        }
        // pin_notify est désormais redundant — le ré-essai est fait à chaque
        // frame. Garde la variable pour ne pas changer l'API publique.
        let _ = pin_notify_send;
        tracing::debug!("UDP audio sender : app_audio_send_rx fermé, arrêt");
    });

    // ----- Receiver task -----
    let socket_recv = socket;
    let remote_recv = remote_addr.clone();
    let pin_notify_recv = pin_notify;
    let receiver_task = tokio::spawn(async move {
        let fec = match FecCodec::new(AUDIO_FEC_K, AUDIO_FEC_M) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = ?e, "FEC init échec, receiver UDP audio arrêté");
                return;
            }
        };
        let aead = AeadSession::new(&recv_key, 1);
        // Si on connaît déjà l'addr (client), on filtre dessus ; sinon on
        // accepte tout et on pin sur le premier decrypt OK.
        let initial_filter = *remote_recv.lock();
        let mut receiver = UdpFecReceiver::new(socket_recv, initial_filter, aead, fec);

        loop {
            match receiver.recv_frame().await {
                Ok((plaintext, src)) => {
                    // V3.1 step 7 : pin l'addr du peer sur le 1er decrypt
                    // réussi quand on n'avait pas de filter initial (cas
                    // serveur). Notifie le sender qui attendait peut-être
                    // de connaître l'addr.
                    if initial_filter.is_none() {
                        let was_unset = {
                            let mut guard = remote_recv.lock();
                            if guard.is_none() {
                                *guard = Some(src);
                                true
                            } else {
                                false
                            }
                        };
                        if was_unset {
                            tracing::info!(?src, "UDP audio: remote addr pinned");
                            pin_notify_recv.notify_waiters();
                        }
                    }
                    let msg: AudioMessage = match bincode::serde::decode_from_slice(
                        &plaintext,
                        bincode::config::standard(),
                    ) {
                        Ok((m, _)) => m,
                        Err(e) => {
                            tracing::warn!(error = %e, "bincode decode AudioMessage échec");
                            continue;
                        }
                    };
                    if app_audio_recv_tx.send(msg).await.is_err() {
                        tracing::debug!("app_audio_recv_tx fermé, receiver UDP audio arrêt");
                        return;
                    }
                }
                Err(e) => {
                    tracing::debug!(error = ?e, "UDP recv_frame erreur (continue)");
                }
            }
        }
    });

    Ok(UdpAudioPipe {
        sender_task,
        receiver_task,
        remote_addr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key() -> AeadKey {
        AeadKey::from_bytes([42u8; 32])
    }

    #[tokio::test]
    async fn audio_pipe_round_trip_loopback() {
        // Client → Serveur direct : les 2 connaissent l'addr de l'autre dès
        // le début. Cas naïf, valide juste le pipe encode/decode.
        use okvm_protocol::AudioMessage;
        use uuid::Uuid;

        let server_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_addr = server_sock.local_addr().unwrap();
        let client_addr = client_sock.local_addr().unwrap();

        let key = make_key();
        let (client_send_tx, client_send_rx) = mpsc::channel::<AudioMessage>(8);
        let (client_recv_tx, _client_recv_rx) = mpsc::channel::<AudioMessage>(8);
        let _client_pipe = spawn_pipe(
            client_sock,
            key.clone(),
            key.clone(),
            Some(server_addr),
            client_send_rx,
            client_recv_tx,
        )
        .unwrap();

        let (_server_send_tx, server_send_rx) = mpsc::channel::<AudioMessage>(8);
        let (server_recv_tx, mut server_recv_rx) = mpsc::channel::<AudioMessage>(8);
        let _server_pipe = spawn_pipe(
            server_sock,
            key.clone(),
            key,
            Some(client_addr),
            server_send_rx,
            server_recv_tx,
        )
        .unwrap();

        let msg = AudioMessage::StreamStart {
            stream_id: Uuid::nil(),
            codec: okvm_core::AudioCodec::Opus,
            sample_rate_hz: 48000,
            channels: 2,
            frame_size_samples: 960,
            source_name: "test loopback".into(),
        };
        client_send_tx.send(msg.clone()).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), server_recv_rx.recv())
            .await
            .expect("timeout — UDP audio pipe doit livrer en <2s")
            .expect("server_recv_rx fermé");
        assert_eq!(received, msg);
    }

    #[tokio::test]
    async fn audio_pipe_recovers_when_pin_happens_after_first_app_send() {
        // REVIEW fix #1 regression test : avant la correction, le sender
        // tombait dans drain-and-drop permanent si le pin arrivait après
        // le timeout 5s. Ce test démontre que :
        // 1. Serveur démarre sans remote_addr
        // 2. App pousse 2 frames AVANT le pin (elles sont droppées en silence)
        // 3. Client envoie 1 frame → pin
        // 4. App pousse 1 nouvelle frame → CETTE FOIS elle part bien
        use okvm_protocol::AudioMessage;
        use uuid::Uuid;

        let server_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_addr = server_sock.local_addr().unwrap();
        let key = make_key();

        // Serveur : pas d'addr.
        let (server_send_tx, server_send_rx) = mpsc::channel::<AudioMessage>(8);
        let (server_recv_tx, mut server_recv_rx) = mpsc::channel::<AudioMessage>(8);
        let server_pipe = spawn_pipe(
            server_sock,
            key.clone(),
            key.clone(),
            None,
            server_send_rx,
            server_recv_tx,
        )
        .unwrap();

        // Étape 2 : pousser 2 frames côté server AVANT le pin.
        // Avec la correction, ces frames sont droppées silencieusement
        // (sans bloquer le sender).
        let early = AudioMessage::StreamStop {
            stream_id: Uuid::from_u128(99),
        };
        server_send_tx.send(early.clone()).await.unwrap();
        server_send_tx.send(early.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Vérifie qu'il n'y a toujours pas de pin.
        assert!(server_pipe.remote_addr.lock().is_none());

        // Client : connaît serveur, envoie une frame.
        let (client_send_tx, client_send_rx) = mpsc::channel::<AudioMessage>(8);
        let (client_recv_tx, mut client_recv_rx) = mpsc::channel::<AudioMessage>(8);
        let _client_pipe = spawn_pipe(
            client_sock,
            key.clone(),
            key,
            Some(server_addr),
            client_send_rx,
            client_recv_tx,
        )
        .unwrap();

        let m1 = AudioMessage::StreamStop {
            stream_id: Uuid::from_u128(1),
        };
        client_send_tx.send(m1.clone()).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), server_recv_rx.recv())
            .await
            .expect("client→server doit aboutir")
            .expect("server recv closed");

        // Le pin a eu lieu (vérifié par la réception).
        assert!(server_pipe.remote_addr.lock().is_some());

        // Étape 4 : nouvelle frame côté server APRÈS pin → doit partir.
        let m_late = AudioMessage::StreamStop {
            stream_id: Uuid::from_u128(77),
        };
        server_send_tx.send(m_late.clone()).await.unwrap();
        let r = tokio::time::timeout(Duration::from_secs(2), client_recv_rx.recv())
            .await
            .expect("server→client APRÈS pin tardif doit aboutir (régression fix #1)")
            .expect("client recv closed");
        assert_eq!(r, m_late);
    }

    #[tokio::test]
    async fn audio_pipe_server_pins_remote_on_first_frame() {
        // V3.1 step 7 : démontre le vrai NAT pinning. Le serveur démarre
        // avec remote_addr = None ; quand le client envoie sa première
        // frame, le serveur pin l'addr et peut ensuite renvoyer dans le
        // sens inverse (server → client).
        use okvm_protocol::AudioMessage;
        use uuid::Uuid;

        let server_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_addr = server_sock.local_addr().unwrap();

        let key = make_key();

        // Client : connaît serveur.
        let (client_send_tx, client_send_rx) = mpsc::channel::<AudioMessage>(8);
        let (client_recv_tx, mut client_recv_rx) = mpsc::channel::<AudioMessage>(8);
        let _client_pipe = spawn_pipe(
            client_sock,
            key.clone(),
            key.clone(),
            Some(server_addr),
            client_send_rx,
            client_recv_tx,
        )
        .unwrap();

        // Serveur : ne connaît PAS le client au démarrage (NAT pinning auto).
        let (server_send_tx, server_send_rx) = mpsc::channel::<AudioMessage>(8);
        let (server_recv_tx, mut server_recv_rx) = mpsc::channel::<AudioMessage>(8);
        let server_pipe = spawn_pipe(
            server_sock,
            key.clone(),
            key,
            None, // <-- pas d'addr initiale
            server_send_rx,
            server_recv_tx,
        )
        .unwrap();
        assert!(server_pipe.remote_addr.lock().is_none());

        // Le client envoie d'abord — le serveur pinera son addr.
        let m1 = AudioMessage::StreamStop {
            stream_id: Uuid::from_u128(1),
        };
        client_send_tx.send(m1.clone()).await.unwrap();

        let r1 = tokio::time::timeout(Duration::from_secs(2), server_recv_rx.recv())
            .await
            .expect("client→server timeout")
            .expect("server recv closed");
        assert_eq!(r1, m1);
        // Vérification du pinning.
        let pinned = *server_pipe.remote_addr.lock();
        assert!(pinned.is_some(), "serveur doit avoir pin l'addr");

        // Maintenant le serveur peut renvoyer vers le client.
        let m2 = AudioMessage::StreamStop {
            stream_id: Uuid::from_u128(2),
        };
        server_send_tx.send(m2.clone()).await.unwrap();

        let r2 = tokio::time::timeout(Duration::from_secs(2), client_recv_rx.recv())
            .await
            .expect("server→client timeout (pinning a échoué)")
            .expect("client recv closed");
        assert_eq!(r2, m2);
    }
}
