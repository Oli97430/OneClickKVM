//! Etat global partage entre les commandes Tauri.
//!
//! Cette version cable les vrais services et integre :
//!
//! - Identite persistante ([`okvm_config::load_or_create_identity`]).
//! - Listener TCP ([`okvm_net::Listener`]) + dispatcher de sessions.
//! - Discovery LAN ([`okvm_discovery::DiscoveryService`]).
//! - Inject Win32 ([`okvm_input_inject::Win32Inject`]) toujours pret a injecter
//!   les `InputMessage` recus des sessions.
//! - Capture Win32 ([`okvm_input_capture::Win32Capture`]) demarree on-demand
//!   quand l'utilisateur passe en mode "master".
//! - Pour chaque session : task qui consume `input_rx` → inject, task qui
//!   consume `ctrl_rx` → log/heartbeat.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use okvm_audio::{AudioCapture, AudioPlayback, CpalCapture, CpalPlayback};
use okvm_core::{Capabilities, DeviceId, Fingerprint, IdentityKeypair};
use okvm_discovery::{caps_bits, DiscoveredPeer, DiscoveryService, SelfAnnounce};
use okvm_files::FileReceiver;
use okvm_input_capture::{InputCapture, Win32Capture};
use okvm_input_inject::{InputInject, Win32Inject};
use okvm_ipc::PeerView;
use okvm_net::{Connector, ConnectorConfig, Listener, ListenerConfig, Session};
use okvm_protocol::{
    consts::TCP_PORT_DEFAULT, messages::RejectReason, AudioMessage, FileMessage, InputMessage,
    VideoMessage,
};
use okvm_switch::{Grid, GridPeer, Rect, SwitchDecision, SwitchEngine};
use okvm_video::{H264Decoder, VideoCapture, WindowsCaptureSource};

