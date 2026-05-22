//! Helpers D3D11 + `IMFDXGIDeviceManager` pour piloter un encodeur MFT
//! hardware (NVENC, Quick Sync Video, AMD AMF) côté V3.3.
//!
//! ## Vue d'ensemble
//!
//! Un MFT hardware reçoit/produit ses samples via la mémoire vidéo (textures
//! `ID3D11Texture2D`) plutôt que via la mémoire système. Pour ça il faut :
//!
//! 1. Créer un **`ID3D11Device`** (BGRA = `D3D11_CREATE_DEVICE_BGRA_SUPPORT`).
//! 2. Créer un **`IMFDXGIDeviceManager`** et lui passer le D3D11 device via
//!    `ResetDevice()`.
//! 3. Au MFT : `ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, &manager)`.
//! 4. Le MFT acceptera désormais des samples liés à des `ID3D11Texture2D`
//!    (allocation via `MFCreateDXSurfaceBuffer`).
//!
//! ## État de l'implémentation
//!
//! - ✅ `create_d3d11_device()` : crée un device hardware-accélérée.
//! - ✅ `create_dxgi_manager(device)` : wrap dans `IMFDXGIDeviceManager`.
//! - 🚧 Wiring complet dans `MfH264Encoder` : V3.3 step 2.
//!
//! Ce module est un **fondation** : les fonctions compilent et fonctionnent
//! isolément, mais ne sont pas encore appelées par l'encoder principal. Le
//! plumbing (sélection MFT hardware via `MFTEnumEx` filtré sur
//! `MFT_ENUM_FLAG_HARDWARE`, allocation samples DXGI, ProcessInput/Output
//! avec textures, gestion async events) demande ~300 lignes de plus.

#![cfg(windows)]
#![allow(missing_docs)]

use okvm_core::{Error, Result};

use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};

/// Wrapper du device D3D11 + son immediate context.
pub struct D3D11Resources {
    pub device: ID3D11Device,
    pub immediate_context: ID3D11DeviceContext,
}

/// Crée un `ID3D11Device` hardware-accélérée avec les flags requis pour
/// un MFT vidéo (BGRA + VIDEO_SUPPORT).
///
/// On essaie les feature levels du plus récent au plus ancien — DX 11.1
/// est nécessaire pour les MFT hardware modernes (NVENC, Intel QSV récents).
///
/// # Erreurs
/// Si aucun adapter D3D11 hardware n'est dispo (machine virtuelle sans GPU
/// passthrough, RDP, …) renvoie une erreur — le caller doit alors retomber
/// sur le MFT software via `CoCreateInstance(CLSID_CMSH264EncoderMFT)`.
pub fn create_d3d11_device() -> Result<D3D11Resources> {
    let feature_levels = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
    ];
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;

    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut chosen_feature_level = D3D_FEATURE_LEVEL_11_0;

    // SAFETY: D3D11CreateDevice est l'API standard ; on lui passe les
    // pointeurs out et un slice de feature levels valides.
    unsafe {
        D3D11CreateDevice(
            None, // adapter par défaut
            D3D_DRIVER_TYPE_HARDWARE,
            None,  // pas de DLL software
            flags, // BGRA + Video
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut chosen_feature_level),
            Some(&mut context),
        )
        .map_err(|e| Error::other(format!("D3D11CreateDevice: {e}")))?;
    }

    let device = device.ok_or_else(|| Error::other("D3D11 device null après création"))?;
    let immediate_context = context.ok_or_else(|| Error::other("D3D11 immediate context null"))?;

    tracing::info!(
        ?chosen_feature_level,
        "D3D11 device créé pour pipeline vidéo hardware"
    );
    Ok(D3D11Resources {
        device,
        immediate_context,
    })
}

/// Crée un `IMFDXGIDeviceManager` lié au D3D11 device fourni.
///
/// À passer ensuite au MFT via :
/// ```ignore
/// transform.ProcessMessage(
///     MFT_MESSAGE_SET_D3D_MANAGER,
///     manager.as_raw() as usize,
/// )?;
/// ```
///
/// # Erreurs
/// Si `MFCreateDXGIDeviceManager` ou `ResetDevice` échoue.
pub fn create_dxgi_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager> {
    let mut reset_token: u32 = 0;
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    // SAFETY: API MF standard ; reset_token et manager sont des out-params
    // initialisés par l'appel.
    unsafe {
        MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
            .map_err(|e| Error::other(format!("MFCreateDXGIDeviceManager: {e}")))?;
    }
    let manager = manager.ok_or_else(|| Error::other("DXGI manager null après création"))?;
    // SAFETY: device est vivant, reset_token vient de MFCreateDXGIDeviceManager.
    unsafe {
        manager
            .ResetDevice(device, reset_token)
            .map_err(|e| Error::other(format!("IMFDXGIDeviceManager::ResetDevice: {e}")))?;
    }
    Ok(manager)
}

/// Tente de créer la paire `(D3D11 device, DXGI manager)` en une étape.
/// Pratique pour les call-sites qui veulent juste « préparer le contexte hardware ».
///
/// # Erreurs
/// Voir [`create_d3d11_device`] et [`create_dxgi_manager`].
pub fn create_d3d11_and_manager() -> Result<(D3D11Resources, IMFDXGIDeviceManager)> {
    let res = create_d3d11_device()?;
    let mgr = create_dxgi_manager(&res.device)?;
    Ok((res, mgr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d3d11_device_creates_or_fails_gracefully() {
        // Sur runner CI sans GPU (ou VM sans D3D11 passthrough), ça échoue.
        // On accepte les 2 outcomes — l'important est que l'API ne panique pas
        // et qu'on logge le HRESULT.
        match create_d3d11_device() {
            Ok(_) => {
                eprintln!("D3D11 device OK — machine a un GPU.");
            }
            Err(e) => {
                eprintln!("D3D11 device unavailable: {e} (normal en CI/VM)");
            }
        }
    }

    #[test]
    fn dxgi_manager_pipeline_or_fail() {
        if let Ok(d3d) = create_d3d11_device() {
            // Si on a un D3D11, on doit pouvoir créer un DXGI manager.
            let _mgr = create_dxgi_manager(&d3d.device)
                .expect("DXGI manager doit se créer si D3D11 existe");
        }
    }
}
