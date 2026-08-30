//! Linux compositor backdrop blur (KWin X11 atom + Wayland ext-background-effect-v1).
//! Windows Acrylic stays in `apply_window_material` via DWM.

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::sync::Mutex;

/// Enable or disable compositor blur behind the window.
pub fn apply(window: &(impl HasWindowHandle + HasDisplayHandle), enable: bool) {
    match apply_inner(window, enable) {
        Ok(msg) => crate::action_log::log("ui", &format!("linux blur: {msg}")),
        Err(e) => crate::action_log::log("ui", &format!("linux blur skipped: {e}")),
    }
}

fn apply_inner(
    window: &(impl HasWindowHandle + HasDisplayHandle),
    enable: bool,
) -> Result<&'static str, String> {
    let wh = window
        .window_handle()
        .map_err(|e| format!("window handle: {e}"))?;
    let dh = window
        .display_handle()
        .map_err(|e| format!("display handle: {e}"))?;
    match (wh.as_raw(), dh.as_raw()) {
        (RawWindowHandle::Xlib(w), RawDisplayHandle::Xlib(d)) => {
            let display = d.display.map(|p| p.as_ptr()).ok_or("xlib display null")?;
            apply_x11_kwin_blur(display, w.window, enable)?;
            Ok(if enable {
                "X11 _KDE_NET_WM_BLUR_BEHIND_REGION"
            } else {
                "X11 blur cleared"
            })
        }
        (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) => {
            apply_wayland_ext_blur(d.display.as_ptr(), w.surface.as_ptr(), enable)?;
            Ok(if enable {
                "Wayland ext-background-effect-v1"
            } else {
                "Wayland blur cleared"
            })
        }
        _ => Err("not X11/Wayland".into()),
    }
}

fn apply_x11_kwin_blur(
    display: *mut std::ffi::c_void,
    window: std::os::raw::c_ulong,
    enable: bool,
) -> Result<(), String> {
    use x11_dl::xlib::{Display, PropModeReplace, Xlib, XA_CARDINAL};
    unsafe {
        let xlib = Xlib::open().map_err(|e| format!("libX11: {e}"))?;
        let dpy = display as *mut Display;
        let atom_name = b"_KDE_NET_WM_BLUR_BEHIND_REGION\0";
        let atom = (xlib.XInternAtom)(dpy, atom_name.as_ptr().cast(), 0);
        if atom == 0 {
            return Err("XInternAtom failed".into());
        }
        if enable {
            // Empty property = whole window (KWin).
            (xlib.XChangeProperty)(
                dpy,
                window,
                atom,
                XA_CARDINAL,
                32,
                PropModeReplace,
                std::ptr::null(),
                0,
            );
        } else {
            (xlib.XDeleteProperty)(dpy, window, atom);
        }
        (xlib.XFlush)(dpy);
    }
    Ok(())
}

struct WaylandBlurKeep {
    _conn: wayland_client::Connection,
    _queue: wayland_client::EventQueue<WaylandBlurHost>,
    _effect: wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
}

static WAYLAND_KEEP: Mutex<Option<WaylandBlurKeep>> = Mutex::new(None);

struct WaylandBlurHost {
    compositor: Option<wayland_client::protocol::wl_compositor::WlCompositor>,
    manager: Option<
        wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
    >,
    blur_cap: bool,
}

impl Default for WaylandBlurHost {
    fn default() -> Self {
        Self {
            compositor: None,
            manager: None,
            blur_cap: false,
        }
    }
}

fn apply_wayland_ext_blur(
    display: *mut std::ffi::c_void,
    surface: *mut std::ffi::c_void,
    enable: bool,
) -> Result<(), String> {
    use wayland_client::{
        backend::ObjectId,
        protocol::wl_surface::WlSurface,
        Connection, Proxy,
    };
    use wayland_backend::sys::client::Backend;

    if !enable {
        if let Ok(mut g) = WAYLAND_KEEP.lock() {
            *g = None;
        }
        return Ok(());
    }

    let backend = unsafe { Backend::from_foreign_display(display.cast()) };
    let conn = Connection::from_backend(backend);
    let mut queue = conn.new_event_queue::<WaylandBlurHost>();
    let qh = queue.handle();
    let mut host = WaylandBlurHost::default();
    let _registry = conn.display().get_registry(&qh, ());
    queue
        .roundtrip(&mut host)
        .map_err(|e| format!("wayland roundtrip: {e}"))?;
    // Capability event is sent when the manager is bound — flush it.
    queue
        .roundtrip(&mut host)
        .map_err(|e| format!("wayland capability roundtrip: {e}"))?;

    let Some(manager) = host.manager.clone() else {
        return Err("no ext-background-effect-v1".into());
    };
    if !host.blur_cap {
        return Err("compositor has no blur capability".into());
    }
    let Some(compositor) = host.compositor.clone() else {
        return Err("no wl_compositor".into());
    };

    let surface_id = unsafe {
        ObjectId::from_ptr(WlSurface::interface(), surface.cast())
            .map_err(|e| format!("surface id: {e}"))?
    };
    let wl_surface =
        WlSurface::from_id(&conn, surface_id).map_err(|e| format!("wl_surface: {e}"))?;

    let effect = manager.get_background_effect(&wl_surface, &qh, ());
    let region = compositor.create_region(&qh, ());
    let span = i32::MAX / 2;
    region.add(0, 0, span, span);
    effect.set_blur_region(Some(&region));
    region.destroy();

    *WAYLAND_KEEP.lock().unwrap_or_else(|e| e.into_inner()) = Some(WaylandBlurKeep {
        _conn: conn,
        _queue: queue,
        _effect: effect,
    });
    Ok(())
}

impl wayland_client::Dispatch<wayland_client::protocol::wl_registry::WlRegistry, ()>
    for WaylandBlurHost
{
    fn event(
        state: &mut Self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _: &(),
        _: &wayland_client::Connection,
        qh: &wayland_client::QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_registry::Event;
        if let Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_compositor" {
                state.compositor = Some(registry.bind::<
                    wayland_client::protocol::wl_compositor::WlCompositor,
                    _,
                    _,
                >(name, version.min(4), qh, ()));
            } else if interface == "ext_background_effect_manager_v1" {
                state.manager = Some(registry.bind(name, 1, qh, ()));
            }
        }
    }
}

impl wayland_client::Dispatch<wayland_client::protocol::wl_compositor::WlCompositor, ()>
    for WaylandBlurHost
{
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_compositor::WlCompositor,
        _: wayland_client::protocol::wl_compositor::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<wayland_client::protocol::wl_region::WlRegion, ()> for WaylandBlurHost {
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_region::WlRegion,
        _: wayland_client::protocol::wl_region::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl
    wayland_client::Dispatch<
        wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
        (),
    > for WaylandBlurHost
{
    fn event(
        state: &mut Self,
        _: &wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
        event: wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
        use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::Event;
        if let Event::Capabilities { flags } = event {
            let _ = flags;
            state.blur_cap = true;
        }
    }
}

impl
    wayland_client::Dispatch<
        wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
        (),
    > for WaylandBlurHost
{
    fn event(
        _: &mut Self,
        _: &wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
        _: wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
    }
}
