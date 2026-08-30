//! Robust clipboard image paste for Windows (Snipping Tool / Win+Shift+S / Paint / browsers).
//!
//! Priority: registered PNG → CF_DIBV5 → CF_DIB (with BI_BITFIELDS) → CF_BITMAP via GDI → files.
//! Retries across OpenClipboard because Snipping Tool often delay-renders after copy.

use arboard::Clipboard;

/// Windows clipboard generation. Increments on every OS copy (this app or another).
/// `None` when the platform cannot observe it.
pub fn sequence_number() -> Option<u32> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
        let n = unsafe { GetClipboardSequenceNumber() };
        if n == 0 {
            None
        } else {
            Some(n)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Returns (width, height, RGBA8 bytes) if an image can be read from the clipboard.
pub fn read_clipboard_rgba() -> Result<(u32, u32, Vec<u8>), String> {
    #[cfg(windows)]
    {
        // Fast path: arboard often works when Win32 OpenClipboard is busy.
        if let Ok(mut cb) = Clipboard::new() {
            if let Ok(img) = cb.get_image() {
                if let Some(out) = arboard_to_rgba(img) {
                    return Ok(out);
                }
            }
        }
        // One Win32 pass (no UI-thread sleep storm — that made paste feel dead).
        match read_windows_clipboard_image() {
            Ok(img) => Ok(img),
            Err(e) => {
                // Brief single retry for Snipping Tool delay-render.
                std::thread::sleep(std::time::Duration::from_millis(20));
                read_windows_clipboard_image().map_err(|_| e)
            }
        }
    }

    #[cfg(not(windows))]
    {
        if let Ok(mut cb) = Clipboard::new() {
            if let Ok(img) = cb.get_image() {
                if let Some(out) = arboard_to_rgba(img) {
                    return Ok(out);
                }
            }
        }
        Err(
            "Clipboard has no usable image (try Copy again, or save screenshot as PNG and Open)"
                .into(),
        )
    }
}

fn arboard_to_rgba(img: arboard::ImageData<'_>) -> Option<(u32, u32, Vec<u8>)> {
    let w = img.width as u32;
    let h = img.height as u32;
    if w == 0 || h == 0 {
        return None;
    }
    let need = (w as usize).saturating_mul(h as usize).saturating_mul(4);
    if img.bytes.len() < need {
        return None;
    }
    let mut rgba = vec![0u8; need];
    rgba.copy_from_slice(&img.bytes[..need]);
    // Some sources put opaque screenshots with alpha=0 — they'd be invisible.
    let any_alpha = rgba.chunks_exact(4).any(|px| px[3] != 0);
    if !any_alpha {
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    Some((w, h, rgba))
}

#[cfg(windows)]
mod cf {
    pub const BITMAP: u32 = 2;
    pub const UNICODETEXT: u32 = 13;
    pub const HDROP: u32 = 15;
    pub const DIB: u32 = 8;
    pub const DIBV5: u32 = 17;
}

#[cfg(windows)]
fn read_windows_clipboard_image() -> Result<(u32, u32, Vec<u8>), String> {
    use windows_sys::Win32::Graphics::Gdi::HBITMAP;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EnumClipboardFormats, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard,
    };

    let mut format_names = Vec::new();

    unsafe {
        let mut opened = false;
        for attempt in 0..4 {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(4 + attempt * 2));
        }
        if !opened {
            return Err(
                "Could not open Windows clipboard (busy — copy again, then Ctrl+V)".into(),
            );
        }

        let result = (|| {
            // 1) PNG / image/png (Win+Shift+S, browsers, many editors).
            for name in [
                "PNG",
                "image/png",
                "image/x-png",
                "JFIF",
                "image/jpeg",
                "JPG",
                "JPEG",
            ] {
                if let Some(fmt) = register_format(name) {
                    if IsClipboardFormatAvailable(fmt) != 0 {
                        if let Some(bytes) = clipboard_bytes(fmt) {
                            if let Some(img) = decode_image_bytes(&bytes) {
                                return Ok(img);
                            }
                        }
                    }
                }
            }

            // 2) CF_DIBV5 then CF_DIB — native formats Snipping Tool exposes.
            for &fmt in &[cf::DIBV5, cf::DIB] {
                if IsClipboardFormatAvailable(fmt) != 0 {
                    if let Some(bytes) = clipboard_bytes(fmt) {
                        if let Some(img) = decode_dib(&bytes) {
                            return Ok(img);
                        }
                        if let Some(img) = decode_image_bytes(&bytes) {
                            return Ok(img);
                        }
                    }
                }
            }

            // 3) CF_BITMAP (HBITMAP) → 32-bit via GDI.
            if IsClipboardFormatAvailable(cf::BITMAP) != 0 {
                let handle = GetClipboardData(cf::BITMAP);
                if !handle.is_null() {
                    if let Some(img) = hbitmap_to_rgba(handle as HBITMAP) {
                        return Ok(img);
                    }
                }
            }

            let mut fmt = EnumClipboardFormats(0);
            while fmt != 0 {
                format_names.push(format_label(fmt));
                fmt = EnumClipboardFormats(fmt);
            }
            Err(())
        })();

        CloseClipboard();

        if let Ok(img) = result {
            return Ok(img);
        }
    }

    // 4) File drop list — needs its own clipboard open.
    if let Ok(files) =
        clipboard_win::get_clipboard::<Vec<std::path::PathBuf>, _>(clipboard_win::formats::FileList)
    {
        for path in files {
            if let Ok(dyn_img) = image::open(&path) {
                let rgba = dyn_img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if w > 0 && h > 0 {
                    return Ok((w, h, rgba.into_raw()));
                }
            }
        }
    }

    Err(format!(
        "Clipboard has no usable image (formats: {}). Copy/screenshot again, then Ctrl+V.",
        if format_names.is_empty() {
            "none".into()
        } else {
            format_names.join(", ")
        }
    ))
}

