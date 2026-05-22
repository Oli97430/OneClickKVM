//! Implementation Win32 du presse-papier.
//!
//! L'API Win32 du presse-papier impose :
//! - `OpenClipboard(hwnd)` puis `CloseClipboard()` (avec un HWND owner).
//! - `GetClipboardData(format)` rend un `HGLOBAL` qu'il faut `GlobalLock` puis
//!   `GlobalUnlock`.
//! - Pour ecrire : `EmptyClipboard()` puis `SetClipboardData(format, hglobal)`.
//!
//! On utilise une fenetre message-only invisible pour servir d'owner.

use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use okvm_core::Result;
use okvm_protocol::ClipboardItem;

use crate::{ClipboardSync, ClipboardWatchHandle};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, EnumClipboardFormats,
    GetClipboardData, OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener,
    SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, RegisterClassW,
    TranslateMessage, HMENU, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE,
    WNDCLASSW,
};

/// Implementation Win32.
pub struct Win32Clipboard;

impl Default for Win32Clipboard {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl ClipboardSync for Win32Clipboard {
    async fn read(&self) -> Result<Vec<ClipboardItem>> {
        tokio::task::spawn_blocking(read_blocking)
            .await
            .map_err(|e| okvm_core::Error::Os(format!("join: {e}")))?
    }

    async fn write(&self, items: &[ClipboardItem]) -> Result<()> {
        let owned: Vec<ClipboardItem> = items.to_vec();
        tokio::task::spawn_blocking(move || write_blocking(&owned))
            .await
            .map_err(|e| okvm_core::Error::Os(format!("join: {e}")))?
    }

    async fn watch(&self, tx: mpsc::Sender<Vec<ClipboardItem>>) -> Result<ClipboardWatchHandle> {
        let (stop_tx, stop_rx) = oneshot::channel();
        let (init_tx, init_rx) = std_mpsc::channel::<std::result::Result<isize, String>>();

        std::thread::Builder::new()
            .name("okvm-clipboard-watcher".into())
            .spawn(move || run_watcher_thread(tx, init_tx))
            .map_err(|e| okvm_core::Error::Os(format!("spawn watcher: {e}")))?;

        match init_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(_hwnd_isize)) => {}
            Ok(Err(e)) => return Err(okvm_core::Error::Os(e)),
            Err(_) => return Err(okvm_core::Error::Os("watcher init timeout".into())),
        }

        // Le shutdown se fait simplement en abandonnant `stop_tx` ; pour propre
        // arret on attendrait WM_QUIT. Pour l'instant : abandon non-bloquant.
        let _ = stop_rx;

        Ok(ClipboardWatchHandle { stop: stop_tx })
    }
}

// ===========================================================================
// Reading
// ===========================================================================

fn read_blocking() -> Result<Vec<ClipboardItem>> {
    let mut items = Vec::new();
    let _guard = ClipboardGuard::open()?;

    let cf_rtf = register("Rich Text Format");
    let cf_html = register("HTML Format");
    let cf_png = register("PNG");

    // Enumere les formats presents.
    let mut formats = Vec::new();
    // SAFETY: clipboard is open.
    let mut next = 0u32;
    loop {
        next = unsafe { EnumClipboardFormats(next) };
        if next == 0 {
            break;
        }
        formats.push(next);
    }

    for fmt in formats {
        if fmt == CF_UNICODETEXT.0 as u32 {
            if let Some(text) = read_unicode_text() {
                items.push(ClipboardItem::Text(text));
            }
        } else if fmt == cf_rtf {
            if let Some(rtf) = read_ascii_bytes(fmt) {
                if let Ok(s) = String::from_utf8(rtf) {
                    items.push(ClipboardItem::Rtf(s));
                }
            }
        } else if fmt == cf_html {
            if let Some(html_bytes) = read_ascii_bytes(fmt) {
                if let Ok(s) = String::from_utf8(html_bytes) {
                    items.push(ClipboardItem::Html {
                        html: s,
                        plaintext: None,
                    });
                }
            }
        } else if fmt == cf_png {
            if let Some(png_bytes) = read_ascii_bytes(fmt) {
                items.push(ClipboardItem::Png(png_bytes));
            }
        } else if fmt == CF_HDROP.0 as u32 {
            if let Some(paths) = read_hdrop() {
                items.push(ClipboardItem::FileList(paths));
            }
        }
    }
    Ok(items)
}

fn read_unicode_text() -> Option<String> {
    // SAFETY: clipboard is open (caller responsibility).
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let p = GlobalLock(HGLOBAL(h.0));
        if p.is_null() {
            return None;
        }
        let size = GlobalSize(HGLOBAL(h.0));
        // Lit u16 jusqu'au NUL.
        let slice = std::slice::from_raw_parts(p.cast::<u16>(), size / 2);
        let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        let s = String::from_utf16_lossy(&slice[..len]);
        let _ = GlobalUnlock(HGLOBAL(h.0));
        Some(s)
    }
}

fn read_ascii_bytes(format: u32) -> Option<Vec<u8>> {
    // SAFETY: clipboard is open ; format est un CF enregistre valide.
    unsafe {
        let h = GetClipboardData(format).ok()?;
        let p = GlobalLock(HGLOBAL(h.0));
        if p.is_null() {
            return None;
        }
        let size = GlobalSize(HGLOBAL(h.0));
        let slice = std::slice::from_raw_parts(p.cast::<u8>(), size);
        // Tronc au premier nul terminal pour les contenus textuels mal calibres.
        let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        let out = slice[..len].to_vec();
        let _ = GlobalUnlock(HGLOBAL(h.0));
        Some(out)
    }
}

