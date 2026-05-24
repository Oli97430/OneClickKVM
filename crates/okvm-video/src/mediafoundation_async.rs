//! V3.3.1 — Wrapper async-mode MFT pour les vrais encoders hardware
//! (NVENC, AMF, Quick Sync récents).
//!
//! # Pourquoi un fichier séparé ?
//!
//! Les MFTs hardware modernes (NVIDIA H.264 Encoder MFT, AMDh264Encoder,
//! Intel Quick Sync HEVC...) sont en **mode async** : ils ne supportent
//! pas le pattern simple `ProcessInput → ProcessOutput` du `MfH264Encoder`
//! sync. À la place, ils signalent via `IMFMediaEventGenerator` quand ils
//! sont prêts à recevoir une frame (`METransformNeedInput`) ou ont produit
//! un NAL (`METransformHaveOutput`).
//!
//! Ce module implémente l'event loop nécessaire :
//! 1. Cast le `IMFTransform` en `IMFMediaEventGenerator`.
//! 2. Crée un `IMFAsyncCallback` (notre struct `EventBridge`) qui sera
//!    invoqué par MF à chaque event.
//! 3. Démarre le pump avec `BeginGetEvent`.
//! 4. Un worker thread interne consomme les events et fait
//!    `ProcessInput`/`ProcessOutput` aux bons moments.
//! 5. L'API publique [`MfH264AsyncEncoder::encode_rgb`] reste synchrone
//!    pour le caller — il push une frame et reçoit le NAL en retour
//!    (blocking avec timeout).
//!
//! # Statut honnête (V3.3.1)
//!
//! - Skeleton COM/callback : compile, init OK sur la machine de dev
//!   (NVIDIA H.264 Encoder MFT activé).
//! - **Pipeline E2E non testé** sur 2 vrais PCs avec consommation NAL :
//!   le caller (`win32.rs`) utilise toujours `MfH264Encoder` sync. Ce
//!   wrapper est exposé `pub` pour permettre l'expérimentation manuelle
//!   et les futurs tests.
//! - Le drain n'est pas encore wired (TODO).

#![cfg(windows)]

use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use okvm_core::{Error, Result};
use windows::core::{implement, Interface};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFMediaEvent, IMFMediaEventGenerator,
    IMFTransform, METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
    MFCreateMemoryBuffer, MFCreateSample, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_INFO,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MF_EVENT_TYPE, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
};

use crate::d3d11_helper;
use crate::h264::H264Config;
use crate::mediafoundation::ensure_mf_init;
use crate::mediafoundation_encoder::{
    create_input_type_nv12, create_output_type_h264, make_input_sample, read_sample_bytes,
    rgb_to_nv12, MfBackend,
};

/// Events que le callback IMFAsyncCallback forward vers le worker thread.
#[derive(Debug)]
enum MftEvent {
    /// Le MFT est prêt à recevoir une frame d'entrée via `ProcessInput`.
    NeedInput,
    /// Le MFT a produit un NAL prêt à être récupéré via `ProcessOutput`.
    HaveOutput,
    /// Réponse à `MFT_MESSAGE_COMMAND_DRAIN` — tous les NAL pending ont
    /// été émis.
    DrainComplete,
}

/// Bridge IMFAsyncCallback → mpsc channel.
///
/// Cette struct est un objet COM (via `#[implement]`) que MF appellera
/// depuis son propre thread pool. Le contrat est minimal : on doit faire
/// vite et ne pas paniquer. On forward juste le type d'event vers le
/// worker thread via un channel.
#[implement(IMFAsyncCallback)]
struct EventBridge {
    /// Le générateur d'events sur lequel re-armer `BeginGetEvent` à chaque
    /// callback.
    event_gen: IMFMediaEventGenerator,
    /// Canal vers le worker thread.
    tx: Mutex<mpsc::Sender<MftEvent>>,
}

