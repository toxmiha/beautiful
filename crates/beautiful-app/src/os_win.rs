//! OS capability helpers (Windows DWM / version / process launch).

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

/// True when DWM backdrop materials (Acrylic/Mica/…) are safe to use.
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
