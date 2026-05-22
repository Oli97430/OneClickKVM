//! Cote envoi : enumere les fichiers, decoupe en chunks, multi-thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, Semaphore};
use uuid::Uuid;

use okvm_core::{Error, Result};
use okvm_protocol::{Compression, FileEntry, FileMessage};

use crate::{crc32, DEFAULT_CHUNK_KIB};

/// Envoyeur de fichier(s).
pub struct FileSender {
    /// Canal `mpsc::Sender<FileMessage>` (typiquement vers la session reseau).
    pub tx: mpsc::Sender<FileMessage>,
    /// Identifiant du transfert.
    pub transfer_id: Uuid,
    /// Nombre de threads paralleles.
    pub threads: u8,
    /// Compression.
    pub compression: Compression,
    /// Taille de chunk en KiB.
    pub chunk_kib: u32,
    /// Callback de progression : `(bytes_done, bytes_total, current_file)`.
    pub on_progress: Option<std::sync::Arc<dyn Fn(u64, u64, &str) + Send + Sync + 'static>>,
}

impl FileSender {
    /// Cree un sender avec valeurs par defaut (4 threads, pas de compression).
    #[must_use]
    pub fn new(tx: mpsc::Sender<FileMessage>) -> Self {
        Self {
            tx,
            transfer_id: Uuid::new_v4(),
            threads: 4,
            compression: Compression::None,
            chunk_kib: DEFAULT_CHUNK_KIB,
            on_progress: None,
        }
    }

    /// Attache un callback de progression appele a chaque chunk envoye.
    #[must_use]
    pub fn with_progress(mut self, cb: impl Fn(u64, u64, &str) + Send + Sync + 'static) -> Self {
        self.on_progress = Some(std::sync::Arc::new(cb));
        self
    }

    /// Lance le transfert d'une liste de chemins (fichiers + dossiers
    /// recursivement).
    pub async fn send_paths(&self, paths: &[PathBuf]) -> Result<()> {
        let entries = enumerate(paths).await?;
        if entries.is_empty() {
            return Err(Error::other("aucun fichier a envoyer"));
        }
        let total_bytes: u64 = entries.iter().map(|e| e.size_bytes).sum();

        let start = FileMessage::TransferStart {
            transfer_id: self.transfer_id,
            files: entries.clone(),
            total_bytes,
            compression: self.compression,
            threads: self.threads,
        };
        self.tx
            .send(start)
            .await
            .map_err(|_| Error::other("canal transfert ferme"))?;

        // Envoie les chunks en parallele, plafonne par un semaphore.
        let sem = Arc::new(Semaphore::new(self.threads as usize));
        let chunk_bytes = self.chunk_kib as usize * 1024;
        let bytes_done = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let on_progress = self.on_progress.clone();
        for entry in entries {
            if entry.is_dir {
                continue;
            }
            let permit = sem
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::other(format!("semaphore: {e}")))?;
            let tx = self.tx.clone();
            let transfer_id = self.transfer_id;
            let bytes_done_c = bytes_done.clone();
            let on_progress_c = on_progress.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(e) = send_one_file(
                    transfer_id,
                    &entry,
                    chunk_bytes,
                    &tx,
                    bytes_done_c,
                    total_bytes,
                    on_progress_c,
                )
                .await
                {
                    tracing::warn!(file = %entry.rel_path, error = %e, "envoi fichier echoue");
                }
            });
        }

        // Attend la fin de tous les threads en re-acquerant tous les permits.
        let _ = sem.acquire_many_owned(self.threads as u32).await;
        // Notifie 100% final.
        if let Some(cb) = &self.on_progress {
            cb(total_bytes, total_bytes, "");
        }
        Ok(())
    }
}

async fn send_one_file(
    transfer_id: Uuid,
    entry: &FileEntry,
    chunk_bytes: usize,
    tx: &mpsc::Sender<FileMessage>,
    bytes_done: Arc<std::sync::atomic::AtomicU64>,
    bytes_total: u64,
    on_progress: Option<std::sync::Arc<dyn Fn(u64, u64, &str) + Send + Sync + 'static>>,
) -> Result<()> {
    // L'enumerate a stocke le chemin absolu dans rel_path pour le sender.
    // En vrai, on devrait avoir un mapping rel_path → absolute path separe.
    // Pour l'instant, on accepte que rel_path soit aussi le chemin de lecture.
    let abs = PathBuf::from(&entry.rel_path);
    let mut f = tokio::fs::File::open(&abs).await?;
    let mut hasher = blake3::Hasher::new();
    let mut offset: u64 = 0;
    let mut buf = vec![0u8; chunk_bytes];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        let is_last = (n < chunk_bytes) || offset + n as u64 >= entry.size_bytes;
        let crc = crc32(&buf[..n]);
        let msg = FileMessage::Chunk {
            transfer_id,
            file_idx: entry.idx,
            thread_idx: 0,
            offset,
            data: buf[..n].to_vec(),
            is_last,
            crc32: crc,
        };
        tx.send(msg)
            .await
            .map_err(|_| Error::other("canal ferme"))?;
        offset += n as u64;
        // Met a jour le compteur global et notifie (throttle simple : a chaque chunk).
        let done_now =
            bytes_done.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed) + n as u64;
        if let Some(cb) = &on_progress {
            cb(done_now, bytes_total, &entry.rel_path);
        }
        if is_last {
            break;
        }
    }
    let mut h = [0u8; 32];
    h.copy_from_slice(hasher.finalize().as_bytes());
    let done = FileMessage::TransferComplete {
        transfer_id,
        file_idx: entry.idx,
        blake3: h,
    };
    tx.send(done)
        .await
        .map_err(|_| Error::other("canal ferme"))?;
    Ok(())
}

async fn enumerate(paths: &[PathBuf]) -> Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    let mut idx = 0u32;
    for p in paths {
        walk(p, p.parent().unwrap_or(Path::new("")), &mut out, &mut idx).await?;
    }
    Ok(out)
}

#[allow(clippy::manual_async_fn)] // recurse async
fn walk<'a>(
    path: &'a Path,
    base: &'a Path,
    out: &'a mut Vec<FileEntry>,
    idx: &'a mut u32,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let meta = tokio::fs::metadata(path).await?;
        let rel_path = path
            .strip_prefix(base)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0i64, |d| d.as_millis() as i64);

        if meta.is_dir() {
            out.push(FileEntry {
                idx: *idx,
                rel_path: rel_path.clone(),
                size_bytes: 0,
                is_dir: true,
                mtime_ms,
                permissions: 0o755,
            });
            *idx += 1;
            let mut rd = tokio::fs::read_dir(path).await?;
            while let Some(ent) = rd.next_entry().await? {
                walk(&ent.path(), base, out, idx).await?;
            }
        } else {
            out.push(FileEntry {
                idx: *idx,
                // Pour le sender, on stocke le chemin absolu ici (HACK ; un
                // mapping idx → abs path proprement vivrait dans FileSender).
                rel_path: path.to_string_lossy().to_string(),
                size_bytes: meta.len(),
                is_dir: false,
                mtime_ms,
                permissions: 0o644,
            });
            *idx += 1;
        }
        Ok(())
    })
}
