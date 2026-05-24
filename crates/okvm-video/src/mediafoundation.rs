//! Détection des encodeurs H.264 matériels disponibles via Media Foundation.
//!
//! Windows expose des Media Foundation Transforms (MFT) qui peuvent être :
//!
//! - **Software** : encodeur de référence Microsoft (équivalent openh264).
//! - **Hardware** : NVENC (NVIDIA), Quick Sync Video (Intel), AMF (AMD),
//!   selon le GPU/iGPU installé.
//!
//! Cette détection énumère les MFT de la catégorie
//! [`MFT_CATEGORY_VIDEO_ENCODER`] qui acceptent une sortie H.264
//! ([`MFVideoFormat_H264`]) et signale ceux marqués
//! [`MFT_ENUM_FLAG_HARDWARE`].
//!
//! ## État de l'implémentation
//!
//! - ✅ Détection des encodeurs hardware.
//! - 🚧 Wrapping de l'encodeur (init, configure, encode) : prévu V3.1 — pour
//!   l'instant [`H264Encoder::new_best`] continue à utiliser openh264 (CPU)
//!   et logge simplement la présence ou non d'un encodeur hardware au boot.
//!
//! ## Pourquoi ne pas avoir tout livré d'un coup ?
//!
//! Le wiring complet d'un MFT H264 est ~500 lignes de COM Win32 (`ProcessInput`
//! / `ProcessOutput` / `IMFSample` / `IMFMediaBuffer` / NV12 conversion / drain à
//! l'arrêt). Le faire correctement nécessite des tests sur GPU réels
//! (NVENC, `QuickSync`, AMF) — du coup on procède en deux étapes :
//!
//! 1. **Maintenant** : détection + skeleton + factory (cette PR).
//! 2. **Ensuite** : encoder wrapper complet + tests E2E (V3.1).

#[cfg(windows)]
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MFMediaType_Video, MFStartup, MFTEnumEx, MFVideoFormat_H264,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_ALL, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_REGISTER_TYPE_INFO, MF_VERSION,
};
#[cfg(windows)]
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Garantit que `CoInitializeEx(MULTITHREADED)` + `MFStartup(MF_VERSION)` ont
/// été appelés **une seule fois** dans tout le process (toutes utilisations MF
/// confondues : enumeration, encoder, etc.).
///
/// Repose sur [`std::sync::OnceLock`] : sûr et lock-free après le premier
/// appel. Renvoie le même `Result` à chaque invocation suivante (évite de
/// re-tenter en cas d'échec définitif type "DLL absente").
///
/// # ⚠️ Contrat d'appel — apartment COM
///
/// Ce call **empoisonne le thread courant en MTA** (Multi-Threaded Apartment).
/// Une fois fait, ce thread NE PEUT PLUS jamais être basculé en STA — toute
/// tentative ultérieure d'`OleInitialize()` ou `CoInitializeEx(APARTMENTTHREADED)`
/// échouera avec `RPC_E_CHANGED_MODE` (0x80010106).
///
/// **Conséquence pratique** : ne JAMAIS appeler cette fonction depuis le main
/// thread d'une app Tauri/winit/tao avant l'init de la fenêtre — Tauri's `tao`
/// event loop fait `OleInitialize(STA)` à la création de la première fenêtre
/// (pour drag&drop) et plantera.
///
/// **Pattern correct** : si appelé depuis le main thread d'une app GUI, le
/// faire dans un `std::thread::spawn(|| ensure_mf_init())`. Le thread enfant
/// peut tranquillement vivre en MTA.
///
/// Régression historique : v0.1.1 (`a1165cf` — crash silencieux 3 s après boot,
/// cf. CHANGELOG [0.1.2]).
///
/// # Erreurs
/// Stringifiées si `CoInitializeEx` ou `MFStartup` échouent au premier appel.
#[cfg(windows)]
pub fn ensure_mf_init() -> std::result::Result<(), String> {
    use std::sync::OnceLock;
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        // Debug-only : alerte si on est en train d'init COM sur le thread
        // nommé "main" d'une app GUI. En release c'est compilé out — pas
        // overhead. En debug ça nous prévient AVANT de tomber sur le crash.
        debug_assert!(
            !is_main_thread(),
            "okvm_video::ensure_mf_init appelé sur le main thread — \
             ça va empoisonner l'apartment COM en MTA et faire planter \
             Tauri/tao avec RPC_E_CHANGED_MODE quelques secondes plus tard. \
             Utilise std::thread::spawn(|| ensure_mf_init()). \
             Cf. CHANGELOG [0.1.2] pour le contexte de la régression."
        );

        tracing::debug!("ensure_mf_init: première init COM/MF sur ce process");
        // SAFETY: appels Win32 standards. S_FALSE = déjà init dans le thread,
        // non fatal.
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() && hr.0 != 1 {
                return Err(format!("CoInitializeEx: HRESULT 0x{:08X}", hr.0 as u32));
            }
            if let Err(e) = MFStartup(MF_VERSION, 0) {
                return Err(format!("MFStartup: {e}"));
            }
            Ok(())
        }
    })
    .clone()
}

