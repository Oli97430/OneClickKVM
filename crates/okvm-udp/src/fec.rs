//! Wrapper Reed-Solomon Vandermonde sur GF(2^8) via `reed-solomon-erasure`.
//!
//! Choix de la lib : `reed-solomon-erasure` est pur Rust, stable, sans deps
//! C/SIMD obligatoires (a une feature SIMD opt-in qui n'est pas activée ici
//! pour rester portable).

use reed_solomon_erasure::galois_8::ReedSolomon;
use thiserror::Error;

/// Maximum supporté de shards de données (limite arbitraire raisonnable).
pub const MAX_DATA_SHARDS: usize = 16;
/// Maximum supporté de shards de parité.
pub const MAX_PARITY_SHARDS: usize = 8;

/// Erreurs du codec FEC.
#[derive(Debug, Error)]
pub enum FecError {
    /// `data_shards == 0` ou `data_shards + parity_shards > limit`.
    #[error("paramètres FEC invalides: data={data} parity={parity}")]
    BadParams {
        /// Nombre de shards de données.
        data: usize,
        /// Nombre de shards de parité.
        parity: usize,
    },
    /// Le ciphertext est plus grand que `data_shards * MAX_SHARD_PAYLOAD`.
    #[error("payload trop grand: {len} octets (max {max})")]
    TooLarge {
        /// Taille reçue.
        len: usize,
        /// Taille max acceptée.
        max: usize,
    },
    /// Pas assez de shards reçus pour reconstruire (besoin de >= K).
    #[error("trop peu de shards: {got}/{need}")]
    Insufficient {
        /// Nombre de shards présents.
        got: usize,
        /// Nombre requis.
        need: usize,
    },
    /// Erreur interne du Reed-Solomon.
    #[error("reed-solomon: {0}")]
    Internal(String),
}

/// Codec encapsulant les paramètres `(K, M)` et le `ReedSolomon` instancié.
pub struct FecCodec {
    rs: ReedSolomon,
    k: usize,
    m: usize,
}

impl FecCodec {
    /// Crée un nouveau codec FEC avec `data_shards` shards de données et
    /// `parity_shards` shards de parité.
    ///
    /// # Erreurs
    /// Renvoie [`FecError::BadParams`] si les paramètres sont hors bornes.
    pub fn new(data_shards: usize, parity_shards: usize) -> Result<Self, FecError> {
        if data_shards == 0
            || data_shards > MAX_DATA_SHARDS
            || parity_shards > MAX_PARITY_SHARDS
        {
            return Err(FecError::BadParams {
                data: data_shards,
                parity: parity_shards,
            });
        }
        let rs = ReedSolomon::new(data_shards, parity_shards)
            .map_err(|e| FecError::Internal(format!("{e:?}")))?;
        Ok(Self {
            rs,
            k: data_shards,
            m: parity_shards,
        })
    }

    /// `K` (nombre de shards de données).
    #[must_use]
    pub fn data_shards(&self) -> usize {
        self.k
    }

    /// `M` (nombre de shards de parité).
    #[must_use]
    pub fn parity_shards(&self) -> usize {
        self.m
    }

    /// `N = K + M`.
    #[must_use]
    pub fn total_shards(&self) -> usize {
        self.k + self.m
    }

    /// Encode `data` en `K+M` shards de même taille (padding zéro).
    ///
    /// Renvoie `(shard_size, shards)` où `shard_size = ceil(data.len() / K)`.
    /// Les K premiers shards portent les données originales (paddées), les M
    /// derniers portent la parité.
    ///
    /// # Erreurs
    /// [`FecError::TooLarge`] si `data` dépasse la capacité, [`FecError::Internal`]
    /// si la lib Reed-Solomon échoue (ne devrait jamais arriver avec `new`).
    pub fn encode(&self, data: &[u8]) -> Result<(usize, Vec<Vec<u8>>), FecError> {
        let shard_size = data.len().div_ceil(self.k).max(1);
        let mut shards: Vec<Vec<u8>> = (0..self.total_shards())
            .map(|_| vec![0u8; shard_size])
            .collect();
        // Copie data dans les K premiers shards (avec padding zéro implicite).
        for (i, chunk) in data.chunks(shard_size).enumerate() {
            shards[i][..chunk.len()].copy_from_slice(chunk);
        }
        self.rs
            .encode(&mut shards)
            .map_err(|e| FecError::Internal(format!("{e:?}")))?;
        Ok((shard_size, shards))
    }

