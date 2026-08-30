# Vendored eframe 0.33.3

Beautiful patches:

1. `DeviceEvent::MouseMotion` no longer forces `RepaintNext` while idle.
   Relative motion is still fed into egui for stroke densify; a frame is only
   scheduled when a pointer button is down.

   Without this, Windows fires MouseMotion at device Hz and bypasses the
   egui-winit idle `CursorMoved` throttle — ~6% CPU just hovering the canvas.

2. wgpu surface: live drag-resize still uses `on_window_resized` / ResizeBuffers
   (recreating the DxgiFromVisual surface every WM_SIZE flickered). A full
   surface recreate (`set_window` None then Some) runs only on a large size
   jump or when the client size matches the monitor — Tab hide-UI covers the
   display, and ResizeBuffers does not `SetContent`+`Commit` the DComp visual.