#[allow(non_snake_case)]
impl IMFAsyncCallback_Impl for EventBridge_Impl {
    fn GetParameters(&self, _pdwflags: *mut u32, _pdwqueue: *mut u32) -> windows::core::Result<()> {
        // Retourner E_NOTIMPL fait que MF utilise les valeurs par défaut
        // (pas de flag spécial, queue par défaut). C'est le pattern
        // recommandé sauf si on a vraiment besoin de queue dédiée.
        Err(windows::core::Error::from_hresult(E_NOTIMPL))
    }

    fn Invoke(&self, async_result: Option<&IMFAsyncResult>) -> windows::core::Result<()> {
        let Some(result) = async_result else {
            return Ok(());
        };

        // Récupère l'event qui a déclenché ce callback.
        // SAFETY: result est un IMFAsyncResult valide passé par MF.
        let event: IMFMediaEvent = unsafe { self.event_gen.EndGetEvent(result)? };
        let event_type_u32 = unsafe { event.GetType()? };
        let event_type = MF_EVENT_TYPE(event_type_u32 as i32);

        // Note : on compare avec if-else plutôt que match parce que les
        // METransform* sont des `const MF_EVENT_TYPE` (non upper case),
        // que le pattern matching prend pour des bindings et warn.
        let kind = if event_type == METransformNeedInput {
            MftEvent::NeedInput
        } else if event_type == METransformHaveOutput {
            MftEvent::HaveOutput
        } else if event_type == METransformDrainComplete {
            MftEvent::DrainComplete
        } else {
            // Event qu'on ne gère pas — on re-arm quand même et on
            // ignore. Important de re-arm sinon le pump s'arrête.
            let cb = self.to_imf_async_callback();
            let _ = unsafe { self.event_gen.BeginGetEvent(&cb, None) };
            return Ok(());
        };

        // Forward au worker. Si le channel est fermé (worker mort), on
        // ignore silencieusement — on est en shutdown.
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(kind);
        }

        // Re-arme pour le prochain event. Sans ça, le pump s'arrête après
        // le 1er event.
        let cb = self.to_imf_async_callback();
        let _ = unsafe { self.event_gen.BeginGetEvent(&cb, None) };

        Ok(())
    }
}

impl EventBridge_Impl {
    /// Renvoie une `IMFAsyncCallback` qui pointe vers ce même objet COM.
    /// Utilisé pour re-armer `BeginGetEvent` depuis `Invoke`.
    fn to_imf_async_callback(&self) -> IMFAsyncCallback {
        // SAFETY: le macro #[implement] garantit qu'on peut cast self
        // (un &EventBridge_Impl wrapper) vers une IMFAsyncCallback.
        // L'AddRef se fait via Interface::cast.
        let unknown: windows::core::IUnknown = unsafe {
            std::mem::transmute_copy::<&EventBridge_Impl, &windows::core::IUnknown>(&self).clone()
        };
        unknown.cast::<IMFAsyncCallback>().unwrap_or_else(|_| {
            // En théorie impossible : le macro #[implement(IMFAsyncCallback)]
            // garantit l'interface. Si ça arrive c'est un bug compilateur.
            unreachable!("IMFAsyncCallback cast failed on EventBridge_Impl");
        })
    }
}

/// Frame à envoyer au worker pour encodage.
struct InputFrame {
    nv12: Vec<u8>,
    ts_hns: i64,
    dur_hns: i64,
}

/// API publique : encoder H.264 async-mode (NVENC / AMF / QSV récents).
pub struct MfH264AsyncEncoder {
    /// Backend descriptor (toujours `MfBackend::Hardware`).
    backend: MfBackend,
    /// Config H.264.
    cfg: H264Config,
    /// Buffer NV12 réutilisé pour la conversion RGB → NV12.
    nv12_buf: Vec<u8>,
    /// Compteur de frames pour les timestamps.
    frame_index: u64,
    /// Durée d'une frame en 100ns ticks.
    frame_duration_hns: i64,
    /// Sender vers le worker (frames d'entrée).
    input_tx: mpsc::Sender<InputFrame>,
    /// Receiver des NAL produits par le worker.
    output_rx: mpsc::Receiver<Vec<u8>>,
    /// Handle du worker thread (joined dans Drop).
    worker: Option<JoinHandle<()>>,
    /// Sentinel pour signaler au worker de s'arrêter.
    shutdown: Arc<Mutex<bool>>,
}

