# Handoff: bright white square in Beautiful workspace

**Project:** `C:\modding\beautiful` — Rust painting app, crates `beautiful-app` + `beautiful-core`, eframe 0.31 + egui + **wgpu**, Windows 11.

**User constraint (HARD):** Do **not** “fix” this by disabling `with_transparent`, disabling Win11 acrylic, or covering the workspace with opaque `#1E1E22` fills. The user wants **transparency + acrylic kept**. Prior agents repeatedly did those workarounds; user rejects them.

---

## Symptom

In the **central workspace** (between left/right panels) there is a **bright white filled rectangle/square**, often described as:

- sitting **behind / around** the canvas;
- roughly **HD-like** aspect on a **4K** document (4096×4096), while the real canvas outline is a **square**;
- Navigator thumb for a new 4K doc is correctly **square white** (document paper).

Screenshot context (earlier): large white landscape plate + thinner **square** gray outline inside it; status showed new 4096×4096 canvas.

---

## Proven by experiment

| Experiment | Result |
|---|---|
| Disable canvas GPU draw + egui texture + canvas border + brush cursor (`DEBUG_DISABLE_CANVAS_DRAW`) | **White square STILL present** |
| Therefore | **Not** `canvas_gpu` paint, **not** `paint_rotated_image`, **not** `paint_rotated_rect_stroke`, **not** brush cursor |
| User: acrylic is **not** the cause | Agents must not keep disabling acrylic; white also appears with plain transparency alone |
| Opaque desk / opaque `clear_color` | User sees as **breaking transparency** and rejects it even if it masks the plate |

---

## What the code actually paints white (full scan)

~37 `.rs` / `.wgsl` files under `crates/`.

### Large white fills (relevant)

1. **`Document.background = Rgba::WHITE`** — `beautiful-core/src/document.rs` (~line 53)  
   - Fed into composite (`composite.rs` writes `background.r/g/b` into flattened buffer).  
   - Uploaded to GPU canvas texture; shader forces opaque (`canvas_gpu.wgsl`: `return vec4(c.rgb, 1.0)`).  
   - This is the **intentional canvas paper**. It is **canvas-sized** when draw is on.  
   - **Ruled out as sole bug** because white remains when canvas draw is fully off.

2. **Framebuffer clear** — `beautiful-app/src/app.rs` `clear_color` → `[0,0,0,0]`  
   - Combined with `main.rs` `.with_transparent(true)`.  
   - CentralPanel `workspace_frame()` uses `BG_CHROME = (0,0,0,0)` (`theme.rs`).  
   - Side panels use translucent dark fills (`BG_PANEL*`).  
   - **Hypothesis (strong for draw-off case):** uncovered central pixels = transparent clear → on this **wgpu + Win11** stack often show as a **bright white plate** (swapchain often lacks real `CompositeAlphaMode::Pre/PostMultiplied`; egui-wgpu falls back to `Auto` and warns). This is a **compositing / surface alpha** issue, **not** an egui `rect_filled(WHITE)` in app code.

### Not a large workspace fill

- Selection marching ants / handles: white **strokes** only (`canvas.rs`).
- Navigator: white view-rect **stroke** + white tint on thumb image (`navigator.rs`).
- Palette cursor rings: white strokes.
- Mesh tint `Color32::WHITE` on textured quad = multiply tint, only when canvas texture path runs.

### No other `rect_filled(255,255,255)` in workspace

Grep does not show a dedicated “ghost HD square” drawable.

---

## Architecture (relevant paths)

```
main.rs
  ViewportBuilder.with_transparent(true)
  eframe + wgpu feature

app.rs
  clear_color = [0,0,0,0]
  apply_win11_acrylic(cc) → window_vibrancy::apply_acrylic(cc, None)
  CentralPanel.frame(workspace_frame()) → CanvasView::show

canvas.rs
  allocate_exact_size(viewport)
  optional DEBUG_DISABLE_CANVAS_DRAW (currently false)
  canvas_gpu::sync_from_document + paint_canvas(paint_rect ≈ canvas AABB)
  paint_rotated_rect_stroke (gray canvas border)

canvas_gpu.rs / .wgsl
  Persistent texture + textured quad, BlendState::REPLACE, alpha forced to 1
  invalidate() on document replace; paint gated on tex size == expect

theme.rs
  BG_CHROME / BG_CANVAS clear (0 alpha) for workspace
  BG_PANEL* translucent for docks
```

eframe 0.31 / egui-wgpu: when transparent window requested, alpha mode prefers PreMultiplied → PostMultiplied → else **Auto + warn** if surface has no transparent mode (`egui-wgpu` `winit.rs`).

---

## Dead ends / do NOT repeat

1. Disabling `with_transparent` or acrylic to “kill the white square”.
2. Covering CentralPanel with opaque `#1E1E22` / translucent “tint” desk as the product fix (user repeatedly rejects).
3. Blaming canvas stamp / LOD / brush performance for this visual (separate issues; brush opts already partially done).
4. Assuming the white plate is only “document paper” — draw-off test contradicts that for the **ghost** plate.

---

## Open hypotheses for next agent (priority order)

1. **Transparent hole + broken wgpu composite alpha on Win11**  
   Empty CentralPanel + `clear_color` α=0 → white plate. Needs a fix that **preserves** acrylic + window transparency, e.g.:
   - Force a wgpu backend / surface config that actually supports Pre/PostMultiplied alpha (DX12 DirectComposition / `DxgiFromHwnd` if exposable via eframe `wgpu_options`); or
   - Glow for window chrome only (hard with custom wgpu canvas); or
   - Another compositing approach that does **not** paint opaque desk over the whole workspace.

2. **Stale / wrong-size GPU quad** (secondary; only when canvas draw on)  
   Old HD-sized white texture after New/Open to 4K. Mitigation already partially present (`on_document_replaced`, `invalidate`, paint size gate). Does **not** explain draw-off white.

3. **User conflating document paper with ghost**  
   Ask for a screenshot with canvas draw off **and** with canvas on, marking which rect is the bug (inside gray border vs outside).

---

## Suggested next steps (without killing acrylic/transparency)

1. Log at startup: surface `alpha_modes`, chosen `CompositeAlphaMode`, adapter backend (DX12/Vulkan/GL).  
2. If only `Opaque` is available: treat as root cause of white-in-transparent-holes; research eframe 0.31 hooks for DX12 composition / transparent swapchain — **not** opaque fills.  
3. Keep `DEBUG_DISABLE_CANVAS_DRAW` toggle for A/B screenshots.  
4. Do not change `apply_acrylic` / `with_transparent` unless implementing a real alpha-capable path.

---

## Current intended product settings (restore after bad “fixes”)

- `with_transparent(true)` — **on**
- `apply_acrylic(..., None)` — **on**
- `clear_color` — `[0,0,0,0]`
- `BG_CHROME` / workspace frame — **fully clear**
- Canvas draw — **on**
- Document paper — white (normal)

---

## Related but separate issues (already touched in tree)

- Large soft brush lag: tip radial LUT, circle clip, rayon stamp rows, spacing loosen for large diameter (`tip.rs`, `engine.rs`).
- GPU size invalidation after New/Open (`canvas_gpu::invalidate`, `CanvasState::on_document_replaced`).

---

*Handoff written for another AI. Prefer evidence (draw-off test, alpha_modes log) over opaque desk workarounds.*