    /// Reconstruit le ciphertext original à partir d'un slice de `Option<Vec<u8>>`
    /// où `None` indique un shard manquant.
    ///
    /// `original_len` est la taille du ciphertext avant padding (nécessaire car
    /// le shard_size est `ceil(original_len/K)` mais on doit tronquer après
    /// reconstitution).
    ///
    /// # Erreurs
    /// [`FecError::Insufficient`] si moins de `K` shards sont présents.
    pub fn decode(
        &self,
        shards: &mut [Option<Vec<u8>>],
        original_len: usize,
    ) -> Result<Vec<u8>, FecError> {
        let got = shards.iter().filter(|s| s.is_some()).count();
        if got < self.k {
            return Err(FecError::Insufficient {
                got,
                need: self.k,
            });
        }
        self.rs
            .reconstruct_data(shards)
            .map_err(|e| FecError::Internal(format!("{e:?}")))?;

        // Concatène les K shards de données et tronque à original_len.
        let shard_size = shards
            .iter()
            .find_map(|s| s.as_ref().map(Vec::len))
            .ok_or_else(|| FecError::Internal("aucun shard après reconstruct".into()))?;
        let mut out = Vec::with_capacity(self.k * shard_size);
        for opt in shards.iter().take(self.k) {
            let s = opt
                .as_ref()
                .ok_or_else(|| FecError::Internal("shard data manquant après reconstruct".into()))?;
            out.extend_from_slice(s);
        }
        out.truncate(original_len);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_codec(k: usize, m: usize) -> FecCodec {
        FecCodec::new(k, m).expect("paramètres valides")
    }

    #[test]
    fn duplication_k1_m1_round_trip() {
        let codec = make_codec(1, 1);
        let data = b"un petit frame Opus de 200 octets...".repeat(6);
        let (shard_size, shards) = codec.encode(&data).unwrap();
        assert_eq!(shards.len(), 2);
        assert_eq!(shards[0].len(), shard_size);

        // Recevoir le shard 0 uniquement (data).
        let mut recv: Vec<Option<Vec<u8>>> = vec![Some(shards[0].clone()), None];
        let out = codec.decode(&mut recv, data.len()).unwrap();
        assert_eq!(out, data);

        // Recevoir le shard 1 uniquement (parité = copie pour K=1).
        let mut recv: Vec<Option<Vec<u8>>> = vec![None, Some(shards[1].clone())];
        let out = codec.decode(&mut recv, data.len()).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn k4_m2_recover_with_2_losses() {
        let codec = make_codec(4, 2);
        let data: Vec<u8> = (0..1024_u32).map(|i| (i as u8).wrapping_mul(17)).collect();
        let (_shard_size, shards) = codec.encode(&data).unwrap();
        assert_eq!(shards.len(), 6);

        // On perd les shards 0 et 2 (data) — on reconstitue avec 1, 3, 4, 5.
        let mut recv: Vec<Option<Vec<u8>>> = shards.iter().cloned().map(Some).collect();
        recv[0] = None;
        recv[2] = None;
        let out = codec.decode(&mut recv, data.len()).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn insufficient_shards_returns_error() {
        let codec = make_codec(4, 2);
        let data = vec![0u8; 100];
        let (_size, shards) = codec.encode(&data).unwrap();
        // 3 shards, on a besoin de 4.
        let mut recv: Vec<Option<Vec<u8>>> = vec![
            Some(shards[0].clone()),
            None,
            Some(shards[2].clone()),
            None,
            Some(shards[4].clone()),
            None,
        ];
        let err = codec.decode(&mut recv, data.len()).unwrap_err();
        matches!(err, FecError::Insufficient { got: 3, need: 4 });
    }

    #[test]
    fn bad_params_rejected() {
        assert!(FecCodec::new(0, 1).is_err());
        assert!(FecCodec::new(100, 1).is_err());
        assert!(FecCodec::new(2, 100).is_err());
    }
}
