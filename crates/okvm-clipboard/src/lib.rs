//! `okvm-clipboard` — synchronisation du presse-papier multi-format.
//!
//! Formats supportes :
//! - **Texte UTF-8** : `CF_UNICODETEXT`
//! - **RTF** : `CF_RTF` (clipboard format enregistre, "Rich Text Format")
//! - **HTML** : `CF_HTML` (clipboard format enregistre)
//! - **Image PNG** : `CF_DIB` (decode → re-encode en bitmap pour le mettre)
//!   et exporte en PNG cote read
//! - **Liste de fichiers** : `CF_HDROP`
//!
//! L'observation des changements utilise `AddClipboardFormatListener` qui
//! envoie `WM_CLIPBOARDUPDATE` a une fenetre message-only.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use async_trait::async_trait;
use tokio::sync::mpsc;

use okvm_core::Result;
use okvm_protocol::ClipboardItem;

/// Trait pour lire et ecrire le presse-papier local.
#[async_trait]
pub trait ClipboardSync: Send + Sync {
    /// Lit le contenu actuel du presse-papier, tous formats disponibles.
    async fn read(&self) -> Result<Vec<ClipboardItem>>;
    /// Ecrit le contenu dans le presse-papier local.
    async fn write(&self, items: &[ClipboardItem]) -> Result<()>;
    /// Demarre l'observation des changements.
    async fn watch(&self, tx: mpsc::Sender<Vec<ClipboardItem>>) -> Result<ClipboardWatchHandle>;
}

/// Handle pour arreter l'observation.
pub struct ClipboardWatchHandle {
    /// Signal d'arret.
    pub stop: tokio::sync::oneshot::Sender<()>,
}

#[cfg(windows)]
pub mod win32;

#[cfg(windows)]
pub use win32::Win32Clipboard;
