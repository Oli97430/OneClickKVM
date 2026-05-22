//! Codec `tokio_util::codec` qui extrait les frames length-prefixed.
//!
//! Une frame sur le wire = `[total_len: u32 BE][total_len octets]`. Ce codec
//! lit le prefixe puis attend `total_len` octets supplementaires. Une fois
//! reuni, il rend le `BytesMut` du payload **post-prefixe**. Le decryptage
//! AEAD lui-meme est laisse a [`okvm_protocol::frame::decode_tcp_frame`].

use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;
use tokio_util::codec::{Decoder, Encoder};

use okvm_protocol::MAX_FRAME_BYTES;

/// Taille maximale acceptee pour le prefixe length (= `MAX_FRAME_BYTES`).
pub const MAX_LENGTH_PREFIX: usize = MAX_FRAME_BYTES;

/// Erreurs du codec.
#[derive(Debug, Error)]
pub enum CodecError {
    /// Frame plus grande que le plafond [`MAX_LENGTH_PREFIX`].
    #[error("frame trop grande: annonce {0} octets, max {1}")]
    TooLarge(usize, usize),
    /// I/O sous-jacente.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Codec length-prefixed pour les frames `OneClick` KVM.
#[derive(Debug, Default)]
pub struct FrameCodec {
    /// Etat : `Some(len)` si on a lu le prefixe et qu'on attend `len` octets.
    expected: Option<usize>,
}

impl FrameCodec {
    /// Cree un codec vierge.
    #[must_use]
    pub fn new() -> Self {
        Self { expected: None }
    }
}

impl Decoder for FrameCodec {
    type Item = BytesMut;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match self.expected {
                None => {
                    if src.len() < 4 {
                        return Ok(None);
                    }
                    let mut buf = [0u8; 4];
                    buf.copy_from_slice(&src[..4]);
                    src.advance(4);
                    let len = u32::from_be_bytes(buf) as usize;
                    if len > MAX_LENGTH_PREFIX {
                        return Err(CodecError::TooLarge(len, MAX_LENGTH_PREFIX));
                    }
                    self.expected = Some(len);
                }
                Some(len) => {
                    if src.len() < len {
                        // Pre-reserve pour eviter realloc.
                        src.reserve(len - src.len());
                        return Ok(None);
                    }
                    let frame = src.split_to(len);
                    self.expected = None;
                    return Ok(Some(frame));
                }
            }
        }
    }
}

impl Encoder<&[u8]> for FrameCodec {
    type Error = CodecError;

    /// Encode le payload comme `[u32 BE len][payload]`.
    fn encode(&mut self, item: &[u8], dst: &mut BytesMut) -> Result<(), Self::Error> {
        if item.len() > MAX_LENGTH_PREFIX {
            return Err(CodecError::TooLarge(item.len(), MAX_LENGTH_PREFIX));
        }
        dst.reserve(4 + item.len());
        dst.put_u32(item.len() as u32);
        dst.put_slice(item);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_decode_round_trip() {
        let mut c = FrameCodec::new();
        let mut buf = BytesMut::new();
        c.encode(&[1, 2, 3, 4, 5][..], &mut buf).unwrap();
        let out = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&out[..], &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn partial_decode_yields_none_then_full() {
        let mut c = FrameCodec::new();
        let mut buf = BytesMut::new();
        // ecrit prefixe partiel
        buf.put_u32(5);
        buf.put_slice(&[10, 20]);
        assert!(c.decode(&mut buf).unwrap().is_none());
        buf.put_slice(&[30, 40, 50]);
        let out = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&out[..], &[10, 20, 30, 40, 50]);
    }

    #[test]
    fn rejects_oversized() {
        let mut c = FrameCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u32(u32::MAX);
        let err = c.decode(&mut buf).unwrap_err();
        assert!(matches!(err, CodecError::TooLarge(_, _)));
    }
}
