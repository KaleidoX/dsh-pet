//! egui front-end: one borderless, always-on-top window that draws the
//! 640x360 canvas at native pixel size.
//!
//! The window is transparent except for the decoded frame. Only the pet's hit
//! box takes clicks; everything else stays click-through. The platforms
//! reach that differently because their window systems do:
//!
//! - X11 restricts the input region with SHAPE, so the window itself only ever
//!   sees pointer events inside the hit box.
//! - Windows and macOS switch the whole window between interactive and
//!   click-through as the pointer enters and leaves the hit box.
//! - Windows uses `WS_EX_TRANSPARENT | WS_EX_LAYERED`. Per-pixel transparency there
//!   additionally needs a DirectComposition swapchain and a window created
//!   without a DWM redirection surface.
//!
//! Dragging moves the native window with the same spring the web client uses; a
//! click (press + release without crossing the drag threshold) plays a random
//! click clip with the squash-and-rebound animation, and clips play once before
//! falling back to the idle pool. The behaviour itself lives in `pet-ui-core`;
//! this crate only feeds it winit's input and applies the result to the window.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use egui::{Pos2, Vec2};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use pet_config::HIT_BOX;
use pet_config::{CANVAS_H, CANVAS_W, PetConfig, WorldArea};
use pet_overlay::{PetScene, SceneLayout};
use pet_ui_core::{FrameInput, PetCore};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::Window;

#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "macos")]
mod macos;

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
        // canvas plus control margins in the app creator, once the scale is known.
        .with_inner_size([CANVAS_W as f32, CANVAS_H as f32])
        .with_resizable(false)
        .with_decorations(false)
        .with_has_shadow(false)
        .with_transparent(true)
        .with_taskbar(false)
        .with_active(false)
        .with_window_level(egui::WindowLevel::AlwaysOnTop);

    let mut native = eframe::NativeOptions { viewport, persist_window: false, ..Default::default() };

    #[cfg(target_os = "macos")]
    {
        // Reduce queued image latency while native window positions move immediately.
        native.wgpu_options.surface = eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY;
        native.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS as _};
            builder.with_activation_policy(ActivationPolicy::Accessory);
            builder.with_activate_ignoring_other_apps(false);
        }));
    }

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
    /// Monitor work area in physical pixels; the pet roams inside it and the
    /// overlay menu is clamped to it.
    world: WorldArea,
    world_read: Instant,
    /// Pet and controls composed synchronously in this window.
    scene: PetScene,
    layout: SceneLayout,
    #[cfg(target_os = "macos")]
    last_position_request: Option<(i32, i32)>,
    #[cfg(target_os = "macos")]
    position_retries: u8,
    /// Re-assert always-on-top until this instant (KWin ignores the creation-time
    /// hint under XWayland, but honours a runtime request).
    on_top_until: Instant,
    on_top_next: Instant,
    #[cfg(target_os = "linux")]
    x11: Option<x11::X11>,
    /// Whether the window currently ignores the pointer. The whole
    /// window is either interactive or click-through, so this has to follow the
    /// pointer in and out of the hit box.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    passthrough: bool,
    /// The global left button state on the previous frame, used to
    /// notice a press the window itself never saw.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pointer_down: bool,
    #[cfg(target_os = "macos")]
    pointer_monitor: macos::PointerMonitor,
}

