//! Encodeur H.264 via un Media Foundation Transform (MFT).
//!
//! Cette implémentation cible la **première étape** d'intégration hardware :
//!
//! 1. Instanciation du **MFT software Microsoft** (`CLSID_CMSH264EncoderMFT`)
//!    qui est toujours présent sur Windows 10/11.
//! 2. Configuration d'un pipeline sync-mode : entrée NV12 system memory,
//!    sortie H.264 Annex-B.
//! 3. Encodage frame-par-frame via `ProcessInput` / `ProcessOutput`.
//!
//! La **deuxième étape** (NVENC / Quick Sync / AMF) demande de wirer un
//! `IMFDXGIDeviceManager` D3D11 — c'est une rallonge de ~300 lignes qui sera
//! livrée séparément. Pour l'instant on a l'API publique stable
//! (`MfH264Encoder::new`, `encode_rgb`) qui pourra rester inchangée le jour
//! où on bascule sur le hardware.
//!
//! ## Pourquoi cette première étape est utile ?
//!
//! - Valide tout le plumbing COM/MF (`CoInitialize`, `MFStartup`, `IMFSample`,
//!   `IMFMediaBuffer`, `IMFTransform`).
//! - Le MFT Microsoft est souvent plus rapide qu'openh264 grâce à ses
//!   optimisations SSE/AVX.
//! - Permet de tester la conversion RGB→NV12 (qui sera réutilisée tel quel
//!   pour le MFT hardware).

#![cfg(windows)]

use std::ptr;

use windows::core::GUID;
use windows::Win32::Media::MediaFoundation::eAVEncH264VProfile_Main;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, IMFTransform, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_INFO, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use crate::mediafoundation::ensure_mf_init;

use okvm_core::{Error, Result};

use crate::h264::H264Config;

/// `CLSID_CMSH264EncoderMFT` — encodeur software Microsoft, toujours présent.
/// Voir <https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder>.
const CLSID_CMSH264_ENCODER_MFT: GUID = GUID::from_u128(0x6ca50344_051a_4ded_9779_a43305165e35);

/// Type de backend MFT effectivement instancié.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MfBackend {
    /// `CLSID_CMSH264EncoderMFT` Microsoft software (toujours présent).
    /// CPU pur, optimisé SSE/AVX. Pas de D3D11 manager.
    Software,
    /// MFT hardware sélectionné via `MFTEnumEx(MFT_ENUM_FLAG_HARDWARE)` +
    /// `IMFActivate::ActivateObject`. Reçoit un `IMFDXGIDeviceManager`
    /// pour potentiellement utiliser des textures D3D11.
    ///
    /// **⚠️ Non validé E2E sur ce projet** : le code COM init compile et
    /// l'init du MFT renvoie OK sur la machine de dev, mais la validité du
    /// bitstream H.264 produit n'est testée que par présence d'un start
    /// code Annex-B — pas par décodage croisé avec un decoder de référence.
    Hardware {
        /// Nom convivial du MFT (ex: `"NVIDIA H.264 Encoder MFT"`).
        friendly_name: String,
    },
}

/// Encodeur H.264 utilisant un MFT Microsoft.
pub struct MfH264Encoder {
    cfg: H264Config,
    transform: IMFTransform,
    /// Buffer NV12 réutilisé pour éviter une allocation par frame.
    nv12_buf: Vec<u8>,
    /// Compteur de frames émises (pour les timestamps).
    frame_index: u64,
    /// Durée d'un frame en unités de 100 ns (HNS).
    frame_duration_hns: i64,
    /// Backend effectivement utilisé (intéressant pour AboutView et debug).
    backend: MfBackend,
    /// Manager D3D11 lié au transform (Some si backend == Hardware).
    /// Doit rester vivant tant que le transform existe (la doc MF dit que
    /// le manager peut être référencé par le transform).
    #[allow(dead_code)]
    d3d_manager: Option<windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager>,
    /// D3D11 device tenu vivant pour la même raison.
    #[allow(dead_code)]
    d3d_device: Option<crate::d3d11_helper::D3D11Resources>,
}

