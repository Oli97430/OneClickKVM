//! Enumeration des moniteurs locaux via Win32 `EnumDisplayMonitors`.

use okvm_core::ScreenInfo;

use windows::Win32::Foundation::{BOOL, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY,
};

/// Enumere les ecrans locaux et renvoie une liste de [`ScreenInfo`].
///
/// L'origine `(origin_x, origin_y)` correspond a la position du moniteur dans
/// le **bureau virtuel** (qui peut commencer par des coordonnees negatives si
/// un moniteur secondaire est a gauche/au-dessus du primaire).
#[must_use]
pub fn enumerate_local_screens() -> Vec<ScreenInfo> {
    let mut collected: Vec<ScreenInfo> = Vec::new();

    // EnumDisplayMonitors prend un callback C-style. On passe l'adresse du Vec
    // via LPARAM.
    let lp = &raw mut collected as isize;
    // SAFETY: EnumDisplayMonitors est l'API standard ; le callback ne fait rien
    // d'unsafe au-dela de la conversion du LPARAM (correctement reconvertie).
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(monitor_proc), LPARAM(lp));
    }

    // Trouve le primaire (au point 0,0).
    // SAFETY: APIs standard.
    let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };

    for (i, s) in collected.iter_mut().enumerate() {
        s.index = i as u32;
        // SAFETY: API standard.
        let hmon = unsafe {
            MonitorFromPoint(
                POINT {
                    x: s.origin_x,
                    y: s.origin_y,
                },
                MONITOR_DEFAULTTOPRIMARY,
            )
        };
        s.is_primary = hmon == primary;
    }
    collected
}

unsafe extern "system" fn monitor_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: lparam a ete construit en envoyant `*mut Vec<ScreenInfo>` ; le
    // callback est appele en sequence depuis EnumDisplayMonitors avec ce ptr.
    let collected: &mut Vec<ScreenInfo> = unsafe { &mut *(lparam.0 as *mut Vec<ScreenInfo>) };

    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mi est correctement initialisee.
    if unsafe { GetMonitorInfoW(hmon, &raw mut mi) }.as_bool() {
        let w = (mi.rcMonitor.right - mi.rcMonitor.left) as u32;
        let h = (mi.rcMonitor.bottom - mi.rcMonitor.top) as u32;
        collected.push(ScreenInfo {
            index: 0, // patche apres
            is_primary: false,
            width_px: w,
            height_px: h,
            dpi: 96, // TODO: GetDpiForMonitor pour la vraie valeur
            origin_x: mi.rcMonitor.left,
            origin_y: mi.rcMonitor.top,
        });
    }
    TRUE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_does_not_panic() {
        // Sur un CI sans display, on accepte 0 ecrans.
        let _ = enumerate_local_screens();
    }
}
