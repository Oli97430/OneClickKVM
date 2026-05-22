//! Commandes Tauri exposees au frontend.

use std::net::SocketAddr;

use tauri::State;

use okvm_ipc::{AppStatus, BackendEvent, PairRequest, PairResult, PeerView};

use crate::state::AppState;

#[tauri::command]
pub async fn get_app_status(state: State<'_, AppState>) -> Result<AppStatus, String> {
    let peers = state.peers.lock();
    let connected = peers.values().filter(|p| p.online && p.paired).count() as u32;
    Ok(AppStatus {
        self_identity: state.identity.public,
        self_fingerprint: state.self_fingerprint(),
        self_hostname: state.hostname.clone(),
        connected_peers: connected,
        listening: state.is_listening(),
    })
}

#[tauri::command]
pub async fn list_peers(state: State<'_, AppState>) -> Result<Vec<PeerView>, String> {
    Ok(state.peer_list())
}

#[tauri::command]
pub async fn start_listening(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state
        .start_services(app_handle.clone())
        .map_err(|e| e.to_string())?;
    emit_status_changed(&app_handle, &state).await;
    Ok(())
}

#[tauri::command]
pub async fn stop_listening(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.stop_services().await;
    emit_status_changed(&app_handle, &state).await;
    emit_event(
        &app_handle,
        BackendEvent::Notification {
            level: okvm_ipc::NotificationLevel::Info,
            title: "Services arretes".into(),
            body: "Plus de connexions entrantes ne seront acceptees.".into(),
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn pair_with_peer(
    req: PairRequest,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<PairResult, String> {
    let addr: SocketAddr = match req.address.parse() {
        Ok(a) => a,
        Err(e) => {
            return Ok(PairResult::Failure {
                reason: format!("adresse invalide: {e}"),
            })
        }
    };
    let pin = if req.pin.trim().is_empty() {
        None
    } else {
        Some(req.pin)
    };
    match state.connect_outbound(addr, pin, app_handle.clone()).await {
        Ok(id) => {
            let fp = id.fingerprint();
            let name = state
                .peers
                .lock()
                .get(&id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "(inconnu)".into());
            emit_status_changed(&app_handle, &state).await;
            Ok(PairResult::Success {
                device_id: id,
                fingerprint: fp,
                name,
            })
        }
        Err(e) => Ok(PairResult::Failure {
            reason: e.to_string(),
        }),
    }
}

#[tauri::command]
pub async fn unpair_peer(
    device_id: [u8; 32],
    state: State<'_, AppState>,
) -> Result<(), String> {
    let did = okvm_core::DeviceId(device_id);
    let removed = state.sessions.lock().remove(&did);
    if let Some(sr) = removed {
        sr.shutdown();
    }
    state.peers.lock().remove(&did);
    state.remove_peer_from_grid(&did);
    state.save_peers();
    Ok(())
}

/// Active le mode "master" : capture clavier/souris et forwarde vers les pairs.
#[tauri::command]
pub async fn become_master(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state
        .become_master(app_handle.clone())
        .await
        .map_err(|e| e.to_string())?;
    emit_status_changed(&app_handle, &state).await;
    Ok(())
}

/// Desactive le mode master.
#[tauri::command]
pub async fn stop_master(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.stop_master(app_handle.clone());
    emit_status_changed(&app_handle, &state).await;
    Ok(())
}

/// Envoie une liste de fichiers (chemins absolus) a un pair connecte.
///
/// Le pair doit etre dans `state.sessions`. Cree un `FileSender` avec 4 threads
/// par defaut, l'execute dans une task background et notifie l'UI a la fin.
#[tauri::command]
pub async fn send_files(
    device_id: [u8; 32],
    files: Vec<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let did = okvm_core::DeviceId(device_id);
    let files_tx = state
        .sessions
        .lock()
        .get(&did)
        .map(|s| s.files_tx.clone());
    let Some(tx) = files_tx else {
        return Err("session inconnue".into());
    };

    let paths: Vec<std::path::PathBuf> = files.into_iter().map(std::path::PathBuf::from).collect();
    let count = paths.len();
    let total_bytes: u64 = paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();

    let app_h = app_handle.clone();
    let peer_name = state
        .peers
        .lock()
        .get(&did)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "(inconnu)".into());
    let transfer_uuid = uuid::Uuid::new_v4();

    tokio::spawn(async move {
        let last_emit = std::sync::Arc::new(parking_lot::Mutex::new(
            std::time::Instant::now() - std::time::Duration::from_secs(60),
        ));
        let app_for_cb = app_h.clone();
        let peer_name_cb = peer_name.clone();
        let mut sender = okvm_files::FileSender::new(tx);
        sender.transfer_id = transfer_uuid;
        let sender = sender.with_progress(move |bytes_done, bytes_total, current_file| {
            let now = std::time::Instant::now();
            let should_emit = {
                let mut g = last_emit.lock();
                let elapsed = now.duration_since(*g);
                let final_frame = bytes_done >= bytes_total && bytes_total > 0;
                if elapsed >= std::time::Duration::from_millis(250) || final_frame {
                    *g = now;
                    true
                } else {
                    false
                }
            };
            if should_emit {
                let state = if bytes_done >= bytes_total && bytes_total > 0 {
                    "done"
                } else {
                    "running"
                };
                let view = okvm_ipc::TransferProgressView {
                    transfer_id: transfer_uuid.to_string(),
                    direction: "outbound".into(),
                    peer_name: peer_name_cb.clone(),
                    current_file: current_file.to_string(),
                    bytes_done,
                    bytes_total,
                    state: state.into(),
                    error: None,
                };
                let _ = emit_event_static(
                    &app_for_cb,
                    okvm_ipc::BackendEvent::TransferProgress { progress: view },
                );
            }
        });

        match sender.send_paths(&paths).await {
            Ok(_) => {
                let _ = emit_event_static(
                    &app_h,
                    okvm_ipc::BackendEvent::Notification {
                        level: okvm_ipc::NotificationLevel::Success,
                        title: "Transfert termine".into(),
                        body: format!(
                            "{count} fichiers ({:.1} Mo) envoyes a {peer_name}",
                            total_bytes as f64 / 1_048_576.0
                        ),
                    },
                );
            }
            Err(e) => {
                let _ = emit_event_static(
                    &app_h,
                    okvm_ipc::BackendEvent::TransferProgress {
                        progress: okvm_ipc::TransferProgressView {
                            transfer_id: transfer_uuid.to_string(),
                            direction: "outbound".into(),
                            peer_name: peer_name.clone(),
                            current_file: String::new(),
                            bytes_done: 0,
                            bytes_total: total_bytes,
                            state: "error".into(),
                            error: Some(e.to_string()),
                        },
                    },
                );
                let _ = emit_event_static(
                    &app_h,
                    okvm_ipc::BackendEvent::Notification {
                        level: okvm_ipc::NotificationLevel::Error,
                        title: "Echec transfert".into(),
                        body: e.to_string(),
                    },
                );
            }
        }
    });
    Ok(())
}

/// Renvoie le chemin du repertoire d'arrivee des fichiers (Inbox).
#[tauri::command]
pub async fn get_inbox_dir(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.inbox_dir.to_string_lossy().into_owned())
}

/// Active le partage audio (WASAPI loopback) vers tous les pairs connectes.
#[tauri::command]
pub async fn start_audio_share(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.start_audio_share().await.map_err(|e| e.to_string())?;
    emit_event(
        &app_handle,
        BackendEvent::Notification {
            level: okvm_ipc::NotificationLevel::Success,
            title: "Audio partage".into(),
            body: "Le son de ce PC est diffuse sur les pairs.".into(),
        },
    );
    Ok(())
}

/// Desactive le partage audio.
#[tauri::command]
pub async fn stop_audio_share(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.stop_audio_share();
    emit_event(
        &app_handle,
        BackendEvent::Notification {
            level: okvm_ipc::NotificationLevel::Info,
            title: "Audio arrete".into(),
            body: "Plus de son partage.".into(),
        },
    );
    Ok(())
}

/// `true` si le partage audio est actif.
#[tauri::command]
pub async fn is_audio_sharing(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.is_audio_sharing())
}

/// Active le partage video (capture ecran) vers tous les pairs connectes.
#[tauri::command]
pub async fn start_video_share(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.start_video_share().await.map_err(|e| e.to_string())?;
    emit_event(
        &app_handle,
        BackendEvent::Notification {
            level: okvm_ipc::NotificationLevel::Success,
            title: "Ecran partage".into(),
            body: "Capture en cours, diffusion MJPEG vers les pairs.".into(),
        },
    );
    Ok(())
}

/// Desactive le partage video.
#[tauri::command]
pub async fn stop_video_share(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.stop_video_share();
    emit_event(
        &app_handle,
        BackendEvent::Notification {
            level: okvm_ipc::NotificationLevel::Info,
            title: "Partage ecran arrete".into(),
            body: "Plus d'ecran partage.".into(),
        },
    );
    Ok(())
}

/// `true` si le partage video est actif.
#[tauri::command]
pub async fn is_video_sharing(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.is_video_sharing())
}

