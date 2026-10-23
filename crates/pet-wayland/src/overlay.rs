//! The interaction overlay: a second layer surface for tooltips, the score
//! card and the context menu.
//!
//! The pet window is exactly the `640 × 360` canvas, so everything that needs
//! more room lives here: a [`Layer::Overlay`] surface of `OVERLAY_W ×
//! OVERLAY_H` logical points, centred on the pet and moved with it every frame.
//! It is click-through while it only paints — its input region is empty — and
//! takes the whole surface while the menu is open, which is what makes the menu
//! clickable and lets a press outside it close it.
//!
//! A layer surface has no "hide" request: it is on screen exactly while it has
//! something to show. The overlay therefore presents one transparent frame when
//! it stops showing anything and then skips passes until something changes
//! again. It is created lazily, once the compositor has told us which output
//! the pet is on, so both surfaces land on the same monitor.
//!
//! It runs its own [`egui::Context`]. A Wayland pointer only reports positions
//! inside the surface it entered, so the pet's input state cannot describe the
//! overlay; fonts are per-context as well, so the bundled font is installed
//! again on the overlay's first pass.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use egui::{Event, Pos2, Rect, ViewportId, ViewportInfo};
use pet_config::{PetConfig, WorldArea};
use pet_overlay::{OVERLAY_H, OVERLAY_W, OverlayInput, OverlayMode, OverlayState, overlay_origin};
use pet_ui_core::{FrameOutput, OverlayAction};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::registry::SimpleGlobal;
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{KeyboardInteractivity, Layer, LayerShell, LayerSurface};
use wayland_client::QueueHandle;
use wayland_client::protocol::wl_output;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::app::WaylandApp;
use crate::input;
use crate::surface::{self, Placement};
use crate::wgpu::Wgpu;

/// Everything needed to build the overlay surface.
pub(crate) struct Builder<'a> {
    pub(crate) compositor: &'a CompositorState,
    pub(crate) layer_shell: &'a LayerShell,
    pub(crate) viewporter: Option<&'a SimpleGlobal<WpViewporter, 1>>,
    pub(crate) qh: &'a QueueHandle<WaylandApp>,
    pub(crate) output: Option<&'a wl_output::WlOutput>,
    /// The pet's renderer; the overlay shares its device and queue.
    pub(crate) parent: &'a Wgpu,
    pub(crate) asset_dir: &'a Path,
    pub(crate) config: &'a PetConfig,
    pub(crate) ppp: f32,
    /// The pet's top-left inside the output, output-logical points.
    pub(crate) pet_local: (f32, f32),
    pub(crate) output_size: (i32, i32),
}

/// What one overlay pass needs to know about the pet's frame.
pub(crate) struct Frame<'a> {
    pub(crate) now: Instant,
    pub(crate) time: f64,
    pub(crate) ppp: f32,
    /// The pet's top-left inside the output, output-logical points.
    pub(crate) pet_local: (f32, f32),
    pub(crate) output_position: (i32, i32),
    pub(crate) output_size: (i32, i32),
    pub(crate) world: WorldArea,
    /// The pet's last pointer position in physical screen pixels. Used while
    /// the overlay has not received its own pointer yet: the menu opens under a
    /// cursor that is still over the pet, and both surfaces live in the same
    /// screen space.
    pub(crate) pet_pointer: Option<(f32, f32)>,
    pub(crate) out: &'a FrameOutput,
}

/// The result of one overlay pass.
pub(crate) struct Pass {
    /// Menu choices made during the pass, for the pet's next frame.
    pub(crate) actions: Vec<OverlayAction>,
    /// How long until the overlay wants to draw again.
    pub(crate) repaint: Duration,
}

