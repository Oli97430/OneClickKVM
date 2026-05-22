//! `okvm-udp` — transport UDP chiffré avec Reed-Solomon FEC.
//!
//! Conçu pour les flux **basse latence** : audio (Opus) et vidéo (H.264).
//! Le canal d'input et le canal de fichiers restent sur TCP (besoin de
//! fiabilité stricte et d'ordonnancement).
//!
//! ## Pipeline d'émission
//!
//! ```text
//! plaintext_frame
//!     │
//!     ▼
//! AeadSession.seal()   →   (seq u64, ciphertext)
//!     │
//!     ▼
//! ReedSolomon.encode() →   N = K+M shards de bytes
//!     │
//!     ▼
//! pour chaque shard i ∈ [0,N) :
//!     UDP send: [seq u64 BE][K u8][M u8][i u8][shard_size u16 BE][ct_total_len u32 BE][shard_bytes…]
//! ```
//!
//! ## Pipeline de réception
//!
//! ```text
//! UDP recv (boucle)
//!     │
//!     ▼
//! parse header, push dans buffer indexé par seq
//!     │
//!     ▼ (quand K shards arrivés pour un seq, ou timeout)
//! ReedSolomon.reconstruct_data() →  ciphertext
//!     │
//!     ▼
//! AeadSession.open(seq, …) →  plaintext_frame
//!     │
//!     ▼
//! mpsc::Sender<Frame> → consommateur (decodeur Opus, decodeur H.264, …)
//! ```
//!
//! ## Choix par défaut
//!
//! - **Audio (Opus 64 kbps)** : K=1, M=1 (duplication simple). Frames typiques
//!   ~160 octets ; le coût FEC reste sous 200% de bandwidth pour une protection
//!   à 50% de perte de paquets.
//! - **Vidéo (H.264 1280×720)** : K=4, M=2. Frames ~5-30 KB ; coût ~50%.
//!
//! ## Sécurité
//!
//! - Le chiffrement AEAD est appliqué **avant** le FEC. Donc même si un attaquant
//!   intercepte un shard, il ne peut pas le déchiffrer (et n'apprend rien sur les
//!   autres shards).
//! - Le compteur AEAD (`seq`) est posé en clair dans l'en-tête UDP : il est utilisé
//!   comme nonce, donc public. Cela ne fuite pas d'info utile pour l'attaquant.
//! - Replay protection : déléguée à `AeadSession.open` qui maintient un sliding
//!   bitmap des 64 derniers seq vus.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

mod fec;
mod framing;
mod sender;
mod receiver;

pub use fec::{FecCodec, FecError, MAX_DATA_SHARDS, MAX_PARITY_SHARDS};
pub use framing::{ShardHeader, HEADER_LEN, MAX_SHARD_PAYLOAD};
pub use receiver::{ReassembleError, UdpFecReceiver};
pub use sender::UdpFecSender;