/// Reference cote control vers une session active : senders uniquement.
pub struct SessionRef {
    /// Identite long-terme distante. Stockée pour traçabilité / debug
    /// (les commandes l'obtiennent via la clé de `state.sessions`).
    #[allow(dead_code)]
    pub remote_identity: DeviceId,
    /// Capabilities annoncees par le pair. Stockees pour traces / introspection
    /// future (UI badges « ce pair sait faire vidéo / audio / fichiers »).
    #[allow(dead_code)]
    pub remote_caps: Capabilities,
    /// Sender pour input outbound.
    pub input_tx: mpsc::Sender<InputMessage>,
    /// Sender pour ctrl outbound. Conservé pour usage futur (envoi de Ping
    /// manuel, RequestStatus, etc.).
    #[allow(dead_code)]
    pub ctrl_tx: mpsc::Sender<okvm_protocol::CtrlMessage>,
    /// Sender pour files outbound.
    pub files_tx: mpsc::Sender<FileMessage>,
    /// Sender pour audio outbound (frames PCM).
    pub audio_tx: mpsc::Sender<AudioMessage>,
    /// Sender pour video outbound (frames MJPEG).
    pub video_tx: mpsc::Sender<VideoMessage>,
    /// Task `input_rx → inject`.
    pub inbound_input_task: JoinHandle<()>,
    /// Task `ctrl_rx → handler`.
    pub inbound_ctrl_task: JoinHandle<()>,
    /// Task `files_rx → FileReceiver`.
    pub inbound_files_task: JoinHandle<()>,
    /// Task `audio_rx → CpalPlayback`.
    pub inbound_audio_task: JoinHandle<()>,
    /// Task `video_rx → emit Tauri event`.
    pub inbound_video_task: JoinHandle<()>,
    /// Channel de shutdown de la session.
    pub shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl SessionRef {
    /// `on_close` est appele **une seule fois** quand la session se ferme
    /// (par n'importe quel cote : EOF, heartbeat timeout, GoodBye, shutdown).
    fn from_session(
        session: Session,
        inject: Arc<dyn InputInject>,
        file_receiver: Arc<FileReceiver>,
        audio_playback: Arc<dyn AudioPlayback>,
        on_video_frame: Arc<dyn Fn(VideoMessage) + Send + Sync + 'static>,
        on_close: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        let remote_identity = session.remote_identity;
        let remote_caps = session.remote_capabilities;
        let input_tx = session.input_tx.clone();
        let ctrl_tx = session.ctrl_tx.clone();
        let files_tx = session.files_tx.clone();
        let audio_tx = session.audio_tx.clone();
        let video_tx = session.video_tx.clone();

        let mut input_rx = session.input_rx;
        let mut ctrl_rx = session.ctrl_rx;
        let mut files_rx = session.files_rx;
        let mut audio_rx = session.audio_rx;
        let mut video_rx = session.video_rx;

        let inject_arc = inject.clone();
        let on_close_input = on_close.clone();
        let inbound_input_task = tokio::spawn(async move {
            while let Some(msg) = input_rx.recv().await {
                if let Err(e) = inject_arc.inject(msg).await {
                    tracing::debug!(error = %e, "inject echec (msg ignore)");
                }
            }
            tracing::debug!("inbound input task: terminee");
            on_close_input();
        });

        let on_close_ctrl = on_close.clone();
        let ctrl_tx_for_pong = ctrl_tx.clone();
        let inbound_ctrl_task = tokio::spawn(async move {
            while let Some(msg) = ctrl_rx.recv().await {
                use okvm_protocol::CtrlMessage;
                match msg {
                    CtrlMessage::Heartbeat { .. } => {}
                    CtrlMessage::Ping { ts_ms } => {
                        let _ = ctrl_tx_for_pong
                            .send(CtrlMessage::Pong {
                                ts_ms: ts_now(),
                                peer_ts_ms: ts_ms,
                            })
                            .await;
                    }
                    CtrlMessage::GoodBye { reason } => {
                        tracing::info!(reason, "GoodBye recu");
                        break;
                    }
                    other => tracing::debug!(ctrl = ?other, "ctrl recu"),
                }
            }
            tracing::debug!("inbound ctrl task: terminee");
            on_close_ctrl();
        });

        let on_close_files = on_close.clone();
        let inbound_files_task = tokio::spawn(async move {
            while let Some(msg) = files_rx.recv().await {
                if let Err(e) = file_receiver.on_message(msg).await {
                    tracing::warn!(error = %e, "FileReceiver echec sur message");
                }
            }
            tracing::debug!("inbound files task: terminee");
            on_close_files();
        });

        let on_close_audio = on_close.clone();
        let inbound_audio_task = tokio::spawn(async move {
            while let Some(msg) = audio_rx.recv().await {
                if let Err(e) = audio_playback.push(msg).await {
                    tracing::debug!(error = %e, "AudioPlayback push echec");
                }
            }
            tracing::debug!("inbound audio task: terminee");
            on_close_audio();
        });

        let on_close_video = on_close.clone();
        let inbound_video_task = tokio::spawn(async move {
            while let Some(msg) = video_rx.recv().await {
                on_video_frame(msg);
            }
            tracing::debug!("inbound video task: terminee");
            on_close_video();
        });

        Self {
            remote_identity,
            remote_caps,
            input_tx,
            ctrl_tx,
            files_tx,
            audio_tx,
            video_tx,
            inbound_input_task,
            inbound_ctrl_task,
            inbound_files_task,
            inbound_audio_task,
            inbound_video_task,
            shutdown: Some(session.handle.shutdown),
        }
    }

    pub fn shutdown(self) {
        self.inbound_input_task.abort();
        self.inbound_ctrl_task.abort();
        self.inbound_files_task.abort();
        self.inbound_audio_task.abort();
        self.inbound_video_task.abort();
        if let Some(s) = self.shutdown {
            let _ = s.send(());
        }
    }
}

fn ts_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_ms_u64() -> u64 {
    ts_now()
}

/// Etat applicatif partage entre toutes les commandes Tauri.
pub struct AppState {
    /// Identite long-terme de ce PC.
    pub identity: IdentityKeypair,
    /// Capacites annoncees par ce PC.
    pub capabilities: Capabilities,
    /// Port TCP sur lequel on ecoute.
    pub tcp_port: u16,
    /// Hostname.
    pub hostname: String,
    /// BBox du bureau virtuel local dans le repere de la grille.
    pub local_bbox: Rect,
    /// Pairs connus (decouverts ou pas) indexes par identite.
    pub peers: Arc<Mutex<HashMap<DeviceId, PeerView>>>,
    /// Sessions actives indexees par identite distante.
    pub sessions: Arc<Mutex<HashMap<DeviceId, SessionRef>>>,
    /// Mapping DeviceId → UUID de pair dans la grille (pour le routage).
    pub grid_id_by_device: Arc<Mutex<HashMap<DeviceId, uuid::Uuid>>>,
    /// Moteur de bascule partage avec le master loop.
    pub switch: Arc<Mutex<SwitchEngine>>,
    /// Injecteur Win32 (toujours disponible).
    pub inject: Arc<dyn InputInject>,
    /// Receveur de fichiers (toujours disponible, dest_root fixe).
    pub file_receiver: Arc<FileReceiver>,
    /// Repertoire de reception des fichiers, expose a l'UI.
    pub inbox_dir: std::path::PathBuf,
    /// Playback audio (toujours disponible, partage entre toutes les sessions).
    pub audio_playback: Arc<dyn AudioPlayback>,
    /// Handle de la capture audio en cours (Some si on partage notre son).
    pub audio_capture_handle: Arc<Mutex<Option<okvm_audio::AudioHandle>>>,
    /// Handle de la capture vidéo en cours (Some si on partage notre écran).
    pub video_capture_handle: Arc<Mutex<Option<okvm_video::VideoHandle>>>,
    /// Etat capture (Some si en mode master).
    pub capture_handle: Arc<Mutex<Option<okvm_input_capture::CaptureHandle>>>,
    /// Services en cours (listener + discovery), `None` si non demarres.
    pub running: Arc<Mutex<Option<RunningServices>>>,
    /// Mode d'appairage : `Some` si on accepte des nouveaux pairs avec PIN.
    /// `None` = seuls les pairs déjà appairés peuvent se connecter.
    pub pairing_mode: Arc<Mutex<Option<PairingMode>>>,
}

/// Nombre maximum de tentatives de PIN ratées avant de désactiver le mode
/// d'appairage. À 6 chiffres + 5 tentatives, la probabilité de devinette
/// aléatoire est de 5 / 1_000_000 ≈ 0.0005 %.
pub const MAX_PIN_ATTEMPTS: u32 = 5;

/// Mode d'appairage actif : génération d'un PIN à durée limitée pour permettre
/// à un nouveau pair de se connecter. Tant que ce mode est actif, le serveur
/// accepte un ClientHello d'identité inconnue si son `pairing_pin_hash` matche
/// `SHA-256(pin || ch.nonce)` (comparaison constant-time).
#[derive(Debug, Clone)]
pub struct PairingMode {
    /// PIN affiché à l'utilisateur (6 chiffres décimaux). Wrappé dans
    /// `Zeroizing` pour effacer la mémoire à la suppression (défense en
    /// profondeur, sans garantie complète vu les copies inhérentes au
    /// passage par const fn / `format!`).
    pub pin: zeroize::Zeroizing<String>,
    /// Timestamp absolu d'expiration en ms unix.
    pub expires_at_ms: u64,
    /// Compteur de tentatives PIN ratées depuis l'activation du mode.
    /// Au-delà de [`MAX_PIN_ATTEMPTS`] le mode est désactivé automatiquement
    /// (anti-brute-force).
    pub failed_attempts: u32,
}

impl PairingMode {
    /// `true` si le PIN est encore valide (non expiré ET sous le seuil
    /// d'attaque).
    pub fn is_alive(&self) -> bool {
        ts_now() < self.expires_at_ms && self.failed_attempts < MAX_PIN_ATTEMPTS
    }
}

/// Handles des services en cours d'execution.
pub struct RunningServices {
    pub listener_task: JoinHandle<()>,
    pub session_dispatcher: JoinHandle<()>,
    pub discovery: Option<DiscoveryService>,
    pub discovery_consumer: JoinHandle<()>,
}

impl AppState {
    /// Construit un nouvel etat en chargeant ou generant l'identite.
    pub fn initialize() -> okvm_core::Result<Self> {
        let identity = okvm_config::load_or_create_identity()?;
        let mut caps = Capabilities::default_windows();
        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "PC".to_string());
        caps.os.hostname = hostname.clone();

        // Enumere les ecrans locaux.
        let screens = okvm_switch::screens::enumerate_local_screens();
        caps.screens = screens.clone();
        let local_bbox = bbox_of_screens(&screens);

