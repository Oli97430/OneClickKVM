//! Implementation Win32 des hooks `WH_KEYBOARD_LL` et `WH_MOUSE_LL`.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot, watch};

use okvm_core::{ButtonState, Edge, MouseButton, Result};
use okvm_protocol::InputMessage;

use crate::{CaptureHandle, InputCapture};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_INJECTED, LLKHF_UP, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYUP,
    WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

/// Capture Win32 active sur Windows.
pub struct Win32Capture;

impl Default for Win32Capture {
    fn default() -> Self {
        Self
    }
}

/// Etat global partage avec les callbacks C-style des hooks.
///
/// Les callbacks `LowLevelKeyboardProc` / `LowLevelMouseProc` ne peuvent pas
/// porter de contexte utilisateur ; on passe par un `Mutex<Option<...>>` static.
static HOOK_STATE: Mutex<Option<HookState>> = parking_lot::const_mutex(None);
static SUPPRESS: AtomicBool = AtomicBool::new(false);
/// Derniere position connue (pour calculer dx/dy si Windows ne nous donne pas).
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);

struct HookState {
    /// Canal vers la task bridge tokio.
    tx: std_mpsc::Sender<InputMessage>,
}

#[async_trait]
impl InputCapture for Win32Capture {
    async fn start(&self, tx: mpsc::Sender<InputMessage>) -> Result<CaptureHandle> {
        // Canal sync std → bridge → canal async tokio.
        let (std_tx, std_rx) = std_mpsc::channel::<InputMessage>();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let (suppress_tx, suppress_rx) = watch::channel(false);

        // Lance le watcher de suppression : convertit watch<bool> → AtomicBool.
        tokio::spawn({
            let mut rx = suppress_rx;
            async move {
                while rx.changed().await.is_ok() {
                    SUPPRESS.store(*rx.borrow(), Ordering::Relaxed);
                }
            }
        });

        // Lance le thread des hooks.
        let (tid_tx, tid_rx) = std_mpsc::channel::<u32>();
        let hook_thread_tx = std_tx.clone();
        let hook_thread = std::thread::Builder::new()
            .name("okvm-input-hooks".into())
            .spawn(move || run_hook_thread(hook_thread_tx, tid_tx))
            .map_err(|e| okvm_core::Error::Os(format!("spawn hook thread: {e}")))?;

        let hook_tid = tid_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| okvm_core::Error::Os("hook thread n'a pas rapporte son tid".into()))?;

