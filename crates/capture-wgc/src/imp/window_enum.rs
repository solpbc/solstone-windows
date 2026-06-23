// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Win32 window enumeration for capture exclusions (platform tier).
//!
//! Produces the [`WindowInfo`] facts the pure evaluator consumes: for every
//! real, visible window on the captured primary monitor, the owning process's
//! exe file name, the window title, and the visible bounds mapped into the
//! monitor's physical-pixel space (the same space as the WGC frame). The
//! evaluation + redaction decision is pure (`observer-exclusion`); this module
//! only gathers facts.
//!
//! **Fail closed.** Geometry that can't be read or mapped is reported as
//! `bounds: None`; identity (exe) that can't be read is reported as
//! `identity_uncertain: true`. The evaluator turns either into a whole-frame
//! drop when a rule is active, so an excluded surface never leaks because we
//! couldn't measure it. If even monitor info or `EnumWindows` fails, the caller
//! drops the frame.
//!
//! **DPI.** Bounds and the monitor rect are read in the same virtual-screen
//! coordinate space; for a per-monitor-DPI-aware (PMv2) process these are
//! physical pixels and the primary monitor's origin is `(0, 0)`, matching the
//! WGC frame. If the monitor's physical size does not equal the frame size, the
//! coordinate spaces are not aligned, so we cannot map reliably — every window's
//! bounds become `None` (excluded ones then drop the frame). The app manifest
//! declares PMv2 so this fallback does not trigger in normal operation.

use std::ffi::c_void;

use observer_exclusion::{Rect, RunningApp, WindowInfo};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, BOOL, FALSE, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

/// Enumerate the real, visible windows on the captured primary monitor and map
/// their geometry into the frame's physical-pixel space.
///
/// `Err(())` means we could not establish the monitor frame of reference or the
/// enumeration itself failed — the caller must drop the frame (fail closed).
pub fn enumerate_primary_monitor_windows(
    frame_width: u32,
    frame_height: u32,
) -> Result<Vec<WindowInfo>, ()> {
    let monitor = primary_monitor_rect().ok_or(())?;
    let mon_w = (monitor.right - monitor.left).max(0) as u32;
    let mon_h = (monitor.bottom - monitor.top).max(0) as u32;
    // Coordinate spaces align only when the monitor's physical size equals the
    // captured frame size (PMv2). Otherwise we cannot map window rects into the
    // frame, so geometry is treated as unknown (excluded windows then drop).
    let geometry_reliable = mon_w == frame_width && mon_h == frame_height;

    let mut hwnds: Vec<HWND> = Vec::new();
    // SAFETY: `collect_hwnds` only pushes HWNDs into the Vec behind `lparam` for
    // the duration of this call; the Vec outlives the enumeration.
    let enumerated = unsafe {
        EnumWindows(
            Some(collect_hwnds),
            LPARAM(&mut hwnds as *mut Vec<HWND> as isize),
        )
    };
    if enumerated.is_err() {
        return Err(());
    }

    let mut windows = Vec::new();
    for hwnd in hwnds {
        if let Some(info) = describe_window(hwnd, &monitor, geometry_reliable) {
            windows.push(info);
        }
    }
    Ok(windows)
}

/// Diagnostic enumeration for `--dump-windows`: the windows on the primary
/// monitor mapped against the monitor's own size (so geometry is reported as
/// reliable). Not used by the capture path — that passes the real WGC frame size
/// to [`enumerate_primary_monitor_windows`] so the DPI guard is exercised.
pub fn dump_primary_monitor_windows() -> Vec<WindowInfo> {
    let Some(monitor) = primary_monitor_rect() else {
        return Vec::new();
    };
    let w = (monitor.right - monitor.left).max(0) as u32;
    let h = (monitor.bottom - monitor.top).max(0) as u32;
    enumerate_primary_monitor_windows(w, h).unwrap_or_default()
}

/// The distinct, user-meaningful running apps for the exclusion picker: every
/// visible, titled, non-tool window's owning exe, de-duplicated, with a
/// representative title as the display label. Sorted by display label. Windows
/// whose exe can't be read are skipped (the owner couldn't meaningfully act on
/// them anyway). Best-effort: an enumeration failure yields an empty list.
pub fn list_running_apps() -> Vec<RunningApp> {
    let mut hwnds: Vec<HWND> = Vec::new();
    // SAFETY: `collect_hwnds` only pushes into the Vec behind `lparam`.
    let enumerated = unsafe {
        EnumWindows(
            Some(collect_hwnds),
            LPARAM(&mut hwnds as *mut Vec<HWND> as isize),
        )
    };
    if enumerated.is_err() {
        return Vec::new();
    }

    let mut apps: Vec<RunningApp> = Vec::new();
    for hwnd in hwnds {
        // SAFETY: read-only Win32 queries on a live HWND from EnumWindows.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() || is_cloaked(hwnd) {
                continue;
            }
            let exstyle = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            if exstyle & WS_EX_TOOLWINDOW.0 != 0 {
                continue;
            }
            let title = read_title(hwnd);
            if title.trim().is_empty() {
                continue; // not a user-meaningful surface for the picker
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let (exe_name, uncertain) = read_exe_name(pid);
            if uncertain || exe_name.is_empty() {
                continue;
            }
            if apps.iter().any(|a| a.exe_name == exe_name) {
                continue;
            }
            apps.push(RunningApp {
                exe_name,
                display_name: title,
            });
        }
    }
    apps.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });
    apps
}

