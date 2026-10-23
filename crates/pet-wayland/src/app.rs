//! Single layer-surface host. Pet and controls share one egui context,
//! input stream and swapchain. Placement tracks the canvas origin; native
//! surface margins include the transparent space around that canvas.
//! GPU resources precede protocol objects in drop order.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use egui::{Event, Pos2, Rect, ViewportId, ViewportInfo};
use pet_config::{CANVAS_H, CANVAS_W, PetConfig, WorldArea};
use pet_overlay::{PetScene, SceneLayout};
use pet_ui_core::{FrameInput, FrameOutput, PetCore};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::globals::GlobalData;
use smithay_client_toolkit::output::{OutputHandler, OutputInfo, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState, SimpleGlobal};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers as SeatModifiers, RawModifiers};
use smithay_client_toolkit::seat::pointer::{PointerData, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    delegate_simple, registry_handlers,
};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{self, WpFractionalScaleV1};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::input;
use crate::surface::{self, Placement};
use crate::wgpu::Wgpu;

/// Where the surface lives, in the compositor's logical space.
struct OutputGeometry {
    output: Option<wl_output::WlOutput>,
    position: (i32, i32),
    size: (i32, i32),
    scale: f32,
}

/// Picks the first output with a known logical size.
///
/// KWin reports `logical_position`/`logical_size` through `zxdg_output_v1`, and
/// the anchors need the size to turn a corner into a margin. A compositor that
/// reports only a mode still works: the logical size is derived from the
/// current mode divided by the scale factor.
fn pick_output(output_state: &OutputState) -> Option<OutputGeometry> {
    let mut fallback = None;
    for output in output_state.outputs() {
        let Some(info) = output_state.info(&output) else { continue };
        let scale = info.scale_factor.max(1) as f32;
        let size = info.logical_size.or_else(|| {
            info.modes
                .iter()
                .find(|mode| mode.current)
                .map(|mode| ((mode.dimensions.0 as f32 / scale).round() as i32, (mode.dimensions.1 as f32 / scale).round() as i32))
        });
        let position = info.logical_position.unwrap_or(info.location);
        let geometry = OutputGeometry { output: Some(output), position, size: size.unwrap_or((0, 0)), scale };
        if size.is_some() {
            log_output(&info, &geometry);
            return Some(geometry);
        }
        if fallback.is_none() {
            fallback = Some(geometry);
        }
    }
    if let Some(geometry) = &fallback {
        log::warn!("no output reports a logical size; dragging will be clamped to an empty output rectangle");
        if let Some(output) = &geometry.output
            && let Some(info) = output_state.info(output)
        {
            log_output(&info, geometry);
        }
    }
    fallback
}

fn log_output(info: &OutputInfo, geometry: &OutputGeometry) {
    log::info!(
        "output {} at {:?}, {:?} logical, integer scale {}",
        info.name.as_deref().unwrap_or(&info.model),
        geometry.position,
        geometry.size,
        info.scale_factor
    );
}

/// Protocol state for one layer-surface window.
pub(crate) struct WaylandApp {
    /// Declared first so it is destroyed before the surface/display it refers
    /// to: wgpu keeps raw pointers into both.
    gpu: Wgpu,
    scene: PetScene,
    input_region: Option<Rect>,

    pointer: Option<ThemedPointer<PointerData>>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    viewporter: Option<WpViewport>,
    /// Kept alive for the protocol object; its only event updates `ppp` through
    /// the `Dispatch` impl below, so the handle itself is never read.
    #[allow(dead_code)]
    fractional_scale: Option<WpFractionalScaleV1>,
    layer: LayerSurface,

    registry_state: RegistryState,
    compositor: CompositorState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    output: Option<wl_output::WlOutput>,
    output_position: (i32, i32),
    output_size: (i32, i32),

    core: PetCore,
    ctx: egui::Context,
    start: Instant,

    layout: SceneLayout,
    ppp: f32,
    /// Whether `ppp` came from `wp_fractional_scale_v1`; if so, integer output
    /// scales must not overwrite it.
    fractional: bool,
    logical: (u32, u32),
    placement: Placement,