// SAFETY: l'encoder n'expose pas l'IMFTransform au caller — toute interaction
// avec COM se fait via les channels vers le worker thread interne. Le worker
// vit en MTA (init via ensure_mf_init).
unsafe impl Send for MfH264AsyncEncoder {}

impl MfH264AsyncEncoder {
    /// Backend MFT effectivement utilisé.
    #[must_use]
    pub fn backend(&self) -> &MfBackend {
        &self.backend
    }

    /// Config H.264 courante.
    #[must_use]
    pub fn config(&self) -> H264Config {
        self.cfg
    }

    /// Tente d'instancier un encoder async-mode hardware.
    ///
    /// **Pattern** : énumère les MFTs HW H.264 et prend le **premier**
    /// (sync ou async indifférent — on les unlock tous en async via
    /// `MF_TRANSFORM_ASYNC_UNLOCK`). Si tous les MFTs hardware échouent,
    /// renvoie une erreur — le caller doit fallback sur `MfH264Encoder`
    /// sync software.
    ///
    /// # Erreurs
    /// - D3D11 indisponible (VM sans GPU passthrough)
    /// - `MFTEnumEx(HARDWARE)` retourne 0 entries
    /// - L'unlock async échoue (peut arriver sur de vieux drivers)
    /// - Cast `IMFTransform → IMFMediaEventGenerator` échoue
    pub fn try_new(cfg: H264Config) -> Result<Self> {
        // Channels worker ↔ encoder. Tout doit être créé AVANT le spawn,
        // car le worker fera lui-même TOUTE l'init COM/MFT (pour ne pas
        // transporter d'IMFTransform à travers le thread boundary — il
        // contient un NonNull<c_void> qui n'est pas Send).
        let (input_tx, input_rx) = mpsc::channel::<InputFrame>();
        let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>();
        let (init_tx, init_rx) = mpsc::channel::<Result<String>>();

        let shutdown = Arc::new(Mutex::new(false));
        let shutdown_w = shutdown.clone();

        let worker = std::thread::Builder::new()
            .name("mf-async-worker".into())
            .spawn(move || {
                // 1. Init COM/MF (mute le main thread — on est en MTA).
                if let Err(e) = ensure_mf_init().map_err(Error::other) {
                    let _ = init_tx.send(Err(e));
                    return;
                }
                // 2. Création du channel d'events MFT (interne au worker).
                let (event_tx, event_rx) = mpsc::channel::<MftEvent>();

                // 3. Setup MFT hardware + callback (le bridge récupère event_tx).
                let (transform, friendly_name, _bridge_keepalive, _dxgi_keepalive) =
                    match setup_async_mft(cfg, event_tx) {
                        Ok(t) => t,
                        Err(e) => {
                            let _ = init_tx.send(Err(e));
                            return;
                        }
                    };
                // 4. Signale au caller que tout est OK.
                let _ = init_tx.send(Ok(friendly_name));

                // 5. Boucle worker — bloque jusqu'à shutdown ou disconnect.
                worker_loop(transform, event_rx, input_rx, output_tx, shutdown_w);

                // _bridge_keepalive et _dxgi_keepalive sont drop ici (fin worker).
            })
            .map_err(|e| Error::other(format!("spawn mf-async-worker: {e}")))?;

        // Attend l'init OK ou Err. Timeout 5 s (création D3D11 + activate MFT
        // peuvent durer un peu).
        let friendly_name = init_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| Error::other("init mf-async-worker timeout 5s"))??;

        let nv12_size = (cfg.width as usize) * (cfg.height as usize) * 3 / 2;
        let frame_duration_hns = (10_000_000_i64 / i64::from(cfg.target_fps.max(1))).max(1);

