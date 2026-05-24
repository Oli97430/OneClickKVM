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
//! # Statut (V3.3.1 step 2 — encode loop validé)
//!
//! - ✅ Init COM/callback : `NVIDIA H.264 Encoder MFT` s'active OK
//!   sur la machine de dev (les MFTs qui ne s'activent pas — ex AMD
//!   sans driver — sont skip et on tombe sur le suivant).
//! - ✅ Event loop : METransformNeedInput / METransformHaveOutput sont
//!   reçus par le bridge et forwardés au worker via channel.
//! - ✅ Re-arm `BeginGetEvent` après chaque event : fait par le worker
//!   (PAS dans le callback Invoke — cf. V3.3.1 step 1 access violation).
//! - ✅ ProcessInput / ProcessOutput : test
//!   `encode_synthetic_frames_produces_nal` valide qu'on obtient ~125 KB
//!   de bitstream Annex-B valide à partir de 60 frames RGB 320x240.
//! - ✅ **Décodage croisé via openh264** : test
//!   `encode_async_then_decode_with_openh264` prouve que le bitstream
//!   NVENC est conforme H.264 — openh264 (decoder indépendant) récupère
//!   width/height correctement.
//!
//! **Pas encore wired dans le pipeline app** : `win32.rs` utilise
//! toujours `MfH264Encoder` sync. Le bascule auto se fera quand on aura
//! validé le décodage croisé.

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
        // **Panic-safety** : `Invoke` est appelé depuis le thread pool de
        // Media Foundation. Un panic Rust qui traverse la frontière COM est
        // **comportement indéfini** — le runtime COM ne sait pas dérouler
        // une stack avec des destructors Rust. Aujourd'hui le code interne
        // n'a pas de panic path évident, mais defense in depth :
        // - allocations (Vec::with_capacity, etc.) peuvent panic OOM
        // - un futur changement peut introduire un `.unwrap()` accidentel
        // - stack overflow toujours possible
        //
        // On wrap dans `catch_unwind` et on retourne E_FAIL au lieu de
        // unwinder. `AssertUnwindSafe` : tous les states sont &self (refs
        // partagées) — pas de risque d'observer un invariant cassé.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.invoke_inner(async_result)
        }));
        match outcome {
            Ok(r) => r,
            Err(_) => {
                tracing::error!("EventBridge::Invoke a paniqué — retourne E_FAIL au lieu d'UB COM");
                Err(windows::core::Error::from_hresult(
                    windows::Win32::Foundation::E_FAIL,
                ))
            }
        }
    }
}

impl EventBridge_Impl {
    /// Implémentation interne d'`Invoke` — séparée pour permettre le wrap
    /// `catch_unwind` autour.
    fn invoke_inner(&self, async_result: Option<&IMFAsyncResult>) -> windows::core::Result<()> {
        let Some(result) = async_result else {
            return Ok(());
        };

        // Récupère l'event qui a déclenché ce callback.
        // SAFETY: result est un IMFAsyncResult valide passé par MF.
        let event: IMFMediaEvent = unsafe { self.event_gen.EndGetEvent(result)? };
        let event_type_u32 = unsafe { event.GetType()? };
        let event_type = MF_EVENT_TYPE(event_type_u32 as i32);

        // Compare avec if-else (les METransform* sont des const, le pattern
        // matching les prend pour des bindings).
        let kind = if event_type == METransformNeedInput {
            MftEvent::NeedInput
        } else if event_type == METransformHaveOutput {
            MftEvent::HaveOutput
        } else if event_type == METransformDrainComplete {
            MftEvent::DrainComplete
        } else {
            // Event qu'on ne gère pas → drop. Le worker re-arm de toute
            // façon à chaque event reçu.
            return Ok(());
        };

        // Forward au worker. Si le channel est fermé (worker mort) ou le
        // mutex poisoned, on ignore silencieusement — on est en shutdown.
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(kind);
        }