    /// Modifier state from the latest `wl_keyboard.modifiers`.
    modifiers: SeatModifiers,
    pointer_down: bool,
    /// Pointer position in canvas pixels, updated only by pointer events.
    ///
    /// It must not be recomputed from the window origin: moving the window
    /// under a stationary pointer would otherwise feed the surface's own
    /// movement back into the drag spring.
    pointer_global: Option<(f32, f32)>,
    cursor: Option<egui::CursorIcon>,

    pending_events: Vec<Event>,
    configured: bool,
    dirty: bool,
    exit: bool,
    next_repaint: Duration,

    /// Declared last: every protocol object above sends requests through it,
    /// and the wgpu objects hold a pointer to its `wl_display`.
    conn: Connection,
}

impl WaylandApp {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        conn: &Connection,
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        compositor: CompositorState,
        layer_shell: LayerShell,
        asset_dir: PathBuf,
        config: &PetConfig,
        stats: bool,
    ) -> Result<Self> {
        let registry_state = RegistryState::new(globals);
        let seat_state = SeatState::new(globals, qh);
        let output_state = OutputState::new(globals, qh);
        let shm = Shm::bind(globals, qh)?;

        // No output is selected yet: `OutputState::new` only binds the proxies,
        // and their geometry arrives as events. `run` performs the first
        // roundtrip and then calls `select_output`.
        let ppp = 1.0;
        let logical = surface::window_size(ppp);
        let placement = Placement::from_position(&config.position());
        let output_position = (0, 0);
        let output_size = (0, 0);
        let output = None;

        let viewporter = match SimpleGlobal::<WpViewporter, 1>::bind(globals, qh) {
            Ok(viewporter) => Some(viewporter),
            Err(err) => {
                log::warn!("wp_viewporter unavailable ({err}); fractional scaling will stretch the canvas");
                None
            }
        };
        let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(qh, 1..=1, GlobalData).ok();

        let surface = compositor.create_surface(qh);
        let viewport = viewporter.as_ref().and_then(|viewporter| viewporter.get().ok()).map(|viewporter| {
            let viewport = viewporter.get_viewport(&surface, qh, ());
            viewport.set_destination(logical.0 as i32, logical.1 as i32);
            viewport
        });
        let fractional_scale = fractional_manager.map(|manager| manager.get_fractional_scale(&surface, qh, ()));

        let layer = layer_shell.create_layer_surface(qh, surface, Layer::Top, Some("whale-pet"), output.as_ref());
        layer.set_size(logical.0, logical.1);
        // Negative exclusive zone: reserve no space in the work area.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        placement.apply_canvas(&layer, SceneLayout::new(ppp));
        if let Err(err) = surface::set_input_region(&compositor, &layer, SceneLayout::new(ppp), false) {
            log::warn!("cannot restrict the input region (clicks outside the pet reach the surface): {err:#}");
        }
        // The first commit must carry no buffer; it is what makes the
        // compositor send the initial configure.
        layer.commit();

        let gpu = Wgpu::new(conn, layer.wl_surface(), SceneLayout::new(ppp).size)?;

        let scene = PetScene::new(&asset_dir, config);
        let core = PetCore::new(asset_dir, config, stats)?;
        let start = Instant::now();
        log::info!("native Wayland (layer shell), axis scale {ppp}, canvas {}x{} physical", CANVAS_W, CANVAS_H);

        Ok(Self {
            gpu,
            scene,
            input_region: None,
            pointer: None,
            keyboard: None,
            viewporter: viewport,
            fractional_scale,
            layer,
            registry_state,
            compositor,
            seat_state,
            output_state,
            shm,
            output,
            output_position,
            output_size,
            core,
            ctx: egui::Context::default(),
            start,
            layout: SceneLayout::new(ppp),
            ppp,
            fractional: false,
            logical,
            placement,
            modifiers: SeatModifiers::default(),
            pointer_down: false,
            pointer_global: None,
            cursor: None,
            pending_events: Vec::new(),
            configured: false,
            dirty: true,
            exit: false,
            next_repaint: Duration::from_millis(16),
            conn: conn.clone(),
        })
    }

    pub(crate) fn should_exit(&self) -> bool {
        self.exit
    }

    /// Fallback when the compositor never reported which output it mapped the
    /// surface onto.
    ///
    /// [`Self::surface_enter`] is authoritative and normally arrives with the
    /// first presented frame, so this only runs when it did not. The first
    /// output that reports a logical size is used, which is where drag
    /// clamping and the global origin come from; the anchors themselves are
    /// relative to whichever output the compositor chose, so placement is
    /// unaffected.
    fn select_output(&mut self) {
        if self.output_size != (0, 0) {
            return;
        }
        let Some(geometry) = pick_output(&self.output_state) else {
            log::warn!("no output reported; dragging will be clamped to an empty rectangle");
            return;
        };
        self.adopt_output(geometry.position, geometry.size, geometry.scale);
        self.output = geometry.output;
        log::warn!(
            "the compositor never reported the surface's output; assuming {} at {:?} for drag geometry",
            self.output
                .as_ref()
                .and_then(|output| self.output_state.info(output))
                .map_or_else(|| "?".to_owned(), |info| info.name.unwrap_or(info.model)),
            geometry.position
        );
        self.dirty = true;
    }

    /// Applies output geometry and the integer scale it implies.
    ///
    /// The integer scale is only a fallback: once the compositor has sent a
    /// `wp_fractional_scale_v1` preference, that value wins for the lifetime of
    /// the surface, since it reflects the scale this particular surface is
    /// composited at.
    fn adopt_output(&mut self, position: (i32, i32), size: (i32, i32), scale: f32) {
        self.output_position = position;
        self.output_size = size;
        if !self.fractional && (self.ppp - scale).abs() > 0.01 {
            self.ppp = scale;
            self.layout = SceneLayout::new(scale);
            self.logical = surface::window_size(scale);
            self.layer.set_size(self.logical.0, self.logical.1);
            self.apply_surface_state();
        }
    }

    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn set_dirty(&mut self, dirty: bool) {
        self.dirty = dirty;
    }

    /// How long until the core wants the next frame.
    ///
    /// egui reports `Duration::MAX` when nothing requested a repaint; that
    /// would overflow `Instant + duration` in the loop, so it is capped to a
    /// one-second heartbeat (decoder progress and state changes still wake it).
    pub(crate) fn next_repaint(&self) -> Duration {
        self.next_repaint.min(Duration::from_secs(1))
    }

    /// Runs one egui pass and presents it.
    pub(crate) fn draw(&mut self) -> Result<()> {
        if !self.configured || self.exit {
            return Ok(());
        }

        let now = Instant::now();
        let size = self.layout.size_points();
        let (width, height) = (size.x, size.y);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(width, height));
        let mut raw = egui::RawInput {
            viewport_id: ViewportId::ROOT,
            screen_rect: Some(screen_rect),
            time: Some(self.start.elapsed().as_secs_f64()),
            focused: true,
            events: std::mem::take(&mut self.pending_events),
            ..Default::default()
        };
        // egui derives its `pixels_per_point` from this field, and `PetCore`
        // reads that value back as `stable_dt`-adjacent state; without it the
        // canvas would be laid out at 1.0 and rendered at the wrong scale.
        raw.viewports.insert(ViewportId::ROOT, ViewportInfo { native_pixels_per_point: Some(self.ppp), ..Default::default() });

        let ppp = self.ppp;
        let origin = self.layout.window_origin(self.window_origin());
        let world = self.world();
        let global = self.pointer_global;
        let button_down = Some(self.pointer_down);
        let mut frame_out = FrameOutput::default();
        let ctx = &self.ctx;
        let core = &mut self.core;
        let scene = &mut self.scene;
        let layout = &mut self.layout;
        let full = ctx.run_ui(raw, |ui| {
            let ui_ctx = ui.ctx().clone();
            let (pressed, released, secondary_pressed, pointer, dt) = ui_ctx.input(|input| {
                (
                    input.pointer.primary_pressed(),
                    input.pointer.primary_released(),
                    input.pointer.secondary_pressed(),
                    input.pointer.interact_pos(),
                    input.stable_dt,
                )
            });
            frame_out = scene.ui(
                ui,
                core,
                FrameInput { now, dt, ppp, pointer, pressed, released, secondary_pressed, global, button_down, window_origin: origin, world },
                layout,
            );
        });

        // Margins are double-buffered: apply them before presenting so the
        // move and the new frame land in the same commit.
        if let Some(target) = frame_out.move_to {
            self.move_window(target);
        }

        let layout = self.layout;
        self.update_input_region()?;
        let primitives = self.ctx.tessellate(full.shapes, full.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [layout.size.0, layout.size.1], pixels_per_point: self.ppp };
        let mut textures = full.textures_delta;
        self.gpu.draw(&primitives, &screen, &mut textures)?;

        self.next_repaint = full.viewport_output.get(&ViewportId::ROOT).map_or(Duration::from_secs(1), |viewport| viewport.repaint_delay);

        if let Some(icon) = frame_out.cursor {
            self.apply_cursor(icon);
        }
        if frame_out.close {
            self.exit = true;
        }
        Ok(())
    }

    /// Top-left of the surface in canvas pixels.
    fn window_origin(&self) -> (i32, i32) {
        let (x, y) = self.placement.local_origin(self.output_size, surface::logical_size(self.ppp));
        (((self.output_position.0 as f32 + x) * self.ppp).round() as i32, ((self.output_position.1 as f32 + y) * self.ppp).round() as i32)
    }

    /// The output rectangle in canvas pixels, or a zero size when no output has
    /// reported geometry yet.
    ///
    /// Layer shell says nothing about panels or struts, so the roam/throw
    /// bounds are the whole output: the pet may cross other panels, which is
    /// the closest equivalent of the upstream work area this protocol offers.
    fn world(&self) -> WorldArea {
        if self.output_size == (0, 0) {
            return WorldArea::default();
        }
        WorldArea {
            x: (self.output_position.0 as f32 * self.ppp).round() as i32,
            y: (self.output_position.1 as f32 * self.ppp).round() as i32,
            width: (self.output_size.0 as f32 * self.ppp).round() as u32,
            height: (self.output_size.1 as f32 * self.ppp).round() as u32,
        }
    }

    /// Moves the surface so its top-left lands on `target` (canvas pixels).
    fn move_window(&mut self, target: (i32, i32)) {
        // Resolve output geometry before converting the canvas target to margins.
        if self.output_size == (0, 0) {
            self.select_output();
        }
        let local = (target.0 as f32 / self.ppp - self.output_position.0 as f32, target.1 as f32 / self.ppp - self.output_position.1 as f32);
        self.placement.set_local_origin(local, self.output_size, surface::logical_size(self.ppp));
        self.placement.apply_canvas(&self.layer, self.layout);
    }

    /// Recomputes the double-buffered surface state for the current scale.
    ///
    /// Both requests are latched by the next `wl_surface.commit` — on Wayland
    /// that is the commit `Queue::present` issues, so this must run before
    /// [`Wgpu::draw`].
    fn apply_surface_state(&mut self) {
        self.gpu.resize(SceneLayout::new(self.ppp).size);
        self.placement.apply_canvas(&self.layer, SceneLayout::new(self.ppp));
        if let Some(viewport) = &self.viewporter {
            viewport.set_destination(self.logical.0 as i32, self.logical.1 as i32);
        }
        if let Err(err) = self.update_input_region() {
            log::warn!("cannot update the input region: {err:#}");
        }
    }

    fn update_input_region(&mut self) -> Result<()> {
        let rect = if self.scene.menu_open() { Rect::from_min_size(Pos2::ZERO, self.layout.size_points()) } else { self.layout.hit_rect() };
        if self.input_region != Some(rect) {
            surface::set_input_region(&self.compositor, &self.layer, self.layout, self.scene.menu_open())?;
            self.input_region = Some(rect);
        }
        Ok(())
    }

    /// Switches to a fractional scale reported by the compositor.
    fn set_scale(&mut self, ppp: f32) {
        if !(0.5..=4.0).contains(&ppp) {
            return;
        }
        self.fractional = true;
        if (self.ppp - ppp).abs() < 0.01 {
            return;
        }
        log::info!("fractional scale {ppp}");
        self.ppp = ppp;
        self.layout = SceneLayout::new(ppp);
        self.logical = surface::window_size(ppp);
        log::info!("{}x{} logical, canvas {}x{} physical", self.logical.0, self.logical.1, CANVAS_W, CANVAS_H);
        self.layer.set_size(self.logical.0, self.logical.1);
        self.apply_surface_state();
        self.dirty = true;
    }

    fn apply_cursor(&mut self, icon: egui::CursorIcon) {
        if self.cursor == Some(icon) {
            return;
        }
        self.cursor = Some(icon);
        let (Some(pointer), Some(icon)) = (&self.pointer, input::cursor_icon(icon)) else { return };
        // Before the first enter there is no serial to attach the cursor to;
        // the pointer handler re-sends the icon after every enter, so the next
        // frame fixes it.
        if let Err(err) = pointer.set_cursor(&self.conn, icon) {
            log::debug!("cannot set the cursor: {err}");
        }
    }

    fn create_pointer(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) {
        let cursor_surface = self.compositor.create_surface(qh);
        match self.seat_state.get_pointer_with_theme(qh, seat, self.shm.wl_shm(), cursor_surface, ThemeSpec::default()) {
            Ok(pointer) => {
                self.pointer = Some(pointer);
                // A fresh pointer has no cursor yet.
                self.cursor = None;
                self.dirty = true;
            }
            Err(err) => log::warn!("cannot create a pointer: {err}"),
        }
    }

    fn create_keyboard(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) {
        match self.seat_state.get_keyboard(qh, seat, None) {
            Ok(keyboard) => self.keyboard = Some(keyboard),
            Err(err) => log::warn!("cannot create a keyboard: {err}"),
        }
    }
}