        Ok(Self {
            backend: MfBackend::Hardware { friendly_name },
            cfg,
            nv12_buf: vec![0u8; nv12_size],
            frame_index: 0,
            frame_duration_hns,
            input_tx,
            output_rx,
            worker: Some(worker),
            shutdown,
        })
    }

    /// Encode une frame RGB. Pour un MFT async, on push la frame dans
    /// le channel d'entrée du worker et on récupère tous les NAL
    /// disponibles dans le channel de sortie (drain non-bloquant).
    ///
    /// **Latence** : 1-2 frames typiquement (le MFT bufferise pour avoir
    /// matière à compresser). Le résultat peut donc être vide pour les
    /// premières frames.
    ///
    /// # Erreurs
    /// Taille RGB invalide, worker mort.
    pub fn encode_rgb(&mut self, rgb: &[u8]) -> Result<Vec<u8>> {
        let expected = (self.cfg.width as usize) * (self.cfg.height as usize) * 3;
        if rgb.len() != expected {
            return Err(Error::other(format!(
                "rgb size mismatch: {} vs attendu {}",
                rgb.len(),
                expected
            )));
        }

        rgb_to_nv12(
            rgb,
            self.cfg.width as usize,
            self.cfg.height as usize,
            &mut self.nv12_buf,
        );

        let ts_hns = self.frame_index as i64 * self.frame_duration_hns;
        let frame = InputFrame {
            nv12: self.nv12_buf.clone(),
            ts_hns,
            dur_hns: self.frame_duration_hns,
        };
        self.input_tx
            .send(frame)
            .map_err(|_| Error::other("worker mf-async-worker mort"))?;

        self.frame_index += 1;

        // Drain non-bloquant du output_rx : on prend tous les NAL disponibles.
        let mut bitstream = Vec::with_capacity(4096);
        while let Ok(nal) = self.output_rx.try_recv() {
            bitstream.extend_from_slice(&nal);
        }
        Ok(bitstream)
    }

    /// Bloque jusqu'à ce que le worker ait drainé tous les NAL pending,
    /// puis retourne le bitstream final.
    ///
    /// # Erreurs
    /// Timeout (5 s) ou worker mort.
    pub fn drain(&mut self) -> Result<Vec<u8>> {
        // TODO V3.3.1 step 2 : signaler au worker de faire
        // ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN) puis attendre l'event
        // METransformDrainComplete. Pour l'instant on collecte juste ce
        // qui reste dans output_rx pendant 200ms.
        let mut bitstream = Vec::with_capacity(4096);
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while std::time::Instant::now() < deadline {
            match self.output_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(nal) => bitstream.extend_from_slice(&nal),
                Err(_) => break,
            }
        }
        Ok(bitstream)
    }
}

impl Drop for MfH264AsyncEncoder {
    fn drop(&mut self) {
        // Signale shutdown au worker, ferme l'input (qui terminera la boucle
        // recv()), puis join.
        if let Ok(mut s) = self.shutdown.lock() {
            *s = true;
        }
        // Drop input_tx : input_rx.recv() retournera Err(Disconnected).
        // On peut pas explicitement drop self.input_tx ici (move), mais on
        // peut wait join — quand l'encoder est drop, input_tx est drop aussi.
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// Bundle de keepalives COM retournés par `setup_async_mft` :
/// - `IMFTransform` : owner principal
/// - `IMFAsyncCallback` : doit rester vivant pendant que BeginGetEvent est armé
/// - `IMFDXGIDeviceManager` : référencé par le MFT via SET_D3D_MANAGER
///   mais on garde aussi notre propre ref pour la durée de vie
type SetupResult = (
    IMFTransform,
    String,
    IMFAsyncCallback,
    windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager,
);

/// Setup complet du MFT hardware async : énumération, activation, unlock,
/// types, BEGIN_STREAMING, cast event generator, BeginGetEvent initial.
///
/// **À appeler uniquement depuis le worker thread** (sinon `IMFTransform`
/// devrait traverser le thread boundary = Send qu'il n'a pas).
///
/// Le `IMFAsyncCallback` retourné doit être maintenu vivant pendant toute
/// la durée d'usage du transform — sinon MF n'a plus de cible à invoquer.
///
/// Le `IMFDXGIDeviceManager` retourné doit aussi rester vivant pour les
/// mêmes raisons (référencé par les samples GPU).
fn setup_async_mft(cfg: H264Config, event_tx: mpsc::Sender<MftEvent>) -> Result<SetupResult> {
    use windows::Win32::Media::MediaFoundation::{
        IMFActivate, IMFAttributes, MFMediaType_Video, MFTEnumEx, MFVideoFormat_H264,
        MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_MESSAGE_SET_D3D_MANAGER, MFT_REGISTER_TYPE_INFO,
    };

    // (ensure_mf_init déjà fait par le worker au démarrage)

    let d3d = d3d11_helper::create_d3d11_device()
        .map_err(|e| Error::other(format!("async MFT requires D3D11: {e}")))?;
    let dxgi_mgr = d3d11_helper::create_dxgi_manager(&d3d.device)?;

    let out_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
    let mut count: u32 = 0;
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            None,
            Some(&out_info),
            &mut activates,
            &mut count,
        )
        .map_err(|e| Error::other(format!("MFTEnumEx(HW async): {e}")))?;
    }
    if count == 0 || activates.is_null() {
        return Err(Error::other("aucun MFT hardware H.264 disponible"));
    }

