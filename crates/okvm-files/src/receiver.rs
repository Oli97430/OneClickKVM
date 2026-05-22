//! Cote reception : valide les chemins, ouvre les fichiers, ecrit les chunks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use okvm_core::{Error, Result};
use okvm_protocol::{FileEntry, FileMessage};

use crate::{crc32, validate_rel_path};

/// Recevoir d'un transfert : maintient un etat par `transfer_id` et applique
/// les `FileMessage` entrants.
pub struct FileReceiver {
    /// Repertoire racine ou ecrire (sandbox).
    pub dest_root: PathBuf,
    transfers: Arc<Mutex<HashMap<Uuid, TransferState>>>,
    /// Callback de progression : `(transfer_id, bytes_done, bytes_total, current_file)`.
    on_progress: Mutex<Option<Arc<dyn Fn(Uuid, u64, u64, &str) + Send + Sync + 'static>>>,
}

struct TransferState {
    /// Mapping `file_idx → (chemin destination, hasher, expected_size)`.
    files: HashMap<u32, FileSlot>,
    /// Octets totaux annonces par le sender.
    bytes_total: u64,
    /// Octets recus tous fichiers confondus.
    bytes_done: u64,
}

struct FileSlot {
    dest_abs: PathBuf,
    hasher: blake3::Hasher,
    /// `tokio::fs::File` derriere un tokio Mutex (pour que le guard puisse
    /// vivre a travers les awaits de seek/write/flush).
    file: Arc<tokio::sync::Mutex<Option<tokio::fs::File>>>,
    expected_size: u64,
    received: u64,
}

impl FileReceiver {
    /// Construit un receveur ancre a `dest_root`.
    #[must_use]
    pub fn new(dest_root: PathBuf) -> Self {
        Self {
            dest_root,
            transfers: Arc::new(Mutex::new(HashMap::new())),
            on_progress: Mutex::new(None),
        }
    }

    /// Definit un callback de progression (`transfer_id, bytes_done, bytes_total, current_file`).
    pub fn set_on_progress(&self, cb: impl Fn(Uuid, u64, u64, &str) + Send + Sync + 'static) {
        *self.on_progress.lock() = Some(Arc::new(cb));
    }

    /// Traite un message entrant.
    pub async fn on_message(&self, msg: FileMessage) -> Result<()> {
        match msg {
            FileMessage::TransferStart {
                transfer_id,
                files,
                total_bytes,
                ..
            } => self.start(transfer_id, &files, total_bytes).await,
            FileMessage::Chunk {
                transfer_id,
                file_idx,
                offset,
                data,
                is_last,
                crc32: expected_crc,
                ..
            } => {
                let actual_crc = crc32(&data);
                if actual_crc != expected_crc {
                    return Err(Error::other(format!(
                        "CRC mismatch sur chunk offset {offset}: attendu {expected_crc:08x}, recu {actual_crc:08x}"
                    )));
                }
                self.write_chunk(transfer_id, file_idx, offset, &data, is_last)
                    .await
            }
            FileMessage::TransferComplete {
                transfer_id,
                file_idx,
                blake3: expected,
            } => self.complete(transfer_id, file_idx, &expected).await,
            FileMessage::TransferCancel {
                transfer_id,
                reason,
            } => {
                self.cancel(transfer_id, &reason);
                Ok(())
            }
            // Le receveur ignore TransferAccept/Reject/ChunkAck (cote sender).
            _ => Ok(()),
        }
    }

