//! Platform-independent pet behaviour: clip playback, pointer interaction,
//! dragging, the Q-squash and the repaint schedule.
//!
//! The host owns the window and feeds one [`FrameInput`] per frame:
//!
//! - the eframe/winit host (`pet-ui-egui`) reads egui's own input and the
//!   platform's global pointer;
//! - the native Wayland host (`pet-wayland`) translates `wl_pointer` and
//!   `wl_keyboard` events into the same shape.
//!
//! Everything the pet wants done to the platform comes back in [`FrameOutput`]:
//! move the window, set the cursor, close. Repaints are still requested through
//! `egui::Context`, so a host that consumes egui's `repaint_delay` (or eframe,
//! which does) needs no extra plumbing.
//!
//! Coordinates follow one rule used by every host: [`FrameInput::pointer`] is in
//! window-local points, [`FrameInput::ppp`] converts points to canvas pixels, and
//! everything else ([`FrameInput::global`], [`FrameInput::window_origin`],
//! [`FrameOutput::move_to`]) is in canvas (= physical) pixels.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use egui::{Color32, Pos2, Rect, TextureOptions, Vec2};
use pet_config::{CANVAS_H, CANVAS_W, DRAG_THRESHOLD, HIT_BOX, PetConfig, SQ_DURATION_SECS, SQ_SQUASH, clip_path};
use pet_decode::Player;
use pet_model::{AnimPools, Spring2, squash_scale};

/// One frame of host-provided state.
#[derive(Debug, Clone, Copy)]
pub struct FrameInput {
    /// Frame timestamp; also the playback clock.
    pub now: Instant,
    /// Seconds since the previous frame (`stable_dt`).
    pub dt: f32,
    /// Window points to canvas pixels (`window.scale_factor()` under
    /// eframe/winit, the fractional output scale on Wayland).
    pub ppp: f32,
    /// Pointer position in window-local points, if known.
    pub pointer: Option<Pos2>,
    /// Left button went down this frame.
    pub pressed: bool,
    /// Left button came up this frame.
    pub released: bool,
    /// Global pointer in canvas pixels, when the platform can read it outside
    /// the window (X11, Windows, Wayland via output geometry).
    pub global: Option<(f32, f32)>,
    /// Global left button state, when known.
    pub button_down: Option<bool>,
    /// Window top-left in canvas pixels (physical screen coordinates).
    pub window_origin: (i32, i32),
}

/// What the host should do after the frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameOutput {
    /// New window top-left in physical screen coordinates.
    pub move_to: Option<(i32, i32)>,
    /// Cursor icon to show, if the pet wants a specific one.
    pub cursor: Option<egui::CursorIcon>,
    /// `Ctrl+Q` was pressed.
    pub close: bool,
    /// The press just became a drag; the host should stop re-applying the
    /// startup placement.
    pub drag_started: bool,
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

/// Animation, interaction and repaint state, shared by every host.
pub struct PetCore {
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
    /// When the current clip was first seen without a frame while a previous
    /// texture was on screen: the pet keeps showing that texture until the new
    /// clip's first frame arrives (logged when it does).
    hold_since: Option<Instant>,
    next_due: Option<Duration>,
    /// Start of the Q-squash; `None` when no squash is running.
    squash: Option<Instant>,
    /// A click was played and the squash starts with its first frame.
    pending_squash: bool,
    press: Option<Press>,
    /// The pointer has been seen with the left button down at least once.
    saw_button_down: bool,
    /// Last window position handed out for dragging.
    drag_pos: Option<(i32, i32)>,
    meter: Meter,
}

