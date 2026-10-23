//! egui front-end: one borderless, always-on-top window that draws the
//! 640x360 canvas at native pixel size.
//!
//! The window is transparent except for the decoded frame. Only the pet's hit
//! box takes clicks; everything else stays click-through. The two platforms
//! reach that differently because their window systems do:
//!
//! - X11 restricts the input region with SHAPE, so the window itself only ever
//!   sees pointer events inside the hit box.
//! - Windows has no per-region input shape for a top-level window, so the whole
//!   window switches between `WS_EX_TRANSPARENT | WS_EX_LAYERED` and normal
//!   as the pointer enters and leaves the hit box. Per-pixel transparency there
//!   additionally needs a DirectComposition swapchain and a window created
//!   without a DWM redirection surface.
//!
//! Dragging moves the native window with the same spring the web client uses; a
//! click (press + release without crossing the drag threshold) plays a random
//! click clip with the squash-and-rebound animation, and clips play once before
//! falling back to the idle pool. The behaviour itself lives in `pet-ui-core`;
//! this crate only feeds it winit's input and applies the result to the window.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui;
#[cfg(target_os = "windows")]
use egui::{Pos2, Vec2};
#[cfg(target_os = "windows")]
use pet_config::HIT_BOX;
use pet_config::{CANVAS_H, CANVAS_W, PetConfig, WorldArea};
use pet_overlay::{OverlayMode, OverlayState, overlay_placement};
use pet_ui_core::{FrameInput, PetCore, anchor_position};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::Window;

#[cfg(target_os = "linux")]
mod x11;

/// Viewport id of the interaction overlay window.
fn overlay_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("whale-pet-overlay")
}
/// How often the work area is re-read (a panel may be added or resized).
const WORLD_REFRESH: Duration = Duration::from_secs(2);

/// Options for [`run`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    /// Log fps, CPU and memory once per second (Linux only).
    pub stats: bool,
    /// Force the X11 (XWayland) backend on Linux when `DISPLAY` is set. KWin
    /// does not let native Wayland clients position themselves, stay on top or
    /// shape their input region, all of which the pet needs.
    pub force_x11: bool,
}

/// Runs the pet until the window is closed.
pub fn run(asset_dir: PathBuf, config: PetConfig, options: RunOptions) -> eframe::Result {
    let viewport = egui::ViewportBuilder::default()
        .with_title("whale-pet")
        .with_app_id("whale-pet")
        // Placeholder size in points; the native window is resized to the exact
        // 640x360 physical pixels in the app creator, once the scale is known.
        .with_inner_size([CANVAS_W as f32, CANVAS_H as f32])
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_taskbar(false)
        .with_active(false)
        .with_window_level(egui::WindowLevel::AlwaysOnTop);

    // Only the Linux path customises the options below.
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut native = eframe::NativeOptions { viewport, persist_window: false, ..Default::default() };

    #[cfg(target_os = "linux")]
    if options.force_x11 && std::env::var_os("DISPLAY").is_some() {
        native.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::x11::EventLoopBuilderExtX11 as _;
            builder.with_x11();
        }));
    }

    // On Windows, transparent pixels only reach the desktop through a
    // DirectComposition swapchain on a window without a DWM redirection surface.
    // wgpu only reports alpha-capable composite modes for the composition target,
    // and winit only accepts the no-redirection style at window creation, so both
    // have to be arranged before eframe builds the window.
    #[cfg(target_os = "windows")]
    {
        use eframe::egui_wgpu::WgpuSetup;
        let mut setup = match native.wgpu_options.wgpu_setup {
            WgpuSetup::CreateNew(create_new) => create_new,
            WgpuSetup::Existing(_) => unreachable!("the default options create a new setup"),
        };
        setup.instance_descriptor.backend_options.dx12.presentation_system = eframe::egui_wgpu::wgpu::Dx12SwapchainKind::DxgiFromVisual;
        native.wgpu_options.wgpu_setup = WgpuSetup::CreateNew(setup);
        egui_winit::set_transparent_no_redirection_bitmap(true);
    }

    eframe::run_native(
        "whale-pet",
        native,
        Box::new(move |cc| {
            let app = PetApp::new(cc, asset_dir, config, options)?;
            Ok(Box::new(app))
        }),
    )
}

