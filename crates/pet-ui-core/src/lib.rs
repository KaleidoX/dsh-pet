//! Platform-independent pet behaviour: clip playback, the random chain, roaming,
//! dragging, throwing, click scoring and the repaint schedule.
//!
//! The host owns the window and feeds one [`FrameInput`] per frame:
//!
//! - the eframe/winit host (`pet-ui-egui`) reads egui's own input and the
//!   platform's global pointer;
//! - the native Wayland host (`pet-wayland`) translates `wl_pointer` and
//!   `wl_keyboard` events into the same shape.
//!
//! Everything the pet wants done to the platform comes back in [`FrameOutput`]:
//! move the window, set the cursor, close, and what the interaction overlay
//! should show. Repaints are still requested through `egui::Context`, so a host
//! that consumes egui's `repaint_delay` (or eframe, which does) needs no extra
//! plumbing.
//!
//! # Coordinates
//!
//! The world is the monitor work area in physical pixels; the pet's bounding box
//! is the 640x360 canvas, whose top-left corner is the pet window's origin.
//! [`FrameInput::pointer`] is in window-local points, [`FrameInput::ppp`]
//! converts points to canvas pixels, and everything else
//! ([`FrameInput::global`], [`FrameInput::window_origin`],
//! [`FrameOutput::move_to`], [`FrameInput::world`]) is in canvas (= physical)
//! pixels.
//!
//! # Two engines
//!
//! The animation chain and the position engine run independently, as they do
//! upstream: a thrown pet keeps playing (and finishing) its clip while gravity
//! moves the window. [`Mode`] only describes what the current clip means.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use egui::{Color32, Pos2, Rect, TextureOptions, Vec2};
use pet_config::{
    BOTTOM_PAD, CANVAS_H, CANVAS_W, CROSSFADE_SECS, DRAG_THRESHOLD, HIT_BOX, PetConfig, Position, SQ_DURATION_SECS, SQ_SQUASH, WORLD_SCALE,
    WorldArea, clip_path,
};
use pet_decode::Player;
use pet_model::{
    AnimPools, DragSample, Facing, MovePlan, RollKind, RollWeights, SCORE_MIN_SPEED, Spring2, ThrowParams, ThrowState, click_score,
    estimate_release_velocity, is_no_mirror, landing_squash, move_x, pick_category_action, plan_move, roll_kind, squash_scale, throw_bounds,
    throw_step, trim_trail,
};

/// Clip duration used when the container does not declare one (`10.09` upstream).
const FALLBACK_CLIP_SECS: f32 = 10.09;

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
    /// Right button went down this frame.
    pub secondary_pressed: bool,
    /// Global pointer in canvas pixels, when the platform can read it outside
    /// the window (X11, Windows, Wayland via output geometry).
    pub global: Option<(f32, f32)>,
    /// Global left button state, when known.
    pub button_down: Option<bool>,
    /// Window top-left in canvas pixels (physical screen coordinates).
    pub window_origin: (i32, i32),
    /// Monitor work area in physical pixels: the space the pet may roam in.
    pub world: WorldArea,
}

/// What the interaction overlay should show this frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum OverlayIntent {
    /// Nothing: the overlay window may be hidden.
    #[default]
    Hidden,
    /// Hover tooltip anchored at a screen position in physical pixels.
    Tooltip { anchor: (f32, f32), text: String },
    /// The context menu is open, anchored at a screen position.
    Menu { anchor: (f32, f32) },
}

/// A click that scored: the overlay spawns the particle burst and card.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreEvent {
    /// Screen position in physical pixels.
    pub anchor: (f32, f32),
    pub points: u32,
    /// Speed in the 462-wide reference space, as displayed.
    pub speed: u32,
    /// Pet size in the reference space, as displayed.
    pub size: u32,
}

/// What the host should do after the frame.
#[derive(Debug, Clone, Default)]
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
    /// Overlay state to apply.
    pub overlay: OverlayIntent,
    /// A scored click happened this frame.
    pub score: Option<ScoreEvent>,
}