// SAFETY: l'encodeur est créé et utilisé exclusivement sur le thread de capture
// vidéo (cf. `okvm_video::win32::run_capture_thread`). Le crate `windows`
// n'auto-implémente pas `Send` pour `IMFTransform` car COM marshalling général
// requiert IMarshal. Pour notre cas mono-thread c'est sûr : on n'expose JAMAIS
// la structure entre threads via channels. La marker existe uniquement pour que
// le trait `GraphicsCaptureApiHandler::start` (qui exige `Self: Send + 'static`)
// soit satisfait — windows-capture transfère le handler vers son propre thread
// message-loop une seule fois à l'init.
unsafe impl Send for MfH264Encoder {}

/// Init COM + MF via le helper unifié (cf. `mediafoundation::ensure_mf_init`).
fn ensure_com_mf_init() -> Result<()> {
    ensure_mf_init().map_err(Error::other)
}

/// Lit l'attribut `MFT_FRIENDLY_NAME_Attribute` d'un `IMFActivate`.
fn read_friendly_name(
    act: &windows::Win32::Media::MediaFoundation::IMFActivate,
    attr_guid: &windows::core::GUID,
) -> String {
    use windows::Win32::System::Com::CoTaskMemFree;
    // SAFETY: GetAllocatedString alloue une wide string via CoTaskMemAlloc.
    unsafe {
        let mut buf_ptr: windows::core::PWSTR = windows::core::PWSTR(std::ptr::null_mut());
        let mut len: u32 = 0;
        if act
            .GetAllocatedString(attr_guid, &mut buf_ptr, &mut len)
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

impl MfH264Encoder {
    /// Backend MFT effectivement utilisé pour cet encodeur.
    #[must_use]
    pub fn backend(&self) -> &MfBackend {
        &self.backend
    }

    /// Tente d'instancier l'encodeur sur un MFT **hardware** (NVENC / QuickSync
    /// / AMF) via `MFTEnumEx` filtré sur `MFT_ENUM_FLAG_HARDWARE` + sortie
    /// H.264. Si plusieurs hardware encoders sont disponibles, prend le
    /// **premier** (l'ordre dépend du driver — typiquement NVIDIA en premier
    /// sur les GPUs gaming).
    ///
    /// Plus complexe que [`Self::new`] :
    /// - Nécessite un D3D11 device + `IMFDXGIDeviceManager`
    /// - Bascule potentielle vers async-mode (non géré ici → erreur explicite)
    /// - Output format H.264 NV12 via le canal D3D si possible
    ///
    /// **⚠️ Honnêteté** : ce code compile et l'init succède sur la machine
    /// de dev, mais l'output H.264 hardware n'a PAS été décodé / comparé à
    /// un golden file. La validité du bitstream est présumée (start code
    /// Annex-B vérifié, c'est tout). Le test E2E sur 2 vrais PCs reste à faire.
    ///
    /// # Erreurs
    /// Init D3D11, énumération MFT, ActivateObject, async-mode non supporté.
    pub fn try_new_hardware(cfg: H264Config) -> Result<Self> {
        use windows::Win32::Media::MediaFoundation::{
            IMFActivate, IMFAttributes, MFTEnumEx, MFT_FRIENDLY_NAME_Attribute,
            MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_MESSAGE_SET_D3D_MANAGER, MFT_REGISTER_TYPE_INFO, MF_TRANSFORM_ASYNC,
        };

        ensure_com_mf_init()?;

        // Crée D3D11 device + DXGI manager AVANT d'énumérer — si le device
        // ne se crée pas (VM sans GPU passthrough), pas la peine d'essayer
        // un MFT hardware.
        let d3d = crate::d3d11_helper::create_d3d11_device()
            .map_err(|e| Error::other(format!("hardware MFT requires D3D11: {e}")))?;
        let dxgi_mgr = crate::d3d11_helper::create_dxgi_manager(&d3d.device)?;

        // Enumere les MFT video encoder hardware avec sortie H.264.
        let out_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
        let mut count: u32 = 0;
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        // SAFETY: MFTEnumEx alloue activates via CoTaskMemAlloc, à libérer ensuite.
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                None,
                Some(&out_info),
                &mut activates,
                &mut count,
            )
            .map_err(|e| Error::other(format!("MFTEnumEx(HARDWARE): {e}")))?;
        }
        if count == 0 || activates.is_null() {
            return Err(Error::other("aucun MFT hardware H.264 disponible"));
        }

        // V3.3 step 2.1 : itère sur tous les HW MFTs et prend le premier en
        // **sync-mode** (le code async n'est pas wrappé, cf. V3.3.1). Sur les
        // machines avec NVIDIA récent (async-only) + AMD récent (async-only)
        // + Microsoft AVC DX12 (sync), on tombera sur DX12.
        let mut chosen: Option<(IMFActivate, String)> = None;
        let mut skipped_async: Vec<String> = Vec::new();
        for i in 0..count as isize {
            // SAFETY: activates[i] est un Option<IMFActivate>.
            let candidate = unsafe { (*activates.offset(i)).as_ref().cloned() };
            let Some(act) = candidate else { continue };
            let name = read_friendly_name(&act, &MFT_FRIENDLY_NAME_Attribute);
            // Lit MF_TRANSFORM_ASYNC sur l'IMFActivate AVANT d'activer (cheap).
            // SAFETY: GetUINT32 retourne Err si attribut absent.
            let is_async = unsafe { act.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) } != 0;
            if is_async {
                skipped_async.push(name);
                continue;
            }
            chosen = Some((act, name));
            break;
        }

        // Libère le tableau (tous les slots restants).
        // SAFETY: activates alloué par MFTEnumEx ; on a cloné le retenu.
        unsafe {
            for i in 0..count as isize {
                let _ = (*activates.offset(i)).take();
            }
            let _ = windows::Win32::System::Com::CoTaskMemFree(Some(activates.cast()));
        }

        let (activate, friendly_name) = chosen.ok_or_else(|| {
            Error::other(format!(
                "Aucun MFT hardware sync-mode. Skipped (async): {}",
                skipped_async.join(", ")
            ))
        })?;

        if !skipped_async.is_empty() {
            tracing::info!(
                chosen = %friendly_name,
                skipped = ?skipped_async,
                "MFT hardware sync sélectionné (async-only MFTs ignorés, cf. V3.3.1)"
            );
        }

        // Active le MFT.
        let transform: IMFTransform = unsafe {
            activate
                .ActivateObject::<IMFTransform>()
                .map_err(|e| Error::other(format!("ActivateObject({friendly_name}): {e}")))?
        };

        // Double-check : certains MFTs peuvent changer leur mode async après
        // activation (rare). Si ça arrive, on garde l'erreur explicite.
        let attrs: IMFAttributes = unsafe {
            transform
                .GetAttributes()
                .map_err(|e| Error::other(format!("GetAttributes: {e}")))?
        };
        let is_async = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) } != 0;
        if is_async {
            return Err(Error::other(format!(
                "MFT {friendly_name} signalé sync à enum mais async après activate"
            )));
        }

        // Set le D3D manager — permet au MFT d'allouer des textures D3D11.
        // SAFETY: dxgi_mgr est vivant ; ProcessMessage prend un usize qui doit
        // être un pointeur vers IUnknown du manager.
        use windows::core::Interface;
        let mgr_ptr = dxgi_mgr.as_raw() as usize;
        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr_ptr)
                .map_err(|e| Error::other(format!("SET_D3D_MANAGER: {e}")))?;
        }

        // === À partir d'ici : même setup que SW ===
        let output_type = create_output_type_h264(&cfg)?;
        unsafe {
            transform
                .SetOutputType(0, &output_type, 0)
                .map_err(|e| Error::other(format!("SetOutputType (HW): {e}")))?;
        }
        let input_type = create_input_type_nv12(&cfg)?;
        unsafe {
            transform
                .SetInputType(0, &input_type, 0)
                .map_err(|e| Error::other(format!("SetInputType (HW): {e}")))?;
        }
        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| Error::other(format!("BEGIN_STREAMING (HW): {e}")))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| Error::other(format!("START_OF_STREAM (HW): {e}")))?;
        }

        let nv12_size = (cfg.width as usize) * (cfg.height as usize) * 3 / 2;
        let frame_duration_hns = (10_000_000_i64 / i64::from(cfg.target_fps.max(1))).max(1);

        tracing::info!(name = %friendly_name, "MfH264Encoder HARDWARE activé");

        Ok(Self {
            cfg,
            transform,
            nv12_buf: vec![0u8; nv12_size],
            frame_index: 0,
            frame_duration_hns,
            backend: MfBackend::Hardware { friendly_name },
            d3d_manager: Some(dxgi_mgr),
            d3d_device: Some(d3d),
        })
    }

    /// Diagnostic léger : retourne quel backend MFT serait choisi par
    /// [`Self::new_best`] sur cette machine, sans garder l'encoder vivant.
    /// Utile pour l'UI (AboutView) qui veut afficher l'état sans démarrer
    /// la capture.
    ///
    /// Implémentation : on tente `try_new_hardware` (qui exerce vraiment
    /// MFTEnumEx + Activate). Si succès → `Hardware`. Sinon → `Software`.
    /// Note : le coût est l'init D3D11 (~10 ms) puis activate du MFT —
    /// non gratuit, mais OK pour un appel one-shot au load de AboutView.
    #[must_use]
    pub fn probe_best_backend(cfg: H264Config) -> MfBackend {
        match Self::try_new_hardware(cfg) {
            Ok(enc) => enc.backend.clone(),
            Err(_) => MfBackend::Software,
        }
    }

    /// Tente d'instancier le **meilleur** encodeur disponible :
    /// 1. [`Self::try_new_hardware`] (NVENC/QSV/AMF si OS support)
    /// 2. [`Self::new`] (MFT Microsoft software) en fallback
    ///
    /// Le caller peut interroger [`Self::backend`] pour savoir lequel a été
    /// retenu et l'afficher dans l'UI.
    ///
    /// # Erreurs
    /// Seulement si AUCUN backend ne marche (improbable sur W10/11 où le
    /// MFT software est toujours présent).
    pub fn new_best(cfg: H264Config) -> Result<Self> {
        match Self::try_new_hardware(cfg) {
            Ok(enc) => Ok(enc),
            Err(e) => {
                tracing::info!(error = %e, "MFT hardware indisponible, fallback software");
                Self::new(cfg)
            }
        }
    }

    /// Crée et configure un encodeur H.264 via le MFT Microsoft software.
    ///
    /// # Erreurs
    /// Init COM/MF, échec instanciation, échec configuration média types.
    pub fn new(cfg: H264Config) -> Result<Self> {
        ensure_com_mf_init()?;

        // SAFETY: COM est init via ensure_com_mf_init.
        let transform: IMFTransform = unsafe {
            CoCreateInstance(&CLSID_CMSH264_ENCODER_MFT, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| Error::other(format!("CoCreateInstance(H264EncoderMFT): {e}")))?
        };

        // === Output type (H.264) — DOIT être set avant l'input type. ===
        let output_type = create_output_type_h264(&cfg)?;
        // SAFETY: pointeurs valides issus de COM, stream_id = 0 par défaut.
        unsafe {
            transform
                .SetOutputType(0, &output_type, 0)
                .map_err(|e| Error::other(format!("SetOutputType: {e}")))?;
        }

        // === Input type (NV12) ===
        let input_type = create_input_type_nv12(&cfg)?;
        unsafe {
            transform
                .SetInputType(0, &input_type, 0)
                .map_err(|e| Error::other(format!("SetInputType: {e}")))?;
        }

        // === Notify begin streaming ===
        // Le MS H264 encoder MFT a besoin de BEGIN_STREAMING **puis**
        // START_OF_STREAM avant d'accepter le premier input. Sans ça
        // ProcessOutput renvoie en boucle MF_E_TRANSFORM_NEED_MORE_INPUT
        // sans jamais produire de NAL.
        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| Error::other(format!("NOTIFY_BEGIN_STREAMING: {e}")))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| Error::other(format!("NOTIFY_START_OF_STREAM: {e}")))?;
        }

        // Pré-alloue le buffer NV12 (taille = 1.5 * width * height).
        let nv12_size = (cfg.width as usize) * (cfg.height as usize) * 3 / 2;
        // 100 ns ticks par frame.
        let frame_duration_hns = (10_000_000_i64 / i64::from(cfg.target_fps.max(1))).max(1);

        Ok(Self {
            cfg,
            transform,
            nv12_buf: vec![0u8; nv12_size],
            frame_index: 0,
            frame_duration_hns,
            backend: MfBackend::Software,
            d3d_manager: None,
            d3d_device: None,
        })
    }

    /// Encode une frame RGB (3 octets/pixel, top-down) en bitstream H.264 Annex-B.
    ///
    /// Le résultat peut être vide si l'encodeur n'a pas encore produit de NAL
    /// (typique pour la première frame en attendant les SPS/PPS).
    ///
    /// # Erreurs
    /// Conversion taille, échec encode COM.
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

        // === Crée un IMFSample portant le NV12 ===
        let input_sample = make_input_sample(
            &self.nv12_buf,
            self.frame_index as i64 * self.frame_duration_hns,
            self.frame_duration_hns,
        )?;

        // === ProcessInput ===
        // SAFETY: input_sample est un IMFSample valide.
        unsafe {
            self.transform
                .ProcessInput(0, &input_sample, 0)
                .map_err(|e| Error::other(format!("ProcessInput: {e}")))?;
        }

        self.frame_index += 1;

        // === Drain ProcessOutput ===
        let mut bitstream = Vec::with_capacity(4096);
        loop {
            match self.process_output_once(&mut bitstream) {
                Ok(true) => continue,    // got data, try again
                Ok(false) => break,      // need more input
                Err(e) => return Err(e), // hard error
            }
        }
        Ok(bitstream)
    }

    /// Pull une frame de sortie. Renvoie `Ok(true)` si on a écrit dans
    /// `out`, `Ok(false)` si l'encodeur a besoin de plus d'input.
    fn process_output_once(&mut self, out: &mut Vec<u8>) -> Result<bool> {
        // 1. Récupère la taille du buffer de sortie attendu.
        let stream_info: MFT_OUTPUT_STREAM_INFO = {
            // SAFETY: stream id 0 existe toujours.
            unsafe {
                self.transform
                    .GetOutputStreamInfo(0)
                    .map_err(|e| Error::other(format!("GetOutputStreamInfo: {e}")))?
            }
        };

        // 2. Alloue un IMFMediaBuffer de cette taille (sauf si le MFT
        //    alloue lui-même via le flag MFT_OUTPUT_STREAM_PROVIDES_SAMPLES).
        let provides_samples = (stream_info.dwFlags
            & windows::Win32::Media::MediaFoundation::MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32)
            != 0;

        let sample = if provides_samples {
            // Le MFT fournira son propre sample.
            None
        } else {
            let size = stream_info.cbSize.max(64 * 1024);
            // SAFETY: size > 0.
            let buf = unsafe {
                MFCreateMemoryBuffer(size)
                    .map_err(|e| Error::other(format!("MFCreateMemoryBuffer: {e}")))?
            };
            let s: IMFSample = unsafe {
                MFCreateSample().map_err(|e| Error::other(format!("MFCreateSample: {e}")))?
            };
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

        // SAFETY: output_buffer pointe vers une struct valide.
        let res = unsafe {
            self.transform.ProcessOutput(
                0,
                std::slice::from_mut(&mut output_buffer),
                &raw mut status,
            )
        };
        // Drop manually-managed fields explicitly to relâcher les refs COM.
        let sample_back = unsafe { std::mem::ManuallyDrop::take(&mut output_buffer.pSample) };
        let _events = unsafe { std::mem::ManuallyDrop::take(&mut output_buffer.pEvents) };

        match res {
            Ok(()) => {
                // On a un sample (soit celui qu'on a alloué, soit fourni par le MFT).
                let s = sample_back.ok_or_else(|| Error::other("ProcessOutput: sample null"))?;
                read_sample_bytes(&s, out)?;
                Ok(true)
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(false),
            Err(e) => Err(Error::other(format!("ProcessOutput: {e}"))),
        }
    }

    /// Force le prochain frame à être un keyframe.
    ///
    /// (Sur le MFT Microsoft, on émet `MFT_MESSAGE_COMMAND_FLUSH` puis
    /// `MFT_MESSAGE_NOTIFY_BEGIN_STREAMING`+`START_OF_STREAM`, ce qui force
    /// un IDR au prochain frame.)
    pub fn force_keyframe(&mut self) {
        // SAFETY: transform est valide tout au long du lifetime de l'encoder.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }
    }

    /// Drain : envoie un signal de fin de flux et récupère tous les NAL
    /// encore en attente dans le buffer interne du MFT. À appeler en fin
    /// de session (ou en tests) pour s'assurer qu'on ne perd pas les
    /// dernières frames.
    ///
    /// **⚠ Après `drain()`, l'encodeur est en état "end-of-stream"** : un
    /// appel ultérieur à `encode_rgb()` échouera. Pour reprendre l'encodage
    /// après un drain, ré-instancier l'encodeur (`MfH264Encoder::new`) ou
    /// appeler `force_keyframe()` qui ré-arme `BEGIN_STREAMING` +
    /// `START_OF_STREAM`.
    ///
    /// # Erreurs
    /// Échec `ProcessOutput`.
    pub fn drain(&mut self) -> Result<Vec<u8>> {
        // SAFETY: transform valide.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
        let mut out = Vec::with_capacity(4096);
        loop {
            match self.process_output_once(&mut out) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Configuration courante.
    #[must_use]
    pub fn config(&self) -> H264Config {
        self.cfg
    }
}

impl Drop for MfH264Encoder {
    fn drop(&mut self) {
        // SAFETY: transform vivant ici.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        }
    }
}

// === Helpers de configuration des media types ===

fn create_output_type_h264(cfg: &H264Config) -> Result<IMFMediaType> {
    // SAFETY: appels MF stockent les attributs dans l'IMFMediaType retourné.
    unsafe {
        let mt =
            MFCreateMediaType().map_err(|e| Error::other(format!("MFCreateMediaType: {e}")))?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| Error::other(format!("SetGUID(MAJOR_TYPE): {e}")))?;
        mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(|e| Error::other(format!("SetGUID(SUBTYPE): {e}")))?;
        mt.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate_kbps * 1000)
            .map_err(|e| Error::other(format!("SetUINT32(AVG_BITRATE): {e}")))?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| Error::other(format!("SetUINT32(INTERLACE): {e}")))?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(cfg.width, cfg.height))
            .map_err(|e| Error::other(format!("SetUINT64(FRAME_SIZE): {e}")))?;
        mt.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(cfg.target_fps, 1))
            .map_err(|e| Error::other(format!("SetUINT64(FRAME_RATE): {e}")))?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))
            .map_err(|e| Error::other(format!("SetUINT64(PAR): {e}")))?;
        mt.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)
            .map_err(|e| Error::other(format!("SetUINT32(PROFILE): {e}")))?;
        Ok(mt)
    }
}