        // Charge les pairs persistes (offline=true par defaut, paired=true).
        let stored_peers = okvm_config::load_peers().unwrap_or_default();
        let mut peers_map: HashMap<DeviceId, PeerView> = HashMap::with_capacity(stored_peers.len());
        for p in stored_peers {
            peers_map.insert(
                p.device_id,
                PeerView {
                    device_id: p.device_id,
                    fingerprint: p.fingerprint,
                    name: p.display_name,
                    paired: true,
                    online: false,
                    discovered: false,
                    last_addr: p.last_tcp_port.and_then(|port| {
                        // On reconstitue une adresse partielle (port connu, IP TBD).
                        // Format affichage purement informatif.
                        Some(format!("(port {port})"))
                    }),
                },
            );
        }

        tracing::info!(
            fingerprint = %identity.public.fingerprint(),
            hostname = %hostname,
            screens = caps.screens.len(),
            bbox = ?local_bbox,
            stored_peers = peers_map.len(),
            "AppState initialized"
        );

        let mut grid = Grid::default();
        grid.peers.insert(
            uuid::Uuid::nil(),
            GridPeer {
                id: uuid::Uuid::nil(),
                name: hostname.clone(),
                screens: screens.clone(),
                origin_in_grid: (local_bbox.x, local_bbox.y),
                hotkey_index: None,
            },
        );
        let switch = SwitchEngine::new(grid);

        // Inbox des fichiers recus.
        let inbox_dir = directories::UserDirs::new()
            .and_then(|u| {
                u.document_dir()
                    .map(|d| d.join("OneClickKVM").join("Inbox"))
            })
            .unwrap_or_else(|| std::path::PathBuf::from("OneClickKVM/Inbox"));
        let _ = std::fs::create_dir_all(&inbox_dir);
        let file_receiver = Arc::new(FileReceiver::new(inbox_dir.clone()));
        // Le callback de progression sera attache dans `start_services` quand
        // on aura l'AppHandle pour emettre des events.

        Ok(Self {
            identity,
            capabilities: caps,
            tcp_port: TCP_PORT_DEFAULT,
            hostname,
            local_bbox,
            peers: Arc::new(Mutex::new(peers_map)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            grid_id_by_device: Arc::new(Mutex::new(HashMap::new())),
            switch: Arc::new(Mutex::new(switch)),
            inject: Arc::new(Win32Inject),
            file_receiver,
            inbox_dir,
            audio_playback: Arc::new(CpalPlayback::new()),
            audio_capture_handle: Arc::new(Mutex::new(None)),
            video_capture_handle: Arc::new(Mutex::new(None)),
            capture_handle: Arc::new(Mutex::new(None)),
            running: Arc::new(Mutex::new(None)),
            pairing_mode: Arc::new(Mutex::new(None)),
        })
    }

    /// Active le mode d'appairage avec un PIN aléatoire à 6 chiffres, valide
    /// pendant `duration`. Renvoie le PIN à afficher à l'utilisateur.
    pub fn start_pairing_mode(&self, duration: std::time::Duration) -> PairingMode {
        use rand_core::{OsRng, RngCore};
        // 6 chiffres décimaux = 1 chance sur 10^6 par essai aveugle ; combiné
        // au plafond [`MAX_PIN_ATTEMPTS`] et à la fenêtre 60s, la probabilité
        // d'une devinette aboutie reste sous 10^-5.
        let n = OsRng.next_u32() % 1_000_000;
        let pin = zeroize::Zeroizing::new(format!("{n:06}"));
        let pm = PairingMode {
            pin,
            expires_at_ms: ts_now() + duration.as_millis() as u64,
            failed_attempts: 0,
        };
        *self.pairing_mode.lock() = Some(pm.clone());
        tracing::info!(
            expires_in_ms = duration.as_millis() as u64,
            "pairing mode active"
        );
        pm
    }

    /// Désactive le mode d'appairage. Les nouveaux pairs ne pourront plus se
    /// connecter tant qu'il n'est pas réactivé.
    pub fn stop_pairing_mode(&self) {
        *self.pairing_mode.lock() = None;
        tracing::info!("pairing mode desactive");
    }

    /// Snapshot du mode d'appairage actuel (None si inactif ou expiré).
    pub fn pairing_mode_snapshot(&self) -> Option<PairingMode> {
        let mut guard = self.pairing_mode.lock();
        match guard.as_ref() {
            Some(pm) if pm.is_alive() => Some(pm.clone()),
            Some(_) => {
                // expiré : on nettoie.
                *guard = None;
                None
            }
            None => None,
        }
    }

