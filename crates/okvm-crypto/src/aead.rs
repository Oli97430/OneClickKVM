//! AEAD AES-256-GCM avec nonce déterministe `epoch||counter`.
//!
//! Voir `docs/SECURITY.md` §4.2 et `docs/PROTOCOL.md` §3.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Key, Nonce as GcmNonce,
};
use thiserror::Error;
use zeroize::Zeroize;

/// Taille du tag d'authentification AES-GCM (octets).
pub const AEAD_TAG_SIZE: usize = 16;

/// Taille du nonce GCM (96 bits, conformément aux recommandations NIST).
pub const NONCE_SIZE: usize = 12;

/// Direction logique d'une session (du point de vue de la **clé**, pas du flux TCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Direction « client vers serveur ».
    ClientToServer,
    /// Direction « serveur vers client ».
    ServerToClient,
}

/// Clé AES-256-GCM enveloppée avec zeroization.
#[derive(Clone)]
pub struct AeadKey([u8; 32]);

impl AeadKey {
    /// Construit la clé depuis un tableau brut.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Drop for AeadKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for AeadKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AeadKey([redacted; 32])")
    }
}

/// Nonce déterministe `epoch || counter`.
///
/// Layout big-endian : 4 octets pour l'epoch, 8 octets pour le counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nonce {
    /// Epoch de la session (incrémenté à chaque rotation de clé).
    pub epoch: u32,
    /// Compteur monotone par direction et par canal.
    pub counter: u64,
}

impl Nonce {
    /// Sérialise en 12 octets big-endian.
    #[must_use]
    pub fn to_bytes(self) -> [u8; NONCE_SIZE] {
        let mut out = [0u8; NONCE_SIZE];
        out[..4].copy_from_slice(&self.epoch.to_be_bytes());
        out[4..].copy_from_slice(&self.counter.to_be_bytes());
        out
    }
}

/// Erreurs AEAD.
#[derive(Debug, Error)]
pub enum AeadError {
    /// Échec de l'authentification (tag invalide, données altérées).
    #[error("decrypt failed (tag mismatch or tampered data)")]
    Decrypt,
    /// Échec à l'encryption (improbable hors panne mémoire).
    #[error("encrypt failed")]
    Encrypt,
    /// Le compteur 64-bit serait épuisé — refus avant cryptage à nonce répété.
    #[error("nonce counter exhausted, key rotation required")]
    NonceExhausted,
}

/// Session AEAD pour **une direction** d'**un canal**.
///
/// Maintient un compteur monotone côté envoi et un *replay window* côté
/// réception (utile en UDP ; en TCP l'ordre est garanti mais le check
/// monotone reste une bonne défense en profondeur).
pub struct AeadSession {
    cipher: Aes256Gcm,
    epoch: u32,
    /// Compteur du prochain `seal`.
    next_seq: u64,
    /// Plus grand compteur reçu (pour détection rejeu).
    max_recv: u64,
    /// Bitmap glissant pour anti-replay (bit 0 = `max_recv`, bit i = `max_recv - i`).
    recv_window: u64,
}

impl AeadSession {
    /// Crée une session à partir d'une clé et d'un epoch.
    #[must_use]
    pub fn new(key: &AeadKey, epoch: u32) -> Self {
        let k = Key::<Aes256Gcm>::from_slice(&key.0);
        Self {
            cipher: Aes256Gcm::new(k),
            epoch,
            next_seq: 0,
            max_recv: 0,
            recv_window: 0,
        }
    }

    /// Le prochain `seq` qui sera utilisé en envoi (équivalent au counter du nonce).
    #[must_use]
    pub fn peek_send_seq(&self) -> u64 {
        self.next_seq
    }

    /// Chiffre `plaintext` avec l'AAD fournie.
    ///
    /// Renvoie `(seq, ciphertext_with_tag)` — l'appelant écrit `seq` dans
    /// l'en-tête de frame puis recompose le nonce côté réception.
    ///
    /// # Erreur
    /// - [`AeadError::NonceExhausted`] si le compteur a atteint `u64::MAX`.
    /// - [`AeadError::Encrypt`] en cas d'échec interne (improbable).
    pub fn seal(&mut self, aad: &[u8], plaintext: &[u8]) -> Result<(u64, Vec<u8>), AeadError> {
        if self.next_seq == u64::MAX {
            return Err(AeadError::NonceExhausted);
        }
        let nonce = Nonce {
            epoch: self.epoch,
            counter: self.next_seq,
        };
        let n_bytes = nonce.to_bytes();
        let ct = self
            .cipher
            .encrypt(
                GcmNonce::from_slice(&n_bytes),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| AeadError::Encrypt)?;
        let seq = self.next_seq;
        self.next_seq += 1;
        Ok((seq, ct))
    }

