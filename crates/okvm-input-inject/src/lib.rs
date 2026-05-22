//! `okvm-input-inject` — injection clavier/souris via `SendInput`.
//!
//! L'injection passe par l'API `SendInput` qui :
//! - est synchrone et serielle (les evenements injectes ne sont pas entrelaces) ;
//! - marque les evenements injectes avec `LLKHF_INJECTED` / `LLMHF_INJECTED`,
//!   ce qui permet aux hooks de capture (cf. `okvm-input-capture`) de les
//!   filtrer pour eviter les boucles.
//!
//! Les coordonnees absolues sont mises a l'echelle `0..65535` du **bureau
//! virtuel** (multi-ecrans) comme attendu par `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use async_trait::async_trait;

use okvm_core::Result;
use okvm_protocol::InputMessage;

/// Cible d'injection.
#[async_trait]
pub trait InputInject: Send + Sync {
    /// Injecte un message d'input dans la session courante.
    async fn inject(&self, msg: InputMessage) -> Result<()>;
}

#[cfg(windows)]
pub mod win32;

#[cfg(windows)]
pub use win32::Win32Inject;