#[cfg(windows)]
fn register_format(name: &str) -> Option<u32> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;
    let wide: Vec<u16> = OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let id = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
    if id == 0 {
        None
    } else {
        Some(id)
    }
}

#[cfg(windows)]
fn format_label(fmt: u32) -> String {
    use windows_sys::Win32::System::DataExchange::GetClipboardFormatNameW;
    match fmt {
        cf::BITMAP => "CF_BITMAP".into(),
        cf::DIB => "CF_DIB".into(),
        cf::DIBV5 => "CF_DIBV5".into(),
        cf::HDROP => "CF_HDROP".into(),
        cf::UNICODETEXT => "CF_UNICODETEXT".into(),
        _ => {
            let mut buf = [0u16; 128];
            let n = unsafe { GetClipboardFormatNameW(fmt, buf.as_mut_ptr(), buf.len() as i32) };
            if n > 0 {
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                format!("#{fmt}")
            }
        }
    }
}

#[cfg(windows)]
fn clipboard_bytes(fmt: u32) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::DataExchange::GetClipboardData;
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    unsafe {
        let handle = GetClipboardData(fmt);
        if handle.is_null() {
            return None;
        }
        let size = GlobalSize(handle as _);
        if size == 0 || size > 512 * 1024 * 1024 {
            return None;
        }
        let ptr = GlobalLock(handle as _);
        if ptr.is_null() {
            return None;
        }
        let slice = std::slice::from_raw_parts(ptr as *const u8, size);
        let bytes = slice.to_vec();
        GlobalUnlock(handle as _);
        Some(bytes)
    }
}