impl PetApp {
    fn new(cc: &eframe::CreationContext<'_>, asset_dir: PathBuf, config: PetConfig, options: RunOptions) -> Result<Self> {
        let window = cc.winit_window().cloned().ok_or_else(|| anyhow!("eframe did not provide a native window"))?;
        let ppp = window.scale_factor() as f32;

        // The window is not visible yet; fixing the composed physical size now
        // avoids the first-frame `InnerSize` round trip and the compositor never
        // sees the placeholder size.
        let mut layout = if cfg!(target_os = "macos") { SceneLayout::compact(ppp) } else { SceneLayout::new(ppp) };
        let _ = window.request_inner_size(PhysicalSize::new(layout.size.0, layout.size.1));

        #[cfg(target_os = "linux")]
        let x11 = match x11::X11::new(&window) {
            Ok(mut x11) => {
                if let Err(err) = x11.set_input_region(layout, false) {
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
        let scene = PetScene::new(&asset_dir, &config);
        #[cfg(target_os = "macos")]
        let mut core = {
            let backend = std::env::var("WHALE_PET_VIDEO_BACKEND").unwrap_or_else(|_| "auto".into());
            if !matches!(backend.as_str(), "auto" | "webm" | "hevc") {
                return Err(anyhow!("WHALE_PET_VIDEO_BACKEND must be auto, webm or hevc"));
            }
            let mov_dir = std::env::var_os("WHALE_PET_MOV_DIR").map(PathBuf::from).unwrap_or_else(|| asset_dir.join("mov"));
            let use_hevc = backend == "hevc" || (backend == "auto" && mov_dir.is_dir());
            if use_hevc {
                let native = (|| -> Result<pet_macos::Renderer> {
                    for entry in std::fs::read_dir(asset_dir.join("webm"))? {
                        let path = entry?.path();
                        if path.extension().is_some_and(|ext| ext == "webm") {
                            let mov = mov_dir.join(path.file_name().unwrap()).with_extension("mov");
                            if !mov.is_file() {
                                return Err(anyhow!("missing HEVC asset: {}", mov.display()));
                            }
                        }
                    }
                    let first = pet_model::AnimPools::from_config(&config).first_idle().ok_or_else(|| anyhow!("no idle animation"))?;
                    // Fail or fall back before selecting a native player, rather than
                    // leaving a transparent window if hardware is unavailable.
                    let mut probe = pet_macos::Decoder::open(&mov_dir.join(first).with_extension("mov"))?;
                    if probe.next_frame()?.is_none() {
                        return Err(anyhow!("HEVC clip is empty"));
                    }
                    let state = cc.wgpu_render_state.clone().ok_or_else(|| anyhow!("HEVC rendering requires wgpu/Metal"))?;
                    pet_macos::Renderer::new(state)
                })();
                match native {
                    Ok(renderer) => {
                        log::info!("video backend: HEVC-alpha hardware + Metal");
                        PetCore::with_native_renderer(asset_dir, &config, options.stats, mov_dir, renderer)?
                    }
                    Err(err) if backend == "auto" => {
                        log::warn!("HEVC unavailable ({err:#}); using WebM/libvpx");
                        PetCore::new(asset_dir, &config, options.stats)?
                    }
                    Err(err) => return Err(err),
                }
            } else {
                log::info!("video backend: WebM/libvpx (set up MOV assets for hardware decoding)");
                PetCore::new(asset_dir, &config, options.stats)?
            }
        };
        #[cfg(not(target_os = "macos"))]
        let mut core = PetCore::new(asset_dir, &config, options.stats)?;
        #[cfg(target_os = "macos")]
        core.set_position_step(window.scale_factor() as f32);
        core.prepare_view(world, ppp);
        let requested = layout.place(core.canvas_origin(), world);
        let requested = PhysicalPosition::new(requested.0, requested.1);
        window.set_outer_position(requested);
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

        let observed = window.outer_position().unwrap_or(requested);
        let now = Instant::now();
        log::info!(
            "window at {},{} ({}x{} physical), canvas {}x{} at {:?}, scale {ppp}, idle clip {}",
            observed.x,
            observed.y,
            layout.size.0,
            layout.size.1,
            CANVAS_W,
            CANVAS_H,
            layout.offset,
            core.clip_name()
        );
        if options.stats && !cfg!(target_os = "linux") {
            log::warn!("--stats is only implemented on Linux");
        }
        #[cfg(target_os = "macos")]
        let pointer_monitor = macos::PointerMonitor::new(cc.egui_ctx.clone(), &window, layout, core.canvas_origin())?;

        Ok(Self {
            core,
            window,
            ppp,
            world,
            world_read: now,
            scene,
            layout,
            #[cfg(target_os = "macos")]
            last_position_request: None,
            #[cfg(target_os = "macos")]
            position_retries: 0,
            on_top_until: now + Duration::from_secs(2),
            on_top_next: now,
            #[cfg(target_os = "linux")]
            x11,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            passthrough: false,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            pointer_down: false,
            #[cfg(target_os = "macos")]
            pointer_monitor,
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
            #[cfg(target_os = "macos")]
            self.pointer_monitor.refresh_screen();
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

        // See `fold_global_pointer`: the global read can supply both
        // the position and a press the window never saw.
        let (global, button_down) = self.global_pointer();
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let (pressed, local) = self.fold_global_pointer(pressed, local, global, button_down);
        let origin = self.window_origin();
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
            window_origin: origin,
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
        #[cfg(target_os = "macos")]
        if let Some((x, y, down)) = macos::pointer(&self.window) {
            return (Some((x as f32, y as f32)), Some(down));
        }
        (None, None)
    }

    /// Fill in a press and position that egui never saw.
    ///
    /// While the window is click-through it gets no pointer events at all, so
    /// egui's position is whatever it was when the pointer left the window; a
    /// press arriving after the pointer re-enters the hit box would then be
    /// tested against that stale position and dropped. The global cursor read is
    /// authoritative here, and its button state gives the press edge.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    fn fold_global_pointer(&mut self, pressed: bool, local: Option<Pos2>, global: Option<(f32, f32)>, down: Option<bool>) -> (bool, Option<Pos2>) {
        let (Some((x, y)), Some(down)) = (global, down) else { return (pressed, local) };
        let rising = down && !self.pointer_down;
        self.pointer_down = down;
        let origin = self.window_origin();
        let canvas = Vec2::new(x - origin.0 as f32, y - origin.1 as f32);
        let local = Some(Pos2::new(canvas.x / self.ppp, canvas.y / self.ppp));
        (pressed || (rising && !self.core.has_press()), local)
    }

    /// Keep the window interactive exactly while the pointer is
    /// over the hit box (or a press is in progress).
    ///
    /// Windows transparent styles and macOS `ignoresMouseEvents` apply to the
    /// whole window. It no longer receives pointer events, so the position
    /// has to come from the global cursor read rather than from egui.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    fn update_passthrough(&mut self, ctx: &egui::Context, global: Option<(f32, f32)>) {
        let Some((x, y)) = global else { return };
        let origin = self.core.canvas_origin();
        let canvas_x = x - origin.0 as f32;
        let canvas_y = y - origin.1 as f32;
        // A press keeps the window interactive even after the pointer leaves the
        // hit box: winit installed the mouse capture on button-down, and a
        // click-through window would never see the release.
        let passthrough = !self.core.has_press() && !self.scene.menu_open() && !HIT_BOX.contains(canvas_x, canvas_y);
        if passthrough != self.passthrough {
            log::debug!("passthrough {passthrough}");
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(passthrough));
            self.passthrough = passthrough;
        }
    }

    fn window_origin(&self) -> (i32, i32) {
        self.layout.window_origin(self.core.canvas_origin())
    }

    /// Window geometry is an output of the video position. Readback may retry
    /// an ignored move, but must never become another source of video position.
    fn apply_window_position(&mut self) {
        let origin = self.window_origin();
        let requested = PhysicalPosition::new(origin.0, origin.1);
        let observed = self.window.outer_position().ok();
        if observed != Some(requested) {
            #[cfg(target_os = "macos")]
            {
                if self.last_position_request != Some(origin) {
                    self.last_position_request = Some(origin);
                    self.position_retries = 0;
                }
                if self.position_retries >= 3 {
                    return;
                }
                self.position_retries += 1;
            }
            log::trace!("window position request {requested:?}, observed {observed:?}");
            self.window.set_outer_position(requested);
        } else {
            #[cfg(target_os = "macos")]
            {
                self.last_position_request = Some(origin);
                self.position_retries = 0;
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn sync_menu_layout(&mut self, ctx: &egui::Context) {
        let menu = self.scene.menu_open();
        if self.layout.is_compact() != menu {
            return;
        }
        let canvas = self.core.canvas_origin();
        self.layout = if menu { SceneLayout::new(self.ppp) } else { SceneLayout::compact(self.ppp) };
        self.layout.place(canvas, self.world);
        let _ = self.window.request_inner_size(PhysicalSize::new(self.layout.size.0, self.layout.size.1));
        self.last_position_request = None;
        self.apply_window_position();
        ctx.request_repaint();
    }
}

impl eframe::App for PetApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        #[cfg(target_os = "macos")]
        {
            self.core.set_position_step(self.window.scale_factor() as f32);
            self.sync_menu_layout(&ctx);
        }

        if now >= self.on_top_next && now < self.on_top_until {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
            self.on_top_next = now + Duration::from_millis(500);
        }

        let ppp = ctx.pixels_per_point();
        if (ppp - self.ppp).abs() > 0.001 {
            let canvas = self.core.canvas_origin();
            self.ppp = ppp;
            #[cfg(target_os = "macos")]
            {
                self.world = read_world(&self.window);
                self.world_read = now;
            }
            self.layout = if cfg!(target_os = "macos") && !self.scene.menu_open() { SceneLayout::compact(ppp) } else { SceneLayout::new(ppp) };
            let size = self.layout.size;
            let _ = self.window.request_inner_size(PhysicalSize::new(size.0, size.1));
            #[cfg(target_os = "macos")]
            {
                self.last_position_request = None;
            }
            self.layout.place(canvas, self.world);
            self.core.prepare_view(self.world, ppp);
            self.apply_window_position();
        }
        let input = self.frame_input(&ctx, now);
        let out = self.scene.ui(ui, &mut self.core, input, &mut self.layout);
        self.apply_window_position();
        if let Some(cursor) = out.cursor {
            ctx.set_cursor_icon(cursor);
        }

        #[cfg(target_os = "linux")]
        if let Some(x11) = &mut self.x11 {
            if let Err(err) = x11.set_input_region(self.layout, self.scene.menu_open()) {
                log::warn!("cannot update input region: {err:#}");
            }
        }

        if out.close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            self.update_passthrough(&ctx, input.global);
        }
        #[cfg(target_os = "macos")]
        {
            if self.layout.is_compact() == self.scene.menu_open() {
                ctx.request_repaint();
            }
            self.pointer_monitor.update(&self.window, self.layout, self.core.canvas_origin(), self.scene.menu_open(), self.core.has_press());
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
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "macos")]
fn read_world(window: &Window) -> WorldArea {
    macos::work_area(window).unwrap_or_else(|| monitor_world(window))
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
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