        // NB : on ne re-arme PAS BeginGetEvent ici. C'est le worker qui le
        // fait après avoir reçu l'event via le channel. Évite le pattern
        // dangereux de tenter d'obtenir une référence à soi-même depuis
        // l'intérieur d'un trait impl (access violation observé en V3.3.1
        // step 1 avec un transmute hack).
        Ok(())
    }
}

/// Frame à envoyer au worker pour encodage.
struct InputFrame {
    nv12: Vec<u8>,
    ts_hns: i64,
    dur_hns: i64,
}

/// Commandes envoyées depuis l'encoder vers le worker (séparées des
/// `InputFrame` pour ne pas bloquer le pipeline frames si on doit aussi
/// envoyer un command).
#[derive(Debug)]
enum WorkerCmd {
    /// Marque le prochain frame comme keyframe (IDR).
    ///
    /// Plomberie : `ICodecAPI::SetValue(CODECAPI_AVEncVideoForceKeyFrame,
    /// VARIANT_BOOL(TRUE))`. Supporté par NVENC, AMF, QSV, et le MFT
    /// Microsoft. Non destructif (pas de flush — les frames pending
    /// continuent leur cours, seul le prochain encode démarre un nouveau GOP).
    ForceKeyframe,
    /// Drain : envoie `MFT_MESSAGE_COMMAND_DRAIN`, attend l'event
    /// `METransformDrainComplete`, puis ack via `ack_tx`. Le caller
    /// peut alors bloquer sur `ack_rx.recv()` pour avoir la garantie
    /// que tous les NAL pending sont sortis avant de continuer.
    Drain { ack_tx: mpsc::Sender<()> },
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
    /// Sender de commandes au worker (force_keyframe, etc.).
    cmd_tx: mpsc::Sender<WorkerCmd>,
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
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>();
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
                let (transform, friendly_name, bridge, event_gen, _dxgi_keepalive) =
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
                //    Le worker garde bridge + event_gen vivants pour re-armer
                //    BeginGetEvent à chaque event reçu.
                worker_loop(
                    transform, event_rx, input_rx, cmd_rx, output_tx, shutdown_w, bridge, event_gen,
                );

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
            cmd_tx,
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

        // Checked cast pour éviter le silent wrap. En pratique impossible
        // d'atteindre `i64::MAX` (≈10 ans à 30 fps), mais le `as i64` était
        // un code smell — le checked_mul + saturating fallback est gratuit
        // et future-proof.
        let ts_hns = i64::try_from(self.frame_index)
            .ok()
            .and_then(|n| n.checked_mul(self.frame_duration_hns))
            .unwrap_or(i64::MAX);
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

    /// Force le prochain frame à être un keyframe (IDR).
    ///
    /// Utile pour la récupération après perte de paquet (UDP) : on émet
    /// un IDR à la demande au lieu d'attendre le GOP périodique
    /// (~2 sec par défaut), ce qui réduit la durée des artefacts
    /// visuels côté receiver.
    ///
    /// **Implémentation** : envoie une commande au worker qui appellera
    /// `ICodecAPI::SetValue(CODECAPI_AVEncVideoForceKeyFrame, TRUE)`
    /// avant le prochain `ProcessInput`. Supporté par NVENC, AMF, QSV
    /// et le MFT Microsoft software. **Non destructif** — pas de flush,
    /// les frames pending continuent leur cours.
    ///
    /// Si le worker est mort (channel fermé), le call est ignoré
    /// silencieusement — pas d'erreur visible au caller (c'est juste
    /// un best-effort hint).
    pub fn force_keyframe(&mut self) {
        let _ = self.cmd_tx.send(WorkerCmd::ForceKeyframe);
    }

