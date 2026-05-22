//! Encodage / decodage H.264 via `openh264` (implementation Cisco reference).
//!
//! Pas hardware-accelere (CPU only) ; suffisant pour 720p15 a 1.5-2 Mbps
//! sur un CPU moderne. Pour du 1080p60, V3 utilisera Media Foundation
//! hardware (NVENC / AMF / QSV).

use okvm_core::{Error, Result};

use openh264::{
    encoder::{BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType},
    formats::{RgbSliceU8, YUVBuffer, YUVSource},
    OpenH264API,
};

/// Configuration d'un encoder H.264.
#[derive(Debug, Clone, Copy)]
pub struct H264Config {
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// Frame rate cible (Hz).
    pub target_fps: u32,
    /// Bitrate cible (kbps).
    pub bitrate_kbps: u32,
}

/// Encoder H.264 sur des buffers RGB.
pub struct H264Encoder {
    cfg: H264Config,
    encoder: Encoder,
}

impl H264Encoder {
    /// Cree un encoder H.264 configure.
    pub fn new(cfg: H264Config) -> Result<Self> {
        let enc_cfg = EncoderConfig::new()
            .bitrate(BitRate::from_bps(cfg.bitrate_kbps * 1000))
            .max_frame_rate(FrameRate::from_hz(cfg.target_fps as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            .usage_type(UsageType::ScreenContentRealTime);

        let api = OpenH264API::from_source();
        let encoder = Encoder::with_api_config(api, enc_cfg)
            .map_err(|e| Error::other(format!("H264 encoder init: {e}")))?;
        Ok(Self { cfg, encoder })
    }

    /// Renvoie la configuration utilisee a la creation.
    #[must_use]
    pub fn config(&self) -> H264Config {
        self.cfg
    }

    /// Encode un buffer RGB (3 octets par pixel, top-down) → NAL units H.264.
    /// Retourne les bytes du bitstream concatenes (Annex-B avec start codes).
    pub fn encode_rgb(&mut self, rgb: &[u8]) -> Result<Vec<u8>> {
        let expected = (self.cfg.width as usize) * (self.cfg.height as usize) * 3;
        if rgb.len() != expected {
            return Err(Error::other(format!(
                "rgb size mismatch: {} vs attendu {}",
                rgb.len(),
                expected
            )));
        }
        let rgb_source = RgbSliceU8::new(rgb, (self.cfg.width as usize, self.cfg.height as usize));
        let yuv = YUVBuffer::from_rgb_source(rgb_source);
        let bitstream = self
            .encoder
            .encode(&yuv)
            .map_err(|e| Error::other(format!("H264 encode: {e}")))?;
        Ok(bitstream.to_vec())
    }

    /// Force un keyframe au prochain encode.
    pub fn force_keyframe(&mut self) {
        self.encoder.force_intra_frame();
    }
}

/// Decoder H.264 → RGB.
pub struct H264Decoder {
    decoder: openh264::decoder::Decoder,
}

impl H264Decoder {
    /// Cree un decoder H.264.
    pub fn new() -> Result<Self> {
        let api = OpenH264API::from_source();
        let decoder = openh264::decoder::Decoder::with_api_config(api, Default::default())
            .map_err(|e| Error::other(format!("H264 decoder init: {e}")))?;
        Ok(Self { decoder })
    }

    /// Decode un paquet H.264. Retourne `Some((width, height, rgb_bytes))` si
    /// une frame complete est sortie, `None` sinon.
    ///
    /// **Note** : la premiere frame keyframe peut necessiter plusieurs appels
    /// successifs avant de sortir une frame decodee (delay typique 1-2 frames).
    pub fn decode(&mut self, nal: &[u8]) -> Result<Option<(u32, u32, Vec<u8>)>> {
        match self.decoder.decode(nal) {
            Ok(Some(yuv)) => {
                let (w, h) = yuv.dimensions();
                let mut rgb = vec![0u8; w * h * 3];
                yuv.write_rgb8(&mut rgb);
                Ok(Some((w as u32, h as u32, rgb)))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(Error::other(format!("H264 decode: {e}"))),
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
        let _ = H264Encoder::new(cfg).expect("encoder init");
    }

    #[test]
    fn decoder_init() {
        let _ = H264Decoder::new().expect("decoder init");
    }

    #[test]
    fn round_trip_solid_color() {
        let cfg = H264Config {
            width: 320,
            height: 240,
            target_fps: 15,
            bitrate_kbps: 500,
        };
        let mut enc = H264Encoder::new(cfg).unwrap();
        let mut dec = H264Decoder::new().unwrap();

        // Image rouge unie 320×240.
        let rgb: Vec<u8> = (0..320usize * 240).flat_map(|_| [200u8, 30, 30]).collect();

        // openh264 peut prendre plusieurs frames avant qu'une frame sorte du decoder
        // (pour les SPS/PPS, B-frames, etc.) ; on encode plusieurs fois.
        let mut decoded_any = false;
        for _ in 0..5 {
            let bs = enc.encode_rgb(&rgb).unwrap();
            if bs.is_empty() {
                continue;
            }
            if let Some((w, h, _decoded)) = dec.decode(&bs).unwrap() {
                assert_eq!(w, 320);
                assert_eq!(h, 240);
                decoded_any = true;
                break;
            }
        }
        assert!(decoded_any, "aucune frame decodee apres 5 frames");
    }
}
