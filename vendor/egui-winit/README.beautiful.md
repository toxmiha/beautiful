# Vendored egui-winit 0.33.3

Patches for Beautiful:

1. `with_transparent(true)` also sets winit `with_no_redirection_bitmap(true)`
   (`WS_EX_NOREDIRECTIONBITMAP`) so wgpu `DxgiFromVisual` is not covered by the
   Win32 redirection surface.

2. Idle `CursorMoved` repaints are coalesced (~30 Hz). Full rate while a pointer
   button is down (stroke latency).

3. Ctrl+V / paste always emits `Event::Paste` (including empty string) so
   image-only clipboards are not swallowed silently.