    /// Demarre la capture video de l'ecran et l'envoie a tous les pairs connectes.
    pub async fn start_video_share(&self) -> okvm_core::Result<()> {
        if self.video_capture_handle.lock().is_some() {
            return Ok(());
        }
        let (tx, mut rx) = mpsc::channel::<VideoMessage>(64);
        // Lit la config : backend H.264 + index de moniteur. Si l'index ne
        // correspond plus à un écran physique présent, retombe sur 0
        // (silencieux pour éviter de casser l'UI après un changement de
        // config matériel).
        let cfg = okvm_config::load_app_config().unwrap_or_default();
        let backend = match cfg.h264_backend {
            okvm_config::H264BackendChoice::Openh264 => okvm_video::H264Backend::Openh264,
            okvm_config::H264BackendChoice::MediaFoundation => {
                okvm_video::H264Backend::MediaFoundation
            }
        };
        let screen_count = self.capabilities.screens.len() as u32;
        let screen_idx = if cfg.video_screen_idx < screen_count.max(1) {
            cfg.video_screen_idx
        } else {
            tracing::warn!(
                requested = cfg.video_screen_idx,
                available = screen_count,
                "moniteur demande introuvable, fallback sur 0"
            );
            0
        };
        let capture = WindowsCaptureSource {
            h264_backend: backend,
            ..WindowsCaptureSource::default()
        };
        tracing::info!(?backend, screen_idx, "video share start");
        let handle = capture.start(screen_idx, tx).await?;
        *self.video_capture_handle.lock() = Some(handle);

        // Fan-out : chaque frame video vers tous les pairs.
        let sessions_arc = self.sessions.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let senders: Vec<mpsc::Sender<VideoMessage>> = sessions_arc
                    .lock()
                    .values()
                    .map(|s| s.video_tx.clone())
                    .collect();
                for s in &senders {
                    let _ = s.try_send(msg.clone());
                }
            }
        });
        Ok(())
    }

    /// Arrete la capture video.
    pub fn stop_video_share(&self) {
        if let Some(handle) = self.video_capture_handle.lock().take() {
            let _ = handle.stop.send(());
            handle.bridge.abort();
        }
    }

    /// `true` si on partage actuellement notre ecran.
    pub fn is_video_sharing(&self) -> bool {
        self.video_capture_handle.lock().is_some()
    }

    /// Demarre la capture audio loopback et l'envoie a tous les pairs connectes.
    pub async fn start_audio_share(&self) -> okvm_core::Result<()> {
        if self.audio_capture_handle.lock().is_some() {
            return Ok(());
        }
        let (tx, mut rx) = mpsc::channel::<AudioMessage>(256);
        let capture = CpalCapture;
        let handle = capture.start(tx).await?;
        *self.audio_capture_handle.lock() = Some(handle);

        // Fan-out : chaque frame audio est envoyee a tous les pairs.
        let sessions_arc = self.sessions.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                // Snapshot des senders sous le lock (cheap), puis envoie sans lock.
                let senders: Vec<mpsc::Sender<AudioMessage>> = sessions_arc
                    .lock()
                    .values()
                    .map(|s| s.audio_tx.clone())
                    .collect();
                for s in &senders {
                    // try_send : on prefere drop une frame que bloquer la capture.
                    let _ = s.try_send(msg.clone());
                }
            }
        });
        Ok(())
    }

    /// Arrete la capture audio.
    pub fn stop_audio_share(&self) {
        if let Some(handle) = self.audio_capture_handle.lock().take() {
            let _ = handle.stop.send(());
            handle.bridge.abort();
        }
    }

    /// `true` si on partage actuellement notre audio.
    pub fn is_audio_sharing(&self) -> bool {
        self.audio_capture_handle.lock().is_some()
    }

    /// Sauvegarde la liste des pairs appaires sur disque.
    pub fn save_peers(&self) {
        let to_save: Vec<okvm_config::PeerProfile> = self
            .peers
            .lock()
            .values()
            .filter(|p| p.paired)
            .map(|p| okvm_config::PeerProfile {
                device_id: p.device_id,
                fingerprint: p.fingerprint,
                display_name: p.name.clone(),
                permissions: okvm_core::Permission::default(),
                wol_mac: None,
                last_tcp_port: Some(TCP_PORT_DEFAULT),
                last_seen_ms: Some(now_ms_u64()),
            })
            .collect();
        if let Err(e) = okvm_config::save_peers(&to_save) {
            tracing::warn!(error = %e, "save_peers echec");
        }
    }

    /// Retire un pair de la grille.
    pub fn remove_peer_from_grid(&self, device_id: &DeviceId) {
        let mut mapping = self.grid_id_by_device.lock();
        if let Some(uuid) = mapping.remove(device_id) {
            self.switch.lock().grid.peers.remove(&uuid);
            tracing::info!(peer = %uuid, "pair retire de la grille");
        }
    }

    pub fn self_fingerprint(&self) -> Fingerprint {
        self.identity.public.fingerprint()
    }

    pub fn is_listening(&self) -> bool {
        self.running.lock().is_some()
    }

    /// `true` si la capture clavier/souris (mode master) est active.
    /// Disponible pour les tests d'intégration et l'introspection future.
    #[allow(dead_code)]
    pub fn is_master(&self) -> bool {
        self.capture_handle.lock().is_some()
    }

    /// Demarre listener TCP + decouverte.
    pub fn start_services(&self, app: tauri::AppHandle) -> okvm_core::Result<()> {
        let mut guard = self.running.lock();
        if guard.is_some() {
            return Ok(());
        }

        // === Branche le callback de progression sur le FileReceiver ===
        // (throttle a 4Hz pour ne pas spammer le frontend.)
        let app_for_progress = app.clone();
        let last_emit = Arc::new(Mutex::new(
            std::time::Instant::now() - std::time::Duration::from_secs(60),
        ));
        self.file_receiver.set_on_progress(
            move |transfer_id, bytes_done, bytes_total, current_file| {
                let now = std::time::Instant::now();
                let should_emit = {
                    let mut g = last_emit.lock();
                    let elapsed = now.duration_since(*g);
                    let final_frame = bytes_done >= bytes_total && bytes_total > 0;
                    if elapsed >= std::time::Duration::from_millis(250) || final_frame {
                        *g = now;
                        true
                    } else {
                        false
                    }
                };
                if should_emit {
                    let state = if bytes_done >= bytes_total && bytes_total > 0 {
                        "done"
                    } else {
                        "running"
                    };
                    let view = okvm_ipc::TransferProgressView {
                        transfer_id: transfer_id.to_string(),
                        direction: "inbound".into(),
                        peer_name: String::new(),
                        current_file: current_file.to_string(),
                        bytes_done,
                        bytes_total,
                        state: state.into(),
                        error: None,
                    };
                    let _ = emit_event(
                        &app_for_progress,
                        okvm_ipc::BackendEvent::TransferProgress { progress: view },
                    );
                }
            },
        );

        // === Listener TCP ===
        let listener_cfg = ListenerConfig {
            bind: format!("[::]:{}", self.tcp_port)
                .parse::<SocketAddr>()
                .map_err(|e| okvm_core::Error::Net(format!("bind: {e}")))?,
            ..ListenerConfig::default()
        };
        // === ACL : PIN flow strict ===
        // 1. Pair déjà appairé (identité présente dans peers.json) → accept.
        // 2. Identité inconnue + pairing_mode actif + PIN valide → accept.
        // 3. Identité inconnue + pairing_mode actif + PIN absent/faux → PairingFailed.
        // 4. Identité inconnue + pairing_mode inactif → UnknownPeer.
        let peers_for_acl = self.peers.clone();
        let pairing_for_acl = self.pairing_mode.clone();
        let acl: okvm_net::listener::AclHook =
            Arc::new(move |ch| -> std::result::Result<(), RejectReason> {
                let device_id = okvm_core::DeviceId(ch.identity_pub);
                // 1. Déjà appairé ?
                if peers_for_acl
                    .lock()
                    .get(&device_id)
                    .is_some_and(|p| p.paired)
                {
                    return Ok(());
                }
                // 2-4. Vérifie le mode d'appairage. On garde le lock pendant TOUT
                // le check + l'incrément du compteur d'attempts, pour éviter une
                // course où un attaquant ferait N essais en parallèle et
                // contournerait le plafond.
                let mut g = pairing_for_acl.lock();
                let pm = match g.as_ref() {
                    Some(pm) if pm.is_alive() => pm.clone(),
                    Some(_) => {
                        *g = None;
                        tracing::info!(?device_id, "ACL: pairing mode expiré ou bloqué");
                        return Err(RejectReason::UnknownPeer);
                    }
                    None => {
                        tracing::info!(?device_id, "ACL: unknown peer rejected (pairing mode off)");
                        return Err(RejectReason::UnknownPeer);
                    }
                };
                // PIN obligatoire dans ce flow strict.
                let Some(client_hash) = ch.pairing_pin_hash else {
                    // Pas de PIN fourni : on compte ça comme une tentative ratée
                    // (sinon un attaquant pourrait sonder sans incrémenter).
                    if let Some(pm_mut) = g.as_mut() {
                        pm_mut.failed_attempts = pm_mut.failed_attempts.saturating_add(1);
                        if pm_mut.failed_attempts >= MAX_PIN_ATTEMPTS {
                            tracing::warn!(
                                ?device_id,
                                "ACL: pairing mode désactivé (max attempts)"
                            );
                            *g = None;
                        }
                    }
                    tracing::info!(?device_id, "ACL: pin manquant");
                    return Err(RejectReason::PairingFailed);
                };
                // Recompute expected = SHA-256(pin || ch.nonce).
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(pm.pin.as_bytes());
                h.update(ch.nonce);
                let expected = h.finalize();
                // Constant-time comparison.
                use subtle::ConstantTimeEq;
                if expected.ct_eq(&client_hash).into() {
                    tracing::info!(?device_id, "ACL: pin OK, new pair accepted");
                    Ok(())
                } else {
                    // Échec : incrémente, désactive si plafond atteint.
                    if let Some(pm_mut) = g.as_mut() {
                        pm_mut.failed_attempts = pm_mut.failed_attempts.saturating_add(1);
                        let n = pm_mut.failed_attempts;
                        tracing::warn!(?device_id, attempts = n, "ACL: pin invalide");
                        if n >= MAX_PIN_ATTEMPTS {
                            tracing::warn!(
                                ?device_id,
                                "ACL: pairing mode désactivé (max attempts)"
                            );
                            *g = None;
                        }
                    }
                    Err(RejectReason::PairingFailed)
                }
            });
        let listener = Listener::new(
            listener_cfg,
            self.identity.clone(),
            self.capabilities.clone(),
            acl,
        );

        let (sess_tx, mut sess_rx) = mpsc::channel::<Session>(8);
        let listener_task = tokio::spawn(async move {
            if let Err(e) = listener.run(sess_tx).await {
                tracing::warn!(error = %e, "listener: run sortie en erreur");
            }
        });

        // === Dispatcher : prend chaque Session entrante, spawn handlers, enregistre ===
        let ctx = DispatchCtx {
            inject: self.inject.clone(),
            file_receiver: self.file_receiver.clone(),
            audio_playback: self.audio_playback.clone(),
            sessions: self.sessions.clone(),
            peers: self.peers.clone(),
            switch: self.switch.clone(),
            grid_map: self.grid_id_by_device.clone(),
            local_bbox: self.local_bbox,
        };
        let app_h1 = app.clone();
        let session_dispatcher = tokio::spawn(async move {
            while let Some(session) = sess_rx.recv().await {
                register_session_with_grid(session, &ctx, &app_h1);
            }
        });

        // === Discovery (best effort) ===
        let announce = SelfAnnounce {
            device_id: self.identity.public,
            name: self.hostname.clone(),
            tcp_port: self.tcp_port,
            capabilities_short: caps_bits::KM | caps_bits::FILES | caps_bits::WOL | caps_bits::LOCK,
        };
        let (disc_tx, mut disc_rx) = mpsc::channel::<DiscoveredPeer>(32);
        let discovery = match DiscoveryService::start(announce, disc_tx, true, true) {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!(error = %e, "discovery indisponible (continue sans)");
                None
            }
        };

        let peers_arc = self.peers.clone();
        let identity_clone = self.identity.clone();
        let caps_clone = self.capabilities.clone();
        let dispatch_ctx_auto = DispatchCtx {
            inject: self.inject.clone(),
            file_receiver: self.file_receiver.clone(),
            audio_playback: self.audio_playback.clone(),
            sessions: self.sessions.clone(),
            peers: self.peers.clone(),
            switch: self.switch.clone(),
            grid_map: self.grid_id_by_device.clone(),
            local_bbox: self.local_bbox,
        };
        let sessions_arc = self.sessions.clone();
        let app_h2 = app.clone();
        let discovery_consumer = tokio::spawn(async move {
            while let Some(peer) = disc_rx.recv().await {
                let already_paired = peers_arc
                    .lock()
                    .get(&peer.device_id)
                    .map(|p| p.paired)
                    .unwrap_or(false);
                let already_connected = sessions_arc.lock().contains_key(&peer.device_id);

                let view = PeerView {
                    device_id: peer.device_id,
                    fingerprint: peer.device_id.fingerprint(),
                    name: peer.name.clone(),
                    paired: already_paired,
                    online: already_connected,
                    discovered: true,
                    last_addr: Some(peer.addr.to_string()),
                };
                peers_arc
                    .lock()
                    .entry(peer.device_id)
                    .and_modify(|existing| {
                        existing.name = view.name.clone();
                        existing.discovered = true;
                        existing.last_addr = view.last_addr.clone();
                    })
                    .or_insert_with(|| view.clone());
                let _ = emit_event(
                    &app_h2,
                    okvm_ipc::BackendEvent::PeerDiscovered { peer: view },
                );

                // === Auto-reconnect ===
                // Si un pair appairé réapparaît sur le LAN sans qu'on ait de
                // session active, on tente la reconnexion immédiate. Le user
                // voit un toast "Reconnexion à ..." puis succès/échec.
                if already_paired && !already_connected {
                    let id = identity_clone.clone();
                    let caps = caps_clone.clone();
                    let ctx = dispatch_ctx_auto.clone();
                    let addr = peer.addr;
                    let peer_name = peer.name.clone();
                    let app_h3 = app_h2.clone();
                    tokio::spawn(async move {
                        tracing::info!(addr = %addr, peer = %peer_name, "auto-reconnect tentative");
                        let _ = emit_event(
                            &app_h3,
                            okvm_ipc::BackendEvent::Notification {
                                level: okvm_ipc::NotificationLevel::Info,
                                title: "Reconnexion".into(),
                                body: format!("Tentative de reconnexion à {peer_name}…"),
                            },
                        );
                        let cfg = ConnectorConfig {
                            remote: addr,
                            pairing_pin: None,
                            ..ConnectorConfig::default()
                        };
                        let connector = Connector::new(cfg, id, caps);
                        match connector.connect().await {
                            Ok(session) => {
                                register_session_with_grid(session, &ctx, &app_h3);
                                tracing::info!(addr = %addr, "auto-reconnect reussi");
                                let _ = emit_event(
                                    &app_h3,
                                    okvm_ipc::BackendEvent::Notification {
                                        level: okvm_ipc::NotificationLevel::Success,
                                        title: "Reconnecté".into(),
                                        body: format!("Session restaurée avec {peer_name}."),
                                    },
                                );
                            }
                            Err(e) => {
                                tracing::debug!(addr = %addr, error = %e, "auto-reconnect echec");
                                // Pas de toast d'erreur ici — la tentative se relancera
                                // au prochain pulse de discovery, on évite le spam.
                            }
                        }
                    });
                }
            }
        });

        *guard = Some(RunningServices {
            listener_task,
            session_dispatcher,
            discovery,
            discovery_consumer,
        });

        let _ = emit_event(
            &app,
            okvm_ipc::BackendEvent::Notification {
                level: okvm_ipc::NotificationLevel::Success,
                title: "Services demarres".into(),
                body: format!("Ecoute sur le port {}", self.tcp_port),
            },
        );

        Ok(())
    }

    pub async fn stop_services(&self) {
        let services = {
            let mut g = self.running.lock();
            g.take()
        };
        let Some(s) = services else {
            return;
        };
        s.listener_task.abort();
        s.session_dispatcher.abort();
        s.discovery_consumer.abort();
        if let Some(d) = s.discovery {
            d.shutdown().await;
        }
        // Coupe toutes les sessions actives.
        let active: Vec<SessionRef> = std::mem::take(&mut *self.sessions.lock())
            .into_values()
            .collect();
        for sr in active {
            sr.shutdown();
        }
    }

    /// Ouvre une session sortante vers `addr`. Optionnellement avec un PIN
    /// d'appairage (TOFU si `None`).
    pub async fn connect_outbound(
        &self,
        addr: SocketAddr,
        pairing_pin: Option<String>,
        app: tauri::AppHandle,
    ) -> okvm_core::Result<DeviceId> {
        let cfg = ConnectorConfig {
            remote: addr,
            pairing_pin,
            ..ConnectorConfig::default()
        };
        let connector = Connector::new(cfg, self.identity.clone(), self.capabilities.clone());
        let session = connector
            .connect()
            .await
            .map_err(|e| okvm_core::Error::Net(format!("connect: {e}")))?;
        let remote_id = session.remote_identity;
        let ctx = DispatchCtx {
            inject: self.inject.clone(),
            file_receiver: self.file_receiver.clone(),
            audio_playback: self.audio_playback.clone(),
            sessions: self.sessions.clone(),
            peers: self.peers.clone(),
            switch: self.switch.clone(),
            grid_map: self.grid_id_by_device.clone(),
            local_bbox: self.local_bbox,
        };
        register_session_with_grid(session, &ctx, &app);
        Ok(remote_id)
    }

    /// Demarre la capture clavier/souris (mode master).
    ///
    /// Le flow est :
    /// 1. Capture Win32 emet `InputMessage` sur un mpsc.
    /// 2. Master loop passe chaque message au `SwitchEngine` avec `local_bbox`.
    /// 3. Si le curseur reste local → on laisse passer (la suppression Win32
    ///    n'est pas active, donc l'OS bouge la souris localement).
    /// 4. Si le `SwitchEngine` decide d'une bascule → on active la suppression
    ///    Win32 (les events sont avales localement), on envoie un `SwitchEnter`
    ///    au pair cible, et on lui route tous les events suivants jusqu'au
    ///    retour local (touche `0` ou `SwitchLeave` distant).
    pub async fn become_master(&self, app: tauri::AppHandle) -> okvm_core::Result<()> {
        if self.capture_handle.lock().is_some() {
            return Ok(());
        }
        let (tx, mut rx) = mpsc::channel::<InputMessage>(1024);
        let capture = Win32Capture;
        let handle = capture.start(tx).await?;
        let suppress_tx = handle.set_suppress.clone();
        *self.capture_handle.lock() = Some(handle);

        let sessions_arc = self.sessions.clone();
        let switch_arc = self.switch.clone();
        let grid_map_arc = self.grid_id_by_device.clone();
        let local_bbox = self.local_bbox;
        let app_h = app.clone();
        tokio::spawn(async move {
            // Etat local du loop.
            let mut current_target: Option<DeviceId> = None;
            while let Some(msg) = rx.recv().await {
                // 1. Passe au SwitchEngine. On capture aussi son etat `current`
                //    pour detecter un retour local via Ctrl+Alt+Win+0 (qui
                //    n'emet pas de SwitchDecision::SwitchTo).
                let (decision, engine_current) = {
                    let mut s = switch_arc.lock();
                    let d = s.on_input(&msg, local_bbox);
                    (d, s.current)
                };

                // Retour local detecte ?
                if engine_current.is_none() && current_target.is_some() {
                    tracing::info!("retour local declenche (hotkey 0)");
                    current_target = None;
                    let _ = suppress_tx.send(false);
                    let _ = emit_event(
                        &app_h,
                        okvm_ipc::BackendEvent::Notification {
                            level: okvm_ipc::NotificationLevel::Info,
                            title: "Retour local".into(),
                            body: "Curseur de retour sur ce PC.".into(),
                        },
                    );
                }

                if let SwitchDecision::SwitchTo {
                    target,
                    edge,
                    enter_x,
                    enter_y,
                } = decision
                {
                    // Resolve grid UUID -> DeviceId (inverse mapping).
                    let device_id = {
                        let map = grid_map_arc.lock();
                        map.iter().find(|(_, u)| **u == target).map(|(d, _)| *d)
                    };
                    let Some(device_id) = device_id else {
                        tracing::warn!(target = ?target, "switch vers un grid_id sans mapping DeviceId");
                        continue;
                    };
                    // Active la suppression locale et envoie SwitchEnter.
                    let _ = suppress_tx.send(true);
                    current_target = Some(device_id);

                    let sender_opt = sessions_arc
                        .lock()
                        .get(&device_id)
                        .map(|s| s.input_tx.clone());
                    if let Some(sender) = sender_opt {
                        let switch_msg = InputMessage::SwitchEnter {
                            from_peer: uuid::Uuid::nil(),
                            enter_x,
                            enter_y,
                            edge,
                        };
                        let _ = sender.send(switch_msg).await;
                        tracing::info!(target = %device_id.fingerprint(), "switch envoye");
                        let _ = emit_event(
                            &app_h,
                            okvm_ipc::BackendEvent::Notification {
                                level: okvm_ipc::NotificationLevel::Info,
                                title: "Bascule".into(),
                                body: format!("→ {}", device_id.fingerprint()),
                            },
                        );
                    }
                }

                // 2. Forwarde l'event vers la cible courante (s'il y en a une).
                if let Some(target_id) = current_target {
                    let sender_opt = sessions_arc
                        .lock()
                        .get(&target_id)
                        .map(|s| s.input_tx.clone());
                    if let Some(sender) = sender_opt {
                        if let Err(e) = sender.send(msg).await {
                            tracing::warn!(error = %e, "input forward echec, retour local");
                            current_target = None;
                            let _ = suppress_tx.send(false);
                        }
                    } else {
                        // Cible disparue : retour local.
                        tracing::info!("cible disparue, retour local");
                        current_target = None;
                        let _ = suppress_tx.send(false);
                    }
                }
                // Sinon : on est en local → on laisse passer (pas de suppression).
            }
        });

        let _ = emit_event(
            &app,
            okvm_ipc::BackendEvent::Notification {
                level: okvm_ipc::NotificationLevel::Success,
                title: "Mode master active".into(),
                body: "Curseur partage selon les bords d'ecran et hotkeys (Ctrl+Alt+Win+1..9)."
                    .into(),
            },
        );
        Ok(())
    }

    /// Arrete la capture.
    pub fn stop_master(&self, app: tauri::AppHandle) {
        if let Some(handle) = self.capture_handle.lock().take() {
            let _ = handle.stop.send(());
            handle.bridge.abort();
        }
        let _ = emit_event(
            &app,
            okvm_ipc::BackendEvent::Notification {
                level: okvm_ipc::NotificationLevel::Info,
                title: "Mode master arrete".into(),
                body: "Clavier/souris locaux seulement.".into(),
            },
        );
    }

    pub fn peer_list(&self) -> Vec<PeerView> {
        let mut out: Vec<PeerView> = self.peers.lock().values().cloned().collect();
        out.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then(b.discovered.cmp(&a.discovered))
                .then(a.name.cmp(&b.name))
        });
        out
    }
}

