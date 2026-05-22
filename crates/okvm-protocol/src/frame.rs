//! Framing binaire `[total_len: u32 BE][channel: u8][nonce_counter: u64 BE][AEAD payload + tag]`.
//!
//! Voir `docs/PROTOCOL.md` §3.

use thiserror::Error;

use okvm_crypto::{AeadSession, AEAD_TAG_SIZE};

use crate::consts::{Channel, MAX_FRAME_BYTES};

/// Taille de l'en-tête de frame TCP **après** `total_len` :
/// `channel (1) + nonce_counter (8)` = 9 octets.
///
/// `total_len` lui-même (4 octets) précède l'en-tête mais n'est **pas** inclus
/// dans son décompte (idem que pour WebSocket et la plupart des protocoles
/// length-prefixed).
pub const FRAME_HEADER_SIZE: usize = 1 + 8;

/// Erreurs de framing.
#[derive(Debug, Error)]
pub enum FrameError {
    /// Frame déclarée plus grande que [`MAX_FRAME_BYTES`].
    #[error("frame trop grande: {0} octets (max {1})")]
    TooLarge(usize, usize),
    /// Canal inconnu.
    #[error("canal inconnu: {0}")]
    UnknownChannel(u8),
    /// Buffer trop court pour contenir une frame valide.
    #[error("frame tronquée: {got} octets, attendu au moins {need}")]
    Truncated {
        /// Taille reçue.
        got: usize,
        /// Taille minimum requise.
        need: usize,
    },
    /// Échec de déchiffrement AEAD (tag, rejeu, AAD).
    #[error("AEAD decrypt failed (tampering or replay)")]
    AeadDecrypt,
    /// Échec de chiffrement AEAD (improbable).
    #[error("AEAD encrypt failed")]
    AeadEncrypt,
}

/// En-tête extrait d'une frame, hors AEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Canal logique.
    pub channel: Channel,
    /// Compteur de nonce (= seq AEAD).
    pub nonce_counter: u64,
}

impl FrameHeader {
    /// AAD = `channel || nonce_counter` (9 octets) — utilisé par l'AEAD.
    #[must_use]
    pub fn aad(self) -> [u8; FRAME_HEADER_SIZE] {
        let mut out = [0u8; FRAME_HEADER_SIZE];
        out[0] = self.channel.as_u8();
        out[1..].copy_from_slice(&self.nonce_counter.to_be_bytes());
        out
    }
}

/// Encode une frame TCP **prête à émettre** :
/// `total_len (BE u32) || channel (u8) || nonce_counter (BE u64) || ciphertext+tag`.
///
/// `plaintext` est chiffré par `session` ; l'AAD est l'en-tête (9 octets).
///
/// # Erreur
/// - [`FrameError::AeadEncrypt`] si le chiffrement échoue.
/// - [`FrameError::TooLarge`] si la frame finale dépasse `MAX_FRAME_BYTES`.
pub fn encode_tcp_frame(
    session: &mut AeadSession,
    channel: Channel,
    plaintext: &[u8],
) -> Result<Vec<u8>, FrameError> {
    // 1. Prépare un AAD provisoire avec un seq placeholder pour mesurer la taille.
    //    En vrai on demande directement le seq via `peek_send_seq`.
    let seq = session.peek_send_seq();
    let header = FrameHeader {
        channel,
        nonce_counter: seq,
    };
    let aad = header.aad();

    let (used_seq, ct) = session
        .seal(&aad, plaintext)
        .map_err(|_| FrameError::AeadEncrypt)?;
    debug_assert_eq!(used_seq, seq);

    let payload_len = FRAME_HEADER_SIZE + ct.len();
    if payload_len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(payload_len, MAX_FRAME_BYTES));
    }

    let mut out = Vec::with_capacity(4 + payload_len);
    out.extend_from_slice(&(payload_len as u32).to_be_bytes());
    out.push(channel.as_u8());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Décode une frame TCP **complète** (header + payload chiffré).
