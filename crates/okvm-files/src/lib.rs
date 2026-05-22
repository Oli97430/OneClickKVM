//! `okvm-files` — transfert de fichiers multi-thread et drag & drop.
//!
//! Caracteristiques :
//!
//! - Plusieurs sous-streams paralleles par fichier (negocies au `TransferStart`).
//! - Verification BLAKE3 par fichier complet.
//! - Securisation des chemins recus : rejet de `..`, drive letters, ADS `:`.
//!
//! L'API publique :
//! - [`FileSender`] : enumere les chemins locaux, calcule les `FileEntry`,
//!   envoie `TransferStart` puis streame les `Chunk` en parallele.
//! - [`FileReceiver`] : accepte un `TransferStart`, ouvre les fichiers
//!   destination, ecrit les chunks dans l'ordre et verifie le BLAKE3.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

pub mod receiver;
pub mod sender;

use std::path::PathBuf;

use async_trait::async_trait;
use uuid::Uuid;

use okvm_core::Result;
use okvm_protocol::FileEntry;

pub use receiver::FileReceiver;
pub use sender::FileSender;

/// Direction d'un transfert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Envoi local → distant.
    Outbound,
    /// Reception distant → local.
    Inbound,
}

/// Statut d'un transfert.
#[derive(Debug, Clone)]
pub struct TransferProgress {
    /// ID.
    pub transfer_id: Uuid,
    /// Octets transferes.
    pub bytes_done: u64,
    /// Octets totaux.
    pub bytes_total: u64,
    /// Fichier actuellement en cours.
    pub current_file: Option<String>,
}

/// Trait gestionnaire de transferts.
#[async_trait]
pub trait FileTransferManager: Send + Sync {
    /// Demarre un transfert sortant.
    async fn send(
        &self,
        target_peer: Uuid,
        files: Vec<PathBuf>,
        threads: u8,
    ) -> Result<Uuid>;

    /// Accepte un transfert entrant (annonce via `TransferStart`).
    async fn accept_inbound(
        &self,
        transfer_id: Uuid,
        dest_dir: PathBuf,
        files: &[FileEntry],
    ) -> Result<()>;

    /// Annule un transfert en cours.
    async fn cancel(&self, transfer_id: Uuid) -> Result<()>;
}

/// Erreurs specifiques.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileError {
    /// Chemin recu invalide (path traversal, ADS, etc.).
    #[error("chemin rejete: {0}")]
    BadPath(String),
    /// Hash BLAKE3 final ne correspond pas.
    #[error("integrite: BLAKE3 mismatch sur {0}")]
    HashMismatch(String),
    /// Transfert annule par l'utilisateur.
    #[error("annule: {0}")]
    Cancelled(String),
    /// CRC32 d'un chunk invalide.
    #[error("CRC32 invalide sur chunk offset {0}")]
    BadChunkCrc(u64),
}

/// Taille de chunk par defaut (KiB).
pub const DEFAULT_CHUNK_KIB: u32 = 256;

/// Valide qu'un `rel_path` recu sur le wire est sur pour `dest_dir/rel_path`.
///
/// Rejette :
/// - composants `..` (path traversal),
/// - chemins absolus,
/// - drive letters Windows (`C:` etc.),
/// - alternate data streams `nom:stream`.
pub fn validate_rel_path(rel_path: &str) -> Result<PathBuf> {
    use std::path::{Component, Path};
    if rel_path.contains(':') {
        return Err(okvm_core::Error::other(format!(
            "chemin avec ':' (ADS ou drive letter) rejete: {rel_path}"
        )));
    }
    let path = Path::new(rel_path);
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => {
                return Err(okvm_core::Error::other(format!(
                    "composant chemin interdit dans: {rel_path}"
                )));
            }
        }
    }
    Ok(out)
}

/// CRC32 ultra-rapide (Castagnoli) sur un slice. Equivalent SSE4.2 si dispo
/// via la crate `crc32fast` ; ici implementation pure simple (suffisante pour
/// du checksum de debug). En cas d'usage intensif, basculer sur `crc32fast`.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    const POLY: u32 = 0xEDB8_8320;
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ POLY } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_dir() {
        assert!(validate_rel_path("../etc/passwd").is_err());
        assert!(validate_rel_path("a/../b").is_err());
    }

    #[test]
    fn rejects_absolute_and_drive_letter() {
        assert!(validate_rel_path("/etc/passwd").is_err());
        assert!(validate_rel_path("C:/Windows").is_err());
    }

    #[test]
    fn rejects_ads() {
        assert!(validate_rel_path("file.txt:hidden").is_err());
    }

    #[test]
    fn accepts_normal() {
        let p = validate_rel_path("docs/sub/file.txt").unwrap();
        assert!(p.ends_with("file.txt"));
    }

    #[test]
    fn crc32_known_value() {
        // "abc" → 0x352441C2 (CRC-32, polynome IEEE 802.3)
        assert_eq!(crc32(b"abc"), 0x3524_41C2);
    }

    #[test]
    fn crc32_empty() {
        assert_eq!(crc32(b""), 0);
    }
}
