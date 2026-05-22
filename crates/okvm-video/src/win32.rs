//! Capture d'écran Windows via `windows-capture` + encodage H.264 (V2)
//! avec fallback MJPEG si l'encoder openh264 ne peut pas s'initialiser.
//!
//! Le crate `windows-capture` s'appuie sur l'API Windows Graphics Capture.
//! L'API expose un trait `GraphicsCaptureApiHandler` dont la méthode
//! `start()` **bloque** le thread courant en pumpant la message loop ; on
//! l'exécute donc sur un thread OS dédié.

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use uuid::Uuid;

use okvm_core::{Error, Result, VideoCodec};
use okvm_protocol::VideoMessage;

use crate::{H264Backend, H264Config, H264Encoder, MfH264Encoder, VideoCapture, VideoHandle};

use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

/// Implementation Windows Graphics Capture.
pub struct WindowsCaptureSource {
    /// Largeur cible d'encodage (downscale si plus petit que la source).
    pub target_width: u32,
    /// Hauteur cible.
    pub target_height: u32,
    /// Cadence cible en fps (approximative).
    pub target_fps: u32,
    /// Qualite JPEG si on retombe sur le fallback (1..100).
    pub jpeg_quality: u8,
    /// Bitrate H.264 cible en kbps.
    pub h264_bitrate_kbps: u32,
    /// Si `true`, tente H.264 ; si `false` ou si l'init H.264 echoue, utilise MJPEG.
    pub prefer_h264: bool,
    /// Backend H.264 préféré quand `prefer_h264` est `true`.
    pub h264_backend: H264Backend,
}

impl Default for WindowsCaptureSource {
    fn default() -> Self {
        Self {
            target_width: 1280,
            target_height: 720,
            target_fps: 15,
            jpeg_quality: 75,
            // 1500 kbps = ~30x moins que MJPEG ~10 Mbps a 720p15
            h264_bitrate_kbps: 1500,
            prefer_h264: true,
            h264_backend: H264Backend::default(),
        }
    }
}

#[async_trait]
impl VideoCapture for WindowsCaptureSource {
    async fn start(
        &self,
        screen_idx: u32,
        tx: mpsc::Sender<VideoMessage>,
    ) -> Result<VideoHandle> {
        let (stop_tx, _stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (frame_tx, frame_rx) = std_mpsc::channel::<EncodedFrame>();
        let (init_tx, init_rx) =
            std_mpsc::channel::<std::result::Result<StreamInfo, String>>();

        let target_width = self.target_width;
        let target_height = self.target_height;
        let target_fps = self.target_fps;
        let jpeg_quality = self.jpeg_quality;
        let h264_bitrate_kbps = self.h264_bitrate_kbps;
        let prefer_h264 = self.prefer_h264;
        let h264_backend = self.h264_backend;

        // Thread OS pour la capture (windows-capture::start() bloque).
        std::thread::Builder::new()
            .name("okvm-video-capture".into())
            .spawn(move || {
                run_capture_thread(
                    screen_idx,
                    target_width,
                    target_height,
                    target_fps,
                    jpeg_quality,
                    h264_bitrate_kbps,
                    prefer_h264,
                    h264_backend,
                    frame_tx,
                    init_tx,
                );
            })
            .map_err(|e| Error::Os(format!("spawn video thread: {e}")))?;

        let stream_info = init_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| Error::Os("video thread n'a pas rapporte StreamInfo".into()))?
            .map_err(Error::Os)?;

        tracing::info!(
            w = stream_info.width,
            h = stream_info.height,
            fps = stream_info.target_fps,
            codec = ?stream_info.codec,
            bitrate_kbps = stream_info.bitrate_kbps,
            "video capture demarree"
        );

        // Annonce le stream a la couche superieure.
        let stream_id = Uuid::new_v4();
        let _ = tx
            .send(VideoMessage::StreamStart {
                stream_id,
                screen_idx,
                codec: stream_info.codec,
                width_px: stream_info.width,
                height_px: stream_info.height,
                target_fps: stream_info.target_fps,
                bitrate_kbps: stream_info.bitrate_kbps,
            })
            .await;

        // Bridge std → tokio.
        let bridge = tokio::task::spawn_blocking(move || {
            let mut seq: u32 = 0;
            while let Ok(frame) = frame_rx.recv() {
                let msg = VideoMessage::StreamFrame {
                    stream_id,
                    seq,
                    ts_us: now_us(),
                    is_keyframe: frame.is_keyframe,
                    fec_group: 0,
                    fec_index: 0,
                    fec_k: 1,
                    fec_n: 1,
                    payload: frame.payload,
                };
                seq = seq.wrapping_add(1);
                if tx.blocking_send(msg).is_err() {
                    tracing::debug!("video bridge: canal ferme");
                    break;
                }
            }
            let _ = tx.blocking_send(VideoMessage::StreamStop { stream_id });
        });
        let bridge = tokio::spawn(async move {
            let _ = bridge.await;
        });

        Ok(VideoHandle {
            stop: stop_tx,
            bridge,
        })
    }
}

