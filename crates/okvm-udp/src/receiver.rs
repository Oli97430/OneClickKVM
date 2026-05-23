//! [`UdpFecReceiver`] : collecte les shards UDP, reconstitue et déchiffre.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use okvm_crypto::{aead::AeadError, AeadSession};

use crate::fec::{FecCodec, FecError};
use crate::framing::{ShardHeader, MAX_SHARD_PAYLOAD};

/// Erreurs côté réception.
#[derive(Debug, thiserror::Error)]
pub enum ReassembleError {
    /// I/O UDP.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// AEAD (déchiffrement / anti-rejeu).
    #[error("aead: {0}")]
    Aead(#[from] AeadError),
    /// FEC.
    #[error("fec: {0}")]
    Fec(#[from] FecError),
    /// En-tête de shard mal formé.
    #[error("framing: {0}")]
    Framing(#[from] crate::framing::FramingError),
    /// Le socket a renvoyé un datagramme provenant d'un peer inattendu.
    #[error("source inattendue: {0}")]
    BadSource(SocketAddr),
}

/// Buffer d'un frame en cours de reconstitution.
struct PendingFrame {
    shards: Vec<Option<Vec<u8>>>,
    data_shards: u8,
    parity_shards: u8,
    plain_len: u32,
    inserted_at: Instant,
    received: u8,
}

impl PendingFrame {
    fn new(hdr: ShardHeader) -> Self {
        let total = hdr.data_shards as usize + hdr.parity_shards as usize;
        Self {
            shards: vec![None; total],
            data_shards: hdr.data_shards,
            parity_shards: hdr.parity_shards,
            plain_len: hdr.plain_len,
            inserted_at: Instant::now(),
            received: 0,
        }
    }
}

/// Plafond du nombre de frames en cours de réassemblage. Au-delà, on évince
/// la plus ancienne entrée (FIFO) — protège contre un attaquant qui spammerait
/// des shards orphelins pour grossir la map indéfiniment.
const MAX_PENDING_FRAMES: usize = 256;

/// Réception UDP+FEC pour un peer donné.
///
/// Le socket est en `Arc<UdpSocket>` pour permettre de partager le même
/// socket avec un [`crate::UdpFecSender`] (bidirectionnel sur un seul port).
pub struct UdpFecReceiver {
    socket: Arc<UdpSocket>,
    expected_remote: Option<SocketAddr>,
    aead: AeadSession,
    /// Codec partagé (K, M négociés au handshake).
    fec: FecCodec,
    /// Frames en cours, indexés par `seq`. `BTreeMap` pour un nettoyage ordonné
    /// (les entrées les plus anciennes sont au début, faciles à évincer).
    ///
    /// Capacité plafonnée à [`MAX_PENDING_FRAMES`] — au-delà on évince les plus
    /// anciens même s'ils ne sont pas encore expirés. C'est borné en mémoire :
    /// `MAX_PENDING_FRAMES * (K+M) * MAX_SHARD_PAYLOAD` ≈ 256 * 24 * 1400 = 8.6 MB
    /// dans le pire cas K=16, M=8.
    pending: BTreeMap<u64, PendingFrame>,
    /// Liste des seq déjà résolus (anti-replay côté reconstitution : si AEAD a
    /// rejeté un seq, on ne réessaye pas inutilement). Glissant 256 entrées.
    resolved: VecDeque<u64>,
    /// Délai max d'attente avant de jeter un frame partiel. Configurable selon
    /// le lien (100ms par défaut, OK pour LAN ; à monter sur WAN saturé).
    pub assemble_timeout: Duration,
    /// Buffer de réception réutilisé.
    recv_buf: Vec<u8>,
}

impl UdpFecReceiver {
    /// Construit un récepteur.
    ///
    /// `expected_remote` : si `Some`, les datagrammes provenant d'un autre
    /// peer sont silencieusement ignorés. `None` accepte tout (utile pour
    /// les tests loopback).
    #[must_use]
    pub fn new(
        socket: impl Into<Arc<UdpSocket>>,
        expected_remote: Option<SocketAddr>,
        aead: AeadSession,
        fec: FecCodec,
    ) -> Self {
        Self {
            socket: socket.into(),
            expected_remote,
            aead,
            fec,
            pending: BTreeMap::new(),
            resolved: VecDeque::with_capacity(256),
            assemble_timeout: Duration::from_millis(100),
            recv_buf: vec![0u8; MAX_SHARD_PAYLOAD + crate::framing::HEADER_LEN + 64],
        }
    }

    /// Adresse locale.
    ///
    /// # Erreurs
    /// I/O.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Bloque jusqu'à ce qu'un frame complet soit reçu (et déchiffré) ou
    /// qu'une erreur fatale survienne. Les erreurs non fatales (datagrammes
    /// corrompus, mauvaise source) sont loguées en `debug` et ignorées.
    ///
    /// # Erreurs
    /// Erreurs I/O ou AEAD fatales (la session devrait être recréée).
    pub async fn recv_frame(&mut self) -> Result<Vec<u8>, ReassembleError> {
        loop {
            self.drain_stale();
            let (n, src) = self.socket.recv_from(&mut self.recv_buf).await?;
            if let Some(expected) = self.expected_remote {
                if src != expected {
                    tracing::debug!(?src, ?expected, "UDP: source ignorée");
                    continue;
                }
            }
            let datagram = &self.recv_buf[..n];
            let (hdr, payload) = match ShardHeader::parse(datagram) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(error = %e, "UDP: datagramme corrompu");
                    continue;
                }
            };

            // Vérifie la cohérence avec notre codec local.
            if hdr.data_shards as usize != self.fec.data_shards()
                || hdr.parity_shards as usize != self.fec.parity_shards()
            {
                tracing::debug!(
                    expected = ?(self.fec.data_shards(), self.fec.parity_shards()),
                    got = ?(hdr.data_shards, hdr.parity_shards),
                    "UDP: paramètres FEC incompatibles, shard ignoré"
                );
                continue;
            }

            // Déjà résolu ? on ignore.
            if self.resolved.contains(&hdr.seq) {
                continue;
            }

            // Cap protectif : avant d'insérer un nouveau seq, on évince le
            // plus ancien si on est à la limite. Évite la croissance non
            // bornée sous attaque spray-orphan-shards.
            if !self.pending.contains_key(&hdr.seq) && self.pending.len() >= MAX_PENDING_FRAMES {
                if let Some((&oldest, _)) = self.pending.iter().next() {
                    self.pending.remove(&oldest);
                    tracing::debug!(seq = oldest, "UDP: pending plein, eviction FIFO");
                }
            }
            let pf = self
                .pending
                .entry(hdr.seq)
                .or_insert_with(|| PendingFrame::new(hdr));
            // On copie le payload dans le shard, sauf si déjà présent (dup).
            if pf.shards[hdr.index as usize].is_none() {
                pf.shards[hdr.index as usize] = Some(payload.to_vec());
                pf.received += 1;
            }

            // Si on a K shards (data + parity confondus), on tente la reconstitution.
            if pf.received >= pf.data_shards {
                let plain_len = pf.plain_len as usize;
                let mut shards_owned = std::mem::take(&mut pf.shards);
                let data_k = pf.data_shards as usize;
                let parity_m = pf.parity_shards as usize;
                self.pending.remove(&hdr.seq);
                self.mark_resolved(hdr.seq);

                // Le codec local doit correspondre exactement (K, M).
                debug_assert_eq!(data_k, self.fec.data_shards());
                debug_assert_eq!(parity_m, self.fec.parity_shards());

                let ciphertext = match self.fec.decode(&mut shards_owned, plain_len) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(seq = hdr.seq, error = %e, "FEC decode échec");
                        continue;
                    }
                };
                let plaintext = match self.aead.open(hdr.seq, &[], &ciphertext) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(seq = hdr.seq, error = %e, "AEAD open échec");
                        // Replay ou clé incorrecte : on continue, c'est sans danger.
                        continue;
                    }
                };
                return Ok(plaintext);
            }
        }
    }

    fn drain_stale(&mut self) {
        let now = Instant::now();
        let stale: Vec<u64> = self
            .pending
            .iter()
            .filter_map(|(seq, pf)| {
                if now.duration_since(pf.inserted_at) > self.assemble_timeout {
                    Some(*seq)
                } else {
                    None
                }
            })
            .collect();
        for seq in stale {
            self.pending.remove(&seq);
            tracing::debug!(seq, "frame UDP abandonné (timeout)");
        }
    }

    fn mark_resolved(&mut self, seq: u64) {
        if self.resolved.len() >= 256 {
            self.resolved.pop_front();
        }
        self.resolved.push_back(seq);
    }
}