#[cfg(windows)]
fn hbitmap_to_rgba(
    hbitmap: windows_sys::Win32::Graphics::Gdi::HBITMAP,
) -> Option<(u32, u32, Vec<u8>)> {
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS,
    };
    unsafe {
        let mut bm = std::mem::zeroed::<BITMAP>();
        if GetObjectW(
            hbitmap as _,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bm as *mut _ as _,
        ) == 0
        {
            return None;
        }
        let w = bm.bmWidth;
        let h = bm.bmHeight.abs();
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut info = std::mem::zeroed::<BITMAPINFO>();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; // top-down
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        let hdc = CreateCompatibleDC(std::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }
        let ok = GetDIBits(
            hdc,
            hbitmap,
            0,
            h as u32,
            pixels.as_mut_ptr() as _,
            &mut info,
            DIB_RGB_COLORS,
        );
        DeleteDC(hdc);
        if ok == 0 {
            return None;
        }
        for px in pixels.chunks_exact_mut(4) {
            let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
            px[0] = r;
            px[1] = g;
            px[2] = b;
            px[3] = if a == 0 { 255 } else { a };
        }
        Some((w as u32, h as u32, pixels))
    }
}

#[cfg(windows)]
fn decode_image_bytes(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let dyn_img = image::load_from_memory(bytes).ok()?;
    let rgba = dyn_img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h, rgba.into_raw()))
}