/// Bundle des refs partagees passees au dispatcher pour qu'il puisse mettre
/// a jour la grille en plus du registre de sessions.
#[derive(Clone)]
struct DispatchCtx {
    inject: Arc<dyn InputInject>,
    file_receiver: Arc<FileReceiver>,
    audio_playback: Arc<dyn AudioPlayback>,
    sessions: Arc<Mutex<HashMap<DeviceId, SessionRef>>>,
    peers: Arc<Mutex<HashMap<DeviceId, PeerView>>>,
    switch: Arc<Mutex<SwitchEngine>>,
    grid_map: Arc<Mutex<HashMap<DeviceId, uuid::Uuid>>>,
    local_bbox: Rect,
}

fn register_session_with_grid(session: Session, ctx: &DispatchCtx, app: &tauri::AppHandle) {
    let id = session.remote_identity;
    let fp = id.fingerprint();
    let name = session.remote_capabilities.os.hostname.clone();
    let remote_screens = session.remote_capabilities.screens.clone();
    tracing::info!(remote = %fp, name = %name, screens = remote_screens.len(), "nouvelle session enregistree");

    // Construit la callback de fermeture (utilisee par les deux inbound tasks).
    let on_close_ctx = ctx.clone();
    let app_for_close = app.clone();
    let once = std::sync::Arc::new(std::sync::Once::new());
    let on_close: Arc<dyn Fn() + Send + Sync + 'static> = {
        let once = once.clone();
        Arc::new(move || {
            once.call_once(|| {
                tracing::info!(remote = %fp, "session close — cleanup");
                // Retire la session de la map.
                on_close_ctx.sessions.lock().remove(&id);
                // Marque le pair offline.
                if let Some(p) = on_close_ctx.peers.lock().get_mut(&id) {
                    p.online = false;
                }
                // Retire le pair de la grille.
                let mut mapping = on_close_ctx.grid_map.lock();
                if let Some(uuid) = mapping.remove(&id) {
                    on_close_ctx.switch.lock().grid.peers.remove(&uuid);
                }
                drop(mapping);
                // Notifie le frontend.
                let _ = emit_event(
                    &app_for_close,
                    okvm_ipc::BackendEvent::PeerDisconnected {
                        device_id: id,
                        reason: "session terminee".into(),
                    },
                );
            });
        })
    };

    // Callback "frame video recue d'un pair" : on emet un event Tauri vers le frontend.
    // Si codec == H264, on decode → re-encode en JPEG (la WebView Tauri n'a pas de
    // decoder H.264 accessible sans MSE custom).
    // Si codec == MJPEG, on forward le JPEG direct.
    let app_for_video = app.clone();
    let video_peer_id = id;
    let h264_decoder: std::sync::Arc<parking_lot::Mutex<Option<H264Decoder>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(None));
    let codec_state: std::sync::Arc<parking_lot::Mutex<Option<okvm_core::VideoCodec>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(None));
    let on_video_frame: Arc<dyn Fn(VideoMessage) + Send + Sync + 'static> = {
        let h264_decoder = h264_decoder.clone();
        let codec_state = codec_state.clone();
        Arc::new(move |msg: VideoMessage| {
            use base64::{engine::general_purpose::STANDARD, Engine};
            use tauri::Emitter;
            match msg {
                VideoMessage::StreamStart {
                    width_px,
                    height_px,
                    target_fps,
                    codec,
                    ..
                } => {
                    *codec_state.lock() = Some(codec);
                    *h264_decoder.lock() = if codec == okvm_core::VideoCodec::H264 {
                        H264Decoder::new().ok()
                    } else {
                        None
                    };
                    tracing::info!(?codec, "video stream start (inbound)");
                    let _ = app_for_video.emit(
                        "okvm://video-stream-start",
                        serde_json::json!({
                            "device_id": video_peer_id,
                            "width": width_px,
                            "height": height_px,
                            "fps": target_fps,
                        }),
                    );
                }
                VideoMessage::StreamFrame { payload, seq, .. } => {
                    let codec = *codec_state.lock();
                    let jpeg_bytes: Option<Vec<u8>> = match codec {
                        Some(okvm_core::VideoCodec::H264) => {
                            // Decode H.264 → RGB → re-encode JPEG.
                            let decoded = h264_decoder
                                .lock()
                                .as_mut()
                                .and_then(|d| d.decode(&payload).ok().flatten());
                            decoded.and_then(|(w, h, rgb)| {
                                let img = image::RgbImage::from_raw(w, h, rgb)?;
                                let mut jpeg = Vec::with_capacity(64 * 1024);
                                let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                                    &mut jpeg, 80,
                                );
                                if enc
                                    .encode_image(&image::DynamicImage::ImageRgb8(img))
                                    .is_ok()
                                {
                                    Some(jpeg)
                                } else {
                                    None
                                }
                            })
                        }
                        Some(okvm_core::VideoCodec::Mjpeg) | None => Some(payload),
                        _ => None,
                    };
                    if let Some(jpeg) = jpeg_bytes {
                        let b64 = STANDARD.encode(&jpeg);
                        let _ = app_for_video.emit(
                            "okvm://video-frame",
                            serde_json::json!({
                                "device_id": video_peer_id,
                                "seq": seq,
                                "jpeg_b64": b64,
                            }),
                        );
                    }
                }
                VideoMessage::StreamStop { .. } => {
                    *codec_state.lock() = None;
                    *h264_decoder.lock() = None;
                    let _ = app_for_video.emit(
                        "okvm://video-stream-stop",
                        serde_json::json!({
                            "device_id": video_peer_id,
                        }),
                    );
                }
                _ => {}
            }
        })
    };

    let sref = SessionRef::from_session(
        session,
        ctx.inject.clone(),
        ctx.file_receiver.clone(),
        ctx.audio_playback.clone(),
        on_video_frame,
        on_close,
    );

    ctx.peers
        .lock()
        .entry(id)
        .and_modify(|p| {
            p.online = true;
            p.paired = true;
        })
        .or_insert_with(|| PeerView {
            device_id: id,
            fingerprint: fp,
            name: name.clone(),
            paired: true,
            online: true,
            discovered: true,
            last_addr: None,
        });

    ctx.sessions.lock().insert(id, sref);

    // === Ajoute le pair a la grille ===
    let mut mapping = ctx.grid_map.lock();
    let new_uuid = *mapping.entry(id).or_insert_with(uuid::Uuid::new_v4);
    drop(mapping);

    let mut switch = ctx.switch.lock();
    let max_right = switch
        .grid
        .peers
        .values()
        .map(|p| {
            let bb = p.bbox();
            bb.x + bb.w
        })
        .max()
        .unwrap_or(ctx.local_bbox.x + ctx.local_bbox.w);

    let hotkey_index = u8::try_from(switch.grid.peers.len())
        .ok()
        .filter(|&n| (1..=9).contains(&n));

    let screens: Vec<okvm_core::ScreenInfo> = if remote_screens.is_empty() {
        vec![okvm_core::ScreenInfo {
            index: 0,
            is_primary: true,
            width_px: 1920,
            height_px: 1080,
            dpi: 96,
            origin_x: 0,
            origin_y: 0,
        }]
    } else {
        remote_screens
    };

    let gp = GridPeer {
        id: new_uuid,
        name: name.clone(),
        screens,
        origin_in_grid: (max_right, ctx.local_bbox.y),
        hotkey_index,
    };
    switch.grid.peers.insert(new_uuid, gp);
    tracing::info!(peer = %name, x = max_right, hotkey = ?hotkey_index, "pair ajoute a la grille");
    drop(switch);

    let _ = emit_event(app, okvm_ipc::BackendEvent::PeerConnected { device_id: id });

    // Persiste apres chaque nouveau pair.
    persist_peers_from_ctx(ctx);
}