fn create_input_type_nv12(cfg: &H264Config) -> Result<IMFMediaType> {
    unsafe {
        let mt =
            MFCreateMediaType().map_err(|e| Error::other(format!("MFCreateMediaType: {e}")))?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| Error::other(format!("SetGUID(MAJOR_TYPE): {e}")))?;
        mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .map_err(|e| Error::other(format!("SetGUID(SUBTYPE NV12): {e}")))?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| Error::other(format!("SetUINT32(INTERLACE): {e}")))?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(cfg.width, cfg.height))
            .map_err(|e| Error::other(format!("SetUINT64(FRAME_SIZE): {e}")))?;
        mt.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(cfg.target_fps, 1))
            .map_err(|e| Error::other(format!("SetUINT64(FRAME_RATE): {e}")))?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))
            .map_err(|e| Error::other(format!("SetUINT64(PAR): {e}")))?;
        Ok(mt)
    }
}

fn pack_u64(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn make_input_sample(nv12: &[u8], ts_hns: i64, dur_hns: i64) -> Result<IMFSample> {
    // SAFETY: tailles valides, on copie le contenu dans le buffer COM.
    unsafe {
        let buf_size = u32::try_from(nv12.len())
            .map_err(|_| Error::other(format!("NV12 buffer trop grand: {}", nv12.len())))?;
        let buf = MFCreateMemoryBuffer(buf_size)
            .map_err(|e| Error::other(format!("MFCreateMemoryBuffer: {e}")))?;
        let mut ptr: *mut u8 = ptr::null_mut();
        let mut max_len: u32 = 0;
        let mut current_len: u32 = 0;
        buf.Lock(
            &raw mut ptr,
            Some(&raw mut max_len),
            Some(&raw mut current_len),
        )
        .map_err(|e| Error::other(format!("Lock: {e}")))?;
        if ptr.is_null() || (max_len as usize) < nv12.len() {
            let _ = buf.Unlock();
            return Err(Error::other("buffer COM trop petit ou null"));
        }
        ptr::copy_nonoverlapping(nv12.as_ptr(), ptr, nv12.len());
        buf.Unlock()
            .map_err(|e| Error::other(format!("Unlock: {e}")))?;
        buf.SetCurrentLength(buf_size)
            .map_err(|e| Error::other(format!("SetCurrentLength: {e}")))?;

        let sample = MFCreateSample().map_err(|e| Error::other(format!("MFCreateSample: {e}")))?;
        sample
            .AddBuffer(&buf)
            .map_err(|e| Error::other(format!("AddBuffer: {e}")))?;
        sample
            .SetSampleTime(ts_hns)
            .map_err(|e| Error::other(format!("SetSampleTime: {e}")))?;
        sample
            .SetSampleDuration(dur_hns)
            .map_err(|e| Error::other(format!("SetSampleDuration: {e}")))?;
        Ok(sample)
    }
}

fn read_sample_bytes(sample: &IMFSample, out: &mut Vec<u8>) -> Result<()> {
    // SAFETY: on lit le seul buffer du sample.
    unsafe {
        let buf = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| Error::other(format!("ConvertToContiguousBuffer: {e}")))?;
        let mut ptr: *mut u8 = ptr::null_mut();
        let mut max_len: u32 = 0;
        let mut current_len: u32 = 0;
        buf.Lock(
            &raw mut ptr,
            Some(&raw mut max_len),
            Some(&raw mut current_len),
        )
        .map_err(|e| Error::other(format!("Lock: {e}")))?;
        if ptr.is_null() {
            let _ = buf.Unlock();
            return Err(Error::other("buffer null en sortie"));
        }
        let slice = std::slice::from_raw_parts(ptr, current_len as usize);
        out.extend_from_slice(slice);
        buf.Unlock()
            .map_err(|e| Error::other(format!("Unlock: {e}")))?;
    }
    Ok(())
}