impl CompositorHandler for WaylandApp {
    fn scale_factor_changed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _surface: &wl_surface::WlSurface, _new_factor: i32) {
        // The fractional-scale protocol is authoritative; the integer scale
        // only ever arrives as a coarse hint.
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _surface: &wl_surface::WlSurface, _time: u32) {}

    fn surface_enter(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _surface: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        // Authoritative answer to "which output is this surface on": the
        // compositor sends this after the first commit. Its geometry drives the
        // canvas-to-global mapping and the drag clamp.
        if self.output.as_ref() == Some(output) && self.output_size != (0, 0) {
            return;
        }
        let Some(info) = self.output_state.info(output) else { return };
        let scale = info.scale_factor.max(1) as f32;
        let size = info.logical_size.or_else(|| {
            info.modes
                .iter()
                .find(|mode| mode.current)
                .map(|mode| ((mode.dimensions.0 as f32 / scale).round() as i32, (mode.dimensions.1 as f32 / scale).round() as i32))
        });
        let Some(size) = size else { return };
        let position = info.logical_position.unwrap_or(info.location);
        self.adopt_output(position, size, scale);
        self.output = Some(output.clone());
        log::info!("surface on {} at {position:?}, {size:?} logical, scale {}", info.name.unwrap_or(info.model), self.ppp);
        self.dirty = true;
    }

    fn surface_leave(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _surface: &wl_surface::WlSurface, _output: &wl_output::WlOutput) {}
}