        // Branche le shutdown : poste WM_QUIT au thread des hooks quand stop_rx fire.
        let stop_thread_join = std::sync::Arc::new(std::sync::Mutex::new(Some(hook_thread)));
        tokio::spawn({
            let stop_thread_join = stop_thread_join.clone();
            async move {
                let _ = stop_rx.await;
                // SAFETY: PostThreadMessageW est thread-safe ; tid est valide.
                unsafe {
                    let _ = PostThreadMessageW(hook_tid, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                // Join le thread (peut bloquer brievement, c'est OK car ici on est en task tokio).
                let join = stop_thread_join.lock().unwrap().take();
                if let Some(j) = join {
                    let _ = tokio::task::spawn_blocking(move || j.join()).await;
                }
            }
        });

        // Bridge std_mpsc → tokio mpsc.
        let bridge = tokio::task::spawn_blocking(move || {
            while let Ok(msg) = std_rx.recv() {
                // On bloque sur send : si le receveur tokio est lent on ralentit les hooks
                // (defense contre flood).
                if tx.blocking_send(msg).is_err() {
                    break;
                }
            }
        });
        // Cast vers JoinHandle<()> au lieu de JoinHandle<JoinResult<()>>
        let bridge: tokio::task::JoinHandle<()> = tokio::spawn(async move {
            let _ = bridge.await;
        });

        Ok(CaptureHandle {
            set_suppress: suppress_tx,
            stop: stop_tx,
            bridge,
        })
    }
}

/// Boucle principale du thread des hooks : install hooks, pump messages, cleanup.
fn run_hook_thread(tx: std_mpsc::Sender<InputMessage>, tid_tx: std_mpsc::Sender<u32>) {
    // Initialise le HOOK_STATE global.
    {
        let mut g = HOOK_STATE.lock();
        *g = Some(HookState { tx });
    }

    // Reporte notre TID pour permettre PostThreadMessageW(WM_QUIT) plus tard.
    // SAFETY: GetCurrentThreadId est thread-safe.
    let tid = unsafe { GetCurrentThreadId() };
    let _ = tid_tx.send(tid);

    // SAFETY: GetModuleHandleW(NULL) retourne le module de l'exe ; valide ici.
    let hmod: HINSTANCE = unsafe {
        GetModuleHandleW(windows::core::PCWSTR::null())
            .map(|h| HINSTANCE(h.0))
            .unwrap_or(HINSTANCE::default())
    };

    // Installe les deux hooks. Note: avec hmod == NULL et un thread_id = 0,
    // ce sont des hooks **globaux** (tous threads de la session).
    // SAFETY: SetWindowsHookExW est thread-safe et le callback est statique.
    let kb_hook =
        unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_keyboard_proc), hmod, 0) };
    let mouse_hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse_proc), hmod, 0) };

    let kb_hook = match kb_hook {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "SetWindowsHookExW WH_KEYBOARD_LL echec");
            HHOOK::default()
        }
    };
    let mouse_hook = match mouse_hook {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "SetWindowsHookExW WH_MOUSE_LL echec");
            HHOOK::default()
        }
    };

    tracing::info!(tid, "okvm input hooks installes");

    // Boucle de messages : indispensable, sinon les hooks ne se declenchent pas.
    // SAFETY: GetMessageW/TranslateMessage/DispatchMessageW sont les APIs standard ;
    // on passe un MSG initialise.
    unsafe {
        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 {
                // WM_QUIT
                break;
            }
            if r.0 == -1 {
                tracing::error!("GetMessageW erreur");
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Cleanup.
    // SAFETY: hooks valides (non-null) ; sinon UnhookWindowsHookEx renvoie BOOL FALSE.
    unsafe {
        if !kb_hook.is_invalid() {
            let _ = UnhookWindowsHookEx(kb_hook);
        }
        if !mouse_hook.is_invalid() {
            let _ = UnhookWindowsHookEx(mouse_hook);
        }
    }
    *HOOK_STATE.lock() = None;
    tracing::info!("okvm input hooks decroches");
}

// ===========================================================================
// Callbacks bas niveau
// ===========================================================================

/// Callback C-style appele par Windows pour chaque evenement clavier.
///
/// # Safety
/// Windows appelle ce callback depuis le thread qui a installe le hook.
/// `lparam` pointe vers un `KBDLLHOOKSTRUCT` valide pour la duree de l'appel.
unsafe extern "system" fn low_level_keyboard_proc(
    n_code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if n_code != HC_ACTION as i32 {
        // SAFETY: passage standard.
        return unsafe { CallNextHookEx(HHOOK::default(), n_code, wparam, lparam) };
    }
    // SAFETY: lparam est un KBDLLHOOKSTRUCT valide pour la duree de l'appel.
    let info: &KBDLLHOOKSTRUCT = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };

    // Ignore les evenements injectes par nous-meme pour eviter les boucles.
    if (info.flags.0 & LLKHF_INJECTED.0) != 0 {
        return unsafe { CallNextHookEx(HHOOK::default(), n_code, wparam, lparam) };
    }

    let state = if (info.flags.0 & LLKHF_UP.0) != 0
        || wparam.0 as u32 == WM_KEYUP
        || wparam.0 as u32 == WM_SYSKEYUP
    {
        ButtonState::Up
    } else {
        ButtonState::Down
    };
    let extended = (info.flags.0 & LLKHF_EXTENDED.0) != 0;
    let modifiers = current_modifiers();

    let msg = InputMessage::KeyEvent {
        vk: info.vkCode as u16,
        scancode: info.scanCode as u16,
        state,
        extended,
        modifiers,
    };

    let suppress = SUPPRESS.load(Ordering::Relaxed);
    forward(msg);

    if suppress {
        // Avalise l'evenement : retour non-zero.
        LRESULT(1)
    } else {
        unsafe { CallNextHookEx(HHOOK::default(), n_code, wparam, lparam) }
    }
}