    // Itère sur les MFTs hardware et prend le PREMIER qui s'active réellement.
    // Sur certaines machines (ex: NVIDIA-only), MFTEnumEx liste quand même
    // AMDh264Encoder en 1er mais ActivateObject échoue avec E_NOTIMPL parce
    // que le driver AMD n'est pas installé. On veut tomber sur NVIDIA dans
    // ce cas, pas abandonner.
    let mut chosen: Option<(IMFTransform, String)> = None;
    let mut skipped: Vec<String> = Vec::new();
    for i in 0..count as isize {
        let candidate = unsafe { (*activates.offset(i)).as_ref().cloned() };
        let Some(act) = candidate else { continue };
        let name = read_friendly_name(&act);
        match unsafe { act.ActivateObject::<IMFTransform>() } {
            Ok(t) => {
                chosen = Some((t, name));
                break;
            }
            Err(e) => {
                skipped.push(format!("{name} ({})", e.code()));
            }
        }
    }
    // Libère le tableau dans tous les cas.
    unsafe {
        for i in 0..count as isize {
            let _ = (*activates.offset(i)).take();
        }
        let _ = windows::Win32::System::Com::CoTaskMemFree(Some(activates.cast()));
    }
    let (transform, friendly_name) = chosen.ok_or_else(|| {
        Error::other(format!(
            "Aucun MFT hardware activable. Échecs: {}",
            skipped.join(", ")
        ))
    })?;
    if !skipped.is_empty() {
        tracing::info!(
            chosen = %friendly_name,
            skipped = ?skipped,
            "async MFT : MFTs ignorés (ActivateObject échoué)"
        );
    }

