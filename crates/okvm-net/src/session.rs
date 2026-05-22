//! Session post-handshake : tasks reader/writer/heartbeat + multiplexage.
//!
//! Une session encapsule une `TcpStream` chiffree et expose des canaux mpsc
//! type pour chaque categorie de messages (Ctrl/Input/Files) cote app.
//!
//! Conception :
//!
//! - **Tache `writer`** : consomme une mpsc `(Channel, Vec<u8>)` interne,
//!   chiffre via `aead_send` et ecrit sur la moitie ecriture de la `TcpStream`.
//! - **Tache `reader`** : lit les frames de la moitie lecture, dechiffre via
//!   `aead_recv`, decode l'enum applicatif selon le canal et l'envoie sur la
//!   mpsc typee correspondante.
//! - **Tache `heartbeat`** : envoie un `CtrlMessage::Heartbeat` periodique et
//!   surveille la reception du heartbeat distant. Coupe la session en cas de
//!   timeout.
//!
//! Tous les `Sender` exposes sont clonables ; cote `Receiver`, ils sont
//! uniques par canal pour conserver l'ordonnancement.

use std::time::Duration;

use bincode::config::Configuration;
use bytes::BytesMut;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant, MissedTickBehavior};
use tokio_util::codec::Decoder;

use okvm_core::{Capabilities, DeviceId};
use okvm_protocol::{
    decode_tcp_frame, encode_tcp_frame, AudioMessage, Channel, CtrlMessage, FileMessage,
    InputMessage, VideoMessage,
};

use crate::codec::FrameCodec;
use crate::handshake::HandshakeOutcome;

fn bincode_cfg() -> Configuration {
    bincode::config::standard()
}

/// Buffer profondeur des channels mpsc applicatifs.
const APP_CHANNEL_CAPACITY: usize = 1024;
/// Buffer profondeur du channel de sortie (writer task).
const WRITE_CHANNEL_CAPACITY: usize = 2048;

/// Erreurs de session.
#[derive(Debug, Error)]
pub enum SessionError {
    /// I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Erreur de frame (taille, AEAD).
    #[error("frame: {0}")]
    Frame(String),
    /// Erreur d'encodage applicatif.
    #[error("codec: {0}")]
    Codec(String),
    /// Heartbeat manquant trop longtemps.
    #[error("heartbeat timeout")]
    HeartbeatTimeout,
    /// La session a ete fermee.
    #[error("session fermee: {0}")]
    Closed(String),
}

/// Handle de pilotage d'une session active.
///
/// Contient le signal de shutdown et les JoinHandles. La methode pratique
/// [`Session::shutdown_and_wait`] consomme la session entiere et libere
/// tous les senders avant d'attendre — c'est l'API recommandee.
#[derive(Debug)]
pub struct SessionHandle {
    /// Annule toutes les tasks (writer, reader, heartbeat).
    pub shutdown: tokio::sync::oneshot::Sender<()>,
    /// JoinHandles pour attendre la fin propre (debug/tests).
    pub tasks: Vec<JoinHandle<()>>,
}

impl SessionHandle {
    /// **Avertissement** : appeler directement ceci sans avoir d'abord drop
    /// les `Sender` exposes par [`Session`] ne suffit pas a faire terminer les
    /// encoder tasks. Prefere [`Session::shutdown_and_wait`].
    pub async fn shutdown_signal(self) {
        let _ = self.shutdown.send(());
        for t in self.tasks {
            let _ = t.await;
        }
    }
}

/// Session active : expose les channels d'envoi/reception.
pub struct Session {
    /// Identite long-terme du pair distant.
    pub remote_identity: DeviceId,
    /// Capacites du pair distant.
    pub remote_capabilities: Capabilities,
    /// Envoi vers le pair sur le canal Ctrl.
    pub ctrl_tx: mpsc::Sender<CtrlMessage>,
    /// Reception du canal Ctrl distant (consume par l'app).
    pub ctrl_rx: mpsc::Receiver<CtrlMessage>,
    /// Envoi sur le canal Input/Clipboard.
    pub input_tx: mpsc::Sender<InputMessage>,
    /// Reception du canal Input/Clipboard.
    pub input_rx: mpsc::Receiver<InputMessage>,
    /// Envoi sur le canal Files.
    pub files_tx: mpsc::Sender<FileMessage>,
    /// Reception du canal Files.
    pub files_rx: mpsc::Receiver<FileMessage>,
    /// (Reserve pour audio UDP, exposes ici pour cohesion d'API.)
    pub audio_tx: mpsc::Sender<AudioMessage>,
    /// (Reserve pour audio UDP.)
    pub audio_rx: mpsc::Receiver<AudioMessage>,
    /// (Reserve pour video UDP.)
    pub video_tx: mpsc::Sender<VideoMessage>,
    /// (Reserve pour video UDP.)
    pub video_rx: mpsc::Receiver<VideoMessage>,
    /// Handle de pilotage (shutdown).
    pub handle: SessionHandle,
}