/// Callback C-style pour la souris.
///
/// # Safety
/// Identique au callback clavier.
unsafe extern "system" fn low_level_mouse_proc(
    n_code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if n_code != HC_ACTION as i32 {
        return unsafe { CallNextHookEx(HHOOK::default(), n_code, wparam, lparam) };
    }
    // SAFETY: lparam pointe sur un MSLLHOOKSTRUCT valide.
    let info: &MSLLHOOKSTRUCT = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };

    // Ignore les evenements injectes par nous-meme.
    if (info.flags & LLMHF_INJECTED) != 0 {
        return unsafe { CallNextHookEx(HHOOK::default(), n_code, wparam, lparam) };
    }

    let x = info.pt.x;
    let y = info.pt.y;

    let wm = wparam.0 as u32;
    let msg = match wm {
        WM_MOUSEMOVE => {
            let last_x = LAST_X.swap(x, Ordering::Relaxed);
            let last_y = LAST_Y.swap(y, Ordering::Relaxed);
            let dx = (x - last_x).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let dy = (y - last_y).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            Some(InputMessage::MouseMove {
                x_global: x,
                y_global: y,
                dx,
                dy,
                screen_idx: 0,
            })
        }
        WM_LBUTTONDOWN => Some(InputMessage::MouseButton {
            button: MouseButton::Left,
            state: ButtonState::Down,
            x,
            y,
        }),
        WM_LBUTTONUP => Some(InputMessage::MouseButton {
            button: MouseButton::Left,
            state: ButtonState::Up,
            x,
            y,
        }),
        WM_RBUTTONDOWN => Some(InputMessage::MouseButton {
            button: MouseButton::Right,
            state: ButtonState::Down,
            x,
            y,
        }),
        WM_RBUTTONUP => Some(InputMessage::MouseButton {
            button: MouseButton::Right,
            state: ButtonState::Up,
            x,
            y,
        }),
        WM_MBUTTONDOWN => Some(InputMessage::MouseButton {
            button: MouseButton::Middle,
            state: ButtonState::Down,
            x,
            y,
        }),
        WM_MBUTTONUP => Some(InputMessage::MouseButton {
            button: MouseButton::Middle,
            state: ButtonState::Up,
            x,
            y,
        }),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            // mouseData high word = bouton X (1=X1, 2=X2).
            let xb = (info.mouseData >> 16) as u16;
            let button = if xb == XBUTTON1 {
                MouseButton::X1
            } else if xb == XBUTTON2 {
                MouseButton::X2
            } else {
                return unsafe { CallNextHookEx(HHOOK::default(), n_code, wparam, lparam) };
            };
            let state = if wm == WM_XBUTTONDOWN {
                ButtonState::Down
            } else {
                ButtonState::Up
            };
            Some(InputMessage::MouseButton {
                button,
                state,
                x,
                y,
            })
        }
        WM_MOUSEWHEEL => {
            let delta = ((info.mouseData >> 16) as i16) as i32;
            Some(InputMessage::MouseWheel {
                delta_x: 0,
                delta_y: delta,
                x,
                y,
            })
        }
        WM_MOUSEHWHEEL => {
            let delta = ((info.mouseData >> 16) as i16) as i32;
            Some(InputMessage::MouseWheel {
                delta_x: delta,
                delta_y: 0,
                x,
                y,
            })
        }
        _ => None,
    };

    let suppress = SUPPRESS.load(Ordering::Relaxed);
    if let Some(m) = msg {
        forward(m);
    }
    if suppress {
        LRESULT(1)
    } else {
        unsafe { CallNextHookEx(HHOOK::default(), n_code, wparam, lparam) }
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn forward(msg: InputMessage) {
    let g = HOOK_STATE.lock();
    if let Some(s) = g.as_ref() {
        if let Err(e) = s.tx.send(msg) {
            tracing::warn!(error = %e, "forward input echec (canal ferme?)");
        }
    }
}

/// Lit l'etat courant des modificateurs via `GetKeyState`. Retourne un bitmask
/// (Shift=1, Ctrl=2, Alt=4, Win=8, CapsLock=16, NumLock=32).
fn current_modifiers() -> u16 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VIRTUAL_KEY, VK_CAPITAL, VK_CONTROL, VK_LWIN, VK_MENU, VK_NUMLOCK, VK_RWIN,
        VK_SHIFT,
    };
    // SAFETY: GetKeyState est thread-safe.
    let down = |vk: VIRTUAL_KEY| (unsafe { GetKeyState(vk.0 as i32) } as u16) & 0x8000 != 0;
    let toggled = |vk: VIRTUAL_KEY| (unsafe { GetKeyState(vk.0 as i32) } as u16) & 0x0001 != 0;
    let mut m = 0u16;
    if down(VK_SHIFT) {
        m |= 1;
    }
    if down(VK_CONTROL) {
        m |= 2;
    }
    if down(VK_MENU) {
        m |= 4;
    }
    if down(VK_LWIN) || down(VK_RWIN) {
        m |= 8;
    }
    if toggled(VK_CAPITAL) {
        m |= 16;
    }
    if toggled(VK_NUMLOCK) {
        m |= 32;
    }
    m
}

// Suppression d'imports warnings : Arc, Edge sont utilises dans des helpers commentes.
#[allow(dead_code)]
fn _suppress_unused() {
    let _ = std::marker::PhantomData::<(Arc<()>, Edge)>;
}
