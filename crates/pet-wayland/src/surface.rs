//! Layer surface geometry and per-surface protocol state.
//!
//! A layer surface cannot be positioned the way a toplevel can: there is no
//! "move to x,y". Placement is expressed as *anchors* plus *margins*, where each
//! margin is the distance from the anchored edge of the output to the
//! corresponding edge of the surface. The configured corner therefore maps
//! directly onto an anchor, and the configured margins are already in output
//! logical pixels — no scaling needed.
//!
//! Dragging is the inverse mapping: a desired top-left corner (in canvas pixels,
//! which are output-local) becomes two margins, which are then double-buffered
//! and latched by the next `wl_surface.commit` (the one `Queue::present` issues).

use anyhow::Result;
use pet_config::{CANVAS_H, CANVAS_W, HIT_BOX, Position};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, LayerSurface};

/// Surface size in output-logical pixels for a canvas drawn at `ppp` physical
/// pixels per point. The buffer stays `CANVAS_W × CANVAS_H`; `wp_viewport`
/// stretches it onto this rectangle.
pub(crate) fn logical_size(ppp: f32) -> (u32, u32) {
    let width = (CANVAS_W as f32 / ppp).round().max(1.0) as u32;
    let height = (CANVAS_H as f32 / ppp).round().max(1.0) as u32;
    (width, height)
}

/// The anchor and margins that pin the surface to one corner of its output.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Placement {
    pub(crate) anchor: Anchor,
    /// Distances from the anchored output edges, in output-logical pixels.
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

impl Placement {
    /// Pins the surface to the output's top-left corner.
    ///
    /// Used by the overlay window, whose position is always computed rather than
    /// configured: with only `TOP | LEFT` anchored, [`Self::set_local_origin`]
    /// writes exactly the two margins that place it.
    pub(crate) fn top_left() -> Self {
        Self { anchor: Anchor::TOP | Anchor::LEFT, left: 0, right: 0, top: 0, bottom: 0 }
    }

    /// The corner and margins from the config file.
    ///
    /// Exactly one horizontal and one vertical edge is anchored per corner, so
    /// only those two margins are meaningful; the other two stay zero.
    pub(crate) fn from_position(position: &Position) -> Self {
        let mut placement = Self { anchor: Anchor::empty(), left: 0, right: 0, top: 0, bottom: 0 };
        match position.corner.as_str() {
            "top-left" => {
                placement.anchor = Anchor::TOP | Anchor::LEFT;
                placement.left = position.margin_x;
                placement.top = position.margin_y;
            }
            "bottom-left" => {
                placement.anchor = Anchor::BOTTOM | Anchor::LEFT;
                placement.left = position.margin_x;
                placement.bottom = position.margin_y;
            }
            "bottom-right" => {
                placement.anchor = Anchor::BOTTOM | Anchor::RIGHT;
                placement.right = position.margin_x;
                placement.bottom = position.margin_y;
            }
            other => {
                if other != "top-right" {
                    log::warn!("unknown corner {other:?}; using top-right");
                }
                placement.anchor = Anchor::TOP | Anchor::RIGHT;
                placement.right = position.margin_x;
                placement.top = position.margin_y;
            }
        }
        placement
    }

    /// Pushes the anchor and margins to the protocol object. They are latched by
    /// the next commit.
    pub(crate) fn apply(&self, layer: &LayerSurface) {
        layer.set_anchor(self.anchor);
        layer.set_margin(self.top, self.right, self.bottom, self.left);
    }

    /// Top-left corner inside the output, in output-logical pixels.
    pub(crate) fn local_origin(&self, output: (i32, i32), window: (u32, u32)) -> (f32, f32) {
        let x = if self.anchor.contains(Anchor::RIGHT) { (output.0 - window.0 as i32 - self.right) as f32 } else { self.left as f32 };
        let y = if self.anchor.contains(Anchor::BOTTOM) { (output.1 - window.1 as i32 - self.bottom) as f32 } else { self.top as f32 };
        (x, y)
    }

    /// Moves the surface so its top-left lands on `origin` (output-logical
    /// pixels), clamped to the output.
    ///
    /// Clamping happens here rather than being left to the compositor so that
    /// [`Self::local_origin`] keeps reporting where the surface really is; the
    /// drag spring feeds on that value and would otherwise chase a position the
    /// compositor refused to apply.
    pub(crate) fn set_local_origin(&mut self, origin: (f32, f32), output: (i32, i32), window: (u32, u32)) {
        let max_x = (output.0 - window.0 as i32).max(0) as f32;
        let max_y = (output.1 - window.1 as i32).max(0) as f32;
        let x = origin.0.clamp(0.0, max_x);
        let y = origin.1.clamp(0.0, max_y);

        if self.anchor.contains(Anchor::RIGHT) {
            self.right = (output.0 as f32 - window.0 as f32 - x).round() as i32;
        } else {
            self.left = x.round() as i32;
        }
        if self.anchor.contains(Anchor::BOTTOM) {
            self.bottom = (output.1 as f32 - window.1 as f32 - y).round() as i32;
        } else {
            self.top = y.round() as i32;
        }
    }
}

/// Restricts pointer input to the pet's hit box, so clicks anywhere else on the
/// transparent canvas reach whatever is underneath.
///
/// Callers commit: the region is double-buffered like every other surface state.
pub(crate) fn set_hit_region(compositor: &CompositorState, layer: &LayerSurface, ppp: f32) -> Result<()> {
    let region = Region::new(compositor)?;
    let x0 = (HIT_BOX.x0 as f32 / ppp).round() as i32;
    let y0 = (HIT_BOX.y0 as f32 / ppp).round() as i32;
    let x1 = (HIT_BOX.x1 as f32 / ppp).round() as i32;
    let y1 = (HIT_BOX.y1 as f32 / ppp).round() as i32;
    region.add(x0, y0, (x1 - x0).max(1), (y1 - y0).max(1));
    layer.set_input_region(Some(region.wl_region()));
    Ok(())
}

/// Sets the overlay surface's input region: empty while it is click-through, or
/// the whole surface while the menu is open.
///
/// An empty region is the protocol's way of saying "no input at all" — a null
/// region would mean the opposite (infinite), so one is always created and left
/// empty in the click-through case. The surface keeps being drawn, which is
/// what the tooltip and the score card need. The overlay is a separate surface
/// from the pet, so this never affects the pet's own hit box.
///
/// Callers commit: the region is double-buffered.
pub(crate) fn set_overlay_region(compositor: &CompositorState, layer: &LayerSurface, size: (u32, u32), interactive: bool) -> Result<()> {
    let region = Region::new(compositor)?;
    if interactive {
        region.add(0, 0, size.0.max(1) as i32, size.1.max(1) as i32);
    }
    layer.set_input_region(Some(region.wl_region()));
    Ok(())
}
