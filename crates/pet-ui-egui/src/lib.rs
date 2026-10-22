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
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui;
#[cfg(target_os = "windows")]
use egui::{Pos2, Vec2};
#[cfg(target_os = "windows")]
use pet_config::HIT_BOX;
use pet_config::{CANVAS_H, CANVAS_W, PetConfig};
use pet_ui_core::{FrameInput, PetCore, corner_position};
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

struct PetApp {
    core: PetCore,
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

        let core = PetCore::new(asset_dir, &config, options.stats)?;

        let last_pos = window.outer_position().unwrap_or_else(|_| requested.unwrap_or(PhysicalPosition::new(0, 0)));
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
            target_pos: requested,
            on_top_until: now + Duration::from_secs(2),
            on_top_next: now,
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
        let (pressed, released, local, dt) =
            ctx.input(|input| (input.pointer.primary_pressed(), input.pointer.primary_released(), input.pointer.interact_pos(), input.stable_dt));

        // See `fold_global_pointer`: on Windows the global read can supply both
        // the position and a press the window never saw.
        #[cfg(target_os = "windows")]
        let (pressed, local) = self.fold_global_pointer(pressed, local);

        let (global, button_down) = self.global_pointer();
        let origin = self.window.outer_position().unwrap_or(self.last_pos);
        FrameInput { now, dt, ppp: self.ppp, pointer: local, pressed, released, global, button_down, window_origin: (origin.x, origin.y) }
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
        if out.close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            #[cfg(target_os = "windows")]
            self.update_passthrough(&ctx);
        }
    }
}

/// Places the window using the configured corner and margins of the primary
/// monitor. Margins are in logical pixels (as in the web client) and monitor
/// coordinates are physical.
fn place_window(window: &Window, position: &pet_config::Position) -> Option<PhysicalPosition<i32>> {
    let monitor = window.primary_monitor().or_else(|| window.current_monitor())?;
    let origin = monitor.position();
    let size = monitor.size();
    let (x, y) = corner_position((origin.x, origin.y), (size.width, size.height), position, window.scale_factor());
    window.set_outer_position(PhysicalPosition::new(x, y));
    Some(PhysicalPosition::new(x, y))
}