/// A menu action chosen in the overlay, handed back to the pet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayAction {
    /// Play one clip once and return to the random chain.
    PlayOnce(String),
    /// Walk with the named move animation (falls back to plain playback when
    /// the walk cannot start).
    MoveTo(String),
    /// Drop any thrown/roaming state and return to the configured corner.
    Home,
    /// The menu was dismissed (click outside, Esc, or after an action).
    MenuClosed,
}

/// What the currently playing clip represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    Turn,
    Move,
    Action,
    Click,
    Drag,
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
    /// Last global pointer sample pushed into the trail. Sampling only on a
    /// real move keeps the release window meaningful: samples that merely
    /// repeat the previous position would defeat the "paused before release"
    /// check in `estimate_release_velocity`.
    last_sample: Option<(f32, f32)>,
}

/// Result of asking the pet to walk.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TryMove {
    /// A walk started; the clip name.
    Started(String),
    /// A walk or throw already owns the position.
    Busy,
    /// No move action applies (empty pool or destination out of the work area).
    Unavailable,
}

/// Animation, interaction and repaint state, shared by every host.
pub struct PetCore {
    asset_dir: PathBuf,
    pools: AnimPools,
    physics: ThrowParams,
    weights: RollWeights,
    fixed: bool,
    name: String,
    position: Position,
    ppp: f32,
    world: WorldArea,
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
    /// Previous clip's last frame, cross-faded out over `CROSSFADE_SECS`.
    prev_texture: Option<egui::TextureHandle>,
    fade_at: Option<Instant>,
    texture_key: Option<(u64, u64)>,
    /// When the current clip was first seen without a frame while a previous
    /// texture was on screen: the pet keeps showing that texture until the new
    /// clip's first frame arrives (logged when it does).
    hold_since: Option<Instant>,
    /// When the current clip produced its first frame; walk timing starts here.
    clip_started: Option<Instant>,
    next_due: Option<Duration>,
    /// Start and depth of the Q-squash; `None` when no squash is running.
    squash: Option<(Instant, f32)>,
    /// A click was played and the squash starts with its first frame.
    pending_squash: bool,
    press: Option<Press>,
    /// The press scored on the flying pet; the release must not play a click.
    press_score_fired: bool,
    /// The pointer has been seen with the left button down at least once.
    saw_button_down: bool,
    facing: Facing,
    /// Window top-left in canvas pixels; the single source of truth for motion.
    pos: (f32, f32),
    /// Pet centre as a fraction of the work area (upstream `customPos`).
    custom_pos: Option<(f32, f32)>,
    /// The first frame establishes the window position.
    placed: bool,
    /// Last window position handed to the host.
    last_reported: Option<(i32, i32)>,
    move_plan: Option<(MovePlan, String)>,
    throw: Option<ThrowState>,
    /// Whether the previous throw step touched the ground.
    was_grounded: bool,
    trail: Vec<DragSample>,
    /// Clock origin for pointer samples and the release window.
    epoch: Instant,
    menu_open: bool,
    menu_anchor: (f32, f32),
    meter: Meter,
}

