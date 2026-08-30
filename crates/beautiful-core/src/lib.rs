mod abr;
mod brush;
mod brush_assets;
mod brush_v2;
mod color;
mod curve;
mod composite;
mod demo;
mod display_lod;
mod display_plate;
mod display_tile;
mod display_sync;
mod doc_op;
mod document;
mod engine;
#[cfg(test)]
mod eraser_test;
mod fill;
pub mod filters;
mod flood;
mod gradient;
mod history;
mod io;
mod jobs;
mod layer;
mod mask_tiles;
mod omit_above;
pub mod perf_probe;
mod preview;
mod projection;
mod psd;
mod resample;
mod selection;
mod shape;
mod stabilizer;
mod stroke_stack;
mod tiles;
mod tip;
mod txmh;
mod text;
mod visibility_cache;
mod warp;

pub use brush::{
    BrushBackend, BrushKind, BrushSettings, BrushShape, BrushTexture, HairDirection, PaintMode,
    StrokeState, BRUSH_SIZE_MAX, BRUSH_SIZE_MIN,
};
pub use abr::{
    extract_abr, extract_abr_tips, import_abr_assets, import_abr_tips_to_dir, AbrExtract,
    AbrImportPaths, AbrPattern, AbrTip,
};
pub use brush_assets::{
    decode_to_gray_png_file, export_btbrush, import_btbrush,     load_asset_thumb, load_gray, load_rgb,
    sample_paper, sample_pattern_doc, sample_shape, shape_outline, AssetKind, BtbrushPack, GrayMap,
    GrayPolarity, RgbMap, MAX_ASSET_SIDE,
};
pub use brush_v2::{
    BrushDef, BrushGraphNode, BrushGraphNodeData, BrushGraphWire, BrushNodeGraph, BrushOutField,
    CompileError,
};
pub use color::{
    linear_to_srgb, load_premul_linear, make_src_premul, source_over_premul, srgb_to_linear,
    store_premul_linear, warm_srgb_luts, DrawingColorSlot, Rgba,
};
pub use curve::{CurveLut, CurvePoint, TransferCurve};
pub use composite::{
    blend_one_layer_packed, blend_layers_range_packed, composite_region_into, composite_region_packed_into,
    composite_region_packed_into_skip, has_visible_adjustment, has_visible_spatial_adjustment,
    CompositeCache, DirtyRect, FloatingBlit, SyncResult, COMPOSITE_BUDGET_PX,
};
pub use filters::{
    AdjustmentKind, BevelMode, BlurEdges, ChromaMode, DitherMethod, FisheyeModel, GlitchMethod, HalftoneMode,
    HalftonePaper, LevelsChannel, LiquidGlassMode, NoiseMethod, OutlineMode, PixelizeMethod,
    ReplaceAffect,
    RippleMode, VignetteShape, isolate_by_coverage, with_blur_edges,
};
pub use projection::{budget as projection_budget, Projection, ProjectionBackend};
pub use display_lod::{
    build_navigator_thumb, build_navigator_thumb_box, build_navigator_thumb_from_layers,
    build_navigator_thumb_from_layers_roi, build_navigator_thumb_from_tiles,
    build_navigator_thumb_from_tiles_roi,
    document_peak_bytes, document_size_allowed, lod_factor_for_document,
    lod_factor_for_document_with_view, lod_factor_for_zoom,
    lod_factor_for_zoom_hysteresis, lod_max_sharp_for_zoom, lod_min_for_gpu_cap,
    resolve_display_lod, size_adjusted_zoom, clamp_gpu_tex_side,
    DisplayMip, GPU_TEX_SIDE_LOW, MAX_DOC_SIDE, MAX_GPU_TEX_SIDE, MAX_LAYER_PIXEL_BYTES,
};
pub use display_plate::{
    compose_vdp_partial, compose_viewport_plate, doc_to_plate_rect, plan_viewport_plate,
    use_viewport_plate, viewport_plate_linear_filter, ViewportPlatePlan,
};
pub use display_tile::{
    cover_exposed_new_doc, display_tile_grid_len, display_tile_key, extract_display_tile_pixels,
    gpu_tile_cache_retain_all, occupancy_to_authoring_tiles, occupancy_to_display_plates,
    snap_rects_to_display_tiles, tile_doc_rect, DisplayTileCache, DISPLAY_TILE_DOC,
    GPU_DISPLAY_TILE_CACHE_BUDGET,
};
pub use display_sync::{
    apply_mip_action, mip_dims, mip_size_matches, plan_display_frame, plan_mip_action,
    skip_projection_for_mip, update_mip_partial, ApplyMipResult, DISPLAY_VIEW_PAD,
    DisplayFramePlan, MipAction,
};
pub use demo::{
    apply_event, decode_demo_bytes, encode_demo_file, load_demo_from_path, path_has_demo,
    play_until, save_sidecar, sidecar_path, spawn_replay_document, DemoEvent, DemoFile, DemoLog,
    DemoStrokeKind,
};
pub use doc_op::{DocOp, DocOpJournal, DocOpKind};
pub use document::{Document, LayerDropPlace, StageRect};
pub use fill::{FillEngine, FillOptions, FillSampleSource};
pub use gradient::{
    gradient_t, lerp_stops_dithered, snap_gradient_end, GradientEnds, GradientInterp,
    GradientOptions, GradientShape,
};
pub use history::SelectionSnap;
pub use io::{
    apply_raster_opts, document_from_rgba, export_image_format, export_jpeg, export_jpeg_with_opts,
    export_png, export_png_with_opts, export_psd_flat, load_document, load_document_with_progress,
    load_raster_bytes, load_raster_image, save_document, ColorRange, ExportBackground, IoError,
    PngCompression, RasterExportOpts,
};
pub use jobs::CancelToken;
pub use layer::{
    ancestor_folder_clip_cov, ancestor_folder_mask_cov, ancestor_folder_mask_cov_span,
    ancestor_folder_opacity, ancestor_has_folder_clip, ancestor_has_folder_mask, blend_over,
    blend_rgb, clip_base_alpha, clip_base_index, effective_blend_mode, layer_effectively_locked,
    layer_effectively_visible, BlendMode, Layer,
};
pub use mask_tiles::AlphaTileMap;
pub use omit_above::{is_omitted as transform_layer_omitted, OmitAboveGuard};
pub use preview::{
    encode_document_preview_jpeg, load_file_preview, load_file_preview_max, FilePreview,
};
pub use psd::{export_psd_layered, load_psd, load_psd_with_progress};
pub use resample::{
    apply_transform_rgba, flip_layer_horizontal, flip_layer_vertical, transform_output_size,
    raster_transform_rgba_rect, resample_bilinear, resample_lanczos3, resample_nearest,
    LivePixelRect, ResampleFilter,
};
pub use selection::{
    outline_from_mask, outline_is_ready, snap_doc_xy, FloatingSelection, Selection,
    SelectionCombine, SelectionMask, SelectionOutline, SelectionRect,
};
pub use shape::{
    arrow_head, dash_visible, ellipse_sdf, ellipse_stroke, poly_dash_dist, poly_sdf, rect_sdf,
    rect_stroke_sharp, shape_polygon, stroke_from_sdf, ShapeKind, ShapeOptions, StrokeAlign,
    StrokeDash,
};
pub use stabilizer::{Stabilizer, StabilizerPreset};
pub use stroke_stack::StrokeStack;
pub use text::{
    hit_test_caret, layout_glyphs, preview_line_rgba, rasterize_cached, reflow_layout,
    register_font_bytes, rotation_needs_trig, try_layout_append, wrap_rotation_deg, GlyphInfo,
    GlyphTweak, TextAlignH, TextAlignV, TextAntiAlias, TextLayout, TextObject, TextPathMode,
    TextPayload, TextRasterCache, TextSpan, TextStyle,
};
pub use tiles::{PaintTileMap, TileBuffer, TileKey, TILE_BYTES, TILE_SIZE};
pub use txmh::{
    load_txmh, load_txmh_with_progress, load_txmh_workspace, load_txmh_workspace_with_progress,
    save_txmh, save_txmh_recovery, save_txmh_workspace, TxmhSheetMeta, TxmhWorkspace,
};
pub use warp::{
    adjacent_secondary_whiskers, apply_warp_whisker_drag, bend_warp_edge_handles,
    default_warp_corner_handles, default_warp_handle_unison, default_warp_node_handles,
    estimate_warp_uv, eval_ffd_bilinear, eval_warp_surface, eval_warp_surface_ex,
    eval_warp_surface_nodes, inverse_bilinear_quad, mesh_warp_rgba, mesh_warp_rgba_ex,
    mesh_warp_rgba_rect, nearest_warp_bezier_edge, opposite_edge_node, pull_warp_patch_at_uv, refit_warp_handles_near,
    refit_warp_handles_smooth, split_warp_axis, split_warp_crosswise, warp_anchor_kind,
    warp_bake_cell_subdiv, warp_live_tess_steps, WarpAnchorKind, WarpBezierEdge,
};
