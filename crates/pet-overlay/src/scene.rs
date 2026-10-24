//! Shared layout and composition for the single native pet window.

use crate::{OVERLAY_H, OVERLAY_W, OverlayInput, OverlayMode, OverlayState};
use egui::{Rect, Vec2};
use pet_config::{CANVAS_H, CANVAS_W, HIT_BOX, PetConfig, WorldArea};
use pet_ui_core::{FrameInput, FrameOutput, PetCore};
use std::path::Path;

/// Native window geometry. The pet canvas remains in physical pixels while
/// controls use logical points. Both hosts use these conversions at their boundary.
#[derive(Clone, Copy, Debug)]
pub struct SceneLayout {
    pub ppp: f32,
    pub size: (u32, u32),
    pub offset: (i32, i32),
    compact: bool,
}

impl SceneLayout {
    pub fn new(ppp: f32) -> Self {
        let ppp = ppp.max(0.01);
        let size = ((OVERLAY_W * ppp).round().max(CANVAS_W as f32) as u32, (OVERLAY_H * ppp).round().max(CANVAS_H as f32) as u32);
        Self { ppp, size, offset: (((size.0 - CANVAS_W) / 2) as i32, ((size.1 - CANVAS_H) / 2) as i32), compact: false }
    }

    /// A moving pet needs no transparent control margins. Its image stays at
    /// (0, 0), so moving the native window cannot race a compensating image move.
    pub fn compact(ppp: f32) -> Self {
        Self { ppp: ppp.max(0.01), size: (CANVAS_W, CANVAS_H), offset: (0, 0), compact: true }
    }

    pub fn is_compact(self) -> bool {
        self.compact
    }

    pub fn offset_points(self) -> Vec2 {
        egui::vec2(self.offset.0 as f32 / self.ppp, self.offset.1 as f32 / self.ppp)
    }

    pub fn size_points(self) -> Vec2 {
        egui::vec2(self.size.0 as f32 / self.ppp, self.size.1 as f32 / self.ppp)
    }

    pub fn canvas_origin(self, window: (i32, i32)) -> (i32, i32) {
        (window.0 + self.offset.0, window.1 + self.offset.1)
    }

    pub fn window_origin(self, canvas: (i32, i32)) -> (i32, i32) {
        (canvas.0 - self.offset.0, canvas.1 - self.offset.1)
    }

    /// Keep controls inside the work area without changing the pet's position.
    /// Window managers may otherwise clamp the native window and move the pet.
    pub fn place(&mut self, canvas: (i32, i32), world: WorldArea) -> (i32, i32) {
        if self.compact {
            self.offset = (0, 0);
            return canvas;
        }
        let centered = Self::new(self.ppp).window_origin(canvas);
        let origin = if world.width > 0 && world.height > 0 {
            (
                centered.0.clamp(world.x, (world.right() - self.size.0 as i32).max(world.x)),
                centered.1.clamp(world.y, (world.bottom() - self.size.1 as i32).max(world.y)),
            )
        } else {
            centered
        };
        self.offset = (canvas.0 - origin.0, canvas.1 - origin.1);
        origin
    }

    pub fn hit_rect(self) -> Rect {
        Rect::from_min_max(
            egui::pos2(HIT_BOX.x0 as f32 / self.ppp, HIT_BOX.y0 as f32 / self.ppp),
            egui::pos2(HIT_BOX.x1 as f32 / self.ppp, HIT_BOX.y1 as f32 / self.ppp),
        )
        .translate(self.offset_points())
    }
}

/// Owns controls directly; there is no secondary viewport or callback state.
pub struct PetScene {
    overlay: OverlayState,
}

impl PetScene {
    pub fn new(assets: &Path, config: &PetConfig) -> Self {
        Self { overlay: OverlayState::new(assets, config) }
    }

    pub fn menu_open(&self) -> bool {
        self.overlay.mode() == OverlayMode::Menu
    }

    /// Input arrives in native-window points and physical screen coordinates.
    /// Only the core sees canvas-local points and the canvas's screen origin.
    pub fn ui(&mut self, ui: &mut egui::Ui, core: &mut PetCore, mut input: FrameInput, layout: &mut SceneLayout) -> FrameOutput {
        let menu_was_open = self.menu_open();
        for action in self.overlay.take_actions() {
            core.overlay_action(action);
        }
        let window_origin = input.window_origin;
        self.overlay.set_window_origin(window_origin);
        self.overlay.set_world(input.world, input.ppp);
        self.overlay.set_input(OverlayInput { pointer: input.global });
        input.window_origin = layout.canvas_origin(window_origin);
        input.pointer = input.pointer.map(|point| point - layout.offset_points());
        if menu_was_open {
            input.pointer = None;
            input.pressed = false;
            input.released = false;
            input.secondary_pressed = false;
        }
        let out = core.update(ui, input);
        if let Some(canvas_origin) = out.move_to {
            self.overlay.set_window_origin(layout.place(canvas_origin, input.world));
        }
        core.paint(ui, input.now, input.ppp, layout.offset_points());
        if let Some(score) = out.score {
            self.overlay.apply_score(score);
        }
        self.overlay.apply_intent(out.overlay.clone());
        let ctx = ui.ctx().clone();
        if self.overlay.ui(&ctx, ui, input.now) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        for action in self.overlay.take_actions() {
            core.overlay_action(action);
        }
        self.overlay.tick(input.now);
        out
    }
}
