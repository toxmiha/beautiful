//! OS capability helpers (Windows DWM / version / process launch).

use eframe::egui;
use std::path::Path;
use std::process::Command;

/// Win11 starts at build 22000. DWM Acrylic/Mica + transparent swapchain
/// often break hit-testing / popups on Win10 (clicks fall outside the app).
#[cfg(target_os = "windows")]
pub fn windows_build_number() -> u32 {
    #[repr(C)]
    struct RtlOsVersionInfoW {
        dw_os_version_info_size: u32,
        dw_major_version: u32,
        dw_minor_version: u32,
        dw_build_number: u32,
        dw_platform_id: u32,
        sz_csd_version: [u16; 128],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut RtlOsVersionInfoW) -> i32;
    }

    unsafe {
        let mut info = std::mem::zeroed::<RtlOsVersionInfoW>();
        info.dw_os_version_info_size = std::mem::size_of::<RtlOsVersionInfoW>() as u32;
        if RtlGetVersion(&mut info) == 0 {
            info.dw_build_number
        } else {
            0
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn windows_build_number() -> u32 {
    0
}

/// True when Win11 DWM Acrylic/Mica are safe (not Win10 — hit-testing breaks).
#[inline]
pub fn dwm_backdrop_supported() -> bool {
    #[cfg(target_os = "windows")]
    {
        windows_build_number() >= 22_000
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Translucent chrome is worth enabling: Win11 DWM, or Linux compositor blur/transparency.
#[inline]
pub fn backdrop_supported() -> bool {
    #[cfg(target_os = "windows")]
    {
        dwm_backdrop_supported()
    }
    #[cfg(target_os = "linux")]
    {
        true
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

/// Hide console flash when spawning helper tools (`curl`, `cmd`, …) from a GUI app.
#[cfg(windows)]
pub fn hide_console(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW
    cmd.creation_flags(0x0800_0000)
}

#[cfg(not(windows))]
pub fn hide_console(cmd: &mut Command) -> &mut Command {
    cmd
}

/// Open a https/http URL in the default browser (no console flash on Windows).
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        hide_console(&mut c);
        let _ = c.args(["/C", "start", "", url]).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

/// Reveal a folder/file in the system file manager.
pub fn open_path(path: &Path) {
    #[cfg(windows)]
    {
        let mut c = Command::new("explorer");
        hide_console(&mut c);
        let _ = c.arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(path).spawn();
    }
}

/// Opaque saved HWND placement so hide-UI can restore after covering the monitor.
#[cfg(windows)]
#[derive(Clone, Copy)]
pub struct SavedWindowPlacement {
    length: u32,
    flags: u32,
    show_cmd: u32,
    min: (i32, i32),
    max: (i32, i32),
    normal: (i32, i32, i32, i32),
}

/// Cover the monitor that currently contains `window` (taskbar included).
/// Synchronous `SetWindowPos` — winit's borderless fullscreen is async and
/// leaves the DxgiFromVisual composition visual at the old size.
#[cfg(windows)]
pub fn cover_monitor(window: impl raw_window_handle::HasWindowHandle) -> Option<SavedWindowPlacement> {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, SetWindowPos, ShowWindow, SWP_NOZORDER, SWP_SHOWWINDOW, SW_RESTORE,
        WINDOWPLACEMENT,
    };

    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        return None;
    };
    let hwnd = win.hwnd.get() as windows_sys::Win32::Foundation::HWND;

    unsafe {
        let mut placement = std::mem::zeroed::<WINDOWPLACEMENT>();
        placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        if GetWindowPlacement(hwnd, &mut placement) == 0 {
            return None;
        }
        let saved = SavedWindowPlacement {
            length: placement.length,
            flags: placement.flags,
            show_cmd: placement.showCmd,
            min: (placement.ptMinPosition.x, placement.ptMinPosition.y),
            max: (placement.ptMaxPosition.x, placement.ptMaxPosition.y),
            normal: (
                placement.rcNormalPosition.left,
                placement.rcNormalPosition.top,
                placement.rcNormalPosition.right,
                placement.rcNormalPosition.bottom,
            ),
        };

        // Maximized windows ignore SetWindowPos until restored.
        let _ = ShowWindow(hwnd, SW_RESTORE);

        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return None;
        }
        let mut info = std::mem::zeroed::<MONITORINFO>();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let r = info.rcMonitor;
        if SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOZORDER | SWP_SHOWWINDOW,
        ) == 0
        {
            return None;
        }
        Some(saved)
    }
}

#[cfg(windows)]
pub fn restore_window(
    window: impl raw_window_handle::HasWindowHandle,
    saved: SavedWindowPlacement,
) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowPlacement, WINDOWPLACEMENT};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        return;
    };
    let hwnd = win.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let mut placement = WINDOWPLACEMENT {
        length: saved.length,
        flags: saved.flags,
        showCmd: saved.show_cmd,
        ptMinPosition: POINT {
            x: saved.min.0,
            y: saved.min.1,
        },
        ptMaxPosition: POINT {
            x: saved.max.0,
            y: saved.max.1,
        },
        rcNormalPosition: windows_sys::Win32::Foundation::RECT {
            left: saved.normal.0,
            top: saved.normal.1,
            right: saved.normal.2,
            bottom: saved.normal.3,
        },
    };
    unsafe {
        let _ = SetWindowPlacement(hwnd, &mut placement);
    }
}