    async fn start(&self, transfer_id: Uuid, files: &[FileEntry], total_bytes: u64) -> Result<()> {
        let mut slots = HashMap::new();
        for entry in files {
            let rel = validate_rel_path(&entry.rel_path)?;
            let dest_abs = self.dest_root.join(&rel);
            // Assure que dest_abs reste dans dest_root (defense en profondeur).
            if !dest_abs.starts_with(&self.dest_root) {
                return Err(Error::other(format!("BadPath: {}", entry.rel_path)));
            }
            if entry.is_dir {
                fs::create_dir_all(&dest_abs).await?;
                continue;
            }
            // Cree les parents.
            if let Some(parent) = dest_abs.parent() {
                fs::create_dir_all(parent).await?;
            }
            // Pre-alloue le fichier.
            let f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&dest_abs)
                .await?;
            f.set_len(entry.size_bytes).await?;
            slots.insert(
                entry.idx,
                FileSlot {
                    dest_abs,
                    hasher: blake3::Hasher::new(),
                    file: Arc::new(tokio::sync::Mutex::new(Some(f))),
                    expected_size: entry.size_bytes,
                    received: 0,
                },
            );
        }
        self.transfers.lock().insert(
            transfer_id,
            TransferState {
                files: slots,
                bytes_total: total_bytes,
                bytes_done: 0,
            },
        );
        Ok(())
    }

    async fn write_chunk(
        &self,
        transfer_id: Uuid,
        file_idx: u32,
        offset: u64,
        data: &[u8],
        is_last: bool,
    ) -> Result<()> {
        // Recupere les references necessaires SANS tenir le Mutex sur await.
        let (file_arc, rel_path, bytes_done_snapshot, bytes_total_snapshot) = {
            let mut g = self.transfers.lock();
            let st = g
                .get_mut(&transfer_id)
                .ok_or_else(|| Error::other(format!("transfer inconnu: {transfer_id}")))?;
            st.bytes_done += data.len() as u64;
            let bytes_done_snapshot = st.bytes_done;
            let bytes_total_snapshot = st.bytes_total;
            let slot = st
                .files
                .get_mut(&file_idx)
                .ok_or_else(|| Error::other(format!("file_idx inconnu: {file_idx}")))?;
            slot.hasher.update(data);
            slot.received += data.len() as u64;
            let rel_path = slot
                .dest_abs
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            (
                slot.file.clone(),
                rel_path,
                bytes_done_snapshot,
                bytes_total_snapshot,
            )
        };

        // Callback de progression (sans lock).
        if let Some(cb) = self.on_progress.lock().clone() {
            cb(
                transfer_id,
                bytes_done_snapshot,
                bytes_total_snapshot,
                &rel_path,
            );
        }
        // Ecrit sous lock du file uniquement (tokio Mutex : guard Send-friendly).
        let mut g = file_arc.lock().await;
        let f = g
            .as_mut()
            .ok_or_else(|| Error::other("fichier deja ferme"))?;
        f.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(Error::Io)?;
        f.write_all(data).await.map_err(Error::Io)?;
        if is_last {
            f.flush().await.map_err(Error::Io)?;
        }
        Ok(())
    }

    async fn complete(&self, transfer_id: Uuid, file_idx: u32, expected: &[u8; 32]) -> Result<()> {
        let (hasher, dest_abs, file_arc, expected_size, received) = {
            let mut g = self.transfers.lock();
            let st = g
                .get_mut(&transfer_id)
                .ok_or_else(|| Error::other("transfer inconnu"))?;
            let slot = st
                .files
                .remove(&file_idx)
                .ok_or_else(|| Error::other("file_idx inconnu"))?;
            (
                slot.hasher,
                slot.dest_abs,
                slot.file,
                slot.expected_size,
                slot.received,
            )
        };
        // Ferme le fichier.
        let mut g = file_arc.lock().await;
        if let Some(mut f) = g.take() {
            f.flush().await.map_err(Error::Io)?;
        }
        drop(g);

        if received < expected_size {
            return Err(Error::other(format!(
                "fichier incomplet: {received}/{expected_size} octets"
            )));
        }

        let got = hasher.finalize();
        if got.as_bytes() != expected {
            // Supprime le fichier corrompu.
            let _ = fs::remove_file(&dest_abs).await;
            return Err(Error::other(format!(
                "BLAKE3 mismatch sur {}",
                dest_abs.display()
            )));
        }
        tracing::info!(file = %dest_abs.display(), "fichier recu et verifie");
        Ok(())
    }

    fn cancel(&self, transfer_id: Uuid, reason: &str) {
        let mut g = self.transfers.lock();
        if let Some(st) = g.remove(&transfer_id) {
            tracing::info!(transfer = %transfer_id, files = st.files.len(), reason, "transfer annule");
            // Les fichiers sont laisses sur disque (potentiellement partiels) —
            // l'app decide si elle veut les supprimer.
        }
    }
}

// Petit shim pour AsyncWriteExt::seek depuis tokio::fs::File via AsyncSeekExt.
use tokio::io::AsyncSeekExt;