fn read_hdrop() -> Option<Vec<String>> {
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    // SAFETY: clipboard is open.
    unsafe {
        let h = GetClipboardData(CF_HDROP.0 as u32).ok()?;
        let hdrop = HDROP(h.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let mut buf = [0u16; 1024];
            let n = DragQueryFileW(hdrop, i, Some(&mut buf));
            if n > 0 {
                let s = String::from_utf16_lossy(&buf[..n as usize]);
                out.push(s);
            }
        }
        Some(out)
    }
}

// ===========================================================================
// Writing
// ===========================================================================

fn write_blocking(items: &[ClipboardItem]) -> Result<()> {
    let _guard = ClipboardGuard::open()?;
    // SAFETY: clipboard is open.
    unsafe {
        EmptyClipboard().map_err(|e| okvm_core::Error::Os(format!("EmptyClipboard: {e}")))?;
    }

    let cf_rtf = register("Rich Text Format");
    let cf_html = register("HTML Format");
    let cf_png = register("PNG");

    for item in items {
        match item {
            ClipboardItem::Text(s) => {
                let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
                set_data(CF_UNICODETEXT.0 as u32, bytemuck_u16_to_bytes(&wide))?;
            }
            ClipboardItem::Rtf(s) => {
                let mut bytes = s.as_bytes().to_vec();
                bytes.push(0);
                set_data(cf_rtf, &bytes)?;
            }
            ClipboardItem::Html { html, .. } => {
                let mut bytes = html.as_bytes().to_vec();
                bytes.push(0);
                set_data(cf_html, &bytes)?;
            }
            ClipboardItem::Png(bytes) => {
                set_data(cf_png, bytes)?;
            }
            ClipboardItem::FileList(_) => {
                // CF_HDROP necessite DROPFILES + chemins UTF-16 double-null-terminated.
                // Implementation laissee pour une iteration suivante.
                tracing::debug!("CF_HDROP write non implemente — utilisez le canal Files");
            }
            _ => {
                tracing::debug!("ClipboardItem variant non gere a l'ecriture");
            }
        }
    }
    Ok(())
}

fn set_data(format: u32, bytes: &[u8]) -> Result<()> {
    // SAFETY: GlobalAlloc + GlobalLock + memcpy + SetClipboardData sequence.
    unsafe {
        let hglob = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
            .map_err(|e| okvm_core::Error::Os(format!("GlobalAlloc: {e}")))?;
        let p = GlobalLock(hglob);
        if p.is_null() {
            return Err(okvm_core::Error::Os("GlobalLock failed".into()));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(hglob);
        // SetClipboardData consomme l'ownership en cas de succes.
        SetClipboardData(format, HANDLE(hglob.0))
            .map_err(|e| okvm_core::Error::Os(format!("SetClipboardData: {e}")))?;
        Ok(())
    }
}

// ===========================================================================
// Watcher (message-only window)
// ===========================================================================

fn run_watcher_thread(
    tx: mpsc::Sender<Vec<ClipboardItem>>,
    init_tx: std_mpsc::Sender<std::result::Result<isize, String>>,
) {
    // SAFETY: APIs Win32 standard ; on n'expose pas le HWND a d'autres threads.
    unsafe {
        let hinstance = match GetModuleHandleW(PCWSTR::null()) {
            Ok(h) => h,
            Err(e) => {
                let _ = init_tx.send(Err(format!("GetModuleHandleW: {e}")));
                return;
            }
        };
        let class_name = w!("OkvmClipboardWatcher");
        let mut wc = WNDCLASSW::default();
        wc.lpfnWndProc = Some(window_proc);
        wc.hInstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);
        wc.lpszClassName = class_name;
        let _ = RegisterClassW(&wc); // ok si deja enregistre

        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            HMENU::default(),
            windows::Win32::Foundation::HINSTANCE(hinstance.0),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                let _ = init_tx.send(Err(format!("CreateWindowExW: {e}")));
                return;
            }
        };

        if let Err(e) = AddClipboardFormatListener(hwnd) {
            let _ = init_tx.send(Err(format!("AddClipboardFormatListener: {e}")));
            let _ = DestroyWindow(hwnd);
            return;
        }

        let _ = init_tx.send(Ok(hwnd.0 as isize));

        // Pump messages.
        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, hwnd, 0, 0);
            if r.0 <= 0 {
                break;
            }
            if msg.message == WM_CLIPBOARDUPDATE {
                // Lit le clipboard maintenant et envoie sur tx.
                if let Ok(items) = read_blocking() {
                    if tx.blocking_send(items).is_err() {
                        break;
                    }
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = RemoveClipboardFormatListener(hwnd);
        let _ = DestroyWindow(hwnd);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: DefWindowProcW est l'API standard pour les messages non geres.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

// ===========================================================================
// Helpers
// ===========================================================================

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Result<Self> {
        // SAFETY: on appelle OpenClipboard sans HWND owner (acceptable pour
        // un user-mode app sans fenetre persistante).
        unsafe {
            OpenClipboard(None).map_err(|e| okvm_core::Error::Os(format!("OpenClipboard: {e}")))?;
        }
        Ok(Self)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: on a ouvert le clipboard via open() — symetrique.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn register(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: PCWSTR pointe sur une chaine nul-terminee.
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }
}

fn bytemuck_u16_to_bytes(slice: &[u16]) -> &[u8] {
    // SAFETY: la representation de [u16] est `slice.len() * 2` octets contigus.
    // Pas de reinterpretation lvalue, juste lecture.
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast::<u8>(), slice.len() * 2) }
}