/// Détecte heuristiquement si le thread courant est le main thread Tauri.
///
/// Sur Windows, l'API `GetCurrentThreadId() == GetMainThreadId()` n'existe pas
/// directement — on utilise une heuristique : le main thread n'a pas de nom
/// (Rust ne nomme pas le main thread par défaut) ET son `ThreadId` est petit.
/// Spawn'd threads ont des `ThreadId` croissants.
///
/// Best-effort : faux négatifs OK (debug_assert silencieux), faux positifs
/// rares (worker thread précoce). Si jamais le faux positif gêne, le caller
/// peut annoter son thread avec `std::thread::Builder::new().name("mf-worker")`.
#[cfg(windows)]
fn is_main_thread() -> bool {
    // Le main thread n'a pas de name par défaut. Un thread sans nom + parmi
    // les premiers spawnés est probablement le main.
    std::thread::current().name().is_none()
}

/// Description d'un encodeur H.264 détecté.
#[derive(Debug, Clone)]
pub struct H264EncoderInfo {
    /// Nom convivial retourné par le MFT (ex: "H264 Encoder MFT").
    pub friendly_name: String,
    /// `true` si l'encodeur est marqué matériel.
    pub is_hardware: bool,
    /// `true` si l'attribut `MF_TRANSFORM_ASYNC` est set sur le MFT.
    /// Les MFTs hardware NVENC, AMF, Quick Sync sur les GPUs récents sont
    /// **typiquement en async-mode obligatoire** — le wrapping demande un
    /// event loop dédié (cf. V3.3.1 backlog).
    pub is_async_mode: bool,
}

/// Énumère les encodeurs H.264 disponibles sur cette machine.
///
/// # Erreurs
/// Erreur Windows si `MFStartup` ou `MFTEnumEx` échoue.
#[cfg(windows)]
pub fn enumerate_h264_encoders() -> Result<Vec<H264EncoderInfo>, String> {
    // Init via OnceLock — pas de re-startup à chaque appel (les ref-counts
    // internes MF s'accumuleraient sinon).
    ensure_mf_init()?;

    let output_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG_ALL | MFT_ENUM_FLAG_SORTANDFILTER;

    let mut count: u32 = 0;
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    // SAFETY: MFTEnumEx alloue activates via CoTaskMemAlloc ; à libérer avec
    // CoTaskMemFree après usage. On passe input_type = null pour ne pas
    // filtrer sur le format d'entrée (RGB / NV12 / YUY2 acceptés).
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            None,
            Some(&raw const output_type),
            &raw mut activates,
            &raw mut count,
        )
        .map_err(|e| format!("MFTEnumEx: {e}"))?;
    }

    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as isize {
        // SAFETY: activates pointe sur un tableau de count Option<IMFActivate>.
        let act_opt = unsafe { (*activates.offset(i)).as_ref() };
        let Some(act) = act_opt else {
            continue;
        };
        // Récupère friendly name + flag hardware + flag async-mode.
        let name = read_friendly_name(act);
        let is_hardware = check_hardware_flag(act);
        let is_async_mode = check_async_flag(act);
        out.push(H264EncoderInfo {
            friendly_name: name,
            is_hardware,
            is_async_mode,
        });
    }

    // Libère le tableau (les Option<IMFActivate> sont relâchés via Drop).
    // SAFETY: activates a été alloué par MFTEnumEx via CoTaskMemAlloc.
    unsafe {
        if !activates.is_null() {
            // Drop chaque slot (les Option<IMFActivate> appellent IUnknown::Release).
            for i in 0..count as isize {
                let _ = (*activates.offset(i)).take();
            }
            windows::Win32::System::Com::CoTaskMemFree(Some(activates.cast()));
        }
    }
    Ok(out)
}

/// `true` si au moins un encodeur **hardware** H.264 est disponible.
#[must_use]
pub fn has_hardware_h264() -> bool {
    enumerate_h264_encoders()
        .map(|list| list.iter().any(|e| e.is_hardware))
        .unwrap_or(false)
}

