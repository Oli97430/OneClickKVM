//! Implementation Win32 `SendInput`.

use async_trait::async_trait;

use okvm_core::{ButtonState, MouseButton, Result};
use okvm_protocol::InputMessage;

use crate::InputInject;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, XBUTTON1, XBUTTON2,
};

/// Tag arbitraire ecrit dans `dwExtraInfo` pour identifier nos propres
/// injections (defense en profondeur — `LLKHF_INJECTED` est deja fourni par
/// Windows).
const OKVM_INJECT_TAG: usize = 0x4F43_4B56; // "OCKV"

/// Implementation Win32.
pub struct Win32Inject;

impl Default for Win32Inject {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl InputInject for Win32Inject {
    async fn inject(&self, msg: InputMessage) -> Result<()> {
        // SendInput est synchrone et bloquant ; on le pousse dans spawn_blocking
        // pour ne pas geler le runtime.
        tokio::task::spawn_blocking(move || inject_blocking(msg))
            .await
            .map_err(|e| okvm_core::Error::Os(format!("join: {e}")))?
    }
}

fn inject_blocking(msg: InputMessage) -> Result<()> {
    let inputs = match msg {
        InputMessage::MouseMove { x_global, y_global, .. } => {
            vec![mouse_move(x_global, y_global)]
        }
        InputMessage::MouseButton { button, state, x, y } => {
            // Position d'abord, puis click.
            vec![mouse_move(x, y), mouse_button(button, state)]
        }
        InputMessage::MouseWheel { delta_x, delta_y, x, y } => {
            let mut v = vec![mouse_move(x, y)];
            if delta_y != 0 {
                v.push(mouse_wheel(delta_y, false));
            }
            if delta_x != 0 {
                v.push(mouse_wheel(delta_x, true));
            }
            v
        }
        InputMessage::KeyEvent {
            vk, scancode, state, extended, ..
        } => {
            vec![key_event(vk, scancode, state, extended)]
        }
        InputMessage::KeyText { text } => text.chars().flat_map(unicode_chars_to_inputs).collect(),
        // Les messages applicatifs / switch / clipboard / power ne sont pas injectes ici.
        _ => Vec::new(),
    };

    if inputs.is_empty() {
        return Ok(());
    }

    // SAFETY: `inputs` est un Vec valide ; SendInput respecte sa taille via
    // le second parametre (nombre d'elements) et la taille de chaque INPUT.
    let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if n as usize != inputs.len() {
        return Err(okvm_core::Error::Os(format!(
            "SendInput partiel: {n}/{} (GetLastError disponible separement)",
            inputs.len()
        )));
    }
    Ok(())
}

// ===========================================================================
// Builders
// ===========================================================================

fn mouse_move(x: i32, y: i32) -> INPUT {
    // Calcule la position normalisee sur le bureau virtuel multi-ecrans :
    // 0..=65535 mappe `[origin..origin+size]`.
    let (nx, ny) = to_virtual_desktop_normalized(x, y);
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: nx,
                dy: ny,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: OKVM_INJECT_TAG,
            },
        },
    }
}

fn mouse_button(button: MouseButton, state: ButtonState) -> INPUT {
    let (flags, mouse_data) = match (button, state) {
        (MouseButton::Left, ButtonState::Down) => (MOUSEEVENTF_LEFTDOWN, 0),
        (MouseButton::Left, ButtonState::Up) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, ButtonState::Down) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, ButtonState::Up) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, ButtonState::Down) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, ButtonState::Up) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::X1, ButtonState::Down) => (MOUSEEVENTF_XDOWN, XBUTTON1 as i32),
        (MouseButton::X1, ButtonState::Up) => (MOUSEEVENTF_XUP, XBUTTON1 as i32),
        (MouseButton::X2, ButtonState::Down) => (MOUSEEVENTF_XDOWN, XBUTTON2 as i32),
        (MouseButton::X2, ButtonState::Up) => (MOUSEEVENTF_XUP, XBUTTON2 as i32),
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: mouse_data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OKVM_INJECT_TAG,
            },
        },
    }
}

fn mouse_wheel(delta: i32, horizontal: bool) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta as u32,
                dwFlags: if horizontal {
                    MOUSEEVENTF_HWHEEL
                } else {
                    MOUSEEVENTF_WHEEL
                },
                time: 0,
                dwExtraInfo: OKVM_INJECT_TAG,
            },
        },
    }
}

fn key_event(vk: u16, scancode: u16, state: ButtonState, extended: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if state == ButtonState::Up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if scancode != 0 {
        flags |= KEYEVENTF_SCANCODE;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scancode,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OKVM_INJECT_TAG,
            },
        },
    }
}

/// Convertit un caractere Unicode en `(down, up)` via `KEYEVENTF_UNICODE`.
fn unicode_chars_to_inputs(c: char) -> Vec<INPUT> {
    use windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_UNICODE;
    let mut buf = [0u16; 2];
    let units = c.encode_utf16(&mut buf);
    let mut out = Vec::with_capacity(units.len() * 2);
    for unit in units {
        out.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: *unit,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: OKVM_INJECT_TAG,
                },
            },
        });
        out.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: *unit,
                    dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: OKVM_INJECT_TAG,
                },
            },
        });
    }
    out
}

fn to_virtual_desktop_normalized(x: i32, y: i32) -> (i32, i32) {
    // SAFETY: GetSystemMetrics est thread-safe et sans precondition.
    unsafe {
        let ox = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let oy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let cw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let ch = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        // f64 pour eviter overflow ; clamp 0..65535.
        let nx = (((x - ox) as f64 / cw as f64) * 65535.0).round() as i32;
        let ny = (((y - oy) as f64 / ch as f64) * 65535.0).round() as i32;
        (nx.clamp(0, 65535), ny.clamp(0, 65535))
    }
}