    /// Déchiffre `ciphertext` avec AAD et numéro de séquence reçus.
    ///
    /// Effectue la vérification anti-rejeu basée sur `recv_window`. Renvoie
    /// le plaintext en cas de succès.
    ///
    /// # Erreur
    /// - [`AeadError::Decrypt`] si l'authentification échoue ou si le seq est
    ///   déjà vu / hors fenêtre.
    pub fn open(
        &mut self,
        seq: u64,
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, AeadError> {
        if !self.replay_check(seq) {
            return Err(AeadError::Decrypt);
        }
        let n_bytes = Nonce {
            epoch: self.epoch,
            counter: seq,
        }
        .to_bytes();
        let pt = self
            .cipher
            .decrypt(
                GcmNonce::from_slice(&n_bytes),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| AeadError::Decrypt)?;
        self.replay_commit(seq);
        Ok(pt)
    }

    /// Vérifie qu'un `seq` est acceptable selon la fenêtre anti-rejeu.
    /// Ne modifie pas l'état interne (commit séparé après auth réussie).
    fn replay_check(&self, seq: u64) -> bool {
        const WINDOW: u64 = 64;
        if seq > self.max_recv {
            return true;
        }
        let diff = self.max_recv - seq;
        if diff >= WINDOW {
            return false; // trop ancien
        }
        // bit set => déjà vu
        let bit = 1u64 << diff;
        self.recv_window & bit == 0
    }

    fn replay_commit(&mut self, seq: u64) {
        const WINDOW: u64 = 64;
        if seq > self.max_recv {
            let shift = seq - self.max_recv;
            if shift >= WINDOW {
                self.recv_window = 1;
            } else {
                self.recv_window = (self.recv_window << shift) | 1;
            }
            self.max_recv = seq;
        } else {
            let diff = self.max_recv - seq;
            self.recv_window |= 1u64 << diff;
        }
    }
}

impl std::fmt::Debug for AeadSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AeadSession")
            .field("epoch", &self.epoch)
            .field("next_seq", &self.next_seq)
            .field("max_recv", &self.max_recv)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AeadKey {
        AeadKey::from_bytes([0xAB; 32])
    }

    #[test]
    fn round_trip() {
        let mut sender = AeadSession::new(&key(), 0);
        let mut receiver = AeadSession::new(&key(), 0);
        let aad = &[0u8, 0, 0, 0, 0, 0, 0, 0, 0];
        let (seq, ct) = sender.seal(aad, b"hello world").unwrap();
        let pt = receiver.open(seq, aad, &ct).unwrap();
        assert_eq!(pt, b"hello world");
    }

    #[test]
    fn tampered_aad_rejected() {
        let mut sender = AeadSession::new(&key(), 0);
        let mut receiver = AeadSession::new(&key(), 0);
        let aad_send = &[1, 0, 0, 0, 0, 0, 0, 0, 0];
        let aad_recv = &[2, 0, 0, 0, 0, 0, 0, 0, 0];
        let (seq, ct) = sender.seal(aad_send, b"hi").unwrap();
        assert!(matches!(
            receiver.open(seq, aad_recv, &ct),
            Err(AeadError::Decrypt)
        ));
    }

    #[test]
    fn tampered_ct_rejected() {
        let mut sender = AeadSession::new(&key(), 0);
        let mut receiver = AeadSession::new(&key(), 0);
        let aad = &[0u8; 9];
        let (seq, mut ct) = sender.seal(aad, b"hi").unwrap();
        ct[0] ^= 0x01;
        assert!(matches!(
            receiver.open(seq, aad, &ct),
            Err(AeadError::Decrypt)
        ));
    }

    #[test]
    fn replay_rejected() {
        let mut sender = AeadSession::new(&key(), 0);
        let mut receiver = AeadSession::new(&key(), 0);
        let aad = &[0u8; 9];
        let (seq, ct) = sender.seal(aad, b"hi").unwrap();
        let _ = receiver.open(seq, aad, &ct).unwrap();
        assert!(matches!(
            receiver.open(seq, aad, &ct),
            Err(AeadError::Decrypt)
        ));
    }

    #[test]
    fn out_of_order_within_window() {
        let mut sender = AeadSession::new(&key(), 0);
        let mut receiver = AeadSession::new(&key(), 0);
        let aad = &[0u8; 9];
        let (s1, c1) = sender.seal(aad, b"one").unwrap();
        let (s2, c2) = sender.seal(aad, b"two").unwrap();
        // Reçoit 2 avant 1
        let _ = receiver.open(s2, aad, &c2).unwrap();
        let _ = receiver.open(s1, aad, &c1).unwrap();
    }

    #[test]
    fn epochs_isolate() {
        let mut sender = AeadSession::new(&key(), 0);
        let mut receiver = AeadSession::new(&key(), 1);
        let aad = &[0u8; 9];
        let (seq, ct) = sender.seal(aad, b"hi").unwrap();
        assert!(receiver.open(seq, aad, &ct).is_err());
    }
}
