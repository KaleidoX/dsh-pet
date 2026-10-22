//! The only `unsafe` code in this crate.
//!
//! wgpu has no safe constructor for a surface on a bare `wl_surface`: the only
//! entry point is [`wgpu::Instance::create_surface_unsafe`], which takes raw
//! handles. Both handles are pointers into libwayland's C objects, so they can
//! only come from `wayland-backend`'s FFI accessors, and each needs a SAFETY
//! argument about lifetime and thread access.
//!
//! This is why the crate does not inherit the workspace's `unsafe_code`
//! forbid (same reason as `pet-win32`). Nothing outside this module may write
//! `unsafe`.

use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

use anyhow::{Result, anyhow};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};

/// The `wl_display` behind a [`Connection`], in a form wgpu's instance
/// descriptor accepts.
///
/// `wgpu::InstanceDescriptor::display` is `Box<dyn WgpuHasDisplayHandle>`, which
/// requires `Send + Sync + 'static`; [`NonNull`] is neither `Send` nor `Sync`,
/// so the pointer needs a wrapper that promises what the Wayland connection
/// already guarantees.
pub(crate) struct DisplayPtr(NonNull<c_void>);

impl DisplayPtr {
    pub(crate) fn new(conn: &Connection) -> Result<Self> {
        // `Backend::display_ptr` is a raw accessor: null if the connection is
        // already gone, which at this point means the compositor hung up.
        let ptr = conn.backend().display_ptr().cast::<c_void>();
        let ptr = NonNull::new(ptr).ok_or_else(|| anyhow!("the Wayland connection reported a null wl_display"))?;
        Ok(Self(ptr))
    }
}

impl fmt::Debug for DisplayPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DisplayPtr").field(&self.0).finish()
    }
}

// SAFETY: the wrapped pointer is a `wl_display` owned by the `Connection` that
// produced it. Rust drops struct fields in declaration order, and
// `pet_wayland::app::WaylandApp` declares its `conn` clone *last*, so the
// connection — and with it the display — outlives every wgpu object, the
// `LayerSurface` and all protocol proxies, which are declared before it. The
// `WaylandSource` in the event loop holds another clone and may drop earlier,
// but the display is freed only when the last clone goes. Every consumer
// (wgpu's Vulkan/GLES backends) only reads the pointer to identify the
// connection, and libwayland serialises its own access internally, so sharing
// the immutable pointer across threads is sound. The pointer is never used
// after the last `Connection` clone drops.
unsafe impl Send for DisplayPtr {}
unsafe impl Sync for DisplayPtr {}

impl HasDisplayHandle for DisplayPtr {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(self.0));
        // SAFETY: `raw` holds a live `wl_display` for as long as the `Connection`
        // that created it lives, which outlives `self` (see the impl note above).
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

/// The `wl_surface` as a raw window handle, for `create_surface_unsafe`.
///
/// Returned by value rather than wrapped: wgpu only needs it for the duration
/// of the call, and the surface itself is owned by `WaylandApp`.
pub(crate) fn window_handle(surface: &WlSurface) -> Result<RawWindowHandle> {
    let ptr = surface.id().as_ptr().cast::<c_void>();
    let ptr = NonNull::new(ptr).ok_or_else(|| anyhow!("the wl_surface has already been destroyed"))?;
    Ok(RawWindowHandle::Wayland(WaylandWindowHandle::new(ptr)))
}
