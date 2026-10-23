//! Minimal X11 integration: hit-box input region and global pointer polling.
//!
//! The window is a normal ARGB window created through winit on XWayland. Two
//! things winit does not expose are needed here:
//!
//! - the input region, so only the pet's hit box receives clicks and the rest
//!   of the transparent window stays click-through;
//! - the global pointer position and button state, because once the pointer
//!   leaves the input region during a drag no further motion (or release)
//!   events reach the window.

use anyhow::{Context as _, Result, bail};
use pet_config::HIT_BOX;
use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use winit::window::Window;
use x11rb::connection::Connection as _;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{Atom, AtomEnum, ClipOrdering, ConnectionExt as _, KeyButMask, Rectangle};
use x11rb::rust_connection::RustConnection;

pub(crate) struct X11 {
    conn: RustConnection,
    root: u32,
    window: u32,
}

impl X11 {
    /// Connects to the same display winit used and resolves the native window id.
    pub(crate) fn new(window: &Window) -> Result<Self> {
        let handle = window.window_handle().context("query the native window handle")?.as_raw();
        let id = match handle {
            RawWindowHandle::Xlib(xlib) => xlib.window as u32,
            RawWindowHandle::Xcb(xcb) => xcb.window.get(),
            other => bail!("window is not an X11 window ({other:?})"),
        };
        let (conn, screen) = x11rb::connect(None).context("connect to the X server")?;
        let root = conn.setup().roots[screen].root;
        Ok(Self { conn, root, window: id })
    }

    /// Restricts pointer input to the pet's hit box.
    pub(crate) fn set_input_region(&self) -> Result<()> {
        let rect = Rectangle { x: HIT_BOX.x0 as i16, y: HIT_BOX.y0 as i16, width: HIT_BOX.width() as u16, height: HIT_BOX.height() as u16 };
        self.conn
            .shape_rectangles(SO::SET, SK::INPUT, ClipOrdering::UNSORTED, self.window, 0, 0, &[rect])?
            .check()
            .context("set the input region")?;
        Ok(())
    }

    /// Pointer position in root (global physical) coordinates and left-button state.
    pub(crate) fn pointer(&self) -> Result<(i32, i32, bool)> {
        let reply = self.conn.query_pointer(self.root)?.reply().context("query the pointer")?;
        let down = u16::from(reply.mask) & u16::from(KeyButMask::BUTTON1) != 0;
        Ok((i32::from(reply.root_x), i32::from(reply.root_y), down))
    }

    /// Work area of the current desktop (`_NET_WORKAREA`), or the whole root
    /// window when the window manager does not publish one.
    ///
    /// The property holds four CARDINALs per desktop: x, y, width, height. On a
    /// multi-monitor desktop it is the union of the monitors minus panels, which
    /// is exactly the space the pet is allowed to roam in.
    pub(crate) fn work_area(&self) -> Result<(i32, i32, u32, u32)> {
        let desktop = self.cardinal(self.root, "_NET_CURRENT_DESKTOP").unwrap_or(0);
        if let Some(values) = self.cardinals(self.root, "_NET_WORKAREA") {
            let base = desktop as usize * 4;
            if values.len() >= base + 4 {
                let (x, y, w, h) = (values[base], values[base + 1], values[base + 2], values[base + 3]);
                if w > 0 && h > 0 {
                    return Ok((x as i32, y as i32, w, h));
                }
            }
        }
        let geometry = self.conn.get_geometry(self.root)?.reply().context("read the root geometry")?;
        Ok((0, 0, u32::from(geometry.width), u32::from(geometry.height)))
    }

    fn atom(&self, name: &str) -> Option<Atom> {
        Some(self.conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
    }

    fn property(&self, window: u32, name: &str) -> Option<Vec<u32>> {
        let atom = self.atom(name)?;
        let reply = self.conn.get_property(false, window, atom, AtomEnum::CARDINAL, 0, u32::MAX).ok()?.reply().ok()?;
        if reply.format != 32 {
            return None;
        }
        Some(reply.value32().map(Iterator::collect).unwrap_or_default())
    }

    fn cardinal(&self, window: u32, name: &str) -> Option<u32> {
        self.property(window, name)?.first().copied()
    }

    fn cardinals(&self, window: u32, name: &str) -> Option<Vec<u32>> {
        self.property(window, name)
    }
}