fn persist_peers_from_ctx(ctx: &DispatchCtx) {
    let to_save: Vec<okvm_config::PeerProfile> = ctx
        .peers
        .lock()
        .values()
        .filter(|p| p.paired)
        .map(|p| okvm_config::PeerProfile {
            device_id: p.device_id,
            fingerprint: p.fingerprint,
            display_name: p.name.clone(),
            permissions: okvm_core::Permission::default(),
            wol_mac: None,
            last_tcp_port: Some(TCP_PORT_DEFAULT),
            last_seen_ms: Some(ts_now()),
        })
        .collect();
    if let Err(e) = okvm_config::save_peers(&to_save) {
        tracing::warn!(error = %e, "save_peers echec");
    }
}

fn bbox_of_screens(screens: &[okvm_core::ScreenInfo]) -> Rect {
    if screens.is_empty() {
        return Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for s in screens {
        min_x = min_x.min(s.origin_x);
        min_y = min_y.min(s.origin_y);
        max_x = max_x.max(s.origin_x + s.width_px as i32);
        max_y = max_y.max(s.origin_y + s.height_px as i32);
    }
    Rect {
        x: min_x,
        y: min_y,
        w: max_x - min_x,
        h: max_y - min_y,
    }
}

fn emit_event(app: &tauri::AppHandle, ev: okvm_ipc::BackendEvent) -> okvm_core::Result<()> {
    use tauri::Emitter;
    app.emit("okvm://backend-event", ev)
        .map_err(|e| okvm_core::Error::other(format!("emit: {e}")))
}