/// The overlay surface, as a protocol object plus the state drawn on it.
pub(crate) struct Overlay {
    /// Declared before the surface it refers to: wgpu holds raw pointers into
    /// the `wl_surface`, and fields drop in declaration order.
    gpu: Wgpu,
    viewport: Option<WpViewport>,
    /// Matched against the surface in configure/closed events.
    layer: LayerSurface,
    /// The output the layer surface was created for. A layer surface belongs to
    /// one output for its whole life, so the host rebuilds the overlay when the
    /// pet moves to another one.
    output: Option<wl_output::WlOutput>,
    placement: Placement,
    compositor: CompositorState,
    ctx: egui::Context,
    state: OverlayState,
    /// Pending input for the next pass, in overlay points.
    events: Vec<Event>,
    pointer_down: bool,
    /// Where the compositor last reported the pointer, relative to this surface.
    ///
    /// It is the authority whenever it is `Some`: the compositor reports it
    /// directly, in this window's own coordinates. It is `None` until the
    /// pointer is reported over this surface — the overlay is click-through
    /// most of the time, so no events arrive then — and the host's screen-space
    /// position stands in instead.
    pointer_local: Option<(f32, f32)>,
    configured: bool,
    /// Whether the input region currently covers the surface.
    interactive: bool,
    /// Mode of the last presented frame; `None` before the first present. A
    /// change is what makes a hidden overlay draw its clearing frame.
    drawn: Option<OverlayMode>,
}

impl Overlay {
    /// Builds the surface, without a buffer: it becomes visible on the first
    /// pass that has something to show.
    pub(crate) fn new(builder: Builder<'_>) -> Result<Self> {
        let Builder { compositor, layer_shell, viewporter, qh, output, parent, asset_dir, config, ppp, pet_local, output_size } = builder;
        let size = Self::size();
        let surface = compositor.create_surface(qh);
        let viewport = viewporter.and_then(|viewporter| viewporter.get().ok()).map(|viewporter| {
            let viewport = viewporter.get_viewport(&surface, qh, ());
            viewport.set_destination(size.0 as i32, size.1 as i32);
            viewport
        });
        let layer = layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("whale-pet-overlay"), output);
        layer.set_size(size.0, size.1);
        // Never reserve space: this floats above the desktop like a menu.
        layer.set_exclusive_zone(-1);
        // The keyboard stays with the pet: Escape is forwarded from the pet's
        // own keyboard (see `app.rs`). A surface that only holds a tooltip has
        // no way to gain focus anyway, and taking focus here would also take
        // Ctrl+Q away from the pet.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let mut placement = Placement::top_left();
        placement.set_local_origin(local_origin(pet_local, ppp), output_size, size);
        placement.apply(&layer);
        // Click-through until the menu opens.
        if let Err(err) = surface::set_overlay_region(compositor, &layer, size, false) {
            log::warn!("cannot clear the overlay input region: {err:#}");
        }
        // The first commit carries no buffer; it is what makes the compositor
        // send the initial configure.
        layer.commit();