/// Decode CF_DIB / CF_DIBV5 payload (BITMAPINFOHEADER or BITMAPV5HEADER + optional masks + pixels).
#[cfg(windows)]
fn decode_dib(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if data.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    if header_size < 40 || data.len() < header_size {
        return None;
    }
    let width = i32::from_le_bytes(data[4..8].try_into().ok()?);
    let height_raw = i32::from_le_bytes(data[8..12].try_into().ok()?);
    let bit_count = u16::from_le_bytes(data[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(data[16..20].try_into().ok()?);
    if width <= 0 || height_raw == 0 {
        return None;
    }
    // BI_RGB=0, BI_BITFIELDS=3 — both common for screenshots.
    if compression != 0 && compression != 3 {
        return None;
    }
    if bit_count != 32 && bit_count != 24 && bit_count != 16 && bit_count != 8 {
        return None;
    }

    let w = width as u32;
    let bottom_up = height_raw > 0;
    let h = height_raw.unsigned_abs();

    let mut pixel_offset = header_size;
    const BI_BITFIELDS: u32 = 3;
    // Only BITMAPINFOHEADER (40) stores masks after the header; V4/V5 embed them.
    if compression == BI_BITFIELDS && header_size == 40 {
        pixel_offset += 12;
    }
    if bit_count <= 8 {
        let colors_used = u32::from_le_bytes(data[32..36].try_into().ok()?);
        let ncolors = if colors_used != 0 {
            colors_used as usize
        } else {
            1usize << bit_count
        };
        pixel_offset += ncolors * 4;
    }
    if data.len() < pixel_offset {
        return None;
    }

    let bpp = (bit_count / 8).max(1) as usize;
    let stride = if bit_count >= 24 {
        ((w as usize * bpp + 3) / 4) * 4
    } else if bit_count == 16 {
        ((w as usize * 2 + 3) / 4) * 4
    } else {
        ((w as usize + 3) / 4) * 4
    };
    let pixels = &data[pixel_offset..];
    if pixels.len() < stride * h as usize {
        return None;
    }

    let palette: Option<&[u8]> = if bit_count == 8 {
        let pal_start = header_size
            + if compression == BI_BITFIELDS && header_size == 40 {
                12
            } else {
                0
            };
        Some(&data[pal_start..pixel_offset])
    } else {
        None
    };

    // Optional channel masks for BI_BITFIELDS 32bpp.
    let (r_shift, g_shift, b_shift, a_shift, r_bits, g_bits, b_bits, a_bits) =
        if compression == BI_BITFIELDS && bit_count == 32 {
            let (rm, gm, bm, am) = if header_size >= 56 {
                (
                    u32::from_le_bytes(data[40..44].try_into().ok()?),
                    u32::from_le_bytes(data[44..48].try_into().ok()?),
                    u32::from_le_bytes(data[48..52].try_into().ok()?),
                    if header_size >= 60 {
                        u32::from_le_bytes(data[52..56].try_into().ok()?)
                    } else {
                        0
                    },
                )
            } else if header_size == 40 && data.len() >= pixel_offset {
                let masks = &data[40..52];
                (
                    u32::from_le_bytes(masks[0..4].try_into().ok()?),
                    u32::from_le_bytes(masks[4..8].try_into().ok()?),
                    u32::from_le_bytes(masks[8..12].try_into().ok()?),
                    0u32,
                )
            } else {
                (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000)
            };
            (
                mask_shift(rm),
                mask_shift(gm),
                mask_shift(bm),
                mask_shift(am),
                mask_bits(rm),
                mask_bits(gm),
                mask_bits(bm),
                mask_bits(am),
            )
        } else {
            (16, 8, 0, 24, 8, 8, 8, 8)
        };

    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let src_y = if bottom_up { h - 1 - y } else { y };
        let row = &pixels[(src_y as usize) * stride..];
        for x in 0..w as usize {
            let di = ((y as usize * w as usize + x) * 4) as usize;
            match bit_count {
                32 => {
                    let si = x * 4;
                    let v = u32::from_le_bytes([row[si], row[si + 1], row[si + 2], row[si + 3]]);
                    if compression == BI_BITFIELDS {
                        rgba[di] = expand_channel(v, r_shift, r_bits);
                        rgba[di + 1] = expand_channel(v, g_shift, g_bits);
                        rgba[di + 2] = expand_channel(v, b_shift, b_bits);
                        let a = if a_bits > 0 {
                            expand_channel(v, a_shift, a_bits)
                        } else {
                            255
                        };
                        rgba[di + 3] = if a == 0 { 255 } else { a };
                    } else {
                        rgba[di] = row[si + 2];
                        rgba[di + 1] = row[si + 1];
                        rgba[di + 2] = row[si];
                        let a = row[si + 3];
                        rgba[di + 3] = if a == 0 { 255 } else { a };
                    }
                }
                24 => {
                    let si = x * 3;
                    rgba[di] = row[si + 2];
                    rgba[di + 1] = row[si + 1];
                    rgba[di + 2] = row[si];
                    rgba[di + 3] = 255;
                }
                16 => {
                    let si = x * 2;
                    let v = u16::from_le_bytes([row[si], row[si + 1]]);
                    let r = ((v >> 11) & 0x1F) as u8;
                    let g = ((v >> 5) & 0x3F) as u8;
                    let b = (v & 0x1F) as u8;
                    rgba[di] = (r << 3) | (r >> 2);
                    rgba[di + 1] = (g << 2) | (g >> 4);
                    rgba[di + 2] = (b << 3) | (b >> 2);
                    rgba[di + 3] = 255;
                }
                8 => {
                    let idx = row[x] as usize;
                    if let Some(pal) = palette {
                        let pi = idx * 4;
                        if pi + 2 < pal.len() {
                            rgba[di] = pal[pi + 2];
                            rgba[di + 1] = pal[pi + 1];
                            rgba[di + 2] = pal[pi];
                            rgba[di + 3] = 255;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Some((w, h, rgba))
}

#[cfg(windows)]
fn mask_shift(mask: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    mask.trailing_zeros()
}

#[cfg(windows)]
fn mask_bits(mask: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    (mask >> mask.trailing_zeros()).count_ones()
}

#[cfg(windows)]
fn expand_channel(v: u32, shift: u32, bits: u32) -> u8 {
    if bits == 0 {
        return 0;
    }
    let raw = (v >> shift) & ((1u32 << bits) - 1);
    if bits >= 8 {
        (raw >> (bits - 8)) as u8
    } else {
        ((raw * 255) / ((1u32 << bits) - 1).max(1)) as u8
    }
}
