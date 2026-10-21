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
//! falling back to the idle pool.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui;
use egui::{Color32, Pos2, Rect, TextureOptions, Vec2};
use pet_config::{CANVAS_H, CANVAS_W, DRAG_THRESHOLD, HIT_BOX, PetConfig, SQ_DURATION_SECS, SQ_SQUASH, clip_path};
use pet_decode::Player;
use pet_model::{AnimPools, Spring2, squash_scale};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::Window;

#[cfg(target_os = "linux")]
mod x11;

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

/// What the currently playing clip represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    Drag,
    Click,
}

/// A pointer press that has not been released yet.
#[derive(Debug, Clone, Copy)]
struct Press {
    /// Pointer position in window points when the button went down.
    start: Pos2,
    /// Pointer offset from the window centre, in canvas pixels.
    off: Vec2,
    /// Set once the pointer travelled past [`DRAG_THRESHOLD`].
    dragging: bool,
    /// Window position in physical pixels, integrated by the drag spring.
    spring: Option<Spring2>,
}

struct PetApp {
    asset_dir: PathBuf,
    pools: AnimPools,
    throw_power: f32,
    player: Player,
    clip_name: String,
    mode: Mode,
    /// The current clip already reached its end (handled once).
    clip_done: bool,
    /// Number of consecutive clips that delivered zero frames.
    failed_clips: u32,
    /// Gave up on decoding: no further repaints are scheduled.
    stalled: bool,
    /// Bumped on every clip switch, so texture uploads can tell clips apart.
    play_gen: u64,
    texture: Option<egui::TextureHandle>,
    texture_key: Option<(u64, u64)>,
    next_due: Option<Duration>,
    /// Start of the Q-squash; `None` when no squash is running.
    squash: Option<Instant>,
    /// A click was played and the squash starts with its first frame.
    pending_squash: bool,
    press: Option<Press>,
    /// The pointer has been seen with the left button down at least once.
    saw_button_down: bool,
    window: Arc<Window>,
    ppp: f32,
    last_pos: PhysicalPosition<i32>,
    /// Where the configured corner/margins put the window. KWin ignores moves
    /// issued before the window is mapped, so this is re-applied for a short
    /// while after startup (and dropped once the user drags the pet).
    target_pos: Option<PhysicalPosition<i32>>,
    /// Re-assert always-on-top until this instant (KWin ignores the creation-time
    /// hint under XWayland, but honours a runtime request).
    on_top_until: Instant,
    on_top_next: Instant,
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
    meter: Meter,
}

impl PetApp {
    fn new(cc: &eframe::CreationContext<'_>, asset_dir: PathBuf, config: PetConfig, options: RunOptions) -> Result<Self> {
        let window = cc.winit_window().cloned().ok_or_else(|| anyhow!("eframe did not provide a native window"))?;
        let ppp = window.scale_factor() as f32;

        // The window is not visible yet; fixing the exact physical size now
        // avoids the first-frame `InnerSize` round trip and the compositor never
        // sees the placeholder size.
        let _ = window.request_inner_size(PhysicalSize::new(CANVAS_W, CANVAS_H));

        let requested = place_window(&window, &config.position());
        if requested.is_none() {
            log::warn!("no monitor reported; leaving the window position to the compositor");
        }
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

        let pools = AnimPools::from_config(&config);
        let clip_name = pools.pick_idle(None).ok_or_else(|| anyhow!("no idle animation available"))?;
        let player = Player::new(clip_path(&asset_dir, &clip_name));

        let last_pos = window.outer_position().unwrap_or_else(|_| requested.unwrap_or(PhysicalPosition::new(0, 0)));
        let now = Instant::now();
        log::info!("window at {},{} ({}x{} physical), scale {ppp}, idle clip {clip_name}", last_pos.x, last_pos.y, CANVAS_W, CANVAS_H);
        if options.stats && !cfg!(target_os = "linux") {
            log::warn!("--stats is only implemented on Linux");
        }

        Ok(Self {
            asset_dir,
            pools,
            throw_power: config.throw_power(),
            player,
            clip_name,
            mode: Mode::Idle,
            clip_done: false,
            failed_clips: 0,
            stalled: false,
            play_gen: 0,
            texture: None,
            texture_key: None,
            next_due: None,
            squash: None,
            pending_squash: false,
            press: None,
            saw_button_down: false,
            window,
            ppp,
            last_pos,
            target_pos: requested,
            on_top_until: now + Duration::from_secs(2),
            on_top_next: now,
            #[cfg(target_os = "linux")]
            x11,
            #[cfg(target_os = "windows")]
            passthrough: false,
            #[cfg(target_os = "windows")]
            pointer_down: false,
            meter: Meter::new(options.stats),
        })
    }