/// Lit la configuration applicative.
#[tauri::command]
pub async fn get_app_config() -> Result<okvm_config::AppConfig, String> {
    okvm_config::load_app_config().map_err(|e| e.to_string())
}

/// Sauvegarde la configuration applicative.
#[tauri::command]
pub async fn set_app_config(cfg: okvm_config::AppConfig) -> Result<(), String> {
    okvm_config::save_app_config(&cfg).map_err(|e| e.to_string())
}

/// Reset complet : supprime config + peers + identite. Destructif.
/// Necessite un redemarrage de l'app pour reprendre.
#[tauri::command]
pub async fn reset_all_settings() -> Result<(), String> {
    okvm_config::reset_all().map_err(|e| e.to_string())
}

/// Ouvre dans l'explorateur Windows le dossier `%APPDATA%\OneClickKVM` qui
/// contient `config.json`, `peers.json` et `identity.dpapi`. Utile pour
/// debug / support / sauvegarde manuelle.
#[tauri::command]
pub async fn open_config_dir() -> Result<(), String> {
    let dir = okvm_config::config_dir().map_err(|e| e.to_string())?;
    // S'assure que le dossier existe (sinon explorer.exe l'ouvre vide ou échoue).
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!("create_dir_all: {e}"));
    }
    open_in_explorer(&dir).map_err(|e| e.to_string())
}

