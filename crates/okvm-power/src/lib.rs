//! `okvm-power` — gestion des etats d'alimentation locaux.
//!
//! Implementations Win32 :
//!
//! - [`PowerAction::LockWorkstation`] → `LockWorkstation` (user32).
//! - [`PowerAction::Sleep`] → `SetSuspendState(FALSE, TRUE, FALSE)`.
//! - [`PowerAction::Hibernate`] → `SetSuspendState(TRUE, TRUE, FALSE)`.
//! - [`PowerAction::Restart`] / [`PowerAction::Shutdown`] → `ExitWindowsEx` avec privilege
//!   `SeShutdownPrivilege` requis (sinon erreur ACCESS_DENIED renvoyee proprement).
//!
//! **Note** : Windows ne permet pas de **deverrouiller** la session a distance
//! depuis user-mode. C'est une frontiere de securite explicite (cf.
//! `docs/SECURITY.md` §9).

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]

use async_trait::async_trait;

use okvm_core::Result;

/// Actions de gestion d'energie disponibles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    /// Verrouille la session courante.
    LockWorkstation,
    /// Met le PC en veille.
    Sleep,
    /// Met en veille prolongee (hibernation).
    Hibernate,
    /// Redemarre l'OS.
    Restart,
    /// Eteint l'OS.
    Shutdown,
}

/// Trait pour appliquer une action d'energie locale.
#[async_trait]
pub trait PowerControl: Send + Sync {
    /// Applique l'action demandee.
    async fn apply(&self, action: PowerAction) -> Result<()>;
}

/// Implementation Win32 active sur Windows.
#[cfg(windows)]
pub mod win32 {
    use super::{async_trait, PowerAction, PowerControl, Result};

    use okvm_core::Error;
    use windows::Win32::{
        Foundation::{HANDLE, LUID},
        Security::{
            AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES,
            SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        },
        System::{
            Power::SetSuspendState,
            Shutdown::{
                ExitWindowsEx, EWX_FORCEIFHUNG, EWX_REBOOT, EWX_SHUTDOWN,
                SHTDN_REASON_MAJOR_OTHER,
            },
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };
    use windows::core::PCWSTR;
    use windows::Win32::System::Shutdown::LockWorkStation;

    /// Implementation Win32.
    pub struct Win32Power;

    #[async_trait]
    impl PowerControl for Win32Power {
        async fn apply(&self, action: PowerAction) -> Result<()> {
            // L'action est synchronique cote API : on encapsule pour ne pas bloquer.
            tokio::task::spawn_blocking(move || apply_blocking(action))
                .await
                .map_err(|e| Error::Os(format!("spawn_blocking join: {e}")))?
        }
    }

    fn apply_blocking(action: PowerAction) -> Result<()> {
        match action {
            PowerAction::LockWorkstation => unsafe {
                // SAFETY: LockWorkStation est une API Win32 thread-safe et sans
                // precondition (au-dela de "il y a une session interactive").
                LockWorkStation()
                    .map_err(|e| Error::Os(format!("LockWorkStation: {e}")))?;
                Ok(())
            },
            PowerAction::Sleep => unsafe {
                // SAFETY: SetSuspendState n'a pas de precondition particuliere
                // sur le thread appelant.
                let ok = SetSuspendState(false, true, false);
                if ok.as_bool() {
                    Ok(())
                } else {
                    Err(Error::Os("SetSuspendState(Sleep) a echoue".into()))
                }
            },
            PowerAction::Hibernate => unsafe {
                // SAFETY: idem Sleep.
                let ok = SetSuspendState(true, true, false);
                if ok.as_bool() {
                    Ok(())
                } else {
                    Err(Error::Os("SetSuspendState(Hibernate) a echoue".into()))
                }
            },
            PowerAction::Restart => unsafe {
                // SAFETY: enable_shutdown_privilege a deja gere le token ;
                // ExitWindowsEx sans privilege renvoie une erreur que l'on
                // remonte proprement.
                enable_shutdown_privilege()?;
                ExitWindowsEx(
                    EWX_REBOOT | EWX_FORCEIFHUNG,
                    SHTDN_REASON_MAJOR_OTHER,
                )
                .map_err(|e| Error::Os(format!("ExitWindowsEx(Restart): {e}")))?;
                Ok(())
            },
            PowerAction::Shutdown => unsafe {
                // SAFETY: idem Restart.
                enable_shutdown_privilege()?;
                ExitWindowsEx(
                    EWX_SHUTDOWN | EWX_FORCEIFHUNG,
                    SHTDN_REASON_MAJOR_OTHER,
                )
                .map_err(|e| Error::Os(format!("ExitWindowsEx(Shutdown): {e}")))?;
                Ok(())
            },
        }
    }

    /// Active `SeShutdownPrivilege` pour le token courant.
    ///
    /// # Safety
    /// Appelle plusieurs fonctions Win32 en sequence avec un token de processus
    /// proprement obtenu. Les structures TOKEN_PRIVILEGES sont initialisees
    /// integralement avant utilisation. Aucun pointeur n'est partage.
    unsafe fn enable_shutdown_privilege() -> Result<()> {
        let mut token = HANDLE::default();
        // SAFETY: OpenProcessToken attend un handle de processus et un &mut HANDLE
        // valides ; tous deux sont fournis correctement.
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
            .map_err(|e| Error::Os(format!("OpenProcessToken: {e}")))?;
        }

        let priv_name: Vec<u16> = "SeShutdownPrivilege"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut luid = LUID::default();
        // SAFETY: priv_name est nul-terminee.
        unsafe {
            LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(priv_name.as_ptr()), &mut luid)
                .map_err(|e| Error::Os(format!("LookupPrivilegeValueW: {e}")))?;
        }

        let mut tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };

        // SAFETY: tp est totalement initialise.
        unsafe {
            AdjustTokenPrivileges(token, false, Some(&mut tp), 0, None, None)
                .map_err(|e| Error::Os(format!("AdjustTokenPrivileges: {e}")))?;
        }

        // GetLastError() == ERROR_NOT_ALL_ASSIGNED veut dire qu'on n'avait pas
        // le privilege. On ne le verifie pas ici : ExitWindowsEx renverra
        // ACCESS_DENIED si ca n'a pas pris.
        Ok(())
    }
}
