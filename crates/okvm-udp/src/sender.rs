//! [`UdpFecSender`] : encrypte un frame, FEC-encode, et envoie N shards UDP.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;

use okvm_crypto::{aead::AeadError, AeadSession};

use crate::fec::{FecCodec, FecError};
use crate::framing::{ShardHeader, HEADER_LEN, MAX_SHARD_PAYLOAD};

/// Erreurs côté envoi.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// I/O UDP.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// AEAD.
    #[error("aead: {0}")]
    Aead(#[from] AeadError),
    /// FEC.
    #[error("fec: {0}")]
    Fec(#[from] FecError),
    /// Shard plus grand que le MTU acceptable.
    #[error("shard trop grand: {0} (max {})", MAX_SHARD_PAYLOAD)]
    ShardTooLarge(usize),
}

/// Encoder/émetteur UDP+FEC pour un peer donné.
///
/// Le socket est stocké en `Arc<UdpSocket>` pour permettre de partager le
/// même socket physique entre un sender et un [`crate::UdpFecReceiver`] —
/// nécessaire quand une session bidirectionnelle utilise un seul port UDP
/// (ce qui est le cas typique server-side après bind du port advertise).
pub struct UdpFecSender {
    socket: Arc<UdpSocket>,
    remote: SocketAddr,
    aead: AeadSession,
    fec: FecCodec,
}

impl UdpFecSender {
    /// Construit un sender lié à `socket`, envoyant vers `remote`, avec la
    /// session AEAD `aead` (côté envoi) et le codec FEC `fec`.
    ///
    /// Accepte n'importe quoi qui se convertit en `Arc<UdpSocket>` : passez
    /// soit un `UdpSocket` directement (consommé), soit un `Arc<UdpSocket>`
    /// déjà partagé avec un receiver.
    #[must_use]
    pub fn new(
        socket: impl Into<Arc<UdpSocket>>,
        remote: SocketAddr,
        aead: AeadSession,
        fec: FecCodec,
    ) -> Self {
        Self {
            socket: socket.into(),
            remote,
            aead,
            fec,
        }
    }

    /// Renvoie l'adresse locale bindée du socket UDP.
    ///
    /// # Erreurs
    /// I/O.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Envoie un frame `plaintext` : AEAD-seal puis FEC-encode puis envoi des
    /// N shards en N datagrammes UDP indépendants.
    ///
    /// # Erreurs
    /// Voir [`SendError`].
    pub async fn send_frame(&mut self, plaintext: &[u8]) -> Result<(), SendError> {
        // 1. Chiffrement (aad = compteur AEAD pour rappel ; l'aad est implicite
        //    via le seq qui sert de nonce, on n'a pas besoin d'aad supplémentaire).
        let (seq, ciphertext) = self.aead.seal(&[], plaintext)?;

        // 2. FEC encode.
        let (shard_size, shards) = self.fec.encode(&ciphertext)?;
        if shard_size > MAX_SHARD_PAYLOAD {
            return Err(SendError::ShardTooLarge(shard_size));
        }

        // 3. Émission de chaque shard en datagramme indépendant.
        let mut buf = vec![0u8; HEADER_LEN + shard_size];
        let k = u8::try_from(self.fec.data_shards()).expect("K ≤ MAX_DATA_SHARDS ≤ 16");
        let m = u8::try_from(self.fec.parity_shards()).expect("M ≤ MAX_PARITY_SHARDS ≤ 8");
        let plain_len = u32::try_from(ciphertext.len())
            .map_err(|_| SendError::ShardTooLarge(ciphertext.len()))?;
        let shard_len = u16::try_from(shard_size).expect("shard_size ≤ MAX_SHARD_PAYLOAD ≤ 1400");

        for (i, shard) in shards.iter().enumerate() {
            let hdr = ShardHeader {
                seq,
                data_shards: k,
                parity_shards: m,
                index: u8::try_from(i).expect("i < K+M ≤ 24"),
                plain_len,
                shard_len,
            };
            // header (`encode` retourne le nombre d'octets écrits, qu'on n'utilise
            // pas ici car HEADER_LEN est constante).
            let mut hdr_arr = [0u8; HEADER_LEN];
            let _ = hdr.encode(&mut hdr_arr);
            buf[..HEADER_LEN].copy_from_slice(&hdr_arr);
            buf[HEADER_LEN..HEADER_LEN + shard.len()].copy_from_slice(shard);
            let _ = self
                .socket
                .send_to(&buf[..HEADER_LEN + shard.len()], self.remote)
                .await?;
        }
        Ok(())
    }
}
