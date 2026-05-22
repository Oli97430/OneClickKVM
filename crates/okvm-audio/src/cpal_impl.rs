//! Implementation cpal de la capture loopback et du playback.
//!
//! Sur Windows, ouvrir un OUTPUT device en `build_input_stream` active
//! automatiquement le flag WASAPI `AUDCLNT_STREAMFLAGS_LOOPBACK` ce qui
//! capture ce qui est joue sur ce device. C'est exactement ce qu'on veut
//! pour partager le son d'une session avec un autre PC.

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use async_trait::async_trait;
use audiopus::{
    coder::{Decoder as OpusDecoder, Encoder as OpusEncoder},
    Application, Channels as OpusChannels, SampleRate as OpusSampleRate,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use uuid::Uuid;

use okvm_core::{AudioCodec, Error, Result};
use okvm_protocol::AudioMessage;

use crate::{AudioCapture, AudioHandle, AudioPlayback};

/// Bitrate Opus cible (bits par seconde). 64 kbps = qualité conversation
/// large bande, ~25x moins que du PCM s16le 48kHz stéréo (1.5 Mbps).
const OPUS_BITRATE_BPS: i32 = 64_000;

/// Implementation cpal de la capture audio loopback.
pub struct CpalCapture;

impl Default for CpalCapture {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl AudioCapture for CpalCapture {
    async fn start(&self, tx: mpsc::Sender<AudioMessage>) -> Result<AudioHandle> {
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        // cpal::Stream n'est pas Send sur Windows → on l'heberge sur un
        // thread OS dedie. Un canal std::sync::mpsc transporte les Vec<i16>
        // (PCM) vers une task tokio qui les emballe en AudioMessage et les
        // pousse sur le tx tokio fourni par l'app.
        let (pcm_tx, pcm_rx) = std_mpsc::channel::<PcmFrame>();
        let (init_tx, init_rx) = std_mpsc::channel::<std::result::Result<StreamInfo, String>>();

        std::thread::Builder::new()
            .name("okvm-audio-capture".into())
            .spawn(move || run_capture_thread(pcm_tx, init_tx))
            .map_err(|e| Error::Os(format!("spawn capture thread: {e}")))?;

        let stream_info = init_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| Error::Os("capture thread n'a pas rapporte StreamInfo".into()))?
            .map_err(Error::Os)?;

        tracing::info!(
            sr = stream_info.sample_rate,
            ch = stream_info.channels,
            "audio capture demarree"
        );

        // Tente d'initialiser un encoder Opus.
        let opus_encoder = build_opus_encoder(stream_info.sample_rate, stream_info.channels);
        let codec = if opus_encoder.is_some() {
            AudioCodec::Opus
        } else {
            AudioCodec::Pcm16
        };
        tracing::info!(?codec, "audio codec choisi");

        // Bridge std → tokio.
        let stream_id = Uuid::new_v4();
        let tx_clone = tx.clone();
        let start_msg = AudioMessage::StreamStart {
            stream_id,
            codec,
            sample_rate_hz: stream_info.sample_rate,
            channels: stream_info.channels,
            frame_size_samples: stream_info.frame_size_samples,
            source_name: stream_info.source_name.clone(),
        };
        let _ = tx_clone.send(start_msg).await;

        let bridge = tokio::task::spawn_blocking(move || {
            let mut seq: u32 = 0;
            let mut encoder = opus_encoder;
            let mut opus_out = vec![0u8; 4000]; // marge confortable pour Opus
            while let Ok(frame) = pcm_rx.recv() {
                let payload = match encoder.as_mut() {
                    Some(enc) => {
                        // Encode via Opus.
                        match enc.encode(&frame.samples, &mut opus_out) {
                            Ok(n) => opus_out[..n].to_vec(),
                            Err(e) => {
                                tracing::warn!(error = %e, "opus encode echec, fallback PCM frame");
                                pcm_to_bytes_le(&frame.samples)
                            }
                        }
                    }
                    None => pcm_to_bytes_le(&frame.samples),
                };
                let msg = AudioMessage::StreamFrame {
                    stream_id,
                    seq,
                    ts_us: now_us(),
                    payload,
                };
                seq = seq.wrapping_add(1);
                if tx.blocking_send(msg).is_err() {
                    tracing::debug!("audio bridge: canal ferme");
                    break;
                }
            }
            let _ = tx.blocking_send(AudioMessage::StreamStop { stream_id });
        });

        let bridge = tokio::spawn(async move {
            let _ = bridge.await;
        });

        // stop_rx pas utilise pour l'instant : le thread cpal s'arrete quand
        // le pcm_tx est ferme. On garde stop_tx pour API symetrique.
        let _ = stop_rx;
        Ok(AudioHandle {
            stop: stop_tx,
            bridge,
        })
    }
}

struct PcmFrame {
    samples: Vec<i16>,
}

#[derive(Debug, Clone)]
struct StreamInfo {
    sample_rate: u32,
    channels: u8,
    frame_size_samples: u32,
    source_name: String,
}

/// Boucle principale du thread cpal : ouvre le device, demarre le stream,
/// attend l'arret (canal pcm_tx ferme).
fn run_capture_thread(
    pcm_tx: std_mpsc::Sender<PcmFrame>,
    init_tx: std_mpsc::Sender<std::result::Result<StreamInfo, String>>,
) {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        let _ = init_tx.send(Err("aucun device de sortie par defaut".into()));
        return;
    };
    let device_name = device.name().unwrap_or_else(|_| "(inconnu)".into());

    let config = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(format!("default_output_config: {e}")));
            return;
        }
    };
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as u8;
    let sample_format = config.sample_format();
    let stream_config = cpal::StreamConfig {
        channels: config.channels(),
        sample_rate: config.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    // Frame logique = 20ms d'audio (cible Opus).
    let frame_size = (sample_rate / 50) * u32::from(channels);

    let _ = init_tx.send(Ok(StreamInfo {
        sample_rate,
        channels,
        frame_size_samples: frame_size,
        source_name: device_name.clone(),
    }));

    let pcm_tx_arc = Arc::new(Mutex::new(Some(pcm_tx)));
    let pcm_tx_for_cb = pcm_tx_arc.clone();
    let buf: Arc<Mutex<Vec<i16>>> =
        Arc::new(Mutex::new(Vec::with_capacity(frame_size as usize * 2)));
    let buf_for_cb = buf.clone();

    let err_fn = |e: cpal::StreamError| {
        tracing::warn!(error = %e, "cpal stream error");
    };

    // build_input_stream sur un OUTPUT device → WASAPI loopback.
    let stream: std::result::Result<Stream, _> = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| {
                pcm_callback_f32(data, frame_size as usize, &buf_for_cb, &pcm_tx_for_cb);
            },
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| {
                pcm_callback_i16(data, frame_size as usize, &buf_for_cb, &pcm_tx_for_cb);
            },
            err_fn,
            None,
        ),
        other => {
            tracing::error!(?other, "format audio non supporte");
            return;
        }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "build_input_stream echoue (loopback indisponible?)");
            return;
        }
    };

    if let Err(e) = stream.play() {
        tracing::error!(error = %e, "stream.play() echoue");
        return;
    }

    // Boucle d'attente. cpal::Stream se ferme automatiquement quand on le drop.
    // On park le thread jusqu'a ce que pcm_tx soit ferme (= caller drop le AudioHandle).
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if pcm_tx_arc.lock().is_none() {
            break;
        }
        // Aucun moyen propre de detecter que le receiver tokio a drop le pcm_rx
        // sans un signal explicite — on accepte que cette boucle survive
        // jusqu'a process exit (cpal::Stream Drop coupe alors le device).
    }
    drop(stream);
}

