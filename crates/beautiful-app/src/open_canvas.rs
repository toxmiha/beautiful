//! Open canvas (holst / file) tabs — outer MDI layer.
//!
//! Sheets live inside each canvas's [`Workspace`]. Opening another file creates
//! another OpenCanvas tab, never a sheet in the current holst.

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::workspace::{SheetSnapshot, Workspace};

/// No hard open-tab limit (RAM only). Cold-unload still parks idle tabs.
/// Keep this many recently focused tabs warm in RAM; older clean tabs may go cold.
pub const KEEP_HOT_CANVASES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CanvasId(pub u64);

#[allow(dead_code)]
pub struct SheetThumb {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl From<SheetSnapshot> for SheetThumb {
    fn from(s: SheetSnapshot) -> Self {
        Self {
            width: s.width,
            height: s.height,
            rgba: s.rgba,
        }
    }
}

/// Parked inactive canvas body.
pub enum ParkedCanvas {
    /// Full workspace (sheets hold Documents); caches already parked.
    Warm { workspace: Workspace },
    /// Clean saved file unloaded from RAM — reload on focus.
    Cold { thumb: Option<SheetThumb> },
}

pub struct OpenCanvas {
    pub id: CanvasId,
    pub title: String,
    pub path: Option<PathBuf>,
    /// Library / new-canvas NSFW flag — Discord hides name + preview when set.
    pub nsfw: bool,
    /// `document.edit_generation()` last known for this tab.
    pub edit_gen: u64,
    /// Generation at last save/open for this tab.
    pub saved_edit_gen: u64,
    /// `None` while this tab is active (body on BeautifulApp).
    pub parked: Option<ParkedCanvas>,
    /// Recency for cold-unload (higher = more recent).
    pub touch_seq: u64,
    /// Time spent on this tab while it had no library path (merged on first save).
    pub unsaved_time_secs: u64,
}

impl OpenCanvas {
    pub fn is_dirty(&self) -> bool {
        self.edit_gen != self.saved_edit_gen
    }

    pub fn display_title(&self) -> String {
        let mut t = if self.title.is_empty() {
            "Untitled".into()
        } else {
            self.title.clone()
        };
        if self.is_dirty() {
            t.push('*');
        }
        t
    }