    let attrs: IMFAttributes = unsafe {
        transform
            .GetAttributes()
            .map_err(|e| Error::other(format!("GetAttributes: {e}")))?
    };
    let is_async = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) } != 0;

    if is_async {
        unsafe {
            attrs
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(|e| Error::other(format!("SetUINT32(ASYNC_UNLOCK): {e}")))?;
        }
        tracing::info!(name = %friendly_name, "MfH264AsyncEncoder: ASYNC_UNLOCK appliqué");
    } else {
        tracing::info!(name = %friendly_name, "MfH264AsyncEncoder: MFT sync, fonctionne aussi");
    }

    let mgr_ptr = dxgi_mgr.as_raw() as usize;
    unsafe {
        transform
            .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr_ptr)
            .map_err(|e| Error::other(format!("SET_D3D_MANAGER: {e}")))?;
    }

    let output_type = create_output_type_h264(&cfg)?;
    unsafe {
        transform
            .SetOutputType(0, &output_type, 0)
            .map_err(|e| Error::other(format!("SetOutputType (async): {e}")))?;
    }
    let input_type = create_input_type_nv12(&cfg)?;
    unsafe {
        transform
            .SetInputType(0, &input_type, 0)
            .map_err(|e| Error::other(format!("SetInputType (async): {e}")))?;
    }

    unsafe {
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(|e| Error::other(format!("BEGIN_STREAMING (async): {e}")))?;
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(|e| Error::other(format!("START_OF_STREAM (async): {e}")))?;
    }

    let event_gen: IMFMediaEventGenerator = transform.cast().map_err(|e| {
        Error::other(format!(
            "cast IMFMediaEventGenerator (MFT pas vraiment async ?): {e}"
        ))
    })?;

    // Le bridge sera invoqué par MF avec les events transformés. Le
    // event_tx est passé par le caller (worker thread) qui owns le
    // event_rx local.
    let bridge_inner = EventBridge {
        event_gen: event_gen.clone(),
        tx: Mutex::new(event_tx),
    };
    let bridge: IMFAsyncCallback = bridge_inner.into();

    unsafe {
        event_gen
            .BeginGetEvent(&bridge, None)
            .map_err(|e| Error::other(format!("BeginGetEvent initial: {e}")))?;
    }

    // (drop d3d : MFT a sa propre ref via SET_D3D_MANAGER ; le dxgi_mgr est
    // gardé en sortie pour rester vivant.)
    drop(d3d);
    Ok((transform, friendly_name, bridge, dxgi_mgr))
}

/// Worker thread : owns IMFTransform, réagit aux events MF, fait
/// ProcessInput/ProcessOutput aux bons moments.
///
/// **Credit-based flow** : on tient un compteur `needs` qui reflète
/// combien de fois MF nous a dit `NeedInput` sans qu'on ait répondu par
/// un `ProcessInput`. Quand une frame arrive dans `input_rx`, si `needs
/// > 0` on consomme un crédit et on push. Sinon on attend.
///
/// Pour `HaveOutput`, on appelle ProcessOutput immédiatement et on push
/// les NAL dans `output_tx`.
fn worker_loop(
    transform: IMFTransform,
    event_rx: mpsc::Receiver<MftEvent>,
    input_rx: mpsc::Receiver<InputFrame>,
    output_tx: mpsc::Sender<Vec<u8>>,
    shutdown: Arc<Mutex<bool>>,
) {
    let mut needs: u32 = 0;
    let mut pending_inputs: std::collections::VecDeque<InputFrame> =
        std::collections::VecDeque::new();

    // Poll alterné event_rx / input_rx via recv_timeout pour ne pas
    // bloquer indéfiniment dans l'un.
    loop {
        if shutdown.lock().map(|s| *s).unwrap_or(true) {
            break;
        }

        // 1. Pump les events MFT en non-bloquant.
        while let Ok(ev) = event_rx.try_recv() {
            match ev {
                MftEvent::NeedInput => {
                    needs = needs.saturating_add(1);
                }
                MftEvent::HaveOutput => {
                    if let Err(e) = process_one_output(&transform, &output_tx) {
                        tracing::warn!(error = %e, "mf-async-worker: ProcessOutput échec");
                    }
                }
                MftEvent::DrainComplete => {
                    tracing::debug!("mf-async-worker: drain complete");
                }
            }
        }

        // 2. Pump les input frames en non-bloquant.
        while let Ok(frame) = input_rx.try_recv() {
            pending_inputs.push_back(frame);
        }

        // 3. Match needs avec pending_inputs.
        while needs > 0 && !pending_inputs.is_empty() {
            let frame = pending_inputs.pop_front().unwrap();
            needs -= 1;
            match make_input_sample(&frame.nv12, frame.ts_hns, frame.dur_hns) {
                Ok(sample) => {
                    if let Err(e) = unsafe { transform.ProcessInput(0, &sample, 0) } {
                        tracing::warn!(error = %e, "mf-async-worker: ProcessInput échec");
                        needs += 1; // rétablit le crédit en cas d'erreur
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "mf-async-worker: make_input_sample échec");
                }
            }
        }

        // 4. Si rien à faire, sleep 5ms pour éviter spin.
        if needs == 0 && pending_inputs.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // Cleanup : drain l'encoder avant de quitter.
    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
    }
}