impl OutputHandler for WaylandApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) {}

    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.output.as_ref() != Some(&output) {
            return;
        }
        if let Some(info) = self.output_state.info(&output) {
            self.output_position = info.logical_position.unwrap_or(info.location);
            if let Some(size) = info.logical_size {
                self.output_size = size;
                self.dirty = true;
            }
        }
    }

    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.output.as_ref() == Some(&output) {
            log::warn!("the output holding the pet went away; keeping the last known geometry");
            self.output = None;
        }
    }
}

impl LayerShellHandler for WaylandApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        log::info!("the layer surface was closed by the compositor");
        self.exit = true;
    }

    fn configure(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
        // sctk already acked this configure; acking again would be a protocol
        // error. The suggested size is only a hint because the surface is
        // sized explicitly, so it is logged rather than followed.
        if !self.configured {
            log::info!("first configure: suggested {:?}, using {:?}", configure.new_size, self.logical);
        } else if configure.new_size != (0, 0) && configure.new_size != self.logical {
            log::debug!("configure suggests {:?}, keeping {:?}", configure.new_size, self.logical);
        }
        self.configured = true;
        self.apply_surface_state();
        self.dirty = true;
    }
}

impl SeatHandler for WaylandApp {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        match capability {
            Capability::Pointer if self.pointer.is_none() => self.create_pointer(qh, &seat),
            Capability::Keyboard if self.keyboard.is_none() => self.create_keyboard(qh, &seat),
            _ => {}
        }
    }

    fn remove_capability(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat, capability: Capability) {
        match capability {
            Capability::Pointer => self.pointer = None,
            Capability::Keyboard => self.keyboard = None,
            _ => {}
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
        self.pointer = None;
        self.keyboard = None;
    }
}