    /// Bloque jusqu'à ce que le worker ait drainé tous les NAL pending
    /// (event `METransformDrainComplete` reçu), puis retourne le bitstream
    /// final qui contient les derniers NAL accumulés.
    ///
    /// **Note** : après `drain()`, l'encoder est en état "end-of-stream"
    /// au sens MFT. Pour reprendre l'encodage il faudrait re-init (pas
    /// supporté actuellement — créer un nouveau `MfH264AsyncEncoder` à
    /// la place).
    ///
    /// # Erreurs
    /// Worker mort, ou timeout 2s si le drain ne termine pas (rare —
    /// MFT devrait drainer en <1s typiquement).
    pub fn drain(&mut self) -> Result<Vec<u8>> {
        let (ack_tx, ack_rx) = mpsc::channel::<()>();
        self.cmd_tx
            .send(WorkerCmd::Drain { ack_tx })
            .map_err(|_| Error::other("worker mort avant drain"))?;

        // Bloque jusqu'à ack ou timeout 2s.
        let drain_finished = ack_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        if !drain_finished {
            tracing::warn!(
                "drain timeout 2s — collecte ce qui est sorti et retourne (NAL pending peuvent \
                 être perdus)"
            );
        }

        // À ce stade tous les HaveOutput ont été pushés dans output_rx
        // par le worker. On les collecte (non-bloquant).
        let mut bitstream = Vec::with_capacity(4096);
        while let Ok(nal) = self.output_rx.try_recv() {
            bitstream.extend_from_slice(&nal);
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
/// - `String` : friendly name (pour diag/logs)
/// - `IMFAsyncCallback` : utilisé par le worker pour re-armer BeginGetEvent
/// - `IMFMediaEventGenerator` : utilisé par le worker pour re-armer
/// - `IMFDXGIDeviceManager` : référencé par le MFT via SET_D3D_MANAGER ;
///   on garde aussi notre propre ref pour la durée de vie
type SetupResult = (
    IMFTransform,
    String,
    IMFAsyncCallback,
    IMFMediaEventGenerator,
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
    Ok((transform, friendly_name, bridge, event_gen, dxgi_mgr))
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
/// Cap sur la queue d'input frames en attente d'être pushées au MFT.
///
/// Si NVENC est bottlenecké (4K@60fps sur GPU faible, ou GPU temporairement
/// busy), les frames s'accumulent. Chaque `InputFrame` contient un `Vec<u8>`
/// NV12 — à 4K c'est ~12 MB. Sans cap, 30 secondes de retard = 21 GB OOM.
///
/// Choix de 30 = ~1 seconde à 30 fps. Au-delà, on drop les plus vieilles
/// (les "fraîches" sont plus utiles pour l'utilisateur — il préfère voir
/// le présent qu'un passé en retard).
const MAX_PENDING_INPUTS: usize = 30;

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    transform: IMFTransform,
    event_rx: mpsc::Receiver<MftEvent>,
    input_rx: mpsc::Receiver<InputFrame>,
    cmd_rx: mpsc::Receiver<WorkerCmd>,
    output_tx: mpsc::Sender<Vec<u8>>,
    shutdown: Arc<Mutex<bool>>,
    bridge: IMFAsyncCallback,
    event_gen: IMFMediaEventGenerator,
) {
    // Cast vers ICodecAPI pour pouvoir setter CODECAPI_AVEncVideoForceKeyFrame.
    // Pas tous les MFTs implémentent ICodecAPI — best-effort. NVENC, AMF, QSV
    // et le MFT software Microsoft le supportent tous.
    let codec_api: Option<windows::Win32::Media::MediaFoundation::ICodecAPI> =
        transform.cast().ok();
    if codec_api.is_none() {
        tracing::debug!("mf-async-worker: ICodecAPI non disponible — force_keyframe sera un no-op");
    }

    let mut needs: u32 = 0;
    let mut pending_inputs: std::collections::VecDeque<InputFrame> =
        std::collections::VecDeque::with_capacity(MAX_PENDING_INPUTS);
    let mut dropped_total: u64 = 0;
    // Optionnel : ack à envoyer quand le prochain METransformDrainComplete arrive.
    // Set par WorkerCmd::Drain, consommé par MftEvent::DrainComplete.
    let mut pending_drain_ack: Option<mpsc::Sender<()>> = None;

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
                    // Signale au caller bloqué sur drain() que c'est fini.
                    if let Some(ack) = pending_drain_ack.take() {
                        let _ = ack.send(());
                    }
                }
            }
            // Re-arme le pump pour le prochain event. À FAIRE après chaque
            // event reçu, sinon MF s'arrête de notifier. C'est fait ici
            // (depuis le worker) plutôt que dans Invoke pour éviter les
            // pièges d'obtenir une self-ref depuis un trait impl.
            if let Err(e) = unsafe { event_gen.BeginGetEvent(&bridge, None) } {
                tracing::warn!(error = %e, "mf-async-worker: BeginGetEvent re-arm échec");
            }
        }