struct PetApp {
    core: PetCore,
    window: Arc<Window>,
    ppp: f32,
    last_pos: PhysicalPosition<i32>,
    /// Monitor work area in physical pixels; the pet roams inside it and the
    /// overlay menu is clamped to it.
    world: WorldArea,
    world_read: Instant,
    /// Shared with the overlay viewport's draw callback.
    overlay: Arc<Mutex<OverlayState>>,
    /// Where the configured corner/margins put the window. KWin ignores moves
    /// issued before the window is mapped, so this is re-applied for a short
    /// while after startup (and dropped once the user drags the pet).
    target_pos: Option<PhysicalPosition<i32>>,
    /// Re-assert always-on-top until this instant (KWin ignores the creation-time
    /// hint under XWayland, but honours a runtime request).
    on_top_until: Instant,
    on_top_next: Instant,
    /// Whether the overlay window was visible on the previous frame.
    ///
    /// The overlay is created hidden (eframe always does that), and winit on
    /// X11 only applies the window level at creation time when the window is
    /// visible, so the level has to be re-sent when the window is first shown.
    overlay_visible: bool,
    #[cfg(target_os = "linux")]
    x11: Option<x11::X11>,
    /// Windows: whether the window currently ignores the pointer. The whole
    /// window is either interactive or click-through, so this has to follow the
    /// pointer in and out of the hit box.
    #[cfg(target_os = "windows")]
    passthrough: bool,
    /// Windows: the global left button state on the previous frame, used to
    /// notice a press the window itself never saw.
    #[cfg(target_os = "windows")]
    pointer_down: bool,
}

impl PetApp {
    fn new(cc: &eframe::CreationContext<'_>, asset_dir: PathBuf, config: PetConfig, options: RunOptions) -> Result<Self> {
        let window = cc.winit_window().cloned().ok_or_else(|| anyhow!("eframe did not provide a native window"))?;
        let ppp = window.scale_factor() as f32;

        // The window is not visible yet; fixing the exact physical size now
        // avoids the first-frame `InnerSize` round trip and the compositor never
        // sees the placeholder size.
        let _ = window.request_inner_size(PhysicalSize::new(CANVAS_W, CANVAS_H));

        #[cfg(target_os = "linux")]
        let x11 = match x11::X11::new(&window) {
            Ok(x11) => {
                if let Err(err) = x11.set_input_region() {
                    log::warn!("cannot restrict the input region (clicks outside the pet reach the window): {err:#}");
                }
                Some(x11)
            }
            Err(err) => {
                log::warn!("X11 pointer tracking unavailable: {err:#}");
                None
            }
        };

        // The work area, not the whole monitor: the pet must not walk under the
        // taskbar and the menu must not open into it.
        #[cfg(target_os = "linux")]
        let world = read_world(&window, x11.as_ref());
        #[cfg(not(target_os = "linux"))]
        let world = read_world(&window);
        let requested = place_window(&window, &config.position(), world);
        window.set_window_level(winit::window::WindowLevel::AlwaysOnTop);

        // Even without decorations DWM draws a thin frame and shadow around the
        // window, which reads as a grey outline around the transparent canvas.
        #[cfg(target_os = "windows")]
        {
            use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
            match window.window_handle().map(|handle| handle.as_raw()) {
                Ok(RawWindowHandle::Win32(win32)) => {
                    if !pet_win32::disable_dwm_frame(win32.hwnd.get()) {
                        log::warn!("cannot disable DWM non-client rendering; a frame may outline the window");
                    }
                }
                Ok(other) => log::warn!("unexpected window handle {other:?}"),
                Err(err) => log::warn!("cannot read the native window handle: {err}"),
            }
        }

        let overlay = OverlayState::new(&asset_dir, &config);
        let core = PetCore::new(asset_dir, &config, options.stats)?;

        let last_pos = window.outer_position().unwrap_or(requested);
        let now = Instant::now();
        log::info!("window at {},{} ({}x{} physical), scale {ppp}, idle clip {}", last_pos.x, last_pos.y, CANVAS_W, CANVAS_H, core.clip_name());
        if options.stats && !cfg!(target_os = "linux") {
            log::warn!("--stats is only implemented on Linux");
        }

        Ok(Self {
            core,
            window,
            ppp,
            last_pos,
            world,
            world_read: now,
            overlay: Arc::new(Mutex::new(overlay)),
            target_pos: Some(requested),
            on_top_until: now + Duration::from_secs(2),
            on_top_next: now,
            overlay_visible: false,
            #[cfg(target_os = "linux")]
            x11,
            #[cfg(target_os = "windows")]
            passthrough: false,
            #[cfg(target_os = "windows")]
            pointer_down: false,
        })
    }