    /// Clip switch used for every transition.
    fn play(&mut self, name: String, mode: Mode) {
        self.play_gen += 1;
        self.player = Player::new(clip_path(&self.asset_dir, &name));
        self.clip_name = name;
        self.mode = mode;
        self.clip_done = false;
        self.next_due = None;
    }

    fn play_idle(&mut self) {
        match self.pools.pick_idle(Some(&self.clip_name)) {
            Some(name) => {
                log::debug!("-> idle {name}");
                self.play(name, Mode::Idle);
            }
            None => {
                log::error!("no idle animation available");
                self.stalled = true;
            }
        }
    }

    fn play_drag(&mut self) {
        self.squash = None;
        self.pending_squash = false;
        if let Some(name) = self.pools.pick_drag() {
            log::debug!("-> drag {name}");
            self.play(name, Mode::Drag);
        }
    }

    fn play_click(&mut self) {
        if let Some(name) = self.pools.pick_click() {
            log::debug!("-> click {name}");
            self.play(name, Mode::Click);
            self.squash = None;
            self.pending_squash = true;
        }
    }

    /// Decoder ticks, clip-end handling and the squash start.
    fn advance_clip(&mut self, ctx: &egui::Context, now: Instant) {
        let before = self.player.seq();
        self.next_due = self.player.advance(now);
        let advanced = self.player.seq() != before;
        if advanced && self.pending_squash {
            // The squash runs over the new clip's first frame, as upstream does
            // when the freshly loaded video becomes ready.
            self.pending_squash = false;
            self.squash = Some(now);
            ctx.request_repaint();
        }
        if self.player.ended() && !self.clip_done {
            self.on_clip_end();
        }
    }

    fn on_clip_end(&mut self) {
        self.clip_done = true;
        let produced = self.player.seq() > 0;
        if produced {
            self.failed_clips = 0;
        } else {
            self.failed_clips += 1;
            log::warn!("clip {} produced no frames", self.clip_name);
            if self.failed_clips >= 3 {
                log::error!("three clips in a row failed to decode; stopping playback");
                self.stalled = true;
                return;
            }
        }

        // The web client keeps showing the drag pose while the pointer is still
        // held, even if the (10 s) clip ended in the meantime.
        if self.mode == Mode::Drag && self.press.is_some_and(|press| press.dragging) {
            return;
        }
        self.play_idle();
    }

    /// Press, drag and release handling.
    fn update_pointer(&mut self, ctx: &egui::Context) {
        let (pressed, released, local) =
            ctx.input(|input| (input.pointer.primary_pressed(), input.pointer.primary_released(), input.pointer.interact_pos()));

        // See `fold_global_pointer`: on Windows the global read can supply both
        // the position and a press the window never saw.
        #[cfg(target_os = "windows")]
        let (pressed, local) = self.fold_global_pointer(pressed, local);

        if pressed
            && self.press.is_none()
            && let Some(pos) = local
        {
            let canvas = pos.to_vec2() * self.ppp;
            if HIT_BOX.contains(canvas.x, canvas.y) {
                self.press =
                    Some(Press { start: pos, off: canvas - Vec2::new(CANVAS_W as f32 / 2.0, CANVAS_H as f32 / 2.0), dragging: false, spring: None });
                self.saw_button_down = false;
            }
        }

        let Some(mut press) = self.press else {
            self.set_cursor(ctx, local);
            return;
        };

        let local = local.unwrap_or(press.start);
        if !press.dragging && (local - press.start).length() >= DRAG_THRESHOLD {
            press.dragging = true;
            // The user took over the placement; stop re-applying the startup one.
            self.target_pos = None;
            let window_pos = self.window.outer_position().unwrap_or(self.last_pos);
            press.spring = Some(Spring2::new(window_pos.x as f32, window_pos.y as f32));
            self.play_drag();
        }

        let (gx, gy, down) = self.pointer_global(local);
        if down == Some(true) {
            self.saw_button_down = true;
        }
        if press.dragging {
            self.step_drag(&mut press, gx, gy, ctx);
        }
        self.press = Some(press);

        let pointer_up = matches!(down, Some(false)) && self.saw_button_down;
        if released || pointer_up {
            let press = self.press.take().expect("press was present");
            self.saw_button_down = false;
            if press.dragging {
                log::debug!("drag released");
                self.play_idle();
            } else {
                self.play_click();
            }
        }

        self.set_cursor(ctx, Some(local));
    }