struct EncodedFrame {
    payload: Vec<u8>,
    is_keyframe: bool,
}

struct StreamInfo {
    width: u32,
    height: u32,
    target_fps: u32,
    codec: VideoCodec,
    bitrate_kbps: u32,
}

// ===========================================================================
// Thread de capture
// ===========================================================================

#[allow(clippy::too_many_arguments)]
fn run_capture_thread(
    screen_idx: u32,
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    jpeg_quality: u8,
    h264_bitrate_kbps: u32,
    prefer_h264: bool,
    h264_backend: H264Backend,
    frame_tx: std_mpsc::Sender<EncodedFrame>,
    init_tx: std_mpsc::Sender<std::result::Result<StreamInfo, String>>,
) {
    // Selectionne le moniteur.
    let monitor = match select_monitor(screen_idx) {
        Some(m) => m,
        None => {
            let _ = init_tx.send(Err(format!(
                "moniteur d'index {screen_idx} introuvable"
            )));
            return;
        }
    };

    // Determine le codec : tente H.264 si demande, avec le backend choisi.
    // Validation seulement : le vrai encodeur sera créé dans le handler car
    // openh264::Encoder et IMFTransform ne sont pas Send par défaut.
    let h264_cfg = H264Config {
        width: target_width,
        height: target_height,
        target_fps,
        bitrate_kbps: h264_bitrate_kbps,
    };
    let codec = if prefer_h264 {
        let validation = match h264_backend {
            H264Backend::Openh264 => H264Encoder::new(h264_cfg).map(|_| ()),
            H264Backend::MediaFoundation => MfH264Encoder::new(h264_cfg).map(|_| ()),
        };
        match validation {
            Ok(()) => {
                tracing::info!(?h264_backend, "H264 backend validé");
                VideoCodec::H264
            }
            Err(e) => {
                tracing::warn!(error = %e, ?h264_backend, "H264 init echec, fallback MJPEG");
                VideoCodec::Mjpeg
            }
        }
    } else {
        VideoCodec::Mjpeg
    };
    let bitrate_kbps = if codec == VideoCodec::H264 {
        h264_bitrate_kbps
    } else {
        // MJPEG : estimation grossiere.
        (target_width * target_height * 24 * target_fps) / 1_000_000
    };

    let _ = init_tx.send(Ok(StreamInfo {
        width: target_width,
        height: target_height,
        target_fps,
        codec,
        bitrate_kbps,
    }));

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        CaptureFlags {
            target_width,
            target_height,
            target_fps,
            jpeg_quality,
            h264_bitrate_kbps,
            h264_backend,
            codec,
            frame_tx,
        },
    );

    if let Err(e) = FrameEncoderHandler::start(settings) {
        tracing::error!(error = ?e, "windows-capture start echec");
    }
}

fn select_monitor(idx: u32) -> Option<Monitor> {
    if idx == 0 {
        Monitor::primary().ok()
    } else {
        Monitor::enumerate()
            .ok()
            .and_then(|mons| mons.into_iter().nth(idx as usize))
    }
}

// ===========================================================================
// Handler windows-capture
// ===========================================================================

struct CaptureFlags {
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    jpeg_quality: u8,
    h264_bitrate_kbps: u32,
    h264_backend: H264Backend,
    codec: VideoCodec,
    frame_tx: std_mpsc::Sender<EncodedFrame>,
}

/// Wrapper dispatchant entre les deux backends H.264. Non-Send, créé dans le
/// thread handler.
enum AnyH264Encoder {
    Openh264(H264Encoder),
    MediaFoundation(MfH264Encoder),
}

impl AnyH264Encoder {
    fn encode_rgb(&mut self, rgb: &[u8]) -> okvm_core::Result<Vec<u8>> {
        match self {
            Self::Openh264(e) => e.encode_rgb(rgb),
            Self::MediaFoundation(e) => e.encode_rgb(rgb),
        }
    }
    fn force_keyframe(&mut self) {
        match self {
            Self::Openh264(e) => e.force_keyframe(),
            Self::MediaFoundation(e) => e.force_keyframe(),
        }
    }
}

struct FrameEncoderHandler {
    target_width: u32,
    target_height: u32,
    jpeg_quality: u8,
    codec: VideoCodec,
    h264: Option<AnyH264Encoder>,
    /// Compteur pour forcer un keyframe periodique (toutes les 2s).
    frame_count: u32,
    keyframe_every: u32,
    frame_tx: std_mpsc::Sender<EncodedFrame>,
    last_emit: std::time::Instant,
    min_interval: std::time::Duration,
}