    /// Assembles this frame's host state from egui's input and the platform's
    /// global pointer read.
    fn frame_input(&mut self, ctx: &egui::Context, now: Instant) -> FrameInput {
        if now.saturating_duration_since(self.world_read) >= WORLD_REFRESH {
            #[cfg(target_os = "linux")]
            let world = read_world(&self.window, self.x11.as_ref());
            #[cfg(not(target_os = "linux"))]
            let world = read_world(&self.window);
            self.world = world;
            self.world_read = now;
        }
        let (pressed, released, secondary_pressed, local, dt) = ctx.input(|input| {
            (
                input.pointer.primary_pressed(),
                input.pointer.primary_released(),
                input.pointer.secondary_pressed(),
                input.pointer.interact_pos(),
                input.stable_dt,
            )
        });

        // See `fold_global_pointer`: on Windows the global read can supply both
        // the position and a press the window never saw.
        #[cfg(target_os = "windows")]
        let (pressed, local) = self.fold_global_pointer(pressed, local);

        let (global, button_down) = self.global_pointer();
        let origin = self.window.outer_position().unwrap_or(self.last_pos);
        FrameInput {
            now,
            dt,
            ppp: self.ppp,
            pointer: local,
            pressed,
            released,
            secondary_pressed,
            global,
            button_down,
            window_origin: (origin.x, origin.y),
            world: self.world,
        }
    }

    /// Global pointer position in physical pixels plus the left button state.
    ///
    /// X11 is asked directly because the window only receives pointer events
    /// inside its input region: once the cursor leaves the hit box during a
    /// drag, egui's last known position is stale and the release would be lost.
    /// Windows needs the same for the same reason, plus one of its own: the
    /// window follows the pointer during a drag, so `outer_position + local`
    /// lags behind the real cursor by the spring offset.
    fn global_pointer(&self) -> (Option<(f32, f32)>, Option<bool>) {
        #[cfg(target_os = "linux")]
        if let Some(x11) = &self.x11 {
            match x11.pointer() {
                Ok((x, y, down)) => return (Some((x as f32, y as f32)), Some(down)),
                Err(err) => log::debug!("query_pointer failed: {err}"),
            }
        }
        #[cfg(target_os = "windows")]
        if let Some((x, y, down)) = pet_win32::pointer() {
            return (Some((x as f32, y as f32)), Some(down));
        }
        (None, None)
    }

    /// Windows only: fill in a press and position that egui never saw.
    ///
    /// While the window is click-through it gets no pointer events at all, so
    /// egui's position is whatever it was when the pointer left the window; a
    /// press arriving after the pointer re-enters the hit box would then be
    /// tested against that stale position and dropped. The global cursor read is
    /// authoritative here, and its button state gives the press edge.
    #[cfg(target_os = "windows")]
    fn fold_global_pointer(&mut self, pressed: bool, local: Option<Pos2>) -> (bool, Option<Pos2>) {
        let Some((x, y, down)) = pet_win32::pointer() else { return (pressed, local) };
        let rising = down && !self.pointer_down;
        self.pointer_down = down;
        let origin = self.window.outer_position().unwrap_or(self.last_pos);
        let canvas = Vec2::new((x - origin.x) as f32, (y - origin.y) as f32);
        let local = Some(Pos2::new(canvas.x / self.ppp, canvas.y / self.ppp));
        (pressed || (rising && !self.core.has_press()), local)
    }