    fn set_cursor(&self, ctx: &egui::Context, local: Option<Pos2>) {
        if self.press.is_some_and(|press| press.dragging) {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if local.is_some_and(|pos| {
            let canvas = pos.to_vec2() * self.ppp;
            HIT_BOX.contains(canvas.x, canvas.y)
        }) {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
    }

    /// Integrates the drag spring and moves the native window with it.
    fn step_drag(&mut self, press: &mut Press, gx: f32, gy: f32, ctx: &egui::Context) {
        let Some(spring) = &mut press.spring else { return };
        let dt = ctx.input(|input| input.stable_dt).clamp(0.0, 1.0 / 30.0);
        let target = (gx - press.off.x - CANVAS_W as f32 / 2.0, gy - press.off.y - CANVAS_H as f32 / 2.0);
        spring.step(target, dt, self.throw_power);
        let x = spring.x.round() as i32;
        let y = spring.y.round() as i32;
        if x != self.last_pos.x || y != self.last_pos.y {
            self.window.set_outer_position(PhysicalPosition::new(x, y));
            self.last_pos = PhysicalPosition::new(x, y);
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
    fn pointer_global(&self, local: Pos2) -> (f32, f32, Option<bool>) {
        #[cfg(target_os = "linux")]
        if let Some(x11) = &self.x11 {
            match x11.pointer() {
                Ok((x, y, down)) => return (x as f32, y as f32, Some(down)),
                Err(err) => log::debug!("query_pointer failed: {err}"),
            }
        }
        #[cfg(target_os = "windows")]
        if let Some((x, y, down)) = pet_win32::pointer() {
            return (x as f32, y as f32, Some(down));
        }
        let origin = self.window.outer_position().unwrap_or(self.last_pos);
        let canvas = local.to_vec2() * self.ppp;
        (origin.x as f32 + canvas.x, origin.y as f32 + canvas.y, None)
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
        (pressed || (rising && self.press.is_none()), local)
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
        let passthrough = self.press.is_none() && !HIT_BOX.contains(canvas_x, canvas_y);
        if passthrough != self.passthrough {
            log::debug!("passthrough {passthrough}");
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(passthrough));
            self.passthrough = passthrough;
        }
    }

    /// Uploads the current frame when it changed and paints it at native size.
    fn paint(&mut self, ui: &mut egui::Ui, now: Instant) {
        let Some(frame) = self.player.current() else { return };
        let key = (self.play_gen, self.player.seq());
        if self.texture_key != Some(key) {
            let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.pixels);
            match &mut self.texture {
                Some(texture) if texture.size() == [frame.width, frame.height] => texture.set(image, TextureOptions::LINEAR),
                _ => self.texture = Some(ui.ctx().load_texture("pet-frame", image, TextureOptions::LINEAR)),
            }
            self.texture_key = Some(key);
        }
        let Some(texture) = &self.texture else { return };

        let scale = match self.squash {
            Some(start) => squash_scale((now - start).as_secs_f32() / SQ_DURATION_SECS, SQ_SQUASH),
            None => 1.0,
        };
        let ppp = self.ppp;
        let width = CANVAS_W as f32 / ppp;
        let height = CANVAS_H as f32 * scale / ppp;
        // Anchored at the bottom edge, like `transform-origin: bottom` upstream.
        let rect = Rect::from_min_size(Pos2::new(0.0, CANVAS_H as f32 / ppp - height), Vec2::new(width, height));
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
    }

    fn schedule_repaint(&mut self, ctx: &egui::Context, now: Instant) {
        if let Some(start) = self.squash {
            let elapsed = (now - start).as_secs_f32();
            if elapsed >= SQ_DURATION_SECS {
                self.squash = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(8));
            }
        }
        // Windows has to poll the global cursor to decide whether the window is
        // click-through, and a click-through window receives no events that could
        // wake the loop, so the poll has to be timed rather than event-driven.
        #[cfg(target_os = "windows")]
        ctx.request_repaint_after(Duration::from_millis(16));
        if self.press.is_some_and(|press| press.dragging) {
            ctx.request_repaint();
            return;
        }
        if self.press.is_some() {
            // Poll the button state so a release outside the window is still
            // noticed even when no more window events arrive.
            ctx.request_repaint_after(Duration::from_millis(8));
            return;
        }
        if let Some(due) = self.next_due {
            ctx.request_repaint_after(due);
        } else if !self.stalled && !self.player.ended() {
            ctx.request_repaint_after(Duration::from_millis(8));
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

        if ctx.input(|input| input.key_pressed(egui::Key::Q) && input.modifiers.command) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

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

        self.update_pointer(&ctx);
        #[cfg(target_os = "windows")]
        self.update_passthrough(&ctx);
        self.advance_clip(&ctx, now);
        self.paint(ui, now);
        self.schedule_repaint(&ctx, now);
        self.meter.tick(self.texture.as_ref().map_or(0, |texture| texture.byte_size()));
    }
}

/// Places the window using the configured corner and margins of the primary
/// monitor. Margins are in logical pixels (as in the web client) and monitor
/// coordinates are physical.
fn place_window(window: &Window, position: &pet_config::Position) -> Option<PhysicalPosition<i32>> {
    let monitor = window.primary_monitor().or_else(|| window.current_monitor())?;
    let ppp = window.scale_factor();
    let origin = monitor.position();
    let size = monitor.size();
    let margin_x = (f64::from(position.margin_x) * ppp).round() as i32;
    let margin_y = (f64::from(position.margin_y) * ppp).round() as i32;
    let width = CANVAS_W as i32;
    let height = CANVAS_H as i32;
    let right = origin.x + size.width as i32 - width - margin_x;
    let bottom = origin.y + size.height as i32 - height - margin_y;
    let (x, y) = match position.corner.as_str() {
        "top-left" => (origin.x + margin_x, origin.y + margin_y),
        "bottom-left" => (origin.x + margin_x, bottom),
        "bottom-right" => (right, bottom),
        "top-right" => (right, origin.y + margin_y),
        other => {
            log::warn!("unknown corner {other:?}; using top-right");
            (right, origin.y + margin_y)
        }
    };
    window.set_outer_position(PhysicalPosition::new(x, y));
    Some(PhysicalPosition::new(x, y))
}

// ---------------------------------------------------------------------------
// Optional runtime meter (Linux only).

#[cfg(target_os = "linux")]
struct Meter {
    enabled: bool,
    start: Instant,
    last: Instant,
    frames: u64,
    frames_last: u64,
    last_cpu: (u64, u64),
}

#[cfg(target_os = "linux")]
impl Meter {
    fn new(enabled: bool) -> Self {
        let now = Instant::now();
        Self { enabled, start: now, last: now, frames: 0, frames_last: 0, last_cpu: cpu_ticks() }
    }

    fn tick(&mut self, texture_bytes: usize) {
        if !self.enabled {
            return;
        }
        self.frames += 1;
        let now = Instant::now();
        let elapsed = now.duration_since(self.last);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let fps = (self.frames - self.frames_last) as f64 / elapsed.as_secs_f64();
        self.frames_last = self.frames;
        self.last = now;
        let ticks = cpu_ticks();
        let used = (ticks.0 - self.last_cpu.0) + (ticks.1 - self.last_cpu.1);
        self.last_cpu = ticks;
        let cpu = used as f64 / 100.0 / elapsed.as_secs_f64() * 100.0;
        let (rss, hwm) = rss_kb();
        log::info!(
            target: "whale-pet::stats",
            "t={:.0}s fps={fps:.1} cpu={cpu:.1}% rss={:.1}MB hwm={:.1}MB tex={:.2}MB",
            now.duration_since(self.start).as_secs_f64(),
            rss as f64 / 1024.0,
            hwm as f64 / 1024.0,
            texture_bytes as f64 / (1024.0 * 1024.0),
        );
    }
}

#[cfg(target_os = "linux")]
fn cpu_ticks() -> (u64, u64) {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else { return (0, 0) };
    let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest.trim()) else { return (0, 0) };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let user = fields.get(11).and_then(|value| value.parse().ok()).unwrap_or(0);
    let system = fields.get(12).and_then(|value| value.parse().ok()).unwrap_or(0);
    (user, system)
}

#[cfg(target_os = "linux")]
fn rss_kb() -> (u64, u64) {
    let mut rss = 0;
    let mut hwm = 0;
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("VmRSS:") {
                rss = value.split_whitespace().next().and_then(|value| value.parse().ok()).unwrap_or(0);
            } else if let Some(value) = line.strip_prefix("VmHWM:") {
                hwm = value.split_whitespace().next().and_then(|value| value.parse().ok()).unwrap_or(0);
            }
        }
    }
    (rss, hwm)
}

#[cfg(not(target_os = "linux"))]
struct Meter;

#[cfg(not(target_os = "linux"))]
impl Meter {
    fn new(_enabled: bool) -> Self {
        Self
    }

    fn tick(&mut self, _texture_bytes: usize) {}
}
