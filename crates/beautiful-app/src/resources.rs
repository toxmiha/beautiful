//! Memory / disk indicators (status readout with bars).

use beautiful_core::Document;
use eframe::egui::{self, Color32};

use crate::theme;

#[derive(Debug, Clone, Default)]
pub struct ResourceStats {
    pub doc_bytes: u64,
    pub avail_ram_bytes: Option<u64>,
    pub total_ram_bytes: Option<u64>,
    pub free_disk_bytes: Option<u64>,
    pub total_disk_bytes: Option<u64>,
}

impl ResourceStats {
    pub fn sample(document: &Document) -> Self {
        let mut doc_bytes = 0u64;
        for layer in &document.layers {
            doc_bytes += layer.approx_tile_bytes() as u64;
        }
        // Dense projection (lazy until first sync/stroke, then stays allocated).
        doc_bytes += document.composite.memory_bytes();
        if let Some(m) = &document.selection.mask {
            doc_bytes += m.alpha.len() as u64;
        }

        let (avail_ram_bytes, total_ram_bytes, free_disk_bytes, total_disk_bytes) =
            system_resources();

        Self {
            doc_bytes,
            avail_ram_bytes,
            total_ram_bytes,
            free_disk_bytes,
            total_disk_bytes,
        }
    }

    pub fn ram_used_frac(&self) -> f32 {
        match (self.avail_ram_bytes, self.total_ram_bytes) {
            (Some(a), Some(t)) if t > 0 => 1.0 - (a as f32 / t as f32),
            _ => 0.0,
        }
    }

    pub fn disk_used_frac(&self) -> f32 {
        match (self.free_disk_bytes, self.total_disk_bytes) {
            (Some(f), Some(t)) if t > 0 => 1.0 - (f as f32 / t as f32),
            _ => 0.0,
        }
    }

    pub fn show_bars(&self, ui: &mut egui::Ui) {
        let ram_frac = self.ram_used_frac().clamp(0.0, 1.0);
        let disk_frac = self.disk_used_frac().clamp(0.0, 1.0);

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Drive {:.0}%", disk_frac * 100.0))
                    .small()
                    .color(theme::TEXT_DIM),
            );
            paint_bar(ui, 88.0, disk_frac, theme::DISK_BAR);
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("Mem {:.0}%", ram_frac * 100.0))
                    .small()
                    .color(theme::TEXT_DIM),
            );
            paint_bar(ui, 88.0, ram_frac, theme::MEM_BAR);
        });
    }
}

fn paint_bar(ui: &mut egui::Ui, width: f32, frac: f32, fill: Color32) {
    let height = 8.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 4.0, Color32::from_rgb(50, 50, 54));
    let mut filled = rect;
    filled.set_width((rect.width() * frac).max(2.0));
    ui.painter().rect_filled(filled, 4.0, fill);
}

#[cfg(windows)]
fn system_resources() -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    use std::mem::{size_of, zeroed};

    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(lpBuffer: *mut MemoryStatusEx) -> i32;
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }

    let (avail_ram, total_ram) = unsafe {
        let mut status: MemoryStatusEx = zeroed();
        status.dw_length = size_of::<MemoryStatusEx>() as u32;
        if GlobalMemoryStatusEx(&mut status) != 0 {
            (Some(status.ull_avail_phys), Some(status.ull_total_phys))
        } else {
            (None, None)
        }
    };

    let (free_disk, total_disk) = unsafe {
        let path: Vec<u16> = "C:\\".encode_utf16().chain(std::iter::once(0)).collect();
        let mut free_caller = 0u64;
        let mut total = 0u64;
        let mut free_total = 0u64;
        if GetDiskFreeSpaceExW(path.as_ptr(), &mut free_caller, &mut total, &mut free_total) != 0 {
            (Some(free_caller), Some(total))
        } else {
            (None, None)
        }
    };

    (avail_ram, total_ram, free_disk, total_disk)
}

#[cfg(not(windows))]
fn system_resources() -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    (None, None, None, None)
}