    /// Windows only: keep the window interactive exactly while the pointer is
    /// over the hit box (or a press is in progress).
    ///
    /// `WS_EX_TRANSPARENT | WS_EX_LAYERED` applies to the whole window, and once
    /// it is set the window no longer receives pointer events, so the position
    /// has to come from the global cursor read rather than from egui.
    #[cfg(target_os = "windows")]
    fn update_passthrough(&mut self, ctx: &egui::Context) {
        let Some((x, y, _down)) = pet_win32::pointer() else { return };
        let origin = self.window.outer_position().unwrap_or(self.last_pos);
        let canvas_x = (x - origin.x) as f32;
        let canvas_y = (y - origin.y) as f32;
        // A press keeps the window interactive even after the pointer leaves the
        // hit box: winit installed the mouse capture on button-down, and a
        // click-through window would never see the release.
        let passthrough = !self.core.has_press() && !HIT_BOX.contains(canvas_x, canvas_y);
        if passthrough != self.passthrough {
            log::debug!("passthrough {passthrough}");
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(passthrough));
            self.passthrough = passthrough;
        }
    }
}

impl eframe::App for PetApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();

        if now >= self.on_top_next && now < self.on_top_until {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
            // X11 only keeps a pre-map move if the window manager honours the
            // position hint; KWin under XWayland does not, so the placement is
            // re-sent once the window is mapped.
            if let Some(target) = self.target_pos {
                self.window.set_outer_position(target);
                self.last_pos = target;
            }
            self.on_top_next = now + Duration::from_millis(500);
        }

        // Drain the overlay first: an action picked there (including "closed")
        // must reach the core before this frame computes its overlay intent, or
        // a stale `Menu` intent would immediately reopen the menu.
        let actions = self.overlay.lock().map(|mut overlay| overlay.take_actions()).unwrap_or_default();
        for action in actions {
            self.core.overlay_action(action);
        }

        let input = self.frame_input(&ctx, now);
        let out = self.core.ui(ui, input);
        if let Some(pos) = out.move_to {
            self.window.set_outer_position(PhysicalPosition::new(pos.0, pos.1));
            self.last_pos = PhysicalPosition::new(pos.0, pos.1);
        }
        if out.drag_started {
            self.target_pos = None;
        }
        if let Some(cursor) = out.cursor {
            ctx.set_cursor_icon(cursor);
        }

        let origin = self.window.outer_position().unwrap_or(self.last_pos);
        let origin = (origin.x, origin.y);
        let mode = if let Ok(mut overlay) = self.overlay.lock() {
            overlay.set_world(input.world, self.ppp);
            // The overlay window cannot be trusted to report its own pointer:
            // before it has been placed, a motion event arrives in screen
            // coordinates and egui keeps that value for later passes. Hand it
            // the global cursor the host already reads every frame.
            overlay.set_input(pet_overlay::OverlayInput { pointer: input.global });
            if let Some(score) = out.score {
                overlay.apply_score(score);
            }
            overlay.apply_intent(out.overlay);
            overlay.tick(now);
            overlay.mode()
        } else {
            OverlayMode::Hidden
        };
        self.show_overlay(&ctx, origin, mode, now);

        if out.close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            #[cfg(target_os = "windows")]
            self.update_passthrough(&ctx);
        }
    }
}