impl GraphicsCaptureApiHandler for FrameEncoderHandler {
    type Flags = CaptureFlags;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> std::result::Result<Self, Self::Error> {
        let flags = ctx.flags;
        let interval_ms = (1000 / flags.target_fps.max(1)) as u64;
        let h264_cfg = H264Config {
            width: flags.target_width,
            height: flags.target_height,
            target_fps: flags.target_fps,
            bitrate_kbps: flags.h264_bitrate_kbps,
        };
        let h264 = if flags.codec == VideoCodec::H264 {
            match flags.h264_backend {
                H264Backend::Openh264 => H264Encoder::new(h264_cfg)
                    .ok()
                    .map(AnyH264Encoder::Openh264),
                H264Backend::MediaFoundation => MfH264Encoder::new(h264_cfg)
                    .ok()
                    .map(AnyH264Encoder::MediaFoundation),
            }
        } else {
            None
        };
        Ok(Self {
            target_width: flags.target_width,
            target_height: flags.target_height,
            jpeg_quality: flags.jpeg_quality,
            codec: if h264.is_some() {
                VideoCodec::H264
            } else {
                VideoCodec::Mjpeg
            },
            h264,
            frame_count: 0,
            keyframe_every: flags.target_fps * 2, // toutes les 2s
            frame_tx: flags.frame_tx,
            last_emit: std::time::Instant::now() - std::time::Duration::from_secs(60),
            min_interval: std::time::Duration::from_millis(interval_ms),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _ctrl: InternalCaptureControl,
    ) -> std::result::Result<(), Self::Error> {
        // Throttle simple : on saute les frames si on est trop rapide.
        let now = std::time::Instant::now();
        if now.duration_since(self.last_emit) < self.min_interval {
            return Ok(());
        }
        self.last_emit = now;

        // Recupere le buffer BGRA brut.
        let mut buffer = frame.buffer().map_err(|e| format!("frame.buffer: {e}"))?;
        let src_width = buffer.width();
        let src_height = buffer.height();
        let src_bgra = buffer.as_raw_buffer();

        // Convertit BGRA → RGB (alignement ligne par ligne).
        let mut rgb = Vec::with_capacity((src_width * src_height * 3) as usize);
        let row_stride_bytes = src_bgra.len() / src_height as usize;
        for y in 0..src_height as usize {
            let row_start = y * row_stride_bytes;
            let row = &src_bgra[row_start..row_start + (src_width as usize) * 4];
            for px in row.chunks_exact(4) {
                rgb.push(px[2]);
                rgb.push(px[1]);
                rgb.push(px[0]);
            }
        }

        // Downscale via image crate.
        let img = match image::RgbImage::from_raw(src_width, src_height, rgb) {
            Some(i) => i,
            None => return Ok(()),
        };
        let dyn_img = image::DynamicImage::ImageRgb8(img);
        let resized = if src_width > self.target_width || src_height > self.target_height {
            dyn_img.resize_exact(
                self.target_width,
                self.target_height,
                image::imageops::FilterType::Triangle,
            )
        } else {
            dyn_img
        };
        let rgb_resized = resized.to_rgb8().into_raw();

        // Encode selon le codec.
        let mut is_keyframe = false;
        let payload = match (self.codec, self.h264.as_mut()) {
            (VideoCodec::H264, Some(enc)) => {
                // Force un keyframe periodiquement.
                if self.frame_count % self.keyframe_every == 0 {
                    enc.force_keyframe();
                    is_keyframe = true;
                }
                self.frame_count = self.frame_count.wrapping_add(1);
                match enc.encode_rgb(&rgb_resized) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = %e, "H264 encode echec, frame droppee");
                        return Ok(());
                    }
                }
            }
            _ => {
                // MJPEG : on encode JPEG.
                is_keyframe = true; // chaque JPEG est independant
                let resized_img = image::DynamicImage::ImageRgb8(
                    image::RgbImage::from_raw(self.target_width, self.target_height, rgb_resized)
                        .unwrap_or_else(|| image::RgbImage::new(1, 1)),
                );
                let mut jpeg = Vec::with_capacity(64 * 1024);
                {
                    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                        &mut jpeg,
                        self.jpeg_quality,
                    );
                    if let Err(e) = encoder.encode_image(&resized_img) {
                        tracing::warn!(error = %e, "JPEG encode echec");
                        return Ok(());
                    }
                }
                jpeg
            }
        };

        if self
            .frame_tx
            .send(EncodedFrame {
                payload,
                is_keyframe,
            })
            .is_err()
        {
            return Err("frame_tx ferme".into());
        }
        Ok(())
    }

    fn on_closed(&mut self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
}

fn now_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[allow(dead_code)]
fn _suppress_unused() {
    let _ = std::marker::PhantomData::<(Mutex<()>, Arc<()>)>;
}
