//! OneClick KVM — point d'entree Tauri.
//!
//! Ce crate joue le role de **shell** : il instancie l'application Tauri,
//! enregistre les commandes IPC (`commands.rs`) et configure l'etat partage
//! (`state.rs`).

mod commands;
mod state;

use okvm_logging::init_default;
use state::AppState;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

pub fn run() {
    init_default();

    // Log au boot la disponibilité d'encodeurs H.264 hardware via Media
    // Foundation. Best-effort, n'échoue jamais.
    okvm_video::log_hardware_h264_status();

    let app_state = AppState::initialize().expect("init AppState");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_app_status,
            commands::list_peers,
            commands::start_listening,
            commands::stop_listening,
            commands::pair_with_peer,
            commands::unpair_peer,
            commands::become_master,
            commands::stop_master,
            commands::get_grid,
            commands::send_files,
            commands::get_inbox_dir,
            commands::start_audio_share,
            commands::stop_audio_share,
            commands::is_audio_sharing,
            commands::start_video_share,
            commands::stop_video_share,
            commands::is_video_sharing,
            commands::get_app_config,
            commands::set_app_config,
            commands::reset_all_settings,
            commands::open_config_dir,
            commands::open_inbox_dir,
            commands::start_pairing_mode,
            commands::stop_pairing_mode,
            commands::get_pairing_mode_status,
            commands::list_local_screens,
            commands::get_about_info,
        ])
        .setup(|app| {
            tracing::info!("OneClick KVM demarre");

            // === Restauration position fenêtre ===
            // Si une position est sauvegardée dans AppConfig et qu'elle est
            // toujours visible (sur un moniteur attaché), on la restaure.
            // Sinon : (50, 50) sur le primaire (toujours visible — évite les
            // pièges du centrage sur multi-écran NVIDIA Surround / DP MST).
            if let Some(win) = app.get_webview_window("main") {
                let saved = okvm_config::load_app_config()
                    .ok()
                    .and_then(|c| c.window_state);
                let placed = if let Some(ws) = saved {
                    if window_position_is_visible(&win, ws.x, ws.y, ws.width, ws.height) {
                        let _ = win.set_position(tauri::PhysicalPosition::new(ws.x, ws.y));
                        let _ = win.set_size(tauri::PhysicalSize::new(ws.width, ws.height));
                        true
                    } else {
                        tracing::info!(?ws, "position sauvegardee hors d'ecran, fallback");
                        false
                    }
                } else {
                    false
                };
                if !placed {
                    if let Ok(Some(monitor)) = win.primary_monitor() {
                        let m_pos = monitor.position();
                        let _ = win.set_position(tauri::PhysicalPosition::new(
                            m_pos.x + 50,
                            m_pos.y + 50,
                        ));
                    }
                }
                let _ = win.show();
                let _ = win.set_focus();
                let _ = win.unminimize();
            }

            // === System Tray ===
            // Menu : Ouvrir / Quitter, dans la langue de l'utilisateur (lue
            // depuis AppConfig, qui elle-même tombe sur GetUserDefaultLocaleName
            // au premier lancement).
            let lang = okvm_config::load_app_config()
                .ok()
                .map(|c| c.language)
                .unwrap_or_else(|| "en".into());
            let labels = tray_labels(&lang);
            let open_item = MenuItem::with_id(app, "open", labels.open, true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit_item = MenuItem::with_id(app, "quit", labels.quit, true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &sep, &quit_item])?;

            let _tray = TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().cloned().ok_or("pas d'icone par defaut")?)
                .tooltip(labels.tooltip)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => {
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.set_focus();
                            let _ = win.unminimize();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(win) = app.get_webview_window("main") {
                            if win.is_visible().unwrap_or(false) {
                                let _ = win.hide();
                            } else {
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // === Sauvegarde position/taille à chaque move/resize + hide-on-close ===
            if let Some(win) = app.get_webview_window("main") {
                let win_clone = win.clone();
                win.on_window_event(move |event| {
                    match event {
                        WindowEvent::CloseRequested { api, .. } => {
                            // Hide au lieu de fermer ; l'app reste dans le tray.
                            persist_window_state(&win_clone);
                            let _ = win_clone.hide();
                            api.prevent_close();
                        }
                        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                            // Throttle léger : on persiste à chaque changement.
                            // En pratique Tauri n'envoie pas des centaines
                            // d'events, c'est OK pour ne pas debouncer.
                            persist_window_state(&win_clone);
                        }
                        _ => {}
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("erreur lors du run de l'application Tauri");
}

/// Libellés du menu tray pour une langue donnée. Catalogue inline ; les
/// langues partielles tombent sur l'anglais.
struct TrayLabels {
    open: &'static str,
    quit: &'static str,
    tooltip: &'static str,
}

fn tray_labels(lang: &str) -> TrayLabels {
    match lang {
        "fr" => TrayLabels {
            open: "Ouvrir OneClick KVM",
            quit: "Quitter",
            tooltip: "OneClick KVM — Contrôle multi-PC chiffré",
        },
        "de" => TrayLabels {
            open: "OneClick KVM öffnen",
            quit: "Beenden",
            tooltip: "OneClick KVM — Verschlüsselte Multi-PC-Steuerung",
        },
        "es" => TrayLabels {
            open: "Abrir OneClick KVM",
            quit: "Salir",
            tooltip: "OneClick KVM — Control multi-PC cifrado",
        },
        "it" => TrayLabels {
            open: "Apri OneClick KVM",
            quit: "Esci",
            tooltip: "OneClick KVM — Controllo multi-PC cifrato",
        },
        "pt" => TrayLabels {
            open: "Abrir OneClick KVM",
            quit: "Sair",
            tooltip: "OneClick KVM — Controle multi-PC criptografado",
        },
        "nl" => TrayLabels {
            open: "OneClick KVM openen",
            quit: "Afsluiten",
            tooltip: "OneClick KVM — Versleutelde multi-PC besturing",
        },
        "ja" => TrayLabels {
            open: "OneClick KVM を開く",
            quit: "終了",
            tooltip: "OneClick KVM — 暗号化マルチPC制御",
        },
        "zh" => TrayLabels {
            open: "打开 OneClick KVM",
            quit: "退出",
            tooltip: "OneClick KVM — 加密多电脑控制",
        },
        // EN fallback.
        _ => TrayLabels {
            open: "Open OneClick KVM",
            quit: "Quit",
            tooltip: "OneClick KVM — Encrypted multi-PC control",
        },
    }
}

/// Vérifie qu'un rectangle (x,y,w,h) intersecte au moins un moniteur attaché
/// à hauteur d'au moins 100 px. Sinon la fenêtre serait invisible.
fn window_position_is_visible(
    win: &tauri::WebviewWindow,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> bool {
    let monitors = match win.available_monitors() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let rect_right = x + w as i32;
    let rect_bottom = y + h as i32;
    for m in monitors {
        let p = m.position();
        let s = m.size();
        let m_right = p.x + s.width as i32;
        let m_bottom = p.y + s.height as i32;
        // Intersection
        let ix0 = x.max(p.x);
        let iy0 = y.max(p.y);
        let ix1 = rect_right.min(m_right);
        let iy1 = rect_bottom.min(m_bottom);
        let iw = (ix1 - ix0).max(0);
        let ih = (iy1 - iy0).max(0);
        if iw > 100 && ih > 100 {
            return true;
        }
    }
    false
}

/// Sauvegarde la position/taille courante de la fenêtre dans AppConfig.
/// Best-effort : toute erreur est loggée mais ignorée.
fn persist_window_state(win: &tauri::WebviewWindow) {
    let pos = match win.outer_position() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "outer_position");
            return;
        }
    };
    let size = match win.outer_size() {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "outer_size");
            return;
        }
    };
    let mut cfg = okvm_config::load_app_config().unwrap_or_default();
    let new_state = okvm_config::WindowState {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
    };
    if cfg.window_state == Some(new_state) {
        return; // pas de changement
    }
    cfg.window_state = Some(new_state);
    if let Err(e) = okvm_config::save_app_config(&cfg) {
        tracing::debug!(error = %e, "persist window state");
    }
}