/// Tab hide-UI covers `rcMonitor` (taskbar included). That size must never be
/// persisted: next launch would spawn a frameless window over the whole screen.
/// Returns true if the window was shrunk back into the work area.
#[cfg(windows)]
pub fn uncover_if_monitor_sized(window: impl raw_window_handle::HasWindowHandle) -> bool {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, SetWindowPos, SWP_NOZORDER};

    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        return false;
    };
    let hwnd = win.hwnd.get() as windows_sys::Win32::Foundation::HWND;

    unsafe {
        let mut wnd = std::mem::zeroed::<RECT>();
        if GetWindowRect(hwnd, &mut wnd) == 0 {
            return false;
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return false;
        }
        let mut info = std::mem::zeroed::<MONITORINFO>();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return false;
        }
        let mon = info.rcMonitor;
        let covering = (wnd.left - mon.left).abs() <= 8
            && (wnd.top - mon.top).abs() <= 8
            && (wnd.right - mon.right).abs() <= 8
            && (wnd.bottom - mon.bottom).abs() <= 8;
        if !covering {
            return false;
        }
        let work = info.rcWork;
        let margin = 48;
        let w = (work.right - work.left - margin * 2).max(960);
        let h = (work.bottom - work.top - margin * 2).max(640);
        let x = work.left + ((work.right - work.left - w) / 2).max(0);
        let y = work.top + ((work.bottom - work.top - h) / 2).max(0);
        SetWindowPos(hwnd, std::ptr::null_mut(), x, y, w, h, SWP_NOZORDER) != 0
    }
}

/// Cursor in egui points (physical pixels / `pixels_per_point`). Used to move
/// frameless float hosts without `StartDrag` (which triggers Windows Snap).
pub fn cursor_screen_points(pixels_per_point: f32) -> Option<egui::Pos2> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut p = POINT { x: 0, y: 0 };
        let ok = unsafe { GetCursorPos(&mut p) } != 0;
        if !ok || pixels_per_point <= 0.0 {
            return None;
        }
        Some(egui::pos2(
            p.x as f32 / pixels_per_point,
            p.y as f32 / pixels_per_point,
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = pixels_per_point;
        None
    }
}

/// Left mouse button currently down (OS), so float-host drags survive pointer leaving the hwnd.
pub fn primary_mouse_down() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetAsyncKeyState(vkey: i32) -> i16;
        }
        unsafe { GetAsyncKeyState(0x01) as u16 & 0x8000 != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}