/// Ouvre dans l'explorateur Windows le dossier de réception des fichiers
/// (`Documents/OneClickKVM/Inbox/`).
#[tauri::command]
pub async fn open_inbox_dir(state: State<'_, AppState>) -> Result<(), String> {
    let dir = state.inbox_dir.clone();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!("create_dir_all: {e}"));
    }
    open_in_explorer(&dir).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn open_in_explorer(path: &std::path::Path) -> Result<(), String> {
    // explorer.exe accepte un chemin et l'ouvre dans une nouvelle fenêtre.
    // On ne `wait()` pas pour ne pas bloquer la commande Tauri.
    std::process::Command::new("explorer.exe")
        .arg(path.as_os_str())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("explorer.exe: {e}"))
}

/// Infos systeme / build pour le panneau A propos.
#[tauri::command]
pub async fn get_about_info(state: State<'_, AppState>) -> Result<AboutInfo, String> {
    let h264_encoders: Vec<H264EncoderView> = okvm_video::enumerate_h264_encoders()
        .unwrap_or_default()
        .into_iter()
        .map(|e| H264EncoderView {
            friendly_name: e.friendly_name,
            is_hardware: e.is_hardware,
        })
        .collect();
    let has_hw = h264_encoders.iter().any(|e| e.is_hardware);
    Ok(AboutInfo {
        app_name: "OneClick KVM".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        rust_target: "x86_64-pc-windows-msvc".into(),
        license: "MIT OR Apache-2.0".into(),
        self_fingerprint: state.self_fingerprint(),
        self_hostname: state.hostname.clone(),
        inbox_dir: state.inbox_dir.to_string_lossy().into_owned(),
        tcp_port: state.tcp_port,
        h264_encoders,
        has_hardware_h264: has_hw,
    })
}

#[derive(serde::Serialize)]
pub struct AboutInfo {
    pub app_name: String,
    pub version: String,
    pub rust_target: String,
    pub license: String,
    pub self_fingerprint: okvm_core::Fingerprint,
    pub self_hostname: String,
    pub inbox_dir: String,
    pub tcp_port: u16,
    pub h264_encoders: Vec<H264EncoderView>,
    pub has_hardware_h264: bool,
}

#[derive(serde::Serialize)]
pub struct H264EncoderView {
    pub friendly_name: String,
    pub is_hardware: bool,
}

/// Active le mode d'appairage : génère un PIN à 6 chiffres valide
/// `duration_secs` secondes (clamped à [10, 600]). Tant que ce mode est actif,
/// un nouveau pair peut s'appairer en fournissant ce PIN.
///
/// Si un mode est déjà actif et non expiré, **renvoie l'état courant sans
/// regénérer le PIN** — évite de surprendre l'humain à l'autre PC en lui
/// changeant le code de devinette pendant qu'il le tape.
#[tauri::command]
pub async fn start_pairing_mode(
    duration_secs: u64,
    state: State<'_, AppState>,
) -> Result<PairingModeView, String> {
    if let Some(existing) = state.pairing_mode_snapshot() {
        return Ok(PairingModeView {
            active: true,
            pin: Some(existing.pin.to_string()),
            expires_at_ms: Some(existing.expires_at_ms),
        });
    }
    let secs = duration_secs.clamp(10, 600);
    let pm = state.start_pairing_mode(std::time::Duration::from_secs(secs));
    Ok(PairingModeView {
        active: true,
        pin: Some(pm.pin.to_string()),
        expires_at_ms: Some(pm.expires_at_ms),
    })
}