/// Convertit un buffer RGB 8-bit (top-down) en NV12 (Y plane + UV interleaved).
///
/// L'output doit avoir une taille de `width * height * 3 / 2`.
///
/// **Pré-conditions** :
/// - `width` et `height` doivent être **pairs** (NV12 sous-échantillonne les
///   plans U/V par 2 sur les deux axes). Sinon, la dernière ligne ou colonne
///   est silencieusement perdue. `debug_assert!` panique en mode debug ; en
///   release, on tronque (le caller est responsable d'aligner ses dimensions
///   en amont, ce qui est le cas pour les présets 1280×720 / 1920×1080).
///
/// Algorithme : conversion BT.601 limited range (simple, correct pour du
/// screen content basse résolution). Pour V3.x on pourra passer en BT.709
/// full range si on cible des écrans HD.
pub fn rgb_to_nv12(rgb: &[u8], width: usize, height: usize, out: &mut [u8]) {
    debug_assert_eq!(rgb.len(), width * height * 3);
    debug_assert_eq!(out.len(), width * height * 3 / 2);
    debug_assert_eq!(width % 2, 0, "NV12 exige une largeur paire");
    debug_assert_eq!(height % 2, 0, "NV12 exige une hauteur paire");

    // Y plane (en haut), UV plane interleavé (en bas, demi-résolution).
    let (y_plane, uv_plane) = out.split_at_mut(width * height);

    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 3;
            let r = i32::from(rgb[i]);
            let g = i32::from(rgb[i + 1]);
            let b = i32::from(rgb[i + 2]);
            // BT.601, limited range [16..235].
            let yv = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_plane[y * width + x] = yv.clamp(0, 255) as u8;
        }
    }

    // UV : chaque (2x2 block) → 1 paire U/V.
    let uv_width = width / 2;
    for by in 0..height / 2 {
        for bx in 0..uv_width {
            let mut r_acc = 0i32;
            let mut g_acc = 0i32;
            let mut b_acc = 0i32;
            // Moyenne 4 pixels.
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((by * 2 + dy) * width + (bx * 2 + dx)) * 3;
                    r_acc += i32::from(rgb[i]);
                    g_acc += i32::from(rgb[i + 1]);
                    b_acc += i32::from(rgb[i + 2]);
                }
            }
            r_acc /= 4;
            g_acc /= 4;
            b_acc /= 4;
            let u = ((-38 * r_acc - 74 * g_acc + 112 * b_acc + 128) >> 8) + 128;
            let v = ((112 * r_acc - 94 * g_acc - 18 * b_acc + 128) >> 8) + 128;
            let idx = (by * uv_width + bx) * 2;
            uv_plane[idx] = u.clamp(0, 255) as u8;
            uv_plane[idx + 1] = v.clamp(0, 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_best_succeeds_on_either_backend() {
        // Honnêteté : ce test n'affirme PAS que le hardware encode produit
        // un bitstream valide. Il vérifie juste que new_best() retourne un
        // encoder fonctionnel — soit hardware si dispo, soit fallback software.
        let cfg = H264Config {
            width: 320,
            height: 240,
            target_fps: 15,
            bitrate_kbps: 500,
        };

        // Log explicitement le résultat de try_new_hardware en cas d'échec,
        // pour qu'on sache POURQUOI le fallback SW est utilisé.
        match MfH264Encoder::try_new_hardware(cfg) {
            Ok(enc) => match enc.backend() {
                MfBackend::Hardware { friendly_name } => {
                    eprintln!("try_new_hardware OK : {friendly_name}");
                }
                MfBackend::Software => {
                    eprintln!("try_new_hardware returned Software (bug)");
                }
            },
            Err(e) => {
                eprintln!("try_new_hardware échec : {e}");
            }
        }

        let enc = MfH264Encoder::new_best(cfg).expect("au moins SW doit marcher");
        match enc.backend() {
            MfBackend::Software => {
                eprintln!("new_best() → Software fallback");
            }
            MfBackend::Hardware { friendly_name } => {
                eprintln!("new_best() → Hardware ({friendly_name})");
            }
        }
    }

    #[test]
    fn encoder_init() {
        let cfg = H264Config {
            width: 320,
            height: 240,
            target_fps: 15,
            bitrate_kbps: 500,
        };
        let _ = MfH264Encoder::new(cfg).expect("MFT encoder init");
    }

    #[test]
    fn encode_solid_color_produces_bitstream() {
        let cfg = H264Config {
            width: 320,
            height: 240,
            target_fps: 15,
            bitrate_kbps: 500,
        };
        let mut enc = MfH264Encoder::new(cfg).unwrap();
        let rgb: Vec<u8> = (0..320usize * 240).flat_map(|_| [200u8, 30, 30]).collect();
        // Le MFT MS bufferise plusieurs frames avant d'émettre. On pousse 60
        // frames puis on drain — au moins un NAL doit sortir.
        let mut accumulated = Vec::new();
        for _ in 0..60 {
            let bs = enc.encode_rgb(&rgb).unwrap();
            accumulated.extend_from_slice(&bs);
        }
        let tail = enc.drain().unwrap();
        accumulated.extend_from_slice(&tail);
        assert!(
            !accumulated.is_empty(),
            "aucun NAL produit après 60 frames + drain"
        );
        // Vérifie un start code Annex-B.
        let has_start = accumulated.windows(4).any(|w| w == [0, 0, 0, 1])
            || accumulated.windows(3).any(|w| w == [0, 0, 1]);
        assert!(has_start, "pas de start code Annex-B dans le bitstream");
    }

    #[test]
    fn rgb_to_nv12_correct_size() {
        let mut out = vec![0u8; 320 * 240 * 3 / 2];
        let rgb = vec![128u8; 320 * 240 * 3];
        rgb_to_nv12(&rgb, 320, 240, &mut out);
        // Pour du gris pur RGB(128,128,128), Y ≈ 126 (BT.601 limited), U/V = 128.
        assert!(out[0] >= 120 && out[0] <= 130, "Y={}", out[0]);
        // Premier UV byte (U) ≈ 128.
        let uv_start = 320 * 240;
        assert!(
            out[uv_start] >= 125 && out[uv_start] <= 131,
            "U={}",
            out[uv_start]
        );
    }
}