fn pcm_callback_f32(
    data: &[f32],
    frame_size: usize,
    buf: &Arc<Mutex<Vec<i16>>>,
    pcm_tx: &Arc<Mutex<Option<std_mpsc::Sender<PcmFrame>>>>,
) {
    let mut g = buf.lock();
    g.reserve(data.len());
    for &s in data {
        g.push((s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16);
    }
    flush_frames(&mut g, frame_size, pcm_tx);
}

fn pcm_callback_i16(
    data: &[i16],
    frame_size: usize,
    buf: &Arc<Mutex<Vec<i16>>>,
    pcm_tx: &Arc<Mutex<Option<std_mpsc::Sender<PcmFrame>>>>,
) {
    let mut g = buf.lock();
    g.extend_from_slice(data);
    flush_frames(&mut g, frame_size, pcm_tx);
}

fn flush_frames(
    buf: &mut Vec<i16>,
    frame_size: usize,
    pcm_tx: &Arc<Mutex<Option<std_mpsc::Sender<PcmFrame>>>>,
) {
    while buf.len() >= frame_size {
        let samples: Vec<i16> = buf.drain(..frame_size).collect();
        let g_tx = pcm_tx.lock();
        if let Some(tx) = g_tx.as_ref() {
            if tx.send(PcmFrame { samples }).is_err() {
                // Receiver gone : on disconnect en posant None.
                drop(g_tx);
                *pcm_tx.lock() = None;
                return;
            }
        } else {
            return;
        }
    }
}

/// Construit un encoder Opus si le `(sample_rate, channels)` est supporte.
/// Opus supporte uniquement 8/12/16/24/48 kHz et 1/2 canaux.
fn build_opus_encoder(sample_rate: u32, channels: u8) -> Option<OpusEncoder> {
    let sr = match sample_rate {
        8000 => OpusSampleRate::Hz8000,
        12000 => OpusSampleRate::Hz12000,
        16000 => OpusSampleRate::Hz16000,
        24000 => OpusSampleRate::Hz24000,
        48000 => OpusSampleRate::Hz48000,
        _ => {
            tracing::info!(
                sr = sample_rate,
                "sample rate non supporte par Opus, fallback PCM"
            );
            return None;
        }
    };
    let ch = match channels {
        1 => OpusChannels::Mono,
        2 => OpusChannels::Stereo,
        _ => {
            tracing::info!(
                ch = channels,
                "channels non supporte par Opus, fallback PCM"
            );
            return None;
        }
    };
    match OpusEncoder::new(sr, ch, Application::Audio) {
        Ok(mut enc) => {
            let _ = enc.set_bitrate(audiopus::Bitrate::BitsPerSecond(OPUS_BITRATE_BPS));
            tracing::info!(
                sr = sample_rate,
                ch = channels,
                bitrate = OPUS_BITRATE_BPS,
                "Opus encoder pret"
            );
            Some(enc)
        }
        Err(e) => {
            tracing::warn!(error = %e, "OpusEncoder::new echec, fallback PCM");
            None
        }
    }
}

/// Construit un decoder Opus si le `(sample_rate, channels)` est supporte.
fn build_opus_decoder(sample_rate: u32, channels: u8) -> Option<OpusDecoder> {
    let sr = match sample_rate {
        8000 => OpusSampleRate::Hz8000,
        12000 => OpusSampleRate::Hz12000,
        16000 => OpusSampleRate::Hz16000,
        24000 => OpusSampleRate::Hz24000,
        48000 => OpusSampleRate::Hz48000,
        _ => return None,
    };
    let ch = match channels {
        1 => OpusChannels::Mono,
        2 => OpusChannels::Stereo,
        _ => return None,
    };
    OpusDecoder::new(sr, ch).ok()
}

fn pcm_to_bytes_le(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn bytes_le_to_pcm(bytes: &[u8]) -> Vec<i16> {
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    out
}

fn now_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// ===========================================================================
// Playback
// ===========================================================================

/// Implementation cpal du playback audio.
pub struct CpalPlayback {
    inner: Arc<Mutex<PlaybackInner>>,
}

struct PlaybackInner {
    /// Stream actif (None si pas encore demarre).
    _stream_thread: Option<std::thread::JoinHandle<()>>,
    /// Ring buffer protege par mutex : alimente par `push`, vide par le callback cpal.
    /// Cap raisonnable pour eviter de gonfler indefiniment si le pair envoie plus
    /// vite que la sortie ne peut consommer.
    buffer: Arc<Mutex<RingBuffer>>,
    /// Source: stream_id de l'AudioMessage::StreamStart deja recu.
    pub current_stream: Option<Uuid>,
    /// Codec annonce dans StreamStart (None tant qu'on n'en a pas reçu).
    pub current_codec: Option<AudioCodec>,
    /// Nombre de canaux annonce dans StreamStart.
    pub current_channels: u8,
    /// Decoder Opus si codec == Opus.
    pub opus_decoder: Option<OpusDecoder>,
    /// Buffer temporaire pour decode Opus.
    pub opus_pcm_buf: Vec<i16>,
}

struct RingBuffer {
    data: Vec<i16>,
    /// Cap max ; au-dela, on drop les vieux samples.
    cap: usize,
}

impl RingBuffer {
    fn new(cap: usize) -> Self {
        Self {
            data: Vec::with_capacity(cap),
            cap,
        }
    }

    fn push(&mut self, samples: &[i16]) {
        // Si on deborderait, drop le plus vieux.
        let total = self.data.len() + samples.len();
        if total > self.cap {
            let excess = total - self.cap;
            let drop_n = excess.min(self.data.len());
            self.data.drain(..drop_n);
        }
        self.data.extend_from_slice(samples);
    }

    fn pop(&mut self, n: usize) -> Vec<i16> {
        let take = n.min(self.data.len());
        self.data.drain(..take).collect()
    }
}

impl Default for CpalPlayback {
    fn default() -> Self {
        Self::new()
    }
}

impl CpalPlayback {
    /// Cree un playback prêt à recevoir des AudioMessage via `push`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PlaybackInner {
                _stream_thread: None,
                buffer: Arc::new(Mutex::new(RingBuffer::new(48_000 * 2 * 2))), // 2s de stereo 48k
                current_stream: None,
                current_codec: None,
                current_channels: 2,
                opus_decoder: None,
                opus_pcm_buf: vec![0i16; 48_000 / 50 * 2 * 6], // marge x6 pour FEC/concealment
            })),
        }
    }

    fn ensure_stream(&self, sample_rate: u32, channels: u8) -> Result<()> {
        let mut g = self.inner.lock();
        if g._stream_thread.is_some() {
            return Ok(());
        }
        let buffer = g.buffer.clone();
        let handle = std::thread::Builder::new()
            .name("okvm-audio-playback".into())
            .spawn(move || run_playback_thread(sample_rate, channels, buffer))
            .map_err(|e| Error::Os(format!("spawn playback thread: {e}")))?;
        g._stream_thread = Some(handle);
        Ok(())
    }
}