impl PointerHandler for WaylandApp {
    fn pointer_frame(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, pointer: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        if !self.pointer.as_ref().is_some_and(|themed| themed.pointer() == pointer) {
            return;
        }
        let modifiers = input::egui_modifiers(self.modifiers);
        let on_pet: Vec<PointerEvent> = events.iter().filter(|event| event.surface == *self.layer.wl_surface()).cloned().collect();
        if !on_pet.is_empty() {
            input::translate_frame(&on_pet, modifiers, &mut self.pointer_down, true, &mut self.pending_events);

            // Anchor the global position at the moment the event was produced:
            // the surface may move afterwards, and the cursor does not. Wayland
            // sends no motion when only the surface moves, so this value stays
            // put while the drag window chases the pointer — recomputing it
            // each frame would feed the surface's own movement back into the
            // spring.
            if let Some(event) = on_pet.iter().rev().find(|event| !matches!(event.kind, PointerEventKind::Leave { .. })) {
                let origin = self.layout.window_origin(self.window_origin());
                self.pointer_global =
                    Some((origin.0 as f32 + event.position.0 as f32 * self.ppp, origin.1 as f32 + event.position.1 as f32 * self.ppp));
            }
            if on_pet.last().is_some_and(|event| matches!(event.kind, PointerEventKind::Leave { .. })) && !self.core.has_press() {
                self.pointer_global = None;
            }
        }

        // `set_cursor` needs the serial of the latest enter, so an icon
        // requested before the pointer arrived could not be applied; re-send it
        // now that the serial exists.
        if events.iter().any(|event| matches!(event.kind, PointerEventKind::Enter { .. }))
            && let (Some(pointer), Some(icon)) = (&self.pointer, self.cursor.and_then(input::cursor_icon))
            && let Err(err) = pointer.set_cursor(&self.conn, icon)
        {
            log::debug!("cannot restore the cursor after enter: {err}");
        }
        self.dirty = true;
    }
}