impl PetCore {
    pub fn new(asset_dir: PathBuf, config: &PetConfig, stats: bool) -> Result<Self> {
        let pools = AnimPools::from_config(config);
        let clip_name = pools.pick_idle(None).ok_or_else(|| anyhow!("no idle animation available"))?;
        let player = Player::new(clip_path(&asset_dir, &clip_name));

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
            hold_since: None,
            next_due: None,
            squash: None,
            pending_squash: false,
            press: None,
            saw_button_down: false,
            drag_pos: None,
            meter: Meter::new(stats),
        })
    }

    /// Advances the decoder and paints the current frame. Must be called once
    /// per frame from inside the host's egui pass.
    pub fn ui(&mut self, ui: &mut egui::Ui, input: FrameInput) -> FrameOutput {
        let mut out = FrameOutput::default();

        if ui.ctx().input(|state| state.key_pressed(egui::Key::Q) && state.modifiers.command) {
            out.close = true;
            return out;
        }

        self.update_pointer(&mut out, input);
        self.advance_clip(ui.ctx(), input.now);
        self.paint(ui, input.now, input.ppp);
        self.schedule_repaint(ui.ctx(), input.now);
        self.meter.tick(self.texture.as_ref().map_or(0, |texture| texture.byte_size()));
        out
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
    fn update_pointer(&mut self, out: &mut FrameOutput, input: FrameInput) {
        let ppp = input.ppp;
        let (pressed, released, local) = (input.pressed, input.released, input.pointer);

        if pressed
            && self.press.is_none()
            && let Some(pos) = local
        {
            let canvas = pos.to_vec2() * ppp;
            if HIT_BOX.contains(canvas.x, canvas.y) {
                self.press =
                    Some(Press { start: pos, off: canvas - Vec2::new(CANVAS_W as f32 / 2.0, CANVAS_H as f32 / 2.0), dragging: false, spring: None });
                self.saw_button_down = false;
            }
        }

        let Some(mut press) = self.press else {
            out.cursor = self.cursor(local, ppp);
            return;
        };

        let local = local.unwrap_or(press.start);
        if !press.dragging && (local - press.start).length() >= DRAG_THRESHOLD {
            press.dragging = true;
            // The user took over the placement; stop re-applying the startup one.
            out.drag_started = true;
            self.drag_pos = Some(input.window_origin);
            press.spring = Some(Spring2::new(input.window_origin.0 as f32, input.window_origin.1 as f32));
            self.play_drag();
        }

        let (gx, gy) = self.pointer_global(local, input);
        if input.button_down == Some(true) {
            self.saw_button_down = true;
        }
        if press.dragging {
            self.step_drag(&mut press, gx, gy, input.dt, out);
        }
        self.press = Some(press);

        let pointer_up = matches!(input.button_down, Some(false)) && self.saw_button_down;
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

        out.cursor = self.cursor(Some(local), ppp);
    }

    fn cursor(&self, local: Option<Pos2>, ppp: f32) -> Option<egui::CursorIcon> {
        if self.press.is_some_and(|press| press.dragging) {
            Some(egui::CursorIcon::Grabbing)
        } else if local.is_some_and(|pos| {
            let canvas = pos.to_vec2() * ppp;
            HIT_BOX.contains(canvas.x, canvas.y)
        }) {
            Some(egui::CursorIcon::Grab)
        } else {
            None
        }
    }

    /// Integrates the drag spring and reports the new window position.
    fn step_drag(&mut self, press: &mut Press, gx: f32, gy: f32, dt: f32, out: &mut FrameOutput) {
        let Some(spring) = &mut press.spring else { return };
        let dt = dt.clamp(0.0, 1.0 / 30.0);
        let target = (gx - press.off.x - CANVAS_W as f32 / 2.0, gy - press.off.y - CANVAS_H as f32 / 2.0);
        spring.step(target, dt, self.throw_power);
        let pos = (spring.x.round() as i32, spring.y.round() as i32);
        if self.drag_pos != Some(pos) {
            out.move_to = Some(pos);
            self.drag_pos = Some(pos);
        }
    }

    /// Global pointer position in canvas pixels.
    ///
    /// Hosts read it outside the window where possible (X11 `query_pointer`,
    /// Win32 `GetCursorPos`, Wayland output geometry). The window-local fallback
    /// is only as good as the last pointer event, which is enough when the host
    /// keeps receiving events during a drag.
    fn pointer_global(&self, local: Pos2, input: FrameInput) -> (f32, f32) {
        if let Some((x, y)) = input.global {
            return (x, y);
        }
        let canvas = local.to_vec2() * input.ppp;
        (input.window_origin.0 as f32 + canvas.x, input.window_origin.1 as f32 + canvas.y)
    }

    /// Uploads the current frame when it changed and paints it at native size.
    fn paint(&mut self, ui: &mut egui::Ui, now: Instant, ppp: f32) {
        // A fresh clip has no frame until its decoder thread has opened the file
        // and decoded the first one. Hosts clear their surface every frame, so
        // returning without painting would blank the pet for that gap on every
        // clip switch; the previous texture stays on screen instead, the same
        // way a clip that ended keeps showing its last frame.
        if let Some(frame) = self.player.current() {
            if let Some(since) = self.hold_since.take() {
                log::debug!("{} produced its first frame after {:.0} ms", self.clip_name, (now - since).as_secs_f32() * 1000.0);
            }
            let key = (self.play_gen, self.player.seq());
            if self.texture_key != Some(key) {
                let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.pixels);
                match &mut self.texture {
                    Some(texture) if texture.size() == [frame.width, frame.height] => texture.set(image, TextureOptions::LINEAR),
                    _ => self.texture = Some(ui.ctx().load_texture("pet-frame", image, TextureOptions::LINEAR)),
                }
                self.texture_key = Some(key);
            }
        } else if self.texture.is_some() {
            self.hold_since.get_or_insert(now);
        }
        let Some(texture) = &self.texture else { return };

        let scale = match self.squash {
            Some(start) => squash_scale((now - start).as_secs_f32() / SQ_DURATION_SECS, SQ_SQUASH),
            None => 1.0,
        };
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

    /// Whether the pet currently owns the pointer press (used by a host that has
    /// to keep its window interactive while a drag is in progress).
    pub fn has_press(&self) -> bool {
        self.press.is_some()
    }

    /// Name of the clip playing right now.
    pub fn clip_name(&self) -> &str {
        &self.clip_name
    }

    /// Placeholder size in window points for a canvas of `ppp` scaling.
    pub fn logical_size(ppp: f32) -> (f32, f32) {
        (CANVAS_W as f32 / ppp, CANVAS_H as f32 / ppp)
    }
}

/// Top-left of a `CANVAS_W * CANVAS_H` window placed at `corner` with the given
/// margins. `origin`/`size` are the monitor rectangle in physical pixels and
/// `ppp` converts the logical margins.
pub fn corner_position(origin: (i32, i32), size: (u32, u32), position: &pet_config::Position, ppp: f64) -> (i32, i32) {
    let margin_x = (f64::from(position.margin_x) * ppp).round() as i32;
    let margin_y = (f64::from(position.margin_y) * ppp).round() as i32;
    let width = CANVAS_W as i32;
    let height = CANVAS_H as i32;
    let right = origin.0 + size.0 as i32 - width - margin_x;
    let bottom = origin.1 + size.1 as i32 - height - margin_y;
    match position.corner.as_str() {
        "top-left" => (origin.0 + margin_x, origin.1 + margin_y),
        "bottom-left" => (origin.0 + margin_x, bottom),
        "bottom-right" => (right, bottom),
        "top-right" => (right, origin.1 + margin_y),
        other => {
            log::warn!("unknown corner {other:?}; using top-right");
            (right, origin.1 + margin_y)
        }
    }
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
