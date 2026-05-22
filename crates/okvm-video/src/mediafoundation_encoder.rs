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
//! - Valide tout le plumbing COM/MF (CoInitialize, MFStartup, IMFSample,
//!   IMFMediaBuffer, IMFTransform).
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

impl MfH264Encoder {
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
            self.transform
                .ProcessOutput(0, std::slice::from_mut(&mut output_buffer), &mut status)
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
    /// Échec ProcessOutput.
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
        buf.Lock(&mut ptr, Some(&mut max_len), Some(&mut current_len))
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
        buf.Lock(&mut ptr, Some(&mut max_len), Some(&mut current_len))
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