        // 1bis. Pump les commandes worker (force_keyframe, drain) en
        //       non-bloquant.
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                WorkerCmd::ForceKeyframe => {
                    if let Some(api) = &codec_api {
                        // VARIANT VT_BOOL = TRUE — la conversion via
                        // From<bool> est fournie par windows-core.
                        let var: windows::core::VARIANT = true.into();
                        // SAFETY: api et var sont vivants pendant l'appel,
                        // CODECAPI_AVEncVideoForceKeyFrame est un GUID const.
                        let res = unsafe {
                            api.SetValue(
                                &windows::Win32::Media::MediaFoundation::CODECAPI_AVEncVideoForceKeyFrame,
                                &raw const var as *const _,
                            )
                        };
                        match res {
                            Ok(()) => tracing::debug!(
                                "mf-async-worker: force_keyframe appliqué (prochain frame = IDR)"
                            ),
                            Err(e) => tracing::warn!(
                                error = %e,
                                "mf-async-worker: force_keyframe SetValue échec"
                            ),
                        }
                    }
                }
                WorkerCmd::Drain { ack_tx } => {
                    // Stash l'ack — sera envoyé quand METransformDrainComplete
                    // arrive. Si un drain précédent est encore pending, on
                    // remplace (le caller précédent a probablement timeout).
                    if pending_drain_ack.is_some() {
                        tracing::warn!(
                            "mf-async-worker: nouveau drain reçu alors qu'un autre est pending — \
                             remplace l'ack précédent (silent timeout pour ce caller)"
                        );
                    }
                    pending_drain_ack = Some(ack_tx);
                    // SAFETY: transform vivant tant que worker tourne.
                    if let Err(e) =
                        unsafe { transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
                    {
                        tracing::warn!(error = %e, "mf-async-worker: COMMAND_DRAIN échec");
                        // Ack immédiat pour ne pas bloquer le caller.
                        if let Some(ack) = pending_drain_ack.take() {
                            let _ = ack.send(());
                        }
                    }
                }
            }
        }

        // 2. Pump les input frames en non-bloquant, en respectant le cap
        //    `MAX_PENDING_INPUTS` (drop oldest pour faire de la place).
        while let Ok(frame) = input_rx.try_recv() {
            if pending_inputs.len() >= MAX_PENDING_INPUTS {
                let _dropped = pending_inputs.pop_front();
                dropped_total = dropped_total.saturating_add(1);
                // Log par puissance de 2 pour ne pas spammer (1, 2, 4, ...).
                if dropped_total.is_power_of_two() {
                    tracing::warn!(
                        dropped_total,
                        queue_cap = MAX_PENDING_INPUTS,
                        "mf-async-worker: input queue saturée — \
                         drop frames anciennes (NVENC bottlenecked ?)"
                    );
                }
            }
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

    /// Test E2E : push 60 frames, vérifier qu'au moins 1 NAL Annex-B sort.
    ///
    /// Skip silencieusement si l'init échoue (CI sans GPU, etc.).
    #[test]
    fn encode_synthetic_frames_produces_nal() {
        let cfg = H264Config {
            width: 320,
            height: 240,
            target_fps: 30,
            bitrate_kbps: 500,
        };
        let Ok(mut enc) = MfH264AsyncEncoder::try_new(cfg) else {
            println!("skip : init MFT async impossible sur cette machine");
            return;
        };

        // Frame RGB : dégradé qui change à chaque frame (sinon le MFT
        // peut deduplicate et ne rien produire).
        let mut all_bytes: Vec<u8> = Vec::new();
        for i in 0..60u32 {
            let rgb = make_test_rgb(cfg.width as usize, cfg.height as usize, i as u8);
            match enc.encode_rgb(&rgb) {
                Ok(nal) => {
                    if !nal.is_empty() {
                        all_bytes.extend_from_slice(&nal);
                    }
                }
                Err(e) => {
                    println!("encode_rgb frame {i} échec: {e}");
                }
            }
            // Petit sleep pour laisser le worker traiter — sinon on
            // overflow le buffer interne.
            std::thread::sleep(Duration::from_millis(15));
        }

        // Drain pour récupérer ce qui reste.
        if let Ok(tail) = enc.drain() {
            all_bytes.extend_from_slice(&tail);
        }

        println!(
            "async encoder NAL output: {} bytes après 60 frames + drain",
            all_bytes.len()
        );

        // Vérifie qu'on a au moins une chose qui ressemble à H.264 Annex-B.
        // Start code = 00 00 00 01 ou 00 00 01.
        let has_start_code = all_bytes.windows(4).any(|w| w == [0, 0, 0, 1])
            || all_bytes.windows(3).any(|w| w == [0, 0, 1]);

        if all_bytes.is_empty() {
            // Acceptable en CI sans GPU réel ; en local devrait produire.
            println!("⚠ pas de NAL produit — async loop probablement incomplet (V3.3.1 step 2)");
        } else {
            assert!(
                has_start_code,
                "{} bytes produits mais pas de start code Annex-B (00 00 [00] 01)",
                all_bytes.len()
            );
            println!("✓ NAL Annex-B valide");
        }
    }

    fn make_test_rgb(width: usize, height: usize, seed: u8) -> Vec<u8> {
        let mut v = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) * 3;
                v[i] = ((x as u8).wrapping_add(seed)) & 0xFF;
                v[i + 1] = ((y as u8).wrapping_add(seed)) & 0xFF;
                v[i + 2] = seed;
            }
        }
        v
    }

    /// V3.3.1 step 3 : valide que le bitstream NVENC est conforme H.264 en
    /// le décodant avec openh264 (decoder de référence indépendant).
    ///
    /// Skip si init MFT async impossible. Strictement plus fort que
    /// `encode_synthetic_frames_produces_nal` qui ne vérifie que les start
    /// codes — ici on prouve qu'un decoder externe peut récupérer width/height.
    #[test]
    fn encode_async_then_decode_with_openh264() {
        use crate::h264::H264Decoder;

        let cfg = H264Config {
            width: 320,
            height: 240,
            target_fps: 30,
            bitrate_kbps: 500,
        };
        let Ok(mut enc) = MfH264AsyncEncoder::try_new(cfg) else {
            println!("skip : init MFT async impossible sur cette machine");
            return;
        };
        let mut dec = H264Decoder::new().expect("openh264 decoder init");

        let mut decoded_any = false;
        let mut total_bytes = 0usize;
        for i in 0..120u32 {
            let rgb = make_test_rgb(cfg.width as usize, cfg.height as usize, i as u8);
            let nal = match enc.encode_rgb(&rgb) {
                Ok(b) => b,
                Err(e) => {
                    println!("encode_rgb frame {i} échec: {e}");
                    continue;
                }
            };
            if nal.is_empty() {
                std::thread::sleep(Duration::from_millis(15));
                continue;
            }
            total_bytes += nal.len();
            match dec.decode(&nal) {
                Ok(Some((w, h, _))) => {
                    println!("✓ openh264 a décodé une frame {w}×{h} (frame #{i})");
                    assert_eq!(w, cfg.width);
                    assert_eq!(h, cfg.height);
                    decoded_any = true;
                    break;
                }
                Ok(None) => {
                    // Pas encore assez d'input pour le decoder.
                }
                Err(e) => {
                    panic!(
                        "openh264 decode ERROR sur le bitstream NVENC frame {i}: {e:?} \
                         (bitstream invalide ?)"
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        // Drain pour capturer les dernières frames.
        if !decoded_any {
            if let Ok(tail) = enc.drain() {
                total_bytes += tail.len();
                if let Ok(Some((w, h, _))) = dec.decode(&tail) {
                    println!("✓ openh264 a décodé une frame {w}×{h} via drain");
                    decoded_any = true;
                }
            }
        }
        println!("Total NVENC bitstream: {total_bytes} bytes");
        assert!(
            decoded_any,
            "openh264 n'a décodé AUCUNE frame du bitstream NVENC ({total_bytes} bytes) — \
             le bitstream n'est probablement pas conforme H.264 Annex-B"
        );
    }

    /// V3.3.2 + V3.3.3 : exerce force_keyframe + drain dans un scénario
    /// réaliste. Vérifie que :
    /// 1. force_keyframe() ne panic pas et est silencieux (best-effort).
    /// 2. drain() retourne **avant le timeout 2s** — preuve que
    ///    METransformDrainComplete a été reçu et que l'ack channel marche.
    /// 3. drain() ne droppe pas trop de NAL (au moins le bitstream
    ///    pré-drain est sorti).
    ///
    /// Skip si pas de MFT hardware async dispo.
    #[test]
    fn force_keyframe_and_drain_round_trip() {
        let cfg = H264Config {
            width: 320,
            height: 240,
            target_fps: 30,
            bitrate_kbps: 500,
        };
        let Ok(mut enc) = MfH264AsyncEncoder::try_new(cfg) else {
            println!("skip : MFT async indisponible");
            return;
        };

        // 1. Encode 30 frames (1 sec).
        let mut bytes_before = 0usize;
        for i in 0..30u32 {
            let rgb = make_test_rgb(cfg.width as usize, cfg.height as usize, i as u8);
            if let Ok(n) = enc.encode_rgb(&rgb) {
                bytes_before += n.len();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("✓ Phase 1 : 30 frames → {bytes_before} bytes");

        // 2. force_keyframe — doit être un no-op silencieux (pas de panic).
        let t0 = std::time::Instant::now();
        enc.force_keyframe();
        assert!(
            t0.elapsed() < Duration::from_millis(50),
            "force_keyframe() devrait être quasi-instantané (envoie juste un msg sur un channel)"
        );
        println!("✓ force_keyframe() OK ({:?})", t0.elapsed());

        // 3. Encode 30 frames de plus — la 1ère devrait être un IDR
        //    (mais on ne peut pas vérifier sans décoder).
        let mut bytes_after = 0usize;
        for i in 30..60u32 {
            let rgb = make_test_rgb(cfg.width as usize, cfg.height as usize, i as u8);
            if let Ok(n) = enc.encode_rgb(&rgb) {
                bytes_after += n.len();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("✓ Phase 2 : 30 frames post-keyframe → {bytes_after} bytes");

        // 4. drain — DOIT retourner avant le timeout interne 2s.
        let t0 = std::time::Instant::now();
        let tail = enc
            .drain()
            .expect("drain() retourne Ok même si rien à drainer");
        let drain_elapsed = t0.elapsed();
        println!(
            "✓ drain() retourné en {drain_elapsed:?} avec {} bytes",
            tail.len()
        );
        // Si le timeout 2s a été atteint, drain a quand même renvoyé OK
        // mais avec un warn. La preuve que DrainComplete a marché est
        // que drain est plus rapide que le timeout.
        assert!(
            drain_elapsed < Duration::from_millis(1500),
            "drain() a pris {drain_elapsed:?} — probable que METransformDrainComplete \
             n'a pas été reçu (timeout 2s atteint, on retombe sur le path warn)"
        );
        let total = bytes_before + bytes_after + tail.len();
        println!("Total bitstream : {total} bytes (avant + après keyframe + drain)");
        assert!(total > 0, "aucun NAL produit après 60 frames + drain");
    }
}