unsafe extern "system" fn collect_hwnds(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let windows = &mut *(lparam.0 as *mut Vec<HWND>);
    windows.push(hwnd);
    TRUE // keep enumerating
}

/// The primary monitor's rectangle in virtual-screen coordinates (physical px
/// for a PMv2 process; origin `(0, 0)`).
fn primary_monitor_rect() -> Option<RECT> {
    // SAFETY: standard GDI monitor query; `mi` is initialized with its cbSize.
    unsafe {
        let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            Some(mi.rcMonitor)
        } else {
            None
        }
    }
}

/// Build a [`WindowInfo`] for one window, or `None` if it is not a real visible
/// surface on the captured monitor (and so cannot contribute pixels to the frame).
fn describe_window(hwnd: HWND, monitor: &RECT, geometry_reliable: bool) -> Option<WindowInfo> {
    // SAFETY: all calls are read-only Win32 queries on a live HWND from EnumWindows.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        if is_cloaked(hwnd) {
            return None; // on another virtual desktop / hidden by DWM
        }
        let exstyle = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if exstyle & WS_EX_TOOLWINDOW.0 != 0 {
            return None; // tooltips, overlays — not owner content
        }

        // Geometry: prefer the DWM extended frame bounds (true visible rect, no
        // drop-shadow); fall back to GetWindowRect.
        let raw_rect = window_rect(hwnd);
        let bounds = match raw_rect {
            Some(rect) => {
                let inter = intersect(&rect, monitor);
                inter?; // not on the captured monitor -> skip entirely
                let inter = inter.unwrap();
                if geometry_reliable {
                    Some(Rect {
                        x: inter.left - monitor.left,
                        y: inter.top - monitor.top,
                        width: (inter.right - inter.left).max(0) as u32,
                        height: (inter.bottom - inter.top).max(0) as u32,
                    })
                } else {
                    None // can't map under a DPI mismatch -> excluded => drop
                }
            }
            // Geometry unreadable: we can't tell which monitor it's on, so keep
            // it as an unredactable candidate (excluded => drop). Rare.
            None => None,
        };

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let (exe_name, identity_uncertain) = read_exe_name(pid);
        let title = read_title(hwnd);

        Some(WindowInfo {
            exe_name,
            title,
            bounds,
            identity_uncertain,
        })
    }
}

unsafe fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let ok = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut c_void,
        std::mem::size_of::<u32>() as u32,
    );
    ok.is_ok() && cloaked != 0
}

unsafe fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    let dwm = DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut rect as *mut RECT as *mut c_void,
        std::mem::size_of::<RECT>() as u32,
    );
    if dwm.is_ok() {
        return Some(rect);
    }
    if GetWindowRect(hwnd, &mut rect).is_ok() {
        return Some(rect);
    }
    None
}

/// Intersect two rects; `None` when they do not overlap (zero/negative area).
fn intersect(a: &RECT, b: &RECT) -> Option<RECT> {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    let right = a.right.min(b.right);
    let bottom = a.bottom.min(b.bottom);
    if right > left && bottom > top {
        Some(RECT {
            left,
            top,
            right,
            bottom,
        })
    } else {
        None
    }
}

/// `(lowercased exe file name, identity_uncertain)`. Identity is uncertain when
/// the owning process can't be opened/queried (e.g. a higher-integrity process
/// not readable from the medium-integrity observer); the evaluator fails closed.
unsafe fn read_exe_name(pid: u32) -> (String, bool) {
    if pid == 0 {
        return (String::new(), true);
    }
    let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) {
        Ok(handle) => handle,
        Err(_) => return (String::new(), true),
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let queried = QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_WIN32,
        PWSTR(buf.as_mut_ptr()),
        &mut len,
    );
    let _ = CloseHandle(handle);
    if queried.is_err() || len == 0 {
        return (String::new(), true);
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    let file = full
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(&full)
        .to_ascii_lowercase();
    (file, false)
}

/// The window title, or empty string when absent (an empty title is a normal,
/// non-uncertain state — it simply matches no rule).
unsafe fn read_title(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(hwnd, &mut buf);
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}