///
/// `bytes` doit pointer **après** les 4 octets de `total_len`, c'est-à-dire
/// le slice de longueur `total_len` que l'utilisateur vient de lire depuis
/// le socket après avoir interprété le préfixe.
///
/// Renvoie `(header, plaintext)` en cas de succès.
///
/// # Erreur
/// - [`FrameError::Truncated`] si la frame est trop courte pour contenir au moins
///   l'en-tête + un tag AEAD.
/// - [`FrameError::UnknownChannel`] si le `channel` ne correspond à rien.
/// - [`FrameError::AeadDecrypt`] si l'authentification échoue.
pub fn decode_tcp_frame(
    session: &mut AeadSession,
    bytes: &[u8],
) -> Result<(FrameHeader, Vec<u8>), FrameError> {
    let need = FRAME_HEADER_SIZE + AEAD_TAG_SIZE;
    if bytes.len() < need {
        return Err(FrameError::Truncated {
            got: bytes.len(),
            need,
        });
    }
    let channel = Channel::from_u8(bytes[0]).ok_or(FrameError::UnknownChannel(bytes[0]))?;
    let nonce_counter = u64::from_be_bytes(bytes[1..9].try_into().unwrap());
    let header = FrameHeader {
        channel,
        nonce_counter,
    };
    let aad = header.aad();
    let ciphertext = &bytes[FRAME_HEADER_SIZE..];
    let pt = session
        .open(nonce_counter, &aad, ciphertext)
        .map_err(|_| FrameError::AeadDecrypt)?;
    Ok((header, pt))
}

/// Lit `total_len` depuis un préfixe de 4 octets et plafonne par [`MAX_FRAME_BYTES`].
///
/// # Erreur
/// - [`FrameError::TooLarge`] si la longueur annoncée dépasse le plafond.
#[must_use]
pub fn read_length_prefix(prefix: [u8; 4]) -> Result<usize, FrameError> {
    let n = u32::from_be_bytes(prefix) as usize;
    if n > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(n, MAX_FRAME_BYTES));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use okvm_crypto::AeadKey;

    fn pair() -> (AeadSession, AeadSession) {
        let k = AeadKey::from_bytes([7u8; 32]);
        (AeadSession::new(&k, 0), AeadSession::new(&k, 0))
    }

    #[test]
    fn round_trip_simple() {
        let (mut snd, mut rcv) = pair();
        let frame = encode_tcp_frame(&mut snd, Channel::Input, b"hello world").unwrap();
        // total_len est dans les 4 premiers octets
        let len = read_length_prefix(frame[..4].try_into().unwrap()).unwrap();
        assert_eq!(len, frame.len() - 4);
        let (hdr, pt) = decode_tcp_frame(&mut rcv, &frame[4..]).unwrap();
        assert_eq!(hdr.channel, Channel::Input);
        assert_eq!(hdr.nonce_counter, 0);
        assert_eq!(pt, b"hello world");
    }

    #[test]
    fn replay_detected() {
        let (mut snd, mut rcv) = pair();
        let frame = encode_tcp_frame(&mut snd, Channel::Ctrl, b"ping").unwrap();
        let _ = decode_tcp_frame(&mut rcv, &frame[4..]).unwrap();
        assert!(matches!(
            decode_tcp_frame(&mut rcv, &frame[4..]),
            Err(FrameError::AeadDecrypt)
        ));
    }

    #[test]
    fn channel_mismatch_breaks_aad() {
        let (mut snd, mut rcv) = pair();
        let mut frame = encode_tcp_frame(&mut snd, Channel::Input, b"hi").unwrap();
        // patch le canal à Ctrl côté wire
        frame[4] = Channel::Ctrl.as_u8();
        assert!(matches!(
            decode_tcp_frame(&mut rcv, &frame[4..]),
            Err(FrameError::AeadDecrypt)
        ));
    }

    #[test]
    fn truncated_rejected() {
        let (mut snd, mut rcv) = pair();
        let frame = encode_tcp_frame(&mut snd, Channel::Input, b"hi").unwrap();
        let short = &frame[4..4 + FRAME_HEADER_SIZE]; // pas de tag
        assert!(matches!(
            decode_tcp_frame(&mut rcv, short),
            Err(FrameError::Truncated { .. })
        ));
    }

    #[test]
    fn length_too_large() {
        let prefix = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
        assert!(matches!(
            read_length_prefix(prefix),
            Err(FrameError::TooLarge(_, _))
        ));
    }
}
