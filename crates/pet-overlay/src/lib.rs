//! Tooltip, score and menu drawn over the pet in the same native window.
//! Hosts own one window and one egui context; PetScene composes both layers.
//! Anchors use physical screen coordinates, controls use logical points.

mod font;
mod scene;
pub use scene::{PetScene, SceneLayout};
mod menu;
mod score;
mod tooltip;

use std::time::Instant;

use egui::{Pos2, Rect};
use pet_config::{PetConfig, WorldArea};
use pet_ui_core::{OverlayAction, OverlayIntent, ScoreEvent};

pub use font::install_font;
pub use menu::{MenuItem, MenuNode, build_menu_tree};
pub use score::{SCORE_POPUP_CSS_DURATION, SCORE_POPUP_SECS};
pub use tooltip::tooltip_placement;

/// Minimum width of the shared window in logical points. The pet canvas is
/// `CANVAS_W / ppp` points wide, so this leaves room for a full three-level
/// menu cascade on either side.
pub const OVERLAY_W: f32 = 760.0;
/// Height of the shared window in logical points.
pub const OVERLAY_H: f32 = 480.0;

/// What the overlay currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayMode {
    /// No controls to draw; the pet remains visible.
    #[default]
    Hidden,
    /// Tooltip and/or score popup only; controls do not take input.
    Transient,
    /// The context menu is open; menu input takes priority over pet input.
    Menu,
}

/// Everything the overlay remembers between frames.
pub struct OverlayState {
    mode: OverlayMode,
    tooltip: Option<Tooltip>,
    score: Option<score::ScorePopup>,
    menu: Option<menu::Menu>,
    /// Full menu tree, built once from the configuration.
    tree: Vec<MenuNode>,
    /// Actions for the pet, drained by the host after each pass.
    actions: Vec<OverlayAction>,
    /// Work area the pet may roam in, as the host last reported it. Converted to
    /// overlay points against the window's real origin on every draw.
    world: Option<WorldArea>,
    /// Scale of the last frame, for converting screen pixels into points.
    ppp: f32,
    /// Where the shared window actually is, from the host; see
    /// [`Self::set_window_origin`].
    window_origin: (i32, i32),
    /// What the host knows about the pointer for the frame being drawn.
    input: OverlayInput,
    /// Tooltip text size from the configuration (`--dsh-pet-font-tip`).
    tooltip_font: f32,
    font_loaded: bool,
    font_data: Option<Vec<u8>>,
}

struct Tooltip {
    /// Screen position in physical pixels; converted per frame, see the module
    /// documentation.
    anchor: (f32, f32),
    text: String,
}

impl OverlayState {
    /// Creates the state, reads the optional font file and builds the menu tree.
    pub fn new(asset_dir: &std::path::Path, config: &PetConfig) -> Self {
        let font_path = asset_dir.join(pet_config::FONT_FILE);
        let font_data = std::fs::read(&font_path).map_err(|err| log::debug!("no bundled font at {}: {err}", font_path.display())).ok();
        Self {
            mode: OverlayMode::Hidden,
            tooltip: None,
            score: None,
            menu: None,
            tree: build_menu_tree(config),
            actions: Vec::new(),
            world: None,
            ppp: 1.0,
            window_origin: (i32::MIN / 2, i32::MIN / 2),
            input: OverlayInput::default(),
            tooltip_font: config.font.tooltip_size(),
            font_loaded: false,
            font_data,
        }
    }

    /// The current mode, for the host's window handling.
    ///
    /// Only meaningful after [`Self::tick`] ran on the host's own pass.
    pub fn mode(&self) -> OverlayMode {
        self.mode
    }

    /// Actions chosen in the overlay since the last call.
    pub fn take_actions(&mut self) -> Vec<OverlayAction> {
        std::mem::take(&mut self.actions)
    }

    /// Expires the popups and derives the mode. Must run once per frame from
    /// the pet's pass, whether or not the shared window is shown.
    pub fn tick(&mut self, now: Instant) {
        if self.score.as_ref().is_some_and(|score| !score.alive(now) && !score.animating(now)) {
            self.score = None;
        }
        self.mode = if self.menu.is_some() {
            OverlayMode::Menu
        } else if self.score.is_some() || self.tooltip.is_some() {
            OverlayMode::Transient
        } else {
            OverlayMode::Hidden
        };
    }

    /// Reports the work area the pet may roam in, in physical screen pixels, so
    /// the menu can stay fully on screen when the shared window itself hangs
    /// over the screen edge.
    ///
    /// Called every frame by the host, before [`Self::set_window_origin`] and
    /// [`Self::ui`].
    pub fn set_world(&mut self, world: WorldArea, ppp: f32) {
        self.ppp = ppp.max(0.01);
        if world.width > 0 && world.height > 0 {
            self.world = Some(world);
        }
    }

    /// Reports where the shared window actually ended up on screen, in
    /// physical pixels.
    ///
    /// The host asks for the window to be centred on the pet, but a window
    /// manager may clamp or shift it (a window hanging off the screen edge is
    /// the normal case here), so the drawing code reads the real position
    /// instead of assuming the requested one.
    pub fn set_window_origin(&mut self, origin: (i32, i32)) {
        self.window_origin = origin;
    }

