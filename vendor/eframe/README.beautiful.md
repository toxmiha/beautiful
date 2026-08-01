# Vendored eframe 0.33.3

Beautiful patch: `DeviceEvent::MouseMotion` no longer forces `RepaintNext` while
idle. Relative motion is still fed into egui for stroke densify; a frame is only
scheduled when a pointer button is down.

Without this, Windows fires MouseMotion at device Hz and bypasses the
egui-winit idle `CursorMoved` throttle — ~6% CPU just hovering the canvas.