/// Désactive le mode d'appairage immédiatement.
#[tauri::command]
pub async fn stop_pairing_mode(state: State<'_, AppState>) -> Result<(), String> {
    state.stop_pairing_mode();
    Ok(())
}

/// Renvoie l'état du mode d'appairage. Si expiré, le nettoie et renvoie inactif.
#[tauri::command]
pub async fn get_pairing_mode_status(
    state: State<'_, AppState>,
) -> Result<PairingModeView, String> {
    Ok(match state.pairing_mode_snapshot() {
        Some(pm) => PairingModeView {
            active: true,
            pin: Some(pm.pin.to_string()),
            expires_at_ms: Some(pm.expires_at_ms),
        },
        None => PairingModeView {
            active: false,
            pin: None,
            expires_at_ms: None,
        },
    })
}

#[derive(serde::Serialize)]
pub struct PairingModeView {
    pub active: bool,
    pub pin: Option<String>,
    pub expires_at_ms: Option<u64>,
}

/// Liste les écrans locaux pour le sélecteur de moniteur dans Settings.
/// L'index correspond à celui consommé par `WindowsCaptureSource.start`.
#[tauri::command]
pub async fn list_local_screens(state: State<'_, AppState>) -> Result<Vec<ScreenView>, String> {
    Ok(state
        .capabilities
        .screens
        .iter()
        .map(|s| ScreenView {
            index: s.index,
            is_primary: s.is_primary,
            width_px: s.width_px,
            height_px: s.height_px,
            origin_x: s.origin_x,
            origin_y: s.origin_y,
        })
        .collect())
}

#[derive(serde::Serialize)]
pub struct ScreenView {
    pub index: u32,
    pub is_primary: bool,
    pub width_px: u32,
    pub height_px: u32,
    pub origin_x: i32,
    pub origin_y: i32,
}

/// Renvoie la grille spatiale des pairs (pour visualisation UI).
#[tauri::command]
pub async fn get_grid(state: State<'_, AppState>) -> Result<Vec<GridPeerView>, String> {
    let switch = state.switch.lock();
    let mut out: Vec<GridPeerView> = switch
        .grid
        .peers
        .values()
        .map(|p| {
            let bb = p.bbox();
            GridPeerView {
                name: p.name.clone(),
                hotkey: p.hotkey_index,
                bbox: ScreenRect {
                    x: bb.x,
                    y: bb.y,
                    w: bb.w,
                    h: bb.h,
                },
                is_self: p.id.is_nil(),
            }
        })
        .collect();
    out.sort_by_key(|p| p.bbox.x);
    Ok(out)
}

#[derive(serde::Serialize)]
pub struct GridPeerView {
    pub name: String,
    pub hotkey: Option<u8>,
    pub bbox: ScreenRect,
    pub is_self: bool,
}

#[derive(serde::Serialize)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

// ---------------------------------------------------------------------------
// Helpers internes
// ---------------------------------------------------------------------------

async fn emit_status_changed(app: &tauri::AppHandle, state: &AppState) {
    let connected = state
        .peers
        .lock()
        .values()
        .filter(|p| p.online && p.paired)
        .count() as u32;
    let s = AppStatus {
        self_identity: state.identity.public,
        self_fingerprint: state.self_fingerprint(),
        self_hostname: state.hostname.clone(),
        connected_peers: connected,
        listening: state.is_listening(),
    };
    emit_event(app, BackendEvent::StatusChanged { status: s });
}

fn emit_event(app: &tauri::AppHandle, ev: BackendEvent) {
    use tauri::Emitter;
    if let Err(e) = app.emit("okvm://backend-event", ev) {
        tracing::warn!(error = %e, "emit BackendEvent failed");
    }
}

fn emit_event_static(app: &tauri::AppHandle, ev: BackendEvent) -> okvm_core::Result<()> {
    use tauri::Emitter;
    app.emit("okvm://backend-event", ev)
        .map_err(|e| okvm_core::Error::other(format!("emit: {e}")))
}