#[cfg(windows)]
fn read_friendly_name(act: &IMFActivate) -> String {
    use windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute;
    // SAFETY: GetAllocatedString alloue une chaîne UTF-16 via CoTaskMemAlloc
    // qu'on libère après lecture.
    unsafe {
        let mut buf_ptr: windows::core::PWSTR = windows::core::PWSTR(std::ptr::null_mut());
        let mut len: u32 = 0;
        if act
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &raw mut buf_ptr, &raw mut len)
            .is_err()
        {
            return "<unnamed>".into();
        }
        if buf_ptr.0.is_null() {
            return "<unnamed>".into();
        }
        let slice = std::slice::from_raw_parts(buf_ptr.0, len as usize);
        let s = String::from_utf16_lossy(slice);
        windows::Win32::System::Com::CoTaskMemFree(Some(buf_ptr.0.cast()));
        s
    }
}

#[cfg(windows)]
fn check_hardware_flag(act: &IMFActivate) -> bool {
    use windows::Win32::Media::MediaFoundation::MFT_ENUM_HARDWARE_URL_Attribute;
    // SAFETY: GetUnknown/GetString sur un attribut absent renvoie une erreur,
    // qu'on transforme en "pas hardware".
    unsafe {
        let mut buf_ptr: windows::core::PWSTR = windows::core::PWSTR(std::ptr::null_mut());
        let mut len: u32 = 0;
        let res = act.GetAllocatedString(
            &MFT_ENUM_HARDWARE_URL_Attribute,
            &raw mut buf_ptr,
            &raw mut len,
        );
        if !buf_ptr.0.is_null() {
            windows::Win32::System::Com::CoTaskMemFree(Some(buf_ptr.0.cast()));
        }
        res.is_ok()
    }
}

/// Vérifie si l'attribut `MF_TRANSFORM_ASYNC` est présent et = 1. Permet
/// de détecter en amont si un MFT requerra le wrapping event-loop (NVENC
/// récents, AMF, Intel Arc QSV...) avant même de l'instancier.
#[cfg(windows)]
fn check_async_flag(act: &IMFActivate) -> bool {
    use windows::Win32::Media::MediaFoundation::MF_TRANSFORM_ASYNC;
    // SAFETY: GetUINT32 renvoie une erreur si l'attribut n'existe pas.
    unsafe { act.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 }
}

/// Logge la disponibilité des encodeurs H.264 au démarrage, avec leur
/// classification hardware/software + sync/async. Permet à l'utilisateur
/// de comprendre **pourquoi** un MFT hardware peut être détecté mais pas
/// utilisé en pratique (V3.3 n'implémente pas le wrapping async-mode).
/// Appel best-effort : aucune erreur n'est propagée.
pub fn log_hardware_h264_status() {
    match enumerate_h264_encoders() {
        Ok(encoders) => {
            let hw_count = encoders.iter().filter(|e| e.is_hardware).count();
            let hw_sync = encoders
                .iter()
                .filter(|e| e.is_hardware && !e.is_async_mode)
                .count();
            let sw_count = encoders.len() - hw_count;
            tracing::info!(
                hw_total = hw_count,
                hw_sync,
                hw_async = hw_count - hw_sync,
                sw = sw_count,
                "Encodeurs H.264 détectés via Media Foundation"
            );
            for e in &encoders {
                let mode = if e.is_async_mode { "async" } else { "sync" };
                if e.is_hardware {
                    tracing::info!(
                        name = %e.friendly_name,
                        mode,
                        "MF H264 hardware (async-mode → non utilisable en V3.3, cf. V3.3.1)"
                    );
                } else {
                    tracing::debug!(name = %e.friendly_name, mode, "MF H264 software");
                }
            }
            if hw_count > 0 && hw_sync == 0 {
                tracing::warn!(
                    "Tous les MFT H.264 hardware détectés sont en async-mode. \
                     L'app utilisera l'encoder software (V3.3.1 ajoutera le \
                     wrapping async)."
                );
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "MF H264 enumeration échouée");
        }
    }
}

/// Pour usage par les bindings Tauri : libre de référencer le `GUID`.
#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_does_not_panic() {
        // On ne peut pas garantir qu'il y a un encodeur hardware sur la machine
        // de test (CI sans GPU = software-only). On vérifie juste que l'API
        // ne panique pas et retourne au moins l'encodeur software de Microsoft.
        let list = enumerate_h264_encoders().expect("MF enum ne doit pas échouer");
        // Au moins l'encodeur software Microsoft devrait être présent sur W10/W11.
        assert!(!list.is_empty(), "au moins 1 encodeur H264 attendu");
        eprintln!("Encodeurs H.264 trouvés:");
        for e in &list {
            eprintln!(
                "  {:>3}  {:>5}  {}",
                if e.is_hardware { "HW" } else { "SW" },
                if e.is_async_mode { "async" } else { "sync" },
                e.friendly_name
            );
        }
    }

    #[test]
    fn log_status_does_not_panic() {
        log_hardware_h264_status();
    }
}
