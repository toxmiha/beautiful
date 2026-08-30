//! System UI font enumeration and loading (Windows GDI).

use std::sync::Mutex;

/// Default UI typeface when settings are empty or the chosen face fails to load.
pub const DEFAULT_UI_FONT: &str = "Segoe UI";

static FONT_FAMILIES: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Cached list of installed font family names (sorted, unique, no vertical `@` faces).
pub fn list_system_font_families() -> Vec<String> {
    let mut guard = FONT_FAMILIES.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(enumerate_font_families());
    }
    guard.clone().unwrap_or_default()
}

/// Re-enumerate system fonts (newly installed faces appear without restart).
pub fn refresh_system_font_families() {
    let mut guard = FONT_FAMILIES.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(enumerate_font_families());
}

pub fn normalize_ui_font_name(preferred: &str) -> String {
    let name = preferred.trim();
    if name.is_empty() {
        DEFAULT_UI_FONT.to_owned()
    } else {
        name.to_owned()
    }
}

/// Load TrueType/OpenType bytes for a family via GDI `GetFontData`, with file fallbacks.
pub fn load_font_family_bytes(family: &str) -> Option<Vec<u8>> {
    let family = family.trim();
    if family.is_empty() {
        return None;
    }
    #[cfg(windows)]
    {
        load_via_gdi(family).or_else(|| load_via_fonts_dir(family))
    }
    #[cfg(not(windows))]
    {
        let _ = family;
        None
    }
}

/// Hardcoded Windows paths used when the requested family cannot be loaded.
pub fn load_default_ui_font_bytes() -> Option<Vec<u8>> {
    #[cfg(windows)]
    {
        load_via_gdi(DEFAULT_UI_FONT)
            .or_else(|| load_via_fonts_dir(DEFAULT_UI_FONT))
            .or_else(load_default_candidates)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn enumerate_font_families() -> Vec<String> {
    #[cfg(windows)]
    {
        enumerate_font_families_windows()
    }
    #[cfg(not(windows))]
    {
        vec![DEFAULT_UI_FONT.to_owned()]
    }
}

#[cfg(windows)]
fn enumerate_font_families_windows() -> Vec<String> {
    use std::collections::BTreeSet;
    use windows_sys::Win32::Graphics::Gdi::{
        EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, LOGFONTW,
    };

    let mut names: BTreeSet<String> = BTreeSet::new();

    unsafe extern "system" fn callback(
        lpelfe: *const LOGFONTW,
        _lpntme: *const windows_sys::Win32::Graphics::Gdi::TEXTMETRICW,
        _font_type: u32,
        lparam: windows_sys::Win32::Foundation::LPARAM,
    ) -> i32 {
        if lpelfe.is_null() {
            return 1;
        }
        let names = &mut *(lparam as *mut BTreeSet<String>);
        let face = utf16_z_to_string(&(*lpelfe).lfFaceName);
        if face.is_empty() || face.starts_with('@') {
            return 1;
        }
        names.insert(face);
        1
    }

    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return vec![DEFAULT_UI_FONT.to_owned()];
        }
        let mut logfont = std::mem::zeroed::<LOGFONTW>();
        logfont.lfCharSet = DEFAULT_CHARSET;
        EnumFontFamiliesExW(
            hdc,
            &logfont,
            Some(callback),
            &mut names as *mut BTreeSet<String> as isize,
            0,
        );
        ReleaseDC(std::ptr::null_mut(), hdc);
    }

    if names.is_empty() {
        names.insert(DEFAULT_UI_FONT.to_owned());
    }
    names.into_iter().collect()
}

#[cfg(windows)]
fn load_via_gdi(family: &str) -> Option<Vec<u8>> {
    use windows_sys::Win32::Graphics::Gdi::{
        CreateFontIndirectW, DeleteObject, GetDC, GetFontData, ReleaseDC, SelectObject, DEFAULT_CHARSET,
        LOGFONTW,
    };

    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }

        let mut logfont = std::mem::zeroed::<LOGFONTW>();
        logfont.lfHeight = -16;
        logfont.lfCharSet = DEFAULT_CHARSET;
        write_face_name(&mut logfont.lfFaceName, family);

        let hfont = CreateFontIndirectW(&logfont);
        if hfont.is_null() {
            ReleaseDC(std::ptr::null_mut(), hdc);
            return None;
        }

        let old = SelectObject(hdc, hfont);
        // dwTable = 0 → whole font file (TTF) or collection (TTC).
        let size = GetFontData(hdc, 0, 0, std::ptr::null_mut(), 0);
        let bytes = if size == 0 || size == u32::MAX {
            None
        } else {
            let mut buf = vec![0u8; size as usize];
            let got = GetFontData(hdc, 0, 0, buf.as_mut_ptr().cast(), size);
            if got == 0 || got == u32::MAX || got != size {
                None
            } else {
                Some(buf)
            }
        };

        SelectObject(hdc, old);
        DeleteObject(hfont);
        ReleaseDC(std::ptr::null_mut(), hdc);
        bytes
    }
}

#[cfg(windows)]
fn load_via_fonts_dir(family: &str) -> Option<Vec<u8>> {
    let stem = family
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if stem.is_empty() {
        return None;
    }

    let dirs = [
        std::path::PathBuf::from(r"C:\Windows\Fonts"),
        std::env::var_os("LOCALAPPDATA")
            .map(|p| std::path::PathBuf::from(p).join(r"Microsoft\Windows\Fonts"))
            .unwrap_or_default(),
    ];

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            let ext = ext.to_ascii_lowercase();
            if !matches!(ext.as_str(), "ttf" | "otf" | "ttc") {
                continue;
            }
            let file_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if file_stem == stem || file_stem.starts_with(&stem) {
                if let Ok(bytes) = std::fs::read(&path) {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn load_default_candidates() -> Option<Vec<u8>> {
    for path in [
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\calibri.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

#[cfg(windows)]
fn write_face_name(dest: &mut [u16; 32], name: &str) {
    for slot in dest.iter_mut() {
        *slot = 0;
    }
    for (i, unit) in name.encode_utf16().take(31).enumerate() {
        dest[i] = unit;
    }
}

#[cfg(windows)]
fn utf16_z_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