#[async_trait]
impl AudioPlayback for CpalPlayback {
    async fn push(&self, msg: AudioMessage) -> Result<()> {
        match msg {
            AudioMessage::StreamStart {
                stream_id,
                sample_rate_hz,
                channels,
                codec,
                ..
            } => {
                self.ensure_stream(sample_rate_hz, channels)?;
                let mut g = self.inner.lock();
                g.current_stream = Some(stream_id);
                g.current_codec = Some(codec);
                g.current_channels = channels;
                g.opus_decoder = if codec == AudioCodec::Opus {
                    build_opus_decoder(sample_rate_hz, channels)
                } else {
                    None
                };
                tracing::info!(
                    ?codec,
                    sr = sample_rate_hz,
                    ch = channels,
                    "playback stream init"
                );
                Ok(())
            }
            AudioMessage::StreamFrame { payload, .. } => {
                let pcm = {
                    let mut g = self.inner.lock();
                    let channels = g.current_channels as usize;
                    let codec = g.current_codec;
                    if codec == Some(AudioCodec::Opus) && g.opus_decoder.is_some() {
                        let cap = g.opus_pcm_buf.len();
                        let mut out = vec![0i16; cap];
                        let dec = g.opus_decoder.as_mut().unwrap();
                        let pkt = audiopus::packet::Packet::try_from(payload.as_slice())
                            .map_err(|e| Error::other(format!("opus packet: {e}")))?;
                        let mut_sig = audiopus::MutSignals::try_from(&mut out[..])
                            .map_err(|e| Error::other(format!("opus mut_signals: {e}")))?;
                        match dec.decode(Some(pkt), mut_sig, false) {
                            Ok(samples_per_channel) => {
                                let total = samples_per_channel * channels;
                                out.truncate(total);
                                out
                            }
                            Err(e) => {
                                tracing::debug!(error = %e, "opus decode echec, frame droppee");
                                Vec::new()
                            }
                        }
                    } else {
                        bytes_le_to_pcm(&payload)
                    }
                };
                if !pcm.is_empty() {
                    let buf = self.inner.lock().buffer.clone();
                    buf.lock().push(&pcm);
                }
                Ok(())
            }
            AudioMessage::StreamStop { .. } => {
                let mut g = self.inner.lock();
                g.current_stream = None;
                g.opus_decoder = None;
                g.current_codec = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn run_playback_thread(sample_rate: u32, channels: u8, buffer: Arc<Mutex<RingBuffer>>) {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        tracing::error!("playback: aucun device de sortie");
        return;
    };
    let config = cpal::StreamConfig {
        channels: u16::from(channels),
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let err_fn = |e: cpal::StreamError| {
        tracing::warn!(error = %e, "cpal playback stream error");
    };

    // On vise un output en f32 (le plus courant sur Windows WASAPI).
    let buf_for_cb = buffer.clone();
    let stream = match device.build_output_stream(
        &config,
        move |out: &mut [f32], _| {
            let needed = out.len();
            let pcm = buf_for_cb.lock().pop(needed);
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = pcm
                    .get(i)
                    .copied()
                    .map(|s| f32::from(s) / f32::from(i16::MAX))
                    .unwrap_or(0.0);
            }
        },
        err_fn,
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "build_output_stream echoue");
            return;
        }
    };
    if let Err(e) = stream.play() {
        tracing::error!(error = %e, "playback.play() echoue");
        return;
    }
    tracing::info!(sr = sample_rate, ch = channels, "audio playback demarre");

    // Park le thread tant qu'on a quelque chose a jouer ; en pratique on
    // tourne pour la vie de l'app.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_round_trip() {
        let samples = vec![0i16, 1, -1, i16::MIN, i16::MAX, 1000, -1000];
        let bytes = pcm_to_bytes_le(&samples);
        let back = bytes_le_to_pcm(&bytes);
        assert_eq!(samples, back);
    }

    #[test]
    fn ring_buffer_caps() {
        let mut rb = RingBuffer::new(4);
        rb.push(&[1, 2, 3]);
        rb.push(&[4, 5, 6]);
        // Doit garder les 4 derniers.
        let out = rb.pop(10);
        assert_eq!(out, vec![3, 4, 5, 6]);
    }

    #[test]
    fn ring_buffer_partial_pop() {
        let mut rb = RingBuffer::new(10);
        rb.push(&[1, 2, 3, 4, 5]);
        assert_eq!(rb.pop(3), vec![1, 2, 3]);
        assert_eq!(rb.pop(10), vec![4, 5]);
        assert_eq!(rb.pop(10), Vec::<i16>::new());
    }
}