        let buffer = buffer_size(ppp);
        let gpu = parent.sibling(layer.wl_surface(), buffer)?;
        let state = OverlayState::new(asset_dir, config);
        log::info!("interaction overlay: {size:?} logical points, {buffer:?} physical");
        Ok(Self {
            gpu,
            viewport,
            layer,
            output: output.cloned(),
            placement,
            compositor: compositor.clone(),
            ctx: egui::Context::default(),
            state,
            events: Vec::new(),
            pointer_down: false,
            pointer_local: None,
            configured: false,
            interactive: false,
            drawn: None,
        })
    }

    /// The overlay surface size in logical points.
    fn size() -> (u32, u32) {
        (OVERLAY_W as u32, OVERLAY_H as u32)
    }

    pub(crate) fn surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    /// The compositor finished configuring the surface; frames may present now.
    pub(crate) fn configured(&mut self, suggested: (u32, u32)) {
        if !self.configured {
            log::debug!("overlay configure: suggested {suggested:?}, using {:?}", Self::size());
        }
        self.configured = true;
    }

    /// Whether `layer` is this overlay's surface (for configure/closed events).
    pub(crate) fn owns(&self, layer: &LayerSurface) -> bool {
        layer == &self.layer
    }

    /// Whether this overlay is bound to `output`.
    ///
    /// A layer surface cannot be moved to another output, so a mismatch means
    /// the host has to drop this overlay and build a new one.
    pub(crate) fn belongs_to(&self, output: Option<&wl_output::WlOutput>) -> bool {
        self.output.as_ref() == output
    }

    /// Whether the menu is open, so Escape from the pet's keyboard applies.
    pub(crate) fn shows_menu(&self) -> bool {
        self.state.mode() == OverlayMode::Menu
    }

    /// Turns one pointer frame into egui events for the overlay's pass.
    ///
    /// Only events that landed on this surface are passed in; anything that
    /// happened on the pet belongs to the pet's own pass.
    pub(crate) fn pointer_frame(&mut self, events: &[PointerEvent], modifiers: egui::Modifiers) {
        if events.is_empty() {
            return;
        }
        for event in events {
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_local = Some((event.position.0 as f32, event.position.1 as f32));
                }
                // The pointer is known to be elsewhere now, so the last point
                // over this surface would hit-test a menu that opens later at
                // the wrong place.
                PointerEventKind::Leave { .. } => self.pointer_local = None,
                _ => {}
            }
        }
        input::translate_frame(events, modifiers, &mut self.pointer_down, true, &mut self.events);
    }

    /// Adds one keyboard event. The only key the overlay reacts to is Escape.
    pub(crate) fn key(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Runs one overlay pass and presents the result.
    pub(crate) fn draw(&mut self, frame: Frame<'_>) -> Result<Pass> {
        let idle = Pass { actions: Vec::new(), repaint: Duration::from_secs(1) };
        if !self.configured {
            return Ok(idle);
        }

        // Follow the pet. Margins are double-buffered, so applying them before
        // presenting makes the move and the new frame land in one commit.
        let size = Self::size();
        let before = self.placement.local_origin(frame.output_size, size);
        self.placement.set_local_origin(local_origin(frame.pet_local, frame.ppp), frame.output_size, size);
        let after = self.placement.local_origin(frame.output_size, size);
        let moved = (after.0 - before.0, after.1 - before.1);
        if moved.0.abs() > f32::EPSILON || moved.1.abs() > f32::EPSILON {
            self.placement.apply(&self.layer);
            // The compositor reports pointer positions relative to this
            // surface and sends nothing when only the surface moves. The
            // pointer itself did not move, so its surface-relative position
            // shifts by the inverse of the surface's own movement. Without
            // this, a menu open while the pet walks would hit-test the cursor
            // at the wrong item.
            if let Some((x, y)) = self.pointer_local {
                self.pointer_local = Some((x - moved.0, y - moved.1));
            }
        }
        // The state converts stored screen anchors against the window's real
        // origin. Unlike an X11 window, a layer surface is placed exactly where
        // the margins say, so the computed origin is the real one.
        let origin = (
            ((frame.output_position.0 as f32 + after.0) * frame.ppp).round() as i32,
            ((frame.output_position.1 as f32 + after.1) * frame.ppp).round() as i32,
        );
        self.state.set_window_origin(origin);
        self.state.set_world(frame.world, frame.ppp);
        // The overlay's own pointer wins whenever the compositor has reported
        // one: it is exact and in this window's own coordinates. Before the
        // first enter — the menu usually opens under a cursor still on the
        // pet — the pet's screen position stands in, so the opening pass
        // hit-tests the right point.
        let pointer = match self.pointer_local {
            Some((x, y)) => Some((origin.0 as f32 + x * frame.ppp, origin.1 as f32 + y * frame.ppp)),
            None => frame.pet_pointer,
        };
        self.state.set_input(OverlayInput { pointer });
        if let Some(score) = frame.out.score {
            self.state.apply_score(score);
        }
        self.state.apply_intent(frame.out.overlay.clone());
        self.state.tick(frame.now);

        // The overlay follows the pet and its anchors are screen positions, so
        // a visible overlay has to redraw every frame the pet draws. When it
        // shows nothing, only the pass that clears the last frame and any
        // pending input are worth running: a layer surface with no buffer is
        // not composited at all.
        let mode = self.state.mode();
        let redraw = mode != OverlayMode::Hidden || self.drawn != Some(mode) || !self.events.is_empty();
        if !redraw {
            return Ok(idle);
        }
        // An unmapped surface stays invisible until its first buffer, so the
        // very first pass with nothing to show has nothing to clear either.
        if self.drawn.is_none() && mode == OverlayMode::Hidden {
            self.drawn = Some(mode);
            return Ok(idle);
        }

        let buffer = buffer_size(frame.ppp);
        self.gpu.resize(buffer);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(OVERLAY_W, OVERLAY_H));
        let mut raw = egui::RawInput {
            viewport_id: ViewportId::ROOT,
            screen_rect: Some(screen_rect),
            time: Some(frame.time),
            focused: true,
            events: std::mem::take(&mut self.events),
            ..Default::default()
        };
        raw.viewports.insert(ViewportId::ROOT, ViewportInfo { native_pixels_per_point: Some(frame.ppp), ..Default::default() });

        let state = &mut self.state;
        let ctx = &self.ctx;
        let mut busy = false;
        let full = ctx.run_ui(raw, |ui| {
            busy = state.ui(ctx, ui, frame.now);
        });
        let mode = self.state.mode();
        let actions = self.state.take_actions();

        // The region is double-buffered; `Queue::present` in `Wgpu::draw`
        // commits it together with the frame.
        let interactive = mode == OverlayMode::Menu;
        if interactive != self.interactive {
            self.interactive = interactive;
            // A position reported while the region was the other way round
            // describes a pointer that has since been free to move anywhere, and
            // the compositor only re-picks surfaces on the next real motion. The
            // next menu must therefore start from the host's screen position
            // (the press that opened it) instead of this stale point.
            self.pointer_local = None;
            if let Err(err) = surface::set_overlay_region(&self.compositor, &self.layer, size, interactive) {
                log::warn!("cannot update the overlay input region: {err:#}");
            }
        }

        let repaint = if busy {
            Duration::ZERO
        } else {
            full.viewport_output.get(&ViewportId::ROOT).map_or(Duration::from_secs(1), |viewport| viewport.repaint_delay)
        };
        let primitives = self.ctx.tessellate(full.shapes, full.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [buffer.0, buffer.1], pixels_per_point: frame.ppp };
        let mut textures = full.textures_delta;
        self.gpu.draw(&primitives, &screen, &mut textures)?;

        self.drawn = Some(mode);
        Ok(Pass { actions, repaint })
    }

    /// Reconfigures the buffers after a scale change.
    pub(crate) fn set_scale(&mut self, ppp: f32) {
        self.gpu.resize(buffer_size(ppp));
        if let Some(viewport) = self.viewport.as_ref() {
            viewport.set_destination(OVERLAY_W as i32, OVERLAY_H as i32);
        }
        // The next pass lays the content out again at the new scale.
        self.drawn = None;
    }
}

/// Physical size of the overlay swapchain at `ppp`.
fn buffer_size(ppp: f32) -> (u32, u32) {
    (((OVERLAY_W * ppp).round()).max(1.0) as u32, ((OVERLAY_H * ppp).round()).max(1.0) as u32)
}

/// Top-left of the overlay inside its output, in output-logical points, for a
/// pet at `pet_local`.
///
/// Uses [`overlay_origin`] so the centring and its rounding match the eframe
/// host exactly: both hosts place the same window over the same pet.
fn local_origin(pet_local: (f32, f32), ppp: f32) -> (f32, f32) {
    let (x, y) = overlay_origin(((pet_local.0 * ppp).round() as i32, (pet_local.1 * ppp).round() as i32), ppp);
    (x as f32 / ppp, y as f32 / ppp)
}
