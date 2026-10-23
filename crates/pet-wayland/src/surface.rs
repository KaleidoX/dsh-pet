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
use pet_config::{CANVAS_H, CANVAS_W, Position};
use pet_overlay::SceneLayout;
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
    pub(crate) fn apply_canvas(&self, layer: &LayerSurface, layout: SceneLayout) {
        let offset = layout.offset_points();
        let far_x = (layout.size.0 as f32 - CANVAS_W as f32) / layout.ppp - offset.x;
        let far_y = (layout.size.1 as f32 - CANVAS_H as f32) / layout.ppp - offset.y;
        layer.set_anchor(self.anchor);
        layer.set_margin(
            self.top - offset.y.round() as i32,
            self.right - far_x.round() as i32,
            self.bottom - far_y.round() as i32,
            self.left - offset.x.round() as i32,
        );
    }

    /// Top-left corner inside the output, in output-logical pixels.
    pub(crate) fn local_origin(&self, output: (i32, i32), window: (u32, u32)) -> (f32, f32) {
        let x = if self.anchor.contains(Anchor::RIGHT) { (output.0 - window.0 as i32 - self.right) as f32 } else { self.left as f32 };
        let y = if self.anchor.contains(Anchor::BOTTOM) { (output.1 - window.1 as i32 - self.bottom) as f32 } else { self.top as f32 };
        (x, y)
    }

    /// Stores the canvas origin in output-logical pixels. SceneLayout constrains
    /// the native window; clamping the canvas here would apply a second, different
    /// limit after the scene has already painted its offset.
    pub(crate) fn set_local_origin(&mut self, origin: (f32, f32), output: (i32, i32), window: (u32, u32)) {
        let (x, y) = origin;

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

/// Native surface extent in logical points, including room for controls.
pub(crate) fn window_size(ppp: f32) -> (u32, u32) {
    let size = SceneLayout::new(ppp).size_points();
    (size.x.round() as u32, size.y.round() as u32)
}

/// Pet hit box when idle, full local window while a menu can be dismissed.
pub(crate) fn set_input_region(compositor: &CompositorState, layer: &LayerSurface, layout: SceneLayout, menu: bool) -> Result<()> {
    let region = Region::new(compositor)?;
    let rect = if menu { egui::Rect::from_min_size(egui::Pos2::ZERO, layout.size_points()) } else { layout.hit_rect() };
    let x = rect.min.x.floor() as i32;
    let y = rect.min.y.floor() as i32;
    region.add(x, y, rect.max.x.ceil() as i32 - x, rect.max.y.ceil() as i32 - y);
    layer.set_input_region(Some(region.wl_region()));
    Ok(())
}