impl PetCore {
    pub fn new(asset_dir: PathBuf, config: &PetConfig, stats: bool) -> Result<Self> {
        let pools = AnimPools::from_config(config);
        let clip_name = pools.first_idle().ok_or_else(|| anyhow!("no idle animation available"))?;
        let player = Player::new(clip_path(&asset_dir, &clip_name));
        let now = Instant::now();

        Ok(Self {
            asset_dir,
            pools,
            physics: ThrowParams::from_config(&config.physics()),
            weights: RollWeights::from_config(config),
            fixed: config.fixed(),
            name: config.pet_name(),
            position: config.position(),
            ppp: 1.0,
            world: WorldArea { x: 0, y: 0, width: CANVAS_W, height: CANVAS_H },
            player,
            clip_name,
            mode: Mode::Idle,
            clip_done: false,
            failed_clips: 0,
            stalled: false,
            play_gen: 0,
            texture: None,
            prev_texture: None,
            fade_at: None,
            texture_key: None,
            hold_since: None,
            clip_started: None,
            next_due: None,
            squash: None,
            pending_squash: false,
            press: None,
            press_score_fired: false,
            saw_button_down: false,
            facing: Facing::Left,
            pos: (0.0, 0.0),
            custom_pos: None,
            placed: false,
            last_reported: None,
            move_plan: None,
            throw: None,
            was_grounded: false,
            trail: Vec::new(),
            epoch: now,
            menu_open: false,
            menu_anchor: (0.0, 0.0),
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

        self.ppp = input.ppp.max(0.01);
        if input.world.width > 0 && input.world.height > 0 {
            self.world = input.world;
        }
        if !self.placed {
            self.pos = anchor_position(self.world, &self.position, self.ppp);
            self.placed = true;
            let pos = (self.pos.0.round() as i32, self.pos.1.round() as i32);
            out.move_to = Some(pos);
            self.last_reported = Some(pos);
        }

        self.update_pointer(&mut out, input);
        self.advance_clip(ui.ctx(), input.now);
        self.step_motion(&mut out, input.now);
        self.paint(ui, input.now, input.ppp);
        self.schedule_repaint(ui.ctx(), input.now);
        self.emit_overlay(&mut out, input);
        self.meter.tick(self.texture.as_ref().map_or(0, |texture| texture.byte_size()));
        out
    }

    // -----------------------------------------------------------------------
    // Playback.

    /// Clip switch used for every transition. The previous texture is kept for
    /// the cross-fade, exactly like the two stacked `<video>` elements upstream.
    fn play(&mut self, name: String, mode: Mode, now: Instant) {
        self.play_gen += 1;
        self.prev_texture = self.texture.take();
        self.fade_at = Some(now);
        self.player = Player::new(clip_path(&self.asset_dir, &name));
        self.clip_name = name;
        self.mode = mode;
        self.clip_done = false;
        self.clip_started = None;
        self.next_due = None;
        if mode != Mode::Click && mode != Mode::Drag {
            self.squash = None;
            self.pending_squash = false;
        }
    }

    fn play_once(&mut self, name: String, mode: Mode, now: Instant) {
        log::debug!("-> {mode:?} {name}");
        self.play(name, mode, now);
    }

    /// One idle clip (upstream `playOnce(pick(idle, anim))`), after which the
    /// clip-end handler moves on to the random chain.
    fn play_idle_once(&mut self, now: Instant) {
        match self.pools.pick_idle(Some(&self.clip_name)) {
            Some(name) => self.play_once(name, Mode::Idle, now),
            None => self.pick_next(now),
        }
    }

    /// The random chain (`playIdle` upstream).
    fn pick_next(&mut self, now: Instant) {
        self.stop_move();
        let roll = pet_model::random_unit();
        let current = self.clip_name.clone();
        let kind = roll_kind(roll, self.weights, self.fixed);
        log::debug!("roll {roll:.3} -> {kind:?}");
        match kind {
            RollKind::Idle => match self.pools.pick_idle(Some(&current)) {
                Some(name) => self.play_once(name, Mode::Idle, now),
                None => self.stall(),
            },
            RollKind::Turn => {
                // A missing turn pool degrades to idle, which can never stall on
                // an empty config (the idle pool has built-in defaults).
                let name = self.pools.pick_turn(Some(&current)).or_else(|| self.pools.pick_idle(Some(&current)));
                match name {
                    Some(name) => self.play_once(name, Mode::Turn, now),
                    None => self.stall(),
                }
            }
            RollKind::Move => match self.try_move(None, now) {
                TryMove::Started(_) => {}
                // A walk or throw already owns the position: replay the clip
                // already on screen, never load a nonexistent one.
                TryMove::Busy => {
                    let replay = self.clip_name.clone();
                    let mode = self.mode;
                    self.play_once(replay, mode, now);
                }
                TryMove::Unavailable => match pick_category_action(self.pools.categories(), self.pools.idle_pool(), self.facing, Some(&current)) {
                    Some((_, name)) => self.play_once(name, Mode::Action, now),
                    None => self.stall(),
                },
            },
            RollKind::Action => match pick_category_action(self.pools.categories(), self.pools.idle_pool(), self.facing, Some(&current)) {
                Some((_, name)) => self.play_once(name, Mode::Action, now),
                None => self.stall(),
            },
        }
    }

    fn stall(&mut self) {
        log::error!("no animation available for the random chain");
        self.stalled = true;
    }

    fn play_drag(&mut self, now: Instant) {
        if let Some(name) = self.pools.pick_drag() {
            self.play_once(name, Mode::Drag, now);
        }
    }

    fn play_click(&mut self, now: Instant) {
        if let Some(name) = self.pools.pick_click() {
            self.play_once(name, Mode::Click, now);
            self.squash = None;
            self.pending_squash = true;
        }
    }

    /// Starts a walk towards a random or named move animation.
    fn try_move(&mut self, preferred: Option<&str>, now: Instant) -> TryMove {
        if self.move_plan.is_some() || self.throw.is_some() {
            return TryMove::Busy;
        }
        let Some(action) = self.pools.pick_move(preferred) else { return TryMove::Unavailable };
        let name = action.name.clone();
        let params = self.pools.move_params(action).scaled(WORLD_SCALE);
        let cx = self.pos.0 + CANVAS_W as f32 / 2.0;
        let cy = self.pos.1 + CANVAS_H as f32 / 2.0;
        let Some(plan) = plan_move(cx, cy, self.facing, params, self.world) else { return TryMove::Unavailable };
        self.move_plan = Some((plan, name.clone()));
        self.play_once(name.clone(), Mode::Move, now);
        TryMove::Started(name)
    }

    fn stop_move(&mut self) {
        self.move_plan = None;
    }

    /// Freezes a throw where it is. The position stays authoritative by being
    /// stored as the session position, so the pet does not snap back.
    fn stop_throw(&mut self) {
        if self.throw.take().is_some() {
            self.was_grounded = false;
            self.commit_center();
        }
    }

    /// Stores the current window position as the session position, so it
    /// survives the corner anchor being recomputed.
    fn commit_center(&mut self) {
        let width = self.world.width.max(1) as f32;
        let height = self.world.height.max(1) as f32;
        let cx = self.pos.0 + CANVAS_W as f32 / 2.0;
        let cy = self.pos.1 + CANVAS_H as f32 / 2.0;
        self.custom_pos = Some(((cx - self.world.x as f32) / width, (cy - self.world.y as f32) / height));
    }

    // -----------------------------------------------------------------------
    // Decoder and clip-end dispatch.

    /// Decoder ticks, clip-end handling and squash starts.
    fn advance_clip(&mut self, ctx: &egui::Context, now: Instant) {
        let before = self.player.seq();
        self.next_due = self.player.advance(now);
        let advanced = self.player.seq() != before;
        if advanced {
            if self.clip_started.is_none() {
                self.clip_started = Some(now);
            }
            if self.pending_squash {
                // The squash runs over the new clip's first frame, as upstream
                // does when the freshly loaded video becomes ready.
                self.pending_squash = false;
                self.squash = Some((now, SQ_SQUASH));
                ctx.request_repaint();
            }
        }
        if self.player.ended() && !self.clip_done {
            self.on_clip_end(now);
        }
    }

    fn on_clip_end(&mut self, now: Instant) {
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
        // A turn flips the facing before the chain picks the next clip, so the
        // `noMirror` filter already sees the new direction.
        if self.mode == Mode::Turn {
            self.facing = self.facing.flipped();
            log::debug!("turn ended; facing {:?}", self.facing);
        }
        // Interaction clips go back to idle without entering the chain first.
        if matches!(self.mode, Mode::Click | Mode::Drag) {
            self.play_idle_once(now);
            return;
        }
        self.pick_next(now);
    }

    // -----------------------------------------------------------------------
    // Pointer.

    fn update_pointer(&mut self, out: &mut FrameOutput, input: FrameInput) {
        let ppp = input.ppp;
        let (pressed, released, local) = (input.pressed, input.released, input.pointer);

        if input.secondary_pressed
            && let Some(pos) = local
        {
            let canvas = pos.to_vec2() * ppp;
            // Right-clicking the body opens the menu; a drag in progress owns
            // the pointer and keeps its pose.
            if HIT_BOX.contains(canvas.x, canvas.y) && !self.press.is_some_and(|press| press.dragging) {
                self.stop_throw();
                self.stop_move();
                self.menu_open = true;
                self.menu_anchor = self.pointer_global(pos, input);
                log::debug!("context menu at {:?}", self.menu_anchor);
            }
        }

        if pressed
            && self.press.is_none()
            && !self.menu_open
            && let Some(pos) = local
        {
            let canvas = pos.to_vec2() * ppp;
            if HIT_BOX.contains(canvas.x, canvas.y) {
                // A click on the flying pet scores on press, reading the speed
                // before the throw is stopped.
                if let Some(state) = self.throw {
                    let speed = state.vx.hypot(state.vy);
                    if speed >= SCORE_MIN_SPEED {
                        let points = click_score(speed);
                        out.score = Some(ScoreEvent {
                            anchor: self.pointer_global(pos, input),
                            points,
                            speed: (speed / WORLD_SCALE).round() as u32,
                            size: pet_config::PET_REF_WIDTH as u32,
                        });
                        self.press_score_fired = true;
                        log::debug!("click score +{points} at {speed:.0} px/s");
                    }
                }
                self.stop_throw();
                self.stop_move();
                self.trail.clear();
                self.press = Some(Press {
                    start: pos,
                    off: canvas - Vec2::new(CANVAS_W as f32 / 2.0, CANVAS_H as f32 / 2.0),
                    dragging: false,
                    spring: None,
                    last_sample: None,
                });
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
            // The pet is lifted by the stage pad while held; the spring starts
            // from the current window position.
            press.spring = Some(Spring2::new(self.pos.0, self.pos.1));
            self.play_drag(input.now);
        }

        let (gx, gy) = self.pointer_global(local, input);
        if input.button_down == Some(true) {
            self.saw_button_down = true;
        }
        if press.dragging {
            self.sample_trail(&mut press, gx, gy, input.now);
            self.step_drag(&mut press, gx, gy, input.dt, out);
        }
        self.press = Some(press);

        let pointer_up = matches!(input.button_down, Some(false)) && self.saw_button_down;
        if released || pointer_up {
            let press = self.press.take().expect("press was present");
            self.saw_button_down = false;
            if press.dragging {
                self.release_drag(input, out);
            } else if self.press_score_fired {
                // The score burst was the feedback; no click clip.
                self.press_score_fired = false;
                self.stop_throw();
                self.stop_move();
            } else {
                self.play_click(input.now);
            }
        }

        out.cursor = self.cursor(Some(local), ppp);
    }

    /// Records one trail sample, but only when the pointer actually moved.
    ///
    /// Upstream samples in the `pointermove` handler, so a still pointer adds
    /// nothing. Sampling every frame instead would keep the newest sample
    /// perpetually fresh and make the calm-release check in
    /// [`estimate_release_velocity`] unreachable.
    fn sample_trail(&mut self, press: &mut Press, gx: f32, gy: f32, now: Instant) {
        if press.last_sample.is_some_and(|(x, y)| (x - gx).abs() < 0.01 && (y - gy).abs() < 0.01) {
            return;
        }
        press.last_sample = Some((gx, gy));
        let t = now.duration_since(self.epoch).as_secs_f64() * 1000.0;
        self.trail.push(DragSample { t, x: gx, y: gy });
        trim_trail(&mut self.trail, t);
    }

    fn release_drag(&mut self, input: FrameInput, out: &mut FrameOutput) {
        log::debug!("drag released");
        let now = input.now;
        self.pos = (self.pos.0.round(), self.pos.1.round());
        self.commit_center();
        // Upstream returns to an idle clip before deciding whether to throw.
        self.play_idle_once(now);
        let trail = std::mem::take(&mut self.trail);
        let now_ms = now.duration_since(self.epoch).as_secs_f64() * 1000.0;
        match estimate_release_velocity(&trail, now_ms, self.physics) {
            Some((vx, vy)) => {
                log::debug!("release vx={vx:.0} vy={vy:.0} |v|={:.0}", vx.hypot(vy));
                self.start_throw(vx, vy);
            }
            None => {
                // A gentle placement: the pet stays where the spring left it.
                let pos = (self.pos.0.round() as i32, self.pos.1.round() as i32);
                out.move_to = Some(pos);
                self.last_reported = Some(pos);
            }
        }
    }

    fn start_throw(&mut self, vx: f32, vy: f32) {
        self.stop_move();
        self.throw = Some(ThrowState { x: self.pos.0, y: self.pos.1, vx, vy });
        self.was_grounded = false;
        self.pending_squash = false;
        self.squash = None;
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
        spring.step(target, dt, self.physics.throw_power);
        self.pos = (spring.x, spring.y);
        let pos = (spring.x.round() as i32, spring.y.round() as i32);
        if self.last_reported != Some(pos) {
            out.move_to = Some(pos);
            self.last_reported = Some(pos);
        }
    }

    /// Global pointer position in canvas pixels.
    fn pointer_global(&self, local: Pos2, input: FrameInput) -> (f32, f32) {
        if let Some((x, y)) = input.global {
            return (x, y);
        }
        let canvas = local.to_vec2() * input.ppp;
        (input.window_origin.0 as f32 + canvas.x, input.window_origin.1 as f32 + canvas.y)
    }

    // -----------------------------------------------------------------------
    // Position engines.

    fn step_motion(&mut self, out: &mut FrameOutput, now: Instant) {
        if self.press.is_some_and(|press| press.dragging) {
            return;
        }
        if let Some(state) = self.throw {
            self.step_throw(state, out, now);
            return;
        }
        if let Some((plan, name)) = self.move_plan.clone() {
            self.step_move(&plan, &name, out, now);
            return;
        }
        self.pos = self.target_position();
        let pos = (self.pos.0.round() as i32, self.pos.1.round() as i32);
        if self.last_reported != Some(pos) {
            out.move_to = Some(pos);
            self.last_reported = Some(pos);
        }
    }

    fn step_throw(&mut self, state: ThrowState, out: &mut FrameOutput, now: Instant) {
        let bounds = throw_bounds(self.world);
        let falling_vy = state.vy;
        let step = throw_step(state, 1.0 / 60.0, bounds, self.physics);
        self.throw = Some(step.state);
        self.pos = (step.state.x, step.state.y);
        let pos = (step.state.x.round() as i32, step.state.y.round() as i32);
        if self.last_reported != Some(pos) {
            out.move_to = Some(pos);
            self.last_reported = Some(pos);
        }

        let grounded = step.state.y >= bounds.max_y - 1.0;
        // Landing squash: only on the airborne-to-ground transition, scaled by
        // the impact speed (the vertical speed before this step integrated).
        if step.bounced && grounded && !self.was_grounded {
            let squash = landing_squash(falling_vy);
            log::debug!("landing squash {squash:.2} from {falling_vy:.0} px/s");
            self.squash = Some((now, squash));
        }
        self.was_grounded = grounded;

        if step.at_rest {
            self.throw = None;
            self.was_grounded = false;
            self.commit_center();
            self.pick_next(now);
        }
    }

    fn step_move(&mut self, plan: &MovePlan, name: &str, out: &mut FrameOutput, now: Instant) {
        let duration = self.player.duration().map_or(FALLBACK_CLIP_SECS, |d| d.as_secs_f32()).max(0.01);
        let t = self.clip_started.map_or(0.0, |start| now.duration_since(start).as_secs_f32());
        let x = move_x(plan, t, duration);
        self.pos = (x, plan.start_y);
        let pos = (x.round() as i32, plan.start_y.round() as i32);
        if self.last_reported != Some(pos) {
            out.move_to = Some(pos);
            self.last_reported = Some(pos);
        }
        if t >= duration - plan.tail_sec {
            self.move_plan = None;
            self.commit_center();
            log::debug!("walk {name} finished at {x:.0}");
        }
    }

    /// Where the window belongs when nothing else is driving it.
    fn target_position(&self) -> (f32, f32) {
        if let Some((rx, ry)) = self.custom_pos {
            return (
                self.world.x as f32 + rx * self.world.width as f32 - CANVAS_W as f32 / 2.0,
                self.world.y as f32 + ry * self.world.height as f32 - CANVAS_H as f32 / 2.0,
            );
        }
        anchor_position(self.world, &self.position, self.ppp)
    }

    // -----------------------------------------------------------------------
    // Painting.

    /// Uploads the current frame when it changed and paints it at native size.
    fn paint(&mut self, ui: &mut egui::Ui, now: Instant, ppp: f32) {
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

        let fresh = self.texture.is_some();
        let mut fade = match (self.fade_at, fresh) {
            (Some(start), true) if !reduce_motion() => ((now - start).as_secs_f32() / CROSSFADE_SECS).clamp(0.0, 1.0),
            (Some(_), true) => 1.0,
            _ => 0.0,
        };
        if fade >= 1.0 {
            self.fade_at = None;
            self.prev_texture = None;
            fade = 1.0;
        }
        let old = if self.fade_at.is_some() { self.prev_texture.as_ref() } else { None };

        let scale = match self.squash {
            Some((start, squash)) => squash_scale((now - start).as_secs_f32() / SQ_DURATION_SECS, squash),
            None => 1.0,
        };
        // The dragged pet is lifted by the stage pad, as `transform: none` does
        // upstream; the idle pose sits on the window's bottom edge.
        let lift = if self.press.is_some_and(|press| press.dragging) { BOTTOM_PAD } else { 0.0 };
        let width = CANVAS_W as f32 / ppp;
        let height = CANVAS_H as f32 * scale / ppp;
        let bottom = (CANVAS_H as f32 - lift) / ppp;
        let rect = Rect::from_min_size(Pos2::new(0.0, bottom - height), Vec2::new(width, height));

        // Mirror about the vertical centre for a right-facing pet.
        let uv = if self.facing.is_right() {
            Rect::from_min_max(Pos2::new(1.0, 0.0), Pos2::new(0.0, 1.0))
        } else {
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
        };
        let painter = ui.painter();
        match (old, &self.texture) {
            (Some(old), Some(current)) => {
                painter.image(old.id(), rect, uv, Color32::from_white_alpha(((1.0 - fade) * 255.0).round() as u8));
                painter.image(current.id(), rect, uv, Color32::from_white_alpha((fade * 255.0).round() as u8));
            }
            (Some(old), None) => {
                painter.image(old.id(), rect, uv, Color32::WHITE);
            }
            (None, Some(current)) => {
                painter.image(current.id(), rect, uv, Color32::WHITE);
            }
            (None, None) => {}
        }
    }

    fn schedule_repaint(&mut self, ctx: &egui::Context, now: Instant) {
        if let Some((start, _)) = self.squash {
            let elapsed = (now - start).as_secs_f32();
            if elapsed >= SQ_DURATION_SECS {
                self.squash = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(8));
            }
        }
        if self.fade_at.is_some() && !reduce_motion() {
            ctx.request_repaint_after(Duration::from_millis(8));
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
        if self.throw.is_some() || self.move_plan.is_some() {
            ctx.request_repaint_after(Duration::from_millis(8));
            return;
        }
        if let Some(due) = self.next_due {
            ctx.request_repaint_after(due);
        } else if !self.stalled && !self.player.ended() {
            ctx.request_repaint_after(Duration::from_millis(8));
        }
    }

    /// Tells the host what the interaction overlay should show.
    fn emit_overlay(&self, out: &mut FrameOutput, input: FrameInput) {
        if self.menu_open {
            out.overlay = OverlayIntent::Menu { anchor: self.menu_anchor };
            return;
        }
        let in_body = input.pointer.is_some_and(|pos| {
            let canvas = pos.to_vec2() * input.ppp;
            HIT_BOX.contains(canvas.x, canvas.y)
        }) && !self.press.is_some_and(|press| press.dragging);
        out.overlay = if in_body {
            let anchor = input.pointer.map_or((0.0, 0.0), |pos| self.pointer_global(pos, input));
            OverlayIntent::Tooltip { anchor, text: self.name.clone() }
        } else {
            OverlayIntent::Hidden
        };
    }

    // -----------------------------------------------------------------------
    // Public surface used by the hosts and the overlay.

    /// Applies one action chosen in the interaction overlay.
    pub fn overlay_action(&mut self, action: OverlayAction) {
        match action {
            OverlayAction::PlayOnce(name) => {
                self.menu_open = false;
                // Text clips must not be mirrored: face left before playing them.
                if is_no_mirror(self.pools.categories(), &name) && self.facing.is_right() {
                    self.facing = Facing::Left;
                }
                self.stop_move();
                self.play_once(name, Mode::Action, Instant::now());
            }
            OverlayAction::MoveTo(name) => {
                self.menu_open = false;
                if self.try_move(Some(&name), Instant::now()) == TryMove::Unavailable {
                    self.play_once(name, Mode::Action, Instant::now());
                }
            }
            OverlayAction::Home => {
                self.menu_open = false;
                self.stop_throw();
                self.stop_move();
                self.custom_pos = None;
                self.pos = anchor_position(self.world, &self.position, self.ppp);
            }
            OverlayAction::MenuClosed => self.menu_open = false,
        }
    }

    /// Whether the pet currently owns the pointer press (used by a host that has
    /// to keep its window interactive while a drag is in progress).
    pub fn has_press(&self) -> bool {
        self.press.is_some()
    }

    /// Whether the context menu is open (the overlay needs an interactive
    /// window while it is).
    pub fn menu_open(&self) -> bool {
        self.menu_open
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

fn reduce_motion() -> bool {
    std::env::var_os("WHALE_PET_REDUCE_MOTION").is_some_and(|value| value != "0")
}

/// Top-left of a `CANVAS_W * CANVAS_H` window placed at `corner` with the given
/// margins inside `world` (a work area in physical pixels).
pub fn anchor_position(world: WorldArea, position: &Position, ppp: f32) -> (f32, f32) {
    let margin_x = position.margin_x as f32 * ppp;
    let margin_y = position.margin_y as f32 * ppp;
    let width = CANVAS_W as f32;
    let height = CANVAS_H as f32;
    let left = world.x as f32 + margin_x;
    let top = world.y as f32 + margin_y;
    let right = world.right() as f32 - width - margin_x;
    let bottom = world.bottom() as f32 - height - margin_y;
    match position.corner.as_str() {
        "top-left" => (left, top),
        "bottom-left" => (left, bottom),
        "bottom-right" => (right, bottom),
        "top-right" => (right, top),
        other => {
            log::warn!("unknown corner {other:?}; using top-right");
            (right, top)
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