impl PetApp {
    /// Draws the interaction overlay in its own deferred viewport.
    ///
    /// The viewport has to be requested on every pet pass: egui drops any child
    /// viewport that was not used in the last pass, and the whole builder is
    /// replaced each time, so size and position follow the pet window.
    fn show_overlay(&mut self, ctx: &egui::Context, origin: (i32, i32), mode: OverlayMode, now: Instant) {
        let (position, size) = overlay_placement(origin, self.ppp);
        let builder = egui::ViewportBuilder::default()
            .with_title("whale-pet-overlay")
            .with_app_id("whale-pet-overlay")
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_active(false)
            .with_close_button(false)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_position(position)
            .with_inner_size(size)
            // Hidden windows are not painted at all; `Transient` shows the
            // tooltip/score but lets every click through to whatever is below.
            .with_visible(mode != OverlayMode::Hidden)
            .with_mouse_passthrough(mode == OverlayMode::Transient);

        let overlay = Arc::clone(&self.overlay);
        let timestamp = now;
        ctx.show_viewport_deferred(overlay_id(), builder, move |ui, _class| {
            // The host derived the mode for this frame; the callback must not
            // derive it again (a hidden viewport runs no pass at all).
            let Ok(mut overlay) = overlay.lock() else { return };
            let ctx = ui.ctx().clone();
            // The window manager may clamp the requested position (a window
            // hanging off the screen edge is normal here), so the drawing maps
            // screen anchors against the position the window really got.
            if let Some(rect) = ui.input(|input| input.viewport().outer_rect) {
                let ppp = ctx.pixels_per_point();
                overlay.set_window_origin(((rect.min.x * ppp).round() as i32, (rect.min.y * ppp).round() as i32));
            }
            // A repaint is only needed while something moves; the pet's own
            // pass asks for one as long as the popups live.
            if overlay.ui(&ctx, ui, timestamp) {
                ctx.request_repaint();
            }
        });
        if mode != OverlayMode::Hidden && !self.overlay_visible {
            // eframe creates child viewports hidden, and winit on X11 applies
            // `with_window_level` only at creation when the window is visible.
            // Without this the overlay keeps the level it was created with and
            // stays below the pet window, where the menu can never be clicked.
            ctx.send_viewport_cmd_to(overlay_id(), egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        }
        self.overlay_visible = mode != OverlayMode::Hidden;
        if mode != OverlayMode::Hidden {
            // Registering the viewport does not wake it: a child pass only runs
            // when the child itself asks for one (or its window receives input),
            // so without this the menu would stay undrawn after it opens on an
            // already-visible overlay.
            ctx.request_repaint_of(overlay_id());
        }
    }
}

/// Work area of the monitor the pet lives on, in physical pixels.
///
/// The pet lives on one monitor: the world is that monitor's rectangle minus
/// panels. `_NET_WORKAREA` is the union across every monitor, which on a
/// multi-head desktop is a bounding box that can include empty space (and a
/// top-right corner no output covers), so it is only used to trim the monitor
/// rectangle. Falls back to the monitor and then to the canvas size, so a
/// missing work-area property never leaves the pet with a zero-sized world.
#[cfg(target_os = "linux")]
fn read_world(window: &Window, x11: Option<&x11::X11>) -> WorldArea {
    let monitor = monitor_world(window);
    let work = x11.and_then(|x11| match x11.work_area() {
        Ok((x, y, width, height)) if width > 0 && height > 0 => Some(WorldArea { x, y, width, height }),
        Ok(_) => {
            log::warn!("_NET_WORKAREA is empty; using the monitor rectangle");
            None
        }
        Err(err) => {
            log::debug!("cannot read _NET_WORKAREA: {err:#}");
            None
        }
    });
    work.and_then(|work| intersect_world(monitor, work)).unwrap_or(monitor)
}

/// Intersection of two rectangles, `None` when they do not overlap.
fn intersect_world(a: WorldArea, b: WorldArea) -> Option<WorldArea> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (right > x && bottom > y).then(|| WorldArea { x, y, width: (right - x) as u32, height: (bottom - y) as u32 })
}

/// Windows has no `_NET_WORKAREA`; `GetMonitorInfoW.rcWork` is the same thing.
#[cfg(target_os = "windows")]
fn read_world(window: &Window) -> WorldArea {
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    if let Ok(RawWindowHandle::Win32(win32)) = window.window_handle().map(|handle| handle.as_raw())
        && let Some((x, y, width, height)) = pet_win32::work_area(win32.hwnd.get())
        && width > 0
        && height > 0
    {
        return WorldArea { x, y, width, height };
    }
    monitor_world(window)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn read_world(window: &Window) -> WorldArea {
    monitor_world(window)
}

/// Whole monitor rectangle as the last resort.
///
/// The monitor the window is on wins once it is mapped; before that (startup,
/// before placement) the primary monitor stands in.
fn monitor_world(window: &Window) -> WorldArea {
    match window.current_monitor().or_else(|| window.primary_monitor()) {
        Some(monitor) => {
            let origin = monitor.position();
            let size = monitor.size();
            WorldArea { x: origin.x, y: origin.y, width: size.width, height: size.height }
        }
        None => {
            log::warn!("no monitor reported; the pet roams inside its own canvas");
            WorldArea { x: 0, y: 0, width: CANVAS_W, height: CANVAS_H }
        }
    }
}

/// Places the pet window in the configured corner of the work area.
///
/// Margins are logical pixels (as in the web client); the work area and the
/// resulting position are physical screen pixels.
fn place_window(window: &Window, position: &pet_config::Position, world: WorldArea) -> PhysicalPosition<i32> {
    let (x, y) = anchor_position(world, position, window.scale_factor() as f32);
    let pos = PhysicalPosition::new(x.round() as i32, y.round() as i32);
    window.set_outer_position(pos);
    pos
}