impl KeyboardHandler for WaylandApp {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[smithay_client_toolkit::seat::keyboard::Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
    }

    fn press_key(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _keyboard: &wl_keyboard::WlKeyboard, _serial: u32, event: KeyEvent) {
        // Detected here rather than through egui: the pet window is the only
        // thing that can have focus on this surface, and this keeps the
        // shortcut working even if egui's key mapping lags a layout change.
        if input::is_quit(&event, input::egui_modifiers(self.modifiers)) {
            log::info!("Ctrl+Q pressed");
            self.exit = true;
        }
        if event.keysym == Keysym::Escape {
            self.pending_events.push(Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: input::egui_modifiers(self.modifiers),
            });
            self.dirty = true;
        }
    }

    fn repeat_key(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _keyboard: &wl_keyboard::WlKeyboard, _serial: u32, _event: KeyEvent) {}

    fn release_key(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _keyboard: &wl_keyboard::WlKeyboard, _serial: u32, _event: KeyEvent) {}

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: SeatModifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl ShmHandler for WaylandApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for WaylandApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleManagerV1, GlobalData> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &WpFractionalScaleManagerV1,
        _event: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for WaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &WpFractionalScaleV1,
        event: <WpFractionalScaleV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            // The protocol reports the numerator of a scale whose denominator
            // is 120: 180 is 1.5x.
            state.set_scale(scale as f32 / 120.0);
        }
    }
}

delegate_compositor!(WaylandApp);
delegate_output!(WaylandApp);
delegate_shm!(WaylandApp);
delegate_seat!(WaylandApp);
delegate_keyboard!(WaylandApp);
delegate_pointer!(WaylandApp);
delegate_layer!(WaylandApp);
delegate_registry!(WaylandApp);
delegate_simple!(WaylandApp, WpViewporter, 1);
delegate_noop!(WaylandApp: ignore WpViewport);