    /// Applies what the pet asked for this frame. Anchors are screen positions
    /// in physical pixels; they are kept that way and converted on every draw.
    pub fn apply_intent(&mut self, intent: OverlayIntent) {
        match intent {
            OverlayIntent::Hidden => {
                // While the menu is open the pet reports `Menu` instead, so a
                // `Hidden` here means the menu really is closed.
                if self.menu.is_none() {
                    self.tooltip = None;
                }
            }
            OverlayIntent::Tooltip { anchor, text } => {
                if self.menu.is_some() {
                    return;
                }
                self.tooltip = Some(Tooltip { anchor, text });
            }
            OverlayIntent::Menu { anchor } => match &mut self.menu {
                // A second right-click moves the menu, as upstream remounts it
                // at the new position and collapses the old cascade. The
                // comparison is in screen pixels, so a pet that walks while the
                // menu is open does not count as a second right-click.
                Some(menu) if menu.anchor != anchor => {
                    let tree = self.tree.clone();
                    menu.reopen(anchor, tree);
                }
                Some(_) => {}
                None => {
                    self.tooltip = None;
                    self.menu = Some(menu::Menu::new(anchor, self.tree.clone()));
                }
            },
        }
    }

    /// Registers a scored click: spawns the burst and the card.
    pub fn apply_score(&mut self, event: ScoreEvent) {
        self.tooltip = None;
        self.score = Some(score::ScorePopup::new(event.anchor, event.points, event.speed, event.size, Instant::now()));
    }

    /// Drops every popup (used when the pet exits).
    pub fn clear(&mut self) {
        self.tooltip = None;
        self.score = None;
        self.menu = None;
        self.mode = OverlayMode::Hidden;
    }

    /// Reports the host's view of the pointer for the pass that follows.
    ///
    /// Called by the host before [`Self::ui`]; see [`OverlayInput`].
    pub fn set_input(&mut self, input: OverlayInput) {
        self.input = input;
    }

    /// Draws one frame and returns whether another repaint is needed soon.
    pub fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, now: Instant) -> bool {
        if !self.font_loaded {
            self.font_loaded = true;
            if let Some(data) = self.font_data.take() {
                font::install_font(ctx, data);
            }
        }
        self.tick(now);

        let screen = ui.max_rect();
        let mut busy = false;
        // Anchors are screen positions; the shared window moves with the pet
        // and may be clamped by the window manager, so they are converted
        // against the window's real origin every frame.
        let (window_origin, ppp) = (self.window_origin, self.ppp);
        let to_overlay = move |point: (f32, f32)| screen_to_overlay(point, window_origin, ppp);
        let bounds = self.world.map(|world| overlay_bounds(world, window_origin, ppp));
        // A screen-space pointer is authoritative when the host has one: the
        // window's own position can be stale (see [`OverlayInput`]). Native
        // Wayland has no screen coordinates, so the window's input is used.
        let pointer = self
            .input
            .pointer
            .map(|point| {
                let (x, y) = to_overlay(point);
                Pos2::new(x, y)
            })
            .or_else(|| ui.input(|state| state.pointer.hover_pos()));
        // Press and Escape are edges, so their position does not matter: read
        // them from the window's own input, where they are always accurate.
        let pressed = ui.input(|state| state.pointer.primary_pressed());
        let escape = ui.input(|state| state.key_pressed(egui::Key::Escape));

        // The score card and its particles live below the menu but never take
        // the pointer: they are feedback, not controls.
        if let Some(score) = &self.score {
            busy |= score::ui(ui, screen, to_overlay(score.anchor), score, now);
        }

        if let Some(menu) = &mut self.menu {
            let anchor = to_overlay(menu.anchor);
            match menu::ui(ui, screen, menu, bounds, now, anchor, pointer, pressed, escape) {
                menu::Outcome::Stay => {}
                menu::Outcome::Dismiss => {
                    self.menu = None;
                    self.actions.push(OverlayAction::MenuClosed);
                }
                menu::Outcome::Action(action) => {
                    self.menu = None;
                    self.actions.push(action);
                }
            }
            busy = true;
        } else if let Some(tooltip) = &self.tooltip {
            let anchor = to_overlay(tooltip.anchor);
            tooltip::ui(ui, screen, anchor, &tooltip.text, self.tooltip_font);
        }

        if self.score.is_some() && escape {
            self.score = None;
        }
        busy
    }
}

/// Pointer data the host passes in for a pass.
///
/// Hosts that can read the desktop cursor supply physical screen coordinates.
/// Press and Escape edges still come from the shared window's egui input.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlayInput {
    /// Global cursor in physical screen pixels, when the host can read it.
    ///
    /// `None` means the host has no global pointer (native Wayland), where the
    /// surface is placed by the compositor and the window's own positions are
    /// already correct.
    pub pointer: Option<(f32, f32)>,
}

/// A point in physical screen pixels expressed in overlay-window points, for a
/// window whose top-left really is at `window_origin`.
pub fn screen_to_overlay(point: (f32, f32), window_origin: (i32, i32), ppp: f32) -> (f32, f32) {
    ((point.0 - window_origin.0 as f32) / ppp, (point.1 - window_origin.1 as f32) / ppp)
}

/// A work area in physical pixels expressed in overlay-window points.
pub fn overlay_bounds(world: WorldArea, window_origin: (i32, i32), ppp: f32) -> Rect {
    let min = screen_to_overlay((world.x as f32, world.y as f32), window_origin, ppp);
    let max = screen_to_overlay((world.right() as f32, world.bottom() as f32), window_origin, ppp);
    Rect::from_min_max(Pos2::new(min.0, min.1), Pos2::new(max.0, max.1))
}
