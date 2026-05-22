//! `okvm-input-capture` — capture clavier et souris via les hooks Win32.
//!
//! Architecture :
//!
//! 1. Un **thread dedie OS** (pas Tokio) heberge les hooks `WH_KEYBOARD_LL` et
//!    `WH_MOUSE_LL`. Ces hooks **exigent** une boucle de messages Win32
//!    (`GetMessage`/`DispatchMessage`) en cours d'execution sur le thread qui
//!    appelle `SetWindowsHookExW`.
//! 2. Le thread emet des [`okvm_protocol::InputMessage`] sur un canal mpsc
//!    `std::sync::mpsc` (sync). Un thread bridge converti vers un canal
//!    tokio mpsc pour le reste du runtime async.
//! 3. Un **flag global `AtomicBool`** contrôle la **suppression** des
//!    evenements : quand on a "envoye le curseur" sur un autre PC, on active
//!    la suppression et les hooks renvoient `1` pour avaler les evenements
//!    localement.
//!
//! Notes de securite :
//! - Les hooks WH_*_LL sont **user-mode**. Ils n'interceptent pas Ctrl+Alt+Suppr.
//! - Ils ne peuvent pas non plus capturer les evenements destines a une
//!   fenetre elevee si l'app courante ne l'est pas (UIPI / Mandatory Integrity).
//! - Aucun payload sensible n'est logge (cf. `docs/SECURITY.md` §7).

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use async_trait::async_trait;
use tokio::sync::mpsc;

use okvm_core::Result;
use okvm_protocol::InputMessage;

/// Source de capture d'input.
#[async_trait]
pub trait InputCapture: Send + Sync {
    /// Demarre la capture. Les evenements sont envoyes sur `tx`.
    async fn start(&self, tx: mpsc::Sender<InputMessage>) -> Result<CaptureHandle>;
}

/// Handle pour piloter et arreter une capture en cours.
pub struct CaptureHandle {
    /// `true` = on supprime les evenements localement (curseur "parti").
    /// `false` = on laisse passer (curseur "ici").
    pub set_suppress: tokio::sync::watch::Sender<bool>,
    /// Signal d'arret du thread Win32.
    pub stop: tokio::sync::oneshot::Sender<()>,
    /// `JoinHandle` de la task bridge mpsc.
    pub bridge: tokio::task::JoinHandle<()>,
}

#[cfg(windows)]
pub mod win32;

#[cfg(windows)]
pub use win32::Win32Capture;
