//! Format wire des shards UDP.
//!
//! Chaque datagramme UDP contient :
//!
//! ```text
//! [u64 BE  seq]        # compteur AEAD du frame (unique par sens)
//! [u8       data_k]    # K = nombre de shards de données
//! [u8       parity_m]  # M = nombre de shards de parité
//! [u8       index]     # 0..N-1 (où N = K+M)
//! [u32 BE   plain_len] # taille totale du ciphertext avant padding/sharding
//! [u16 BE   shard_len] # taille de ce shard (== shard_size constant pour ce frame)
//! [bytes…   payload]   # le shard lui-même
//! ```
//!
//! Total header = 8 + 1 + 1 + 1 + 4 + 2 = **17 octets**.
//!
//! Avec un MTU de 1500 (IPv4) et 28 octets IPv4+UDP, on a ~1455 octets utiles
//! par shard payload.

use thiserror::Error;

/// Taille fixe de l'en-tête d'un shard UDP.
pub const HEADER_LEN: usize = 17;

/// Taille max recommandée du payload (sous MTU IPv4 + IPv6 sécurisée).
/// MTU Ethernet = 1500, IPv6+UDP = 48 octets, donc 1452 - 17 (header) ≈ 1435.
/// On laisse une petite marge.
pub const MAX_SHARD_PAYLOAD: usize = 1400;

/// En-tête d'un shard UDP, parsé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardHeader {
    /// Compteur AEAD du frame.
    pub seq: u64,
    /// Nombre de shards de données (K).
    pub data_shards: u8,
    /// Nombre de shards de parité (M).
    pub parity_shards: u8,
    /// Index de ce shard dans [0, K+M).
    pub index: u8,
    /// Taille du ciphertext original (avant padding).
    pub plain_len: u32,
    /// Taille de ce shard (constant pour tous les shards d'un même frame).
    pub shard_len: u16,
}

/// Erreurs de parsing d'un shard.
#[derive(Debug, Error)]
pub enum FramingError {
    /// Datagramme trop court pour contenir l'en-tête.
    #[error("datagramme trop court: {0} < {HEADER_LEN}")]
    TooShort(usize),
    /// `index >= K+M` ou `K == 0`.
    #[error("en-tête invalide: K={k} M={m} index={index}")]
    BadHeader {
        /// `data_shards`.
        k: u8,
        /// `parity_shards`.
        m: u8,
        /// `index`.
        index: u8,
    },
    /// `shard_len` annoncé ne correspond pas à la taille restante du datagramme.
    #[error("shard_len incohérent: annoncé {announced}, restant {actual}")]
    LenMismatch {
        /// Taille déclarée dans le header.
        announced: usize,
        /// Taille réelle du payload après le header.
        actual: usize,
    },
}

impl ShardHeader {
    /// Encode l'en-tête dans `out` (qui doit être ≥ [`HEADER_LEN`]) et retourne
    /// le nombre d'octets écrits.
    #[must_use]
    pub fn encode(self, out: &mut [u8; HEADER_LEN]) -> usize {
        out[0..8].copy_from_slice(&self.seq.to_be_bytes());
        out[8] = self.data_shards;
        out[9] = self.parity_shards;
        out[10] = self.index;
        out[11..15].copy_from_slice(&self.plain_len.to_be_bytes());
        out[15..17].copy_from_slice(&self.shard_len.to_be_bytes());
        HEADER_LEN
    }

    /// Parse un datagramme UDP et retourne `(header, payload)`.
    ///
    /// # Erreurs
    /// Voir [`FramingError`].
    pub fn parse(datagram: &[u8]) -> Result<(Self, &[u8]), FramingError> {
        if datagram.len() < HEADER_LEN {
            return Err(FramingError::TooShort(datagram.len()));
        }
        let mut seq_bytes = [0u8; 8];
        seq_bytes.copy_from_slice(&datagram[0..8]);
        let seq = u64::from_be_bytes(seq_bytes);
        let k = datagram[8];
        let m = datagram[9];
        let index = datagram[10];
        if k == 0 || index >= k.saturating_add(m) {
            return Err(FramingError::BadHeader { k, m, index });
        }
        let mut plain_bytes = [0u8; 4];
        plain_bytes.copy_from_slice(&datagram[11..15]);
        let plain_len = u32::from_be_bytes(plain_bytes);
        let mut shard_bytes = [0u8; 2];
        shard_bytes.copy_from_slice(&datagram[15..17]);
        let shard_len = u16::from_be_bytes(shard_bytes);
        let payload = &datagram[HEADER_LEN..];
        if payload.len() != shard_len as usize {
            return Err(FramingError::LenMismatch {
                announced: shard_len as usize,
                actual: payload.len(),
            });
        }
        Ok((
            Self {
                seq,
                data_shards: k,
                parity_shards: m,
                index,
                plain_len,
                shard_len,
            },
            payload,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let h = ShardHeader {
            seq: 0x0123_4567_89AB_CDEF,
            data_shards: 4,
            parity_shards: 2,
            index: 3,
            plain_len: 8192,
            shard_len: 2048,
        };
        let mut buf = [0u8; HEADER_LEN];
        let _ = h.encode(&mut buf);
        // Construire un faux datagramme : header + payload de la taille déclarée.
        let mut dg = buf.to_vec();
        dg.extend_from_slice(&vec![0xAA_u8; 2048]);
        let (parsed, payload) = ShardHeader::parse(&dg).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(payload.len(), 2048);
    }

    #[test]
    fn too_short_rejected() {
        let dg = [0u8; 5];
        assert!(matches!(
            ShardHeader::parse(&dg).unwrap_err(),
            FramingError::TooShort(5)
        ));
    }

    #[test]
    fn bad_header_rejected() {
        let h = ShardHeader {
            seq: 1,
            data_shards: 4,
            parity_shards: 2,
            index: 10, // > K+M=6
            plain_len: 100,
            shard_len: 25,
        };
        let mut buf = [0u8; HEADER_LEN];
        let _ = h.encode(&mut buf);
        let mut dg = buf.to_vec();
        dg.extend_from_slice(&[0u8; 25]);
        assert!(matches!(
            ShardHeader::parse(&dg).unwrap_err(),
            FramingError::BadHeader { .. }
        ));
    }

    #[test]
    fn len_mismatch_rejected() {
        let h = ShardHeader {
            seq: 1,
            data_shards: 4,
            parity_shards: 2,
            index: 0,
            plain_len: 100,
            shard_len: 50, // annoncé 50
        };
        let mut buf = [0u8; HEADER_LEN];
        let _ = h.encode(&mut buf);
        let mut dg = buf.to_vec();
        dg.extend_from_slice(&[0u8; 30]); // mais on n'a que 30 octets
        assert!(matches!(
            ShardHeader::parse(&dg).unwrap_err(),
            FramingError::LenMismatch { .. }
        ));
    }
}