/// Helper : appelle ProcessOutput une fois, push le NAL dans `output_tx`
/// si succès.
fn process_one_output(transform: &IMFTransform, output_tx: &mpsc::Sender<Vec<u8>>) -> Result<()> {
    let stream_info: MFT_OUTPUT_STREAM_INFO = unsafe {
        transform
            .GetOutputStreamInfo(0)
            .map_err(|e| Error::other(format!("GetOutputStreamInfo: {e}")))?
    };

    let provides_samples = (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

    let sample = if provides_samples {
        None
    } else {
        let size = stream_info.cbSize.max(64 * 1024);
        let buf = unsafe {
            MFCreateMemoryBuffer(size)
                .map_err(|e| Error::other(format!("MFCreateMemoryBuffer: {e}")))?
        };
        let s =
            unsafe { MFCreateSample().map_err(|e| Error::other(format!("MFCreateSample: {e}")))? };
        unsafe {
            s.AddBuffer(&buf)
                .map_err(|e| Error::other(format!("AddBuffer: {e}")))?;
        }
        Some(s)
    };

    let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
        dwStreamID: 0,
        pSample: std::mem::ManuallyDrop::new(sample.clone()),
        dwStatus: 0,
        pEvents: std::mem::ManuallyDrop::new(None),
    };
    let mut status: u32 = 0;

    let res = unsafe {
        transform.ProcessOutput(0, std::slice::from_mut(&mut output_buffer), &raw mut status)
    };
    let sample_back = unsafe { std::mem::ManuallyDrop::take(&mut output_buffer.pSample) };
    let _events = unsafe { std::mem::ManuallyDrop::take(&mut output_buffer.pEvents) };

    match res {
        Ok(()) => {
            let s = sample_back.ok_or_else(|| Error::other("ProcessOutput: sample null"))?;
            let mut bytes = Vec::with_capacity(4096);
            read_sample_bytes(&s, &mut bytes)?;
            output_tx
                .send(bytes)
                .map_err(|_| Error::other("output channel fermé"))?;
            Ok(())
        }
        Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
            // Rare en async — normalement HaveOutput n'est signalé que
            // si la donnée est prête. Mais possible en bord d'event loop.
            Ok(())
        }
        Err(e) => Err(Error::other(format!("ProcessOutput (async): {e}"))),
    }
}

/// Lit `MFT_FRIENDLY_NAME_Attribute` (copie de `mediafoundation_encoder`).
fn read_friendly_name(act: &windows::Win32::Media::MediaFoundation::IMFActivate) -> String {
    use windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute;
    use windows::Win32::System::Com::CoTaskMemFree;
    unsafe {
        let mut buf_ptr: windows::core::PWSTR = windows::core::PWSTR(std::ptr::null_mut());
        let mut len: u32 = 0;
        if act
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut buf_ptr, &mut len)
            .is_err()
            || buf_ptr.0.is_null()
        {
            return "<unnamed>".into();
        }
        let slice = std::slice::from_raw_parts(buf_ptr.0, len as usize);
        let s = String::from_utf16_lossy(slice);
        let _ = CoTaskMemFree(Some(buf_ptr.0.cast()));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_new_async_does_not_panic() {
        // Best-effort : on tente d'instancier, mais on accepte un Err
        // (machine sans GPU, drivers anciens, etc.). L'important est que
        // ça ne panic pas.
        let cfg = H264Config {
            width: 640,
            height: 480,
            target_fps: 15,
            bitrate_kbps: 500,
        };
        match MfH264AsyncEncoder::try_new(cfg) {
            Ok(enc) => {
                println!("async encoder OK: {:?}", enc.backend());
                drop(enc);
            }
            Err(e) => {
                println!("async encoder Err (acceptable): {e}");
            }
        }
    }
}