    pub fn tab_label(&self) -> String {
        self.display_title()
    }
}

pub struct OpenCanvasList {
    tabs: Vec<OpenCanvas>,
    active: usize,
    next_id: u64,
    touch_clock: u64,
}

impl OpenCanvasList {
    /// Home screen: no holst tabs. Do not seed a dummy Untitled (that showed
    /// the 1×1 RAM stub as a real canvas).
    pub fn home_only() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            touch_clock: 0,
        }
    }

    pub fn new_primary(title: impl Into<String>) -> Self {
        Self {
            tabs: vec![OpenCanvas {
                id: CanvasId(1),
                title: title.into(),
                path: None,
                nsfw: false,
                edit_gen: 0,
                saved_edit_gen: 0,
                parked: None,
                touch_seq: 1,
                unsaved_time_secs: 0,
            }],
            active: 0,
            next_id: 2,
            touch_clock: 1,
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_index(&self) -> usize {
        self.active.min(self.tabs.len().saturating_sub(1))
    }

    pub fn tabs(&self) -> &[OpenCanvas] {
        &self.tabs
    }

    pub fn active(&self) -> &OpenCanvas {
        &self.tabs[self.active_index()]
    }

    pub fn active_mut(&mut self) -> &mut OpenCanvas {
        let i = self.active_index();
        &mut self.tabs[i]
    }

    pub fn can_open_more(&self) -> bool {
        true
    }

    pub fn find_path(&self, path: &Path) -> Option<usize> {
        self.tabs.iter().position(|t| t.path.as_deref() == Some(path))
    }

    /// First tab whose `edit_gen != saved_edit_gen` (caller should sync active meta first).
    pub fn first_dirty_index(&self) -> Option<usize> {
        self.tabs.iter().position(|t| t.is_dirty())
    }

    fn bump_touch(&mut self, idx: usize) {
        self.touch_clock = self.touch_clock.wrapping_add(1);
        if let Some(t) = self.tabs.get_mut(idx) {
            t.touch_seq = self.touch_clock;
        }
    }

    /// Sync metadata from the live app session onto the active tab.
    pub fn sync_active_meta(
        &mut self,
        path: Option<PathBuf>,
        title: String,
        edit_gen: u64,
        saved_edit_gen: u64,
    ) {
        let t = self.active_mut();
        t.path = path;
        t.title = title;
        t.edit_gen = edit_gen;
        t.saved_edit_gen = saved_edit_gen;
    }

    pub fn set_active_nsfw(&mut self, nsfw: bool) {
        self.active_mut().nsfw = nsfw;
    }

    /// Park live workspace into the active tab slot.
    pub fn park_active(
        &mut self,
        workspace: Workspace,
        path: Option<PathBuf>,
        title: String,
        edit_gen: u64,
        saved_edit_gen: u64,
    ) {
        let i = self.active_index();
        self.bump_touch(i);
        let t = &mut self.tabs[i];
        t.path = path;
        t.title = title;
        t.edit_gen = edit_gen;
        t.saved_edit_gen = saved_edit_gen;
        t.parked = Some(ParkedCanvas::Warm { workspace });
    }

    /// Push a new active tab (caller installs doc into BeautifulApp; new empty workspace).
    pub fn push_active_new(
        &mut self,
        title: String,
        path: Option<PathBuf>,
        edit_gen: u64,
        saved_edit_gen: u64,
    ) -> Result<usize, &'static str> {
        self.push_active_new_ex(title, path, edit_gen, saved_edit_gen, false)
    }

    pub fn push_active_new_ex(
        &mut self,
        title: String,
        path: Option<PathBuf>,
        edit_gen: u64,
        saved_edit_gen: u64,
        nsfw: bool,
    ) -> Result<usize, &'static str> {
        if !self.can_open_more() {
            return Err("Too many open canvases — close a tab first");
        }
        let id = CanvasId(self.next_id);
        self.next_id += 1;
        self.touch_clock = self.touch_clock.wrapping_add(1);
        self.tabs.push(OpenCanvas {
            id,
            title,
            path,
            nsfw,
            edit_gen,
            saved_edit_gen,
            parked: None,
            touch_seq: self.touch_clock,
            unsaved_time_secs: 0,
        });
        self.active = self.tabs.len() - 1;
        Ok(self.active)
    }

    /// Take parked body for the active tab without switching index (Home → same tab).
    pub fn take_active_parked(&mut self) -> Option<ParkedCanvas> {
        if self.tabs.is_empty() {
            return None;
        }
        let i = self.active_index();
        self.bump_touch(i);
        self.tabs[i].parked.take()
    }

    /// Clear all canvas tabs (Home-only state after closing the last tab).
    pub fn clear_all(&mut self) {
        self.tabs.clear();
        self.active = 0;
    }

    /// True when there are no holst tabs (only Home is shown).
    pub fn has_no_tabs(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Ensure at least one tab exists before syncing meta / parking.
    pub fn ensure_primary(&mut self, title: impl Into<String>) {
        if self.tabs.is_empty() {
            *self = Self::new_primary(title);
        }
    }

    /// Take parked body for `idx` and make it active. Returns Warm workspace or Cold flag.
    pub fn activate(&mut self, idx: usize) -> Option<ParkedCanvas> {
        if idx >= self.tabs.len() {
            return None;
        }
        self.active = idx;
        self.bump_touch(idx);
        self.tabs[idx].parked.take()
    }

    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let active_id = self.tabs[self.active_index()].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self
            .tabs
            .iter()
            .position(|t| t.id == active_id)
            .unwrap_or(0);
    }

    /// Remove tab `idx`. Caller must not remove the last tab without replacing.
    /// If removing active, caller should have already switched away or taken body.
    pub fn remove(&mut self, idx: usize) -> Option<OpenCanvas> {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return None;
        }
        let removed = self.tabs.remove(idx);
        if idx < self.active {
            self.active -= 1;
        } else if idx == self.active {
            self.active = self.active.min(self.tabs.len() - 1);
        }
        Some(removed)
    }

    /// Cold-unload clean saved warm tabs beyond KEEP_HOT.
    pub fn cold_unload_excess(&mut self) {
        if self.tabs.len() <= KEEP_HOT_CANVASES {
            return;
        }
        let mut ranked: Vec<(u64, usize)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.active_index())
            .map(|(i, t)| (t.touch_seq, i))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        let keep: std::collections::HashSet<usize> = ranked
            .iter()
            .take(KEEP_HOT_CANVASES.saturating_sub(1)) // active already hot
            .map(|(_, i)| *i)
            .collect();

        for i in 0..self.tabs.len() {
            if i == self.active_index() || keep.contains(&i) {
                continue;
            }
            let tab = &mut self.tabs[i];
            if tab.is_dirty() || tab.path.is_none() {
                continue;
            }
            // Only cold-unload if the on-disk file is still there — otherwise a later
            // focus would install an empty canvas and sync could clobber identity.
            let path_ok = tab.path.as_ref().is_some_and(|p| p.is_file());
            if !path_ok {
                continue;
            }
            let Some(ParkedCanvas::Warm { workspace }) = tab.parked.take() else {
                continue;
            };
            // Multi-sheet holsts are not representable by a single file reload.
            if workspace.sheets().len() > 1 {
                tab.parked = Some(ParkedCanvas::Warm { workspace });
                continue;
            }
            let thumb = workspace
                .focused_sheet()
                .snapshot
                .as_ref()
                .map(|s| SheetThumb {
                    width: s.width,
                    height: s.height,
                    rgba: s.rgba.clone(),
                })
                .or_else(|| {
                    workspace.sheets().iter().find_map(|s| {
                        s.snapshot.as_ref().map(|sn| SheetThumb {
                            width: sn.width,
                            height: sn.height,
                            rgba: sn.rgba.clone(),
                        })
                    })
                });
            // Drop workspace (and all Documents) — reclaim RAM.
            drop(workspace);
            tab.parked = Some(ParkedCanvas::Cold { thumb });
        }
    }
}

/// Build a tiny title from a path.
pub fn title_from_path(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// egui drag id for canvas tab reorder.
pub fn tab_drag_id(canvas_id: CanvasId) -> egui::Id {
    egui::Id::new(("open_canvas_tab", canvas_id.0))
}