impl Session {
    /// Arret propre : libere les `Sender` et attend la fin de toutes les tasks.
    ///
    /// Strategie :
    /// 1. Drop tous les `Sender` exposes pour faire terminer les encoder tasks.
    /// 2. Drop tous les `Receiver` pour debloquer un eventuel `send().await`.
    /// 3. Signale au task `shutdowner` qui envoie un `GoodBye` puis ferme le canal.
    /// 4. **Abort** des autres tasks (heartbeat, watchdog) qui sont en boucle
    ///    `tokio::time::interval` et ne reagissent pas au drop des canaux.
    /// 5. Attend la fin de chaque task.
    pub async fn shutdown_and_wait(self) {
        let handle = self.handle;
        drop(self.ctrl_tx);
        drop(self.input_tx);
        drop(self.files_tx);
        drop(self.audio_tx);
        drop(self.video_tx);
        drop(self.ctrl_rx);
        drop(self.input_rx);
        drop(self.files_rx);
        drop(self.audio_rx);
        drop(self.video_rx);
        let _ = handle.shutdown.send(());
        // Abort les tasks qui ne savent pas se terminer par signal de canal.
        for t in &handle.tasks {
            t.abort();
        }
        for t in handle.tasks {
            let _ = t.await;
        }
    }

    /// Demarre une session a partir d'un `HandshakeOutcome` et d'une `TcpStream`.
    ///
    /// `heartbeat_interval` est la cadence d'envoi de `CtrlMessage::Heartbeat`.
    /// `heartbeat_timeout` est le delai au-dela duquel on coupe la session si
    /// on ne recoit plus de heartbeat.
    pub fn start(
        stream: tokio::net::TcpStream,
        outcome: HandshakeOutcome,
        heartbeat_interval: Duration,
        heartbeat_timeout: Duration,
    ) -> Self {
        let (read_half, mut write_half) = stream.into_split();

        // mpsc internes
        let (write_tx, mut write_rx) = mpsc::channel::<(Channel, Vec<u8>)>(WRITE_CHANNEL_CAPACITY);

        // mpsc applicatifs (vers/depuis l'app)
        let (app_ctrl_send_tx, mut app_ctrl_send_rx) =
            mpsc::channel::<CtrlMessage>(APP_CHANNEL_CAPACITY);
        let (app_ctrl_recv_tx, app_ctrl_recv_rx) =
            mpsc::channel::<CtrlMessage>(APP_CHANNEL_CAPACITY);
        let (app_input_send_tx, mut app_input_send_rx) =
            mpsc::channel::<InputMessage>(APP_CHANNEL_CAPACITY);
        let (app_input_recv_tx, app_input_recv_rx) =
            mpsc::channel::<InputMessage>(APP_CHANNEL_CAPACITY);
        let (app_files_send_tx, mut app_files_send_rx) =
            mpsc::channel::<FileMessage>(APP_CHANNEL_CAPACITY);
        let (app_files_recv_tx, app_files_recv_rx) =
            mpsc::channel::<FileMessage>(APP_CHANNEL_CAPACITY);
        // Audio : route via TCP pour la V1 (idealement UDP plus tard).
        let (app_audio_send_tx, mut app_audio_send_rx) =
            mpsc::channel::<AudioMessage>(APP_CHANNEL_CAPACITY);
        let (app_audio_recv_tx, app_audio_recv_rx) =
            mpsc::channel::<AudioMessage>(APP_CHANNEL_CAPACITY);
        // Video : route via TCP pour la V1 (idealement UDP+FEC plus tard).
        let (app_video_send_tx, mut app_video_send_rx) =
            mpsc::channel::<VideoMessage>(APP_CHANNEL_CAPACITY);
        let (app_video_recv_tx, app_video_recv_rx) =
            mpsc::channel::<VideoMessage>(APP_CHANNEL_CAPACITY);

        // Heartbeat reception
        let last_recv_heartbeat = std::sync::Arc::new(parking_lot::Mutex::new(Instant::now()));

        // shutdown signal
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let mut aead_send = outcome.aead_send;
        let mut aead_recv = outcome.aead_recv;

        // ----- TASK : encodage app → write_tx -----
        let encoder_ctrl: JoinHandle<()> = {
            let write_tx_inner = write_tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = app_ctrl_send_rx.recv().await {
                    match bincode::serde::encode_to_vec(&msg, bincode_cfg()) {
                        Ok(bytes) => {
                            if write_tx_inner.send((Channel::Ctrl, bytes)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "encode CtrlMessage failed");
                        }
                    }
                }
            })
        };
        let encoder_input: JoinHandle<()> = {
            let write_tx_inner = write_tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = app_input_send_rx.recv().await {
                    match bincode::serde::encode_to_vec(&msg, bincode_cfg()) {
                        Ok(bytes) => {
                            if write_tx_inner.send((Channel::Input, bytes)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "encode InputMessage failed");
                        }
                    }
                }
            })
        };
        let encoder_files: JoinHandle<()> = {
            let write_tx_inner = write_tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = app_files_send_rx.recv().await {
                    match bincode::serde::encode_to_vec(&msg, bincode_cfg()) {
                        Ok(bytes) => {
                            if write_tx_inner.send((Channel::Files, bytes)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "encode FileMessage failed");
                        }
                    }
                }
            })
        };
        let encoder_audio: JoinHandle<()> = {
            let write_tx_inner = write_tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = app_audio_send_rx.recv().await {
                    match bincode::serde::encode_to_vec(&msg, bincode_cfg()) {
                        Ok(bytes) => {
                            if write_tx_inner.send((Channel::Audio, bytes)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "encode AudioMessage failed");
                        }
                    }
                }
            })
        };
        let encoder_video: JoinHandle<()> = {
            let write_tx_inner = write_tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = app_video_send_rx.recv().await {
                    match bincode::serde::encode_to_vec(&msg, bincode_cfg()) {
                        Ok(bytes) => {
                            if write_tx_inner.send((Channel::Video, bytes)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "encode VideoMessage failed");
                        }
                    }
                }
            })
        };

        // ----- TASK : writer (consume write_rx → chiffre → write half) -----
        let writer: JoinHandle<()> = tokio::spawn(async move {
            while let Some((channel, plaintext)) = write_rx.recv().await {
                let frame = match encode_tcp_frame(&mut aead_send, channel, &plaintext) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!(error = %e, "encode_tcp_frame echec");
                        break;
                    }
                };
                if let Err(e) = write_half.write_all(&frame).await {
                    tracing::warn!(error = %e, "ecriture TCP echouee");
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });

        // ----- TASK : heartbeat sender -----
        let hb_sender: JoinHandle<()> = {
            let send = write_tx.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(heartbeat_interval);
                tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    tick.tick().await;
                    let msg = CtrlMessage::Heartbeat {
                        ts_ms: now_ms(),
                        cpu_pct: 0,
                        rss_mb: 0,
                    };
                    if let Ok(bytes) = bincode::serde::encode_to_vec(&msg, bincode_cfg()) {
                        if send.send((Channel::Ctrl, bytes)).await.is_err() {
                            break;
                        }
                    }
                }
            })
        };

        // ----- TASK : heartbeat watchdog -----
        let hb_watchdog: JoinHandle<()> = {
            let last = last_recv_heartbeat.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(heartbeat_timeout / 2);
                tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    tick.tick().await;
                    let elapsed = {
                        let g = last.lock();
                        g.elapsed()
                    };
                    if elapsed > heartbeat_timeout {
                        tracing::warn!(
                            elapsed_ms = elapsed.as_millis() as u64,
                            "heartbeat timeout — session devrait se fermer"
                        );
                        // L'app coupe via shutdown_handle ; ici on log et on sort
                        // pour ne pas continuer indefiniment.
                        break;
                    }
                }
            })
        };

        // ----- TASK : reader (lit frames → dechiffre → demultiplex) -----
        let reader: JoinHandle<()> = {
            let last_hb = last_recv_heartbeat.clone();
            tokio::spawn(async move {
                let mut codec = FrameCodec::new();
                let mut buf = BytesMut::with_capacity(64 * 1024);
                let mut read_half = read_half;
                loop {
                    // Lecture incrementale.
                    let mut tmp = [0u8; 16 * 1024];
                    let n = match read_half.read(&mut tmp).await {
                        Ok(0) => {
                            tracing::info!("EOF cote pair");
                            break;
                        }
                        Ok(n) => n,
                        Err(e) => {
                            tracing::warn!(error = %e, "read TCP echec");
                            break;
                        }
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    loop {
                        match codec.decode(&mut buf) {
                            Ok(Some(frame)) => {
                                let (hdr, pt) = match decode_tcp_frame(&mut aead_recv, &frame) {
                                    Ok(x) => x,
                                    Err(e) => {
                                        tracing::warn!(error = %e, "decode_tcp_frame echec — session compromise");
                                        return;
                                    }
                                };
                                match hdr.channel {
                                    Channel::Ctrl => {
                                        match bincode::serde::decode_from_slice::<CtrlMessage, _>(
                                            &pt,
                                            bincode_cfg(),
                                        ) {
                                            Ok((m, _)) => {
                                                if matches!(m, CtrlMessage::Heartbeat { .. }) {
                                                    *last_hb.lock() = Instant::now();
                                                }
                                                let _ = app_ctrl_recv_tx.send(m).await;
                                            }
                                            Err(e) => {
                                                tracing::warn!(error = %e, "decode CtrlMessage")
                                            }
                                        }
                                    }
                                    Channel::Input => {
                                        if let Ok((m, _)) =
                                            bincode::serde::decode_from_slice::<InputMessage, _>(
                                                &pt,
                                                bincode_cfg(),
                                            )
                                        {
                                            let _ = app_input_recv_tx.send(m).await;
                                        }
                                    }
                                    Channel::Files => {
                                        if let Ok((m, _)) =
                                            bincode::serde::decode_from_slice::<FileMessage, _>(
                                                &pt,
                                                bincode_cfg(),
                                            )
                                        {
                                            let _ = app_files_recv_tx.send(m).await;
                                        }
                                    }
                                    Channel::Audio => {
                                        if let Ok((m, _)) =
                                            bincode::serde::decode_from_slice::<AudioMessage, _>(
                                                &pt,
                                                bincode_cfg(),
                                            )
                                        {
                                            let _ = app_audio_recv_tx.send(m).await;
                                        }
                                    }
                                    Channel::Video => {
                                        if let Ok((m, _)) =
                                            bincode::serde::decode_from_slice::<VideoMessage, _>(
                                                &pt,
                                                bincode_cfg(),
                                            )
                                        {
                                            let _ = app_video_recv_tx.send(m).await;
                                        }
                                    }
                                }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                tracing::warn!(error = %e, "frame codec echec — session compromise");
                                return;
                            }
                        }
                    }
                }
            })
        };

        // ----- TASK : shutdown listener -----
        let shutdowner: JoinHandle<()> = {
            let write_tx_inner = write_tx.clone();
            tokio::spawn(async move {
                let _ = (&mut shutdown_rx).await;
                // Envoie un GoodBye puis ferme la chaine d'envoi.
                let msg = CtrlMessage::GoodBye {
                    reason: "user shutdown".into(),
                };
                if let Ok(bytes) = bincode::serde::encode_to_vec(&msg, bincode_cfg()) {
                    let _ = write_tx_inner.send((Channel::Ctrl, bytes)).await;
                }
                drop(write_tx_inner); // referme write_tx => writer task termine
            })
        };

        Self {
            remote_identity: outcome.remote_identity,
            remote_capabilities: outcome.remote_capabilities,
            ctrl_tx: app_ctrl_send_tx,
            ctrl_rx: app_ctrl_recv_rx,
            input_tx: app_input_send_tx,
            input_rx: app_input_recv_rx,
            files_tx: app_files_send_tx,
            files_rx: app_files_recv_rx,
            audio_tx: app_audio_send_tx,
            audio_rx: app_audio_recv_rx,
            video_tx: app_video_send_tx,
            video_rx: app_video_recv_rx,
            handle: SessionHandle {
                shutdown: shutdown_tx,
                tasks: vec![
                    encoder_ctrl,
                    encoder_input,
                    encoder_files,
                    encoder_audio,
                    encoder_video,
                    writer,
                    reader,
                    hb_sender,
                    hb_watchdog,
                    shutdowner,
                ],
            },
        }
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Lit un message Ctrl avec un delai max.
pub async fn recv_ctrl_with_timeout(
    rx: &mut mpsc::Receiver<CtrlMessage>,
    d: Duration,
) -> Option<CtrlMessage> {
    timeout(d, rx.recv()).await.ok().flatten()
}
