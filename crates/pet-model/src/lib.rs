//! Animation selection, motion math and throw physics shared by the UI.
//!
//! The semantics mirror the upstream web client (`dsh-pet/src/shared/*`):
//!
//! - clicks pick from the click pool without excluding the current clip;
//! - every other transition picks from a pool while excluding the current
//!   clip, falling back to the full pool when the exclusion empties it;
//! - the random chain rolls between idle / turn / move / a weighted category
//!   action, with `turn` and `move` disabled when the pet is pinned;
//! - a dragged pet that is released while moving fast becomes a projectile that
//!   bounces off the work-area edges;
//! - a walk plans a distance in the facing direction and travels it during the
//!   clip's body, staying still for the clip's lead/tail seconds;
//! - a clip that ends naturally is followed by entering the next clip at the
//!   frame whose pose matches the final frame best (see [`pick_entry`]).
//!
//! # Units
//!
//! Upstream works in a 462-wide reference space. This crate works in canvas
//! pixels (640 wide), so every spatial constant and every distance/speed read
//! from the config is multiplied by [`WORLD_SCALE`]. Time constants and
//! dimensionless ratios are not scaled. Pointer samples are already in canvas
//! pixels, matching the world the pet moves in.

use pet_config::{
    ALPHA_THRESHOLD, CANVAS_H, CANVAS_W, Category, Frame, GRID_H, GRID_W, Mask, MoveAction, MoveParams, Physics, Pose, SIDE_ALLOW, WORLD_SCALE,
    WorldArea,
};

/// Upstream constants, kept in their original 462-wide reference space.
mod logical {
    /// Gravity (`GRAVITY` in `physics.ts`).
    pub const GRAVITY: f32 = 1400.0;
    /// Bounce restitution.
    pub const RESTITUTION: f32 = 0.78;
    /// Horizontal friction while touching the ground, per second.
    pub const GROUND_FRICTION: f32 = 2.5;
    /// Below this vertical speed a landing stops instead of bouncing.
    pub const REST_VY: f32 = 40.0;
    /// Below this horizontal speed while grounded the pet is at rest.
    pub const REST_VX: f32 = 15.0;
    /// Throw speed soft cap: `cap * (1 - e^(-s/cap))`.
    pub const MAX_THROW_SPEED: f32 = 3600.0;
    /// Release speeds below this are a gentle placement, not a throw.
    pub const DEAD_ZONE_SPEED: f32 = 500.0;
    /// Reference acceleration for the release-velocity gain.
    pub const ACCEL_REF: f32 = 8000.0;
    /// Landing impact speed that maps to the softest bounce.
    pub const SQ_SOFT_SPEED: f32 = 300.0;
    /// Landing impact speed that maps to the deepest squash.
    pub const SQ_HARD_SPEED: f32 = 1500.0;
    /// Click score threshold.
    pub const SCORE_MIN_SPEED: f32 = 400.0;
    /// Upstream `PET_REF_WIDTH`.
    pub const PET_REF_WIDTH: f32 = 462.0;
    /// One upstream pixel in comparison slack.
    pub const ONE_PX: f32 = 1.0;
}

/// How far ahead of the release the pointer trail is kept (`TRAIL_KEEP_MS`).
pub const TRAIL_KEEP_MS: f64 = 200.0;
/// Release velocity is estimated from this trailing window (`RELEASE_WINDOW_MS`).
pub const RELEASE_WINDOW_MS: f64 = 150.0;
/// A release that comes this long after the last sample is not a throw.
pub const RELEASE_STALE_MS: f64 = 150.0;
/// A trail spanning less than this is noise, not a gesture.
pub const MIN_SPAN_MS: f64 = 20.0;
/// Segments shorter than this are merged forward when measuring peak speed.
pub const SEG_MIN_DT_MS: f64 = 8.0;
/// Weight of the peak speed in the release estimate (`PEAK_WEIGHT`).
pub const PEAK_WEIGHT: f32 = 0.5;
/// Cap on the acceleration gain (`ACCEL_GAIN_MAX`).
pub const ACCEL_GAIN_MAX: f32 = 0.6;
/// Largest physics step, so a stalled frame cannot teleport the pet.
pub const MAX_STEP_DT: f32 = 0.05;

/// Gravity in canvas pixels per second squared.
pub const GRAVITY: f32 = logical::GRAVITY * WORLD_SCALE;
/// Restitution, dimensionless.
pub const RESTITUTION: f32 = logical::RESTITUTION;
/// Ground friction, dimensionless per second.
pub const GROUND_FRICTION: f32 = logical::GROUND_FRICTION;
/// Vertical rest threshold in canvas pixels per second.
pub const REST_VY: f32 = logical::REST_VY * WORLD_SCALE;
/// Horizontal rest threshold in canvas pixels per second.
pub const REST_VX: f32 = logical::REST_VX * WORLD_SCALE;
/// Throw speed soft cap in canvas pixels per second.
pub const MAX_THROW_SPEED: f32 = logical::MAX_THROW_SPEED * WORLD_SCALE;
/// Gentle-placement threshold in canvas pixels per second.
pub const DEAD_ZONE_SPEED: f32 = logical::DEAD_ZONE_SPEED * WORLD_SCALE;
/// Acceleration reference in canvas pixels per second squared.
pub const ACCEL_REF: f32 = logical::ACCEL_REF * WORLD_SCALE;
/// Click-score threshold in canvas pixels per second.
pub const SCORE_MIN_SPEED: f32 = logical::SCORE_MIN_SPEED * WORLD_SCALE;

const DEFAULT_IDLE: &[&str] = &["待机呼吸休闲"];
const DEFAULT_DRAG: &[&str] = &["被鼠标拖拽悬空反馈"];
const DEFAULT_CLICKS: &[&str] = &["点击回应-开心跃动", "点击回应-害羞惊讶", "点击回应-傲娇生气", "点击回应-挠痒咯咯笑", "点击回应-元气挥手"];

/// Which way the pet is facing. Right is drawn mirrored (`scaleX(-1)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    Left,
    Right,
}

impl Facing {
    pub fn flipped(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    pub fn is_right(self) -> bool {
        self == Self::Right
    }

    /// Walk direction on screen: a left-facing pet walks left.
    pub fn walk_dir(self) -> f32 {
        if self.is_right() { 1.0 } else { -1.0 }
    }
}

/// Top-level weights of the random chain (`animationWeights`).
#[derive(Debug, Clone, Copy)]
pub struct RollWeights {
    pub idle: f32,
    pub turn: f32,
    pub walk: f32,
}

impl RollWeights {
    pub fn from_config(config: &pet_config::PetConfig) -> Self {
        let weights = config.weights();
        Self { idle: weights.idle.max(0.0), turn: weights.turn.max(0.0), walk: weights.walk.max(0.0) }
    }
}

/// Outcome of one roll of the random chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollKind {
    Idle,
    Turn,
    Move,
    Action,
}

/// `rollKind` from `src/shared/pickers.ts`: `roll` in `0..1`.
///
/// The three named weights are percentages; everything left over goes to a
/// random category action. A pinned pet rolls with `turn`/`move` at zero.
pub fn roll_kind(roll: f32, weights: RollWeights, fixed: bool) -> RollKind {
    let turn = if fixed { 0.0 } else { weights.turn };
    let mov = if fixed { 0.0 } else { weights.walk };
    if roll < weights.idle / 100.0 {
        RollKind::Idle
    } else if roll < (weights.idle + turn) / 100.0 {
        RollKind::Turn
    } else if roll < (weights.idle + turn + mov) / 100.0 {
        RollKind::Move
    } else {
        RollKind::Action
    }
}

/// Picks one category by weight, skipping `noMirror` categories while the pet
/// faces right. Returns `None` when no category has actions.
pub fn pick_weighted_category(categories: &[Category], facing: Facing) -> Option<&Category> {
    let non_empty: Vec<&Category> = categories.iter().filter(|category| !category.actions.is_empty()).collect();
    if non_empty.is_empty() {
        return None;
    }
    let filtered: Vec<&Category> = non_empty.iter().copied().filter(|category| !(category.no_mirror && facing.is_right())).collect();
    let eligible = if filtered.is_empty() { non_empty } else { filtered };
    let total: f32 = eligible.iter().map(|category| category.weight.max(0.0)).sum();
    if total <= 0.0 {
        return Some(eligible[fastrand::usize(0..eligible.len())]);
    }
    let mut roll = fastrand::f32() * total;
    for category in &eligible {
        roll -= category.weight.max(0.0);
        if roll <= 0.0 {
            return Some(category);
        }
    }
    eligible.last().copied()
}

/// `pickCategoryAction`: a weighted category + one action from it, avoiding the
/// current clip where possible. Falls back to the idle pool when no category
/// is usable (`id` is then `FALLBACK`).
pub fn pick_category_action(categories: &[Category], idle: &[String], facing: Facing, current: Option<&str>) -> Option<(String, String)> {
    match pick_weighted_category(categories, facing) {
        Some(category) => {
            let name = pick(&category.actions, current)?;
            Some((category.id.clone(), name))
        }
        None => Some(("FALLBACK".to_owned(), pick(idle, current)?)),
    }
}

/// Whether `anim` belongs to a `noMirror` category (text clips, which must not
/// be mirrored).
pub fn is_no_mirror(categories: &[Category], anim: &str) -> bool {
    categories.iter().any(|category| category.no_mirror && category.actions.iter().any(|name| name == anim))
}

/// Random animation pools, with the built-in defaults filling empty pools so
/// the app works without a usable `config.jsonc`.
#[derive(Debug, Clone)]
pub struct AnimPools {
    idle: Vec<String>,
    turn: Vec<String>,
    drag: Vec<String>,
    clicks: Vec<String>,
    moves: Vec<MoveAction>,
    move_defaults: MoveParams,
    categories: Vec<Category>,
}

impl AnimPools {
    pub fn from_config(config: &pet_config::PetConfig) -> Self {
        let animations = &config.animations;
        Self {
            idle: fill(&animations.idle, DEFAULT_IDLE),
            turn: animations.turn.clone(),
            drag: fill(&animations.drag, DEFAULT_DRAG),
            clicks: fill(&animations.clicks, DEFAULT_CLICKS),
            moves: animations.moves.actions.clone(),
            move_defaults: animations.moves.defaults,
            categories: animations.categories.clone(),
        }
    }

    /// The clip the pet starts on: `idle[0]` upstream, played once.
    pub fn first_idle(&self) -> Option<String> {
        self.idle.first().cloned()
    }

    /// Idle clip, avoiding `exclude` when the pool has an alternative.
    pub fn pick_idle(&self, exclude: Option<&str>) -> Option<String> {
        pick(&self.idle, exclude)
    }

    /// Turn clip, avoiding `exclude` when possible.
    pub fn pick_turn(&self, exclude: Option<&str>) -> Option<String> {
        pick(&self.turn, exclude)
    }

    /// Clip played while the pet is held.
    pub fn pick_drag(&self) -> Option<String> {
        pick(&self.drag, None)
    }

    /// Clip played on a click; upstream does not exclude the current clip.
    pub fn pick_click(&self) -> Option<String> {
        pick(&self.clicks, None)
    }

    /// A random walk action, or the named one when the menu asks for it.
    pub fn pick_move(&self, preferred: Option<&str>) -> Option<&MoveAction> {
        match preferred {
            Some(name) => self.moves.iter().find(|action| action.name == name),
            None if self.moves.is_empty() => None,
            None => self.moves.get(fastrand::usize(0..self.moves.len())),
        }
    }

    pub fn move_params(&self, action: &MoveAction) -> MoveParams {
        action.resolved(self.move_defaults)
    }

    pub fn has_moves(&self) -> bool {
        !self.moves.is_empty()
    }

    pub fn categories(&self) -> &[Category] {
        &self.categories
    }

    pub fn idle_pool(&self) -> &[String] {
        &self.idle
    }

    pub fn idle_count(&self) -> usize {
        self.idle.len()
    }
}

fn fill(pool: &[String], defaults: &[&str]) -> Vec<String> {
    if pool.is_empty() { defaults.iter().map(|name| (*name).to_owned()).collect() } else { pool.to_vec() }
}

/// Picks from `pool`, avoiding `exclude` when an alternative exists.
pub fn pick(pool: &[String], exclude: Option<&str>) -> Option<String> {
    if pool.is_empty() {
        return None;
    }
    let filtered: Vec<&String> = pool.iter().filter(|name| Some(name.as_str()) != exclude).collect();
    let source: Vec<&String> = if filtered.is_empty() { pool.iter().collect() } else { filtered };
    let index = fastrand::usize(0..source.len());
    Some(source[index].clone())
}

/// Inclusive-exclusive random integer in `[min, max)` (`randomBetween`).
pub fn random_between(min: f32, max: f32) -> f32 {
    if max <= min {
        return min.floor();
    }
    (min + fastrand::f32() * (max - min)).floor()
}

/// Uniform draw in `0..1`, the roll fed to [`roll_kind`].
pub fn random_unit() -> f32 {
    fastrand::f32()
}

// ---------------------------------------------------------------------------
// Movement (roam) planning.

/// One walk: where the box starts, where it ends, and the clip's lead/tail
/// seconds during which the pet stays put.
#[derive(Debug, Clone, Copy)]
pub struct MovePlan {
    /// Box top-left at the start, canvas pixels.
    pub start_x: f32,
    pub start_y: f32,
    /// Box top-left at the destination, canvas pixels.
    pub target_x: f32,
    /// Horizontal sign of the walk: `+1` right, `-1` left.
    pub dir: f32,
    /// Total distance travelled, canvas pixels.
    pub total_dist: f32,
    pub lead_sec: f32,
    pub tail_sec: f32,
}

/// `planMove` for a single work area.
///
/// `cx`/`cy` are the pet *centre*; `params` are already in canvas pixels. The
/// destination must keep the body inside the area: the body half-width is the
/// box half-width minus the transparent side margin, and `margin` is the gap
/// between the body and the screen edge.
pub fn plan_move(cx: f32, cy: f32, facing: Facing, params: MoveParams, area: WorldArea) -> Option<MovePlan> {
    let half_w = CANVAS_W as f32 / 2.0;
    let distance = random_between(params.min_dist, params.max_dist);
    let target = cx + facing.walk_dir() * distance;
    let body_half = half_w - SIDE_ALLOW;
    let left_bound = area.x as f32 + params.margin + body_half;
    let right_bound = area.right() as f32 - params.margin - body_half;
    if target < left_bound || target > right_bound {
        return None;
    }
    Some(MovePlan {
        start_x: cx - half_w,
        start_y: cy - CANVAS_H as f32 / 2.0,
        target_x: target - half_w,
        dir: facing.walk_dir(),
        total_dist: (target - cx).abs(),
        lead_sec: params.lead_sec,
        tail_sec: params.tail_sec,
    })
}

/// Horizontal position of the box during a walk at clip time `t` seconds.
pub fn move_x(plan: &MovePlan, t: f32, duration: f32) -> f32 {
    let travel = (duration - plan.lead_sec - plan.tail_sec).max(0.1);
    if t <= plan.lead_sec {
        plan.start_x
    } else if t >= duration - plan.tail_sec {
        plan.target_x
    } else {
        plan.start_x + plan.dir * plan.total_dist * ((t - plan.lead_sec) / travel)
    }
}

// ---------------------------------------------------------------------------
// Throw physics.

/// Physics parameters for the throw, in canvas pixels.
#[derive(Debug, Clone, Copy)]
pub struct ThrowParams {
    pub gravity: f32,
    pub restitution: f32,
    pub ground_friction: f32,
    pub ceiling_bounce: bool,
    pub throw_power: f32,
}

impl ThrowParams {
    pub fn from_config(physics: &Physics) -> Self {
        let physics = physics.sanitized();
        Self {
            gravity: physics.gravity * WORLD_SCALE,
            restitution: physics.restitution,
            ground_friction: physics.ground_friction,
            ceiling_bounce: physics.ceiling_bounce,
            throw_power: physics.throw_power,
        }
    }
}

/// Allowed range of the box's top-left corner, canvas pixels.
#[derive(Debug, Clone, Copy)]
pub struct ThrowBounds {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

/// `throwBounds` for the work area: the *body* touches the screen edges, so the
/// box may stick out by [`SIDE_ALLOW`] on each side.
pub fn throw_bounds(area: WorldArea) -> ThrowBounds {
    ThrowBounds {
        min_x: area.x as f32 - SIDE_ALLOW,
        min_y: area.y as f32,
        max_x: area.right() as f32 - CANVAS_W as f32 + SIDE_ALLOW,
        max_y: area.bottom() as f32 - CANVAS_H as f32,
    }
}

/// Projectile state: box top-left plus velocity, canvas pixels and px/s.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThrowState {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
}

/// One integration step.
#[derive(Debug, Clone, Copy)]
pub struct ThrowStep {
    pub state: ThrowState,
    /// The step touched a wall, the ceiling or the ground.
    pub bounced: bool,
    /// Grounded and slow enough that the host should stop the loop.
    pub at_rest: bool,
}

/// `throwStep`: gravity, integration, then wall/ground response.
pub fn throw_step(state: ThrowState, dt_raw: f32, bounds: ThrowBounds, params: ThrowParams) -> ThrowStep {
    let dt = dt_raw.clamp(0.0, MAX_STEP_DT);
    let ThrowState { mut x, mut y, mut vx, mut vy } = state;
    vy += params.gravity * dt;
    x += vx * dt;
    y += vy * dt;
    let mut bounced = false;
    if x < bounds.min_x {
        x = bounds.min_x;
        vx = vx.abs() * params.restitution;
        bounced = true;
    } else if x > bounds.max_x {
        x = bounds.max_x;
        vx = -vx.abs() * params.restitution;
        bounced = true;
    }
    if y < bounds.min_y {
        // With ceilingBounce off the pet simply flies off the top and gravity
        // brings it back; the position is left out of bounds on purpose.
        if params.ceiling_bounce {
            y = bounds.min_y;
            vy = vy.abs() * params.restitution;
            bounced = true;
        }
    } else if y >= bounds.max_y {
        y = bounds.max_y;
        vx *= (1.0 - params.ground_friction * dt).max(0.0);
        if vy.abs() < REST_VY {
            vy = 0.0;
        } else {
            vy = -vy.abs() * params.restitution;
        }
        bounced = true;
    }
    let speed = vx.hypot(vy);
    let at_rest = (y >= bounds.max_y - logical::ONE_PX * WORLD_SCALE && vy.abs() < logical::ONE_PX * WORLD_SCALE && vx.abs() < REST_VX)
        || (bounced && speed < REST_VY && vy.abs() < logical::ONE_PX * WORLD_SCALE);
    ThrowStep { state: ThrowState { x, y, vx, vy }, bounced, at_rest }
}

/// One pointer sample taken while dragging.
#[derive(Debug, Clone, Copy)]
pub struct DragSample {
    /// Milliseconds on the host's clock.
    pub t: f64,
    /// Pointer position in canvas pixels.
    pub x: f32,
    pub y: f32,
}

/// Drops samples older than [`TRAIL_KEEP_MS`].
pub fn trim_trail(trail: &mut Vec<DragSample>, now_ms: f64) {
    let cutoff = now_ms - TRAIL_KEEP_MS;
    let keep_from = trail.iter().position(|sample| sample.t >= cutoff).unwrap_or(trail.len());
    if keep_from > 0 {
        trail.drain(..keep_from);
    }
}

/// `softClampSpeed`: monotonic and asymptotic to [`MAX_THROW_SPEED`].
pub fn soft_clamp_speed(speed: f32) -> f32 {
    if speed <= 0.0 {
        return 0.0;
    }
    MAX_THROW_SPEED * (1.0 - (-speed / MAX_THROW_SPEED).exp())
}

/// `estimateReleaseVelocity`: `None` means a gentle placement, not a throw.
///
/// The returned velocity is in canvas pixels per second.
pub fn estimate_release_velocity(trail: &[DragSample], now_ms: f64, params: ThrowParams) -> Option<(f32, f32)> {
    let last = *trail.last()?;
    if now_ms - last.t > RELEASE_STALE_MS {
        return None;
    }
    let window: Vec<DragSample> = trail.iter().copied().filter(|sample| now_ms - sample.t <= RELEASE_WINDOW_MS).collect();
    if window.len() < 2 {
        return None;
    }
    let first = window[0];
    let last = window[window.len() - 1];
    let span_ms = last.t - first.t;
    if span_ms < MIN_SPAN_MS {
        return None;
    }
    let base_vx = ((last.x - first.x) / span_ms as f32) * 1000.0;
    let base_vy = ((last.y - first.y) / span_ms as f32) * 1000.0;
    let base_speed = base_vx.hypot(base_vy);
    if base_speed < 1e-6 {
        return None;
    }

    // Segment speeds, merging samples closer than SEG_MIN_DT_MS.
    let mut segments: Vec<(f32, f64)> = Vec::new();
    let (mut px, mut py, mut pt) = (first.x, first.y, first.t);
    for sample in &window[1..] {
        let dt = sample.t - pt;
        if dt >= SEG_MIN_DT_MS {
            let speed = ((sample.x - px).hypot(sample.y - py) / dt as f32) * 1000.0;
            segments.push((speed, sample.t));
            px = sample.x;
            py = sample.y;
            pt = sample.t;
        }
    }
    let peak_speed = segments.iter().map(|(speed, _)| *speed).fold(base_speed, f32::max);
    let accel = if segments.len() >= 2 {
        let (last_speed, last_t) = segments[segments.len() - 1];
        let (first_speed, first_t) = segments[0];
        (last_speed - first_speed) / ((last_t - first_t) / 1000.0).max(MIN_SPAN_MS / 1000.0) as f32
    } else {
        0.0
    };
    let speed_before_clamp =
        ((1.0 - PEAK_WEIGHT) * base_speed + PEAK_WEIGHT * peak_speed) * (1.0 + (accel.max(0.0) / ACCEL_REF).min(1.0) * ACCEL_GAIN_MAX);
    let speed = soft_clamp_speed(speed_before_clamp) * params.throw_power;
    if speed < DEAD_ZONE_SPEED {
        return None;
    }
    Some((base_vx / base_speed * speed, base_vy / base_speed * speed))
}

/// `landingSquash`: maps a landing impact speed to the squash depth.
pub fn landing_squash(impact_speed: f32) -> f32 {
    let soft = logical::SQ_SOFT_SPEED * WORLD_SCALE;
    let hard = logical::SQ_HARD_SPEED * WORLD_SCALE;
    let t = ((impact_speed.abs() - soft) / (hard - soft)).clamp(0.0, 1.0);
    (1.0 - t * (1.0 - pet_config::SQ_SQUASH)).min(0.8)
}

/// `clickScore`, in upstream units: speed and size are canvas pixels.
///
/// The pet is always the reference size here, so the score is just the speed
/// divided by 100 (in upstream units), at least 1.
pub fn click_score(speed_canvas: f32) -> u32 {
    if speed_canvas <= 0.0 {
        return 0;
    }
    let speed_logical = speed_canvas / WORLD_SCALE;
    let size_logical = logical::PET_REF_WIDTH;
    ((speed_logical / 100.0) * (logical::PET_REF_WIDTH / size_logical)).round().max(1.0) as u32
}

// ---------------------------------------------------------------------------
// Squash and spring helpers (unchanged from the original v1 behaviour).

/// Q-squash curve from `physics.ts`: fast compression to `squash`, then an
/// ease-out-back rebound capped at 1.12. `u` is normalized time in `0..=1`.
pub fn squash_scale(u: f32, squash: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    if u < 0.45 {
        let p = u / 0.45;
        1.0 - (1.0 - squash) * p * p
    } else {
        let p = (u - 0.45) / 0.55;
        let c1 = 1.70158_f32;
        let c3 = c1 + 1.0;
        let f = 1.0 + c3 * (p - 1.0).powi(3) + c1 * (p - 1.0).powi(2);
        (squash + (1.0 - squash) * f.max(0.0)).min(1.12)
    }
}

/// One spring axis step from `pet.ts`: returns the new velocity.
pub fn spring_step(v: f32, x: f32, target: f32, dt: f32, power: f32) -> f32 {
    v + ((target - x) * pet_config::SPRING_K - v * pet_config::SPRING_C) * power * dt
}

/// Two-axis spring used to follow the pointer while dragging.
#[derive(Debug, Clone, Copy)]
pub struct Spring2 {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
}

impl Spring2 {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y, vx: 0.0, vy: 0.0 }
    }

    pub fn step(&mut self, target: (f32, f32), dt: f32, power: f32) {
        self.vx = spring_step(self.vx, self.x, target.0, dt, power);
        self.vy = spring_step(self.vy, self.y, target.1, dt, power);
        self.x += self.vx * dt;
        self.y += self.vy * dt;
    }
}

// ---------------------------------------------------------------------------
// Pose matching
// ---------------------------------------------------------------------------
//
// A simplified form of motion matching: every clip carries a cheap pose
// descriptor for its entry window and its last frame, and a cut from a clip
// that ended naturally enters the next one at whichever window frame looks
// closest. There is no blending, no optical flow and no transition animation —
// the cost only decides which frame to hard-cut to.

/// How different a frame may look from a clip's first frame and still count as
/// part of the entry window.
pub const ENTRY_EPS: f32 = 0.03;
/// Longest entry window, in frames (half a second at 24 fps). Roughly three
/// quarters of the clips stay still this long.
pub const ENTRY_MAX_FRAMES: usize = 12;
/// Smallest cost improvement worth skipping the first frame for.
pub const ENTRY_MIN_GAIN: f32 = 0.005;
/// Fraction of the silhouette's height that counts as the head.
const HEAD_FRACTION: f32 = 0.25;

/// Describes one frame from its [`GRID_W`] x [`GRID_H`] RGBA downsample.
///
/// `rgba` must be exactly [`GRID_W`] x [`GRID_H`] tightly packed RGBA; the
/// native 640x360 frame is accepted only after being resized to the descriptor
/// grid. The generator does that with swscale's area filter, so a cell holds the
/// average of its 20x20 block. Alpha above [`ALPHA_THRESHOLD`] marks the cell
/// opaque. A frame with no opaque cell yields an all-zero descriptor, matching
/// the "nothing on screen" convention in the metadata.
///
/// # Panics
///
/// Panics if `rgba` is not exactly the descriptor grid. Both callers (the
/// offline generator and the tests) build that buffer themselves, so this
/// catches a wrong grid or stride instead of silently describing a truncated
/// frame as a mostly transparent one.
pub fn describe_frame(rgba: &[u8]) -> Pose {
    assert_eq!(rgba.len(), GRID_W * GRID_H * 4, "describe_frame wants a {GRID_W}x{GRID_H} RGBA grid");
    let mut pose = Pose::default();
    let mut area = 0u64;
    let mut sum_x = 0u64;
    let mut sum_y = 0u64;
    let mut sum_r = 0u64;
    let mut sum_g = 0u64;
    let mut sum_b = 0u64;
    let mut left = 0u64;
    let mut right = 0u64;
    let mut top = None;
    let mut bottom = 0usize;

    for y in 0..GRID_H {
        for x in 0..GRID_W {
            let i = (y * GRID_W + x) * 4;
            let pixel = &rgba[i..i + 4];
            if pixel[3] <= ALPHA_THRESHOLD {
                continue;
            }
            pose.mask.0[y] |= 1 << x;
            area += 1;
            sum_x += x as u64;
            sum_y += y as u64;
            sum_r += u64::from(pixel[0]);
            sum_g += u64::from(pixel[1]);
            sum_b += u64::from(pixel[2]);
            left += u64::from(x * 2 < GRID_W);
            right += u64::from(x * 2 >= GRID_W);
            top.get_or_insert(y);
            bottom = y;
        }
    }
    let Some(top) = top else { return pose };
    let area = area as f32;
    pose.body = [sum_x as f32 / area / GRID_W as f32, sum_y as f32 / area / GRID_H as f32];
    pose.feet = bottom as f32 / GRID_H as f32;
    pose.bias = (right as f32 - left as f32) / area;
    pose.rgb = [(sum_r as f32 / area).round() as u8, (sum_g as f32 / area).round() as u8, (sum_b as f32 / area).round() as u8];

    // The head is the head pixel band of the silhouette, not a fixed slice of
    // the canvas: the pet does not touch the top of the frame.
    let head_bottom = top + (((bottom - top) as f32 * HEAD_FRACTION) as usize).max(1);
    let mut head_area = 0u64;
    let mut head_x = 0u64;
    let mut head_y = 0u64;
    for y in top..=head_bottom.min(GRID_H - 1) {
        for x in 0..GRID_W {
            let i = (y * GRID_W + x) * 4;
            let pixel = &rgba[i..i + 4];
            if pixel[3] > ALPHA_THRESHOLD {
                head_area += 1;
                head_x += x as u64;
                head_y += y as u64;
            }
        }
    }
    pose.head = if head_area == 0 {
        pose.body
    } else {
        let head_area = head_area as f32;
        [head_x as f32 / head_area / GRID_W as f32, head_y as f32 / head_area / GRID_H as f32]
    };
    pose
}

/// How different two poses look: 0 is identical, roughly 1 is completely
/// different. Covers silhouette overlap, body and head position, foot line,
/// left/right lean and mean colour, weighted by how visible each is in a cut.
///
/// The cost is invariant to mirroring both poses: the masks' row bits reverse,
/// the centres flip around the middle, `bias` changes sign and the mean colour
/// is unchanged.
pub fn pose_cost(a: &Pose, b: &Pose) -> f32 {
    let silhouette = 1.0 - mask_iou(&a.mask, &b.mask);
    let body = distance(a.body, b.body) / std::f32::consts::SQRT_2;
    let head = distance(a.head, b.head) / std::f32::consts::SQRT_2;
    let feet = (a.feet - b.feet).abs();
    let bias = (a.bias - b.bias).abs() / 2.0;
    let rgb = rgb_distance(a.rgb, b.rgb) / (255.0 * 3.0_f32.sqrt());
    0.45 * silhouette + 0.15 * head + 0.15 * body + 0.10 * feet + 0.05 * bias + 0.10 * rgb
}

/// Picks the frame of `window` to enter `pose` at.
///
/// `window` is the clip's entry frames in playback order, starting at frame 0.
/// Only the prefix that still looks like frame 0 (cost at most [`ENTRY_EPS`],
/// at most [`ENTRY_MAX_FRAMES`] frames) is considered, so a skipped opening
/// never runs past the point where the clip has visibly started moving. The
/// cheapest frame wins when it saves at least `min_gain` over frame 0; empty
/// (all-transparent) frames are never entered, and an all-empty window falls
/// back to frame 0.
pub fn pick_entry(pose: &Pose, window: &[Frame], min_gain: f32) -> usize {
    let window = &window[..window.len().min(ENTRY_MAX_FRAMES)];
    let Some(first) = window.first() else { return 0 };
    let mut len = 1;
    while len < window.len() && pose_cost(&first.pose, &window[len].pose) <= ENTRY_EPS {
        len += 1;
    }

    let base = pose_cost(pose, &first.pose);
    let mut best = 0;
    let mut best_cost = base;
    for (index, frame) in window[..len].iter().enumerate().skip(1) {
        // Entering an empty frame would show nothing at all.
        if frame.pose.is_empty() {
            continue;
        }
        let cost = pose_cost(pose, &frame.pose);
        if cost < best_cost {
            best = index;
            best_cost = cost;
        }
    }
    if base - best_cost < min_gain { 0 } else { best }
}

fn mask_iou(a: &Mask, b: &Mask) -> f32 {
    let mut intersection = 0u32;
    let mut union = 0u32;
    for (row_a, row_b) in a.0.iter().zip(&b.0) {
        intersection += (row_a & row_b).count_ones();
        union += (row_a | row_b).count_ones();
    }
    // Two empty masks describe the same (blank) silhouette.
    if union == 0 { 1.0 } else { intersection as f32 / union as f32 }
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn rgb_distance(a: [u8; 3], b: [u8; 3]) -> f32 {
    let dr = f32::from(a[0]) - f32::from(b[0]);
    let dg = f32::from(a[1]) - f32::from(b[1]);
    let db = f32::from(a[2]) - f32::from(b[2]);
    dr.hypot(dg).hypot(db)
}

#[cfg(test)]
mod pose_tests {
    use super::*;

    fn pose(mask: [u32; GRID_H], body: [f32; 2], head: [f32; 2], feet: f32, bias: f32, rgb: [u8; 3]) -> Pose {
        Pose { mask: Mask(mask), body, head, feet, bias, rgb }
    }

    fn frame(t: u32, pose: Pose) -> Frame {
        Frame { t, pose }
    }

    #[test]
    fn identical_poses_cost_nothing() {
        let mut mask = [0u32; GRID_H];
        mask[5] = 0b1_1111;
        let a = pose(mask, [0.5, 0.5], [0.4, 0.2], 0.9, 0.1, [10, 20, 30]);
        assert!(pose_cost(&a, &a).abs() < 1e-6);
    }

    #[test]
    fn cost_is_symmetric() {
        let mut left = [0u32; GRID_H];
        left[4] = 0b1111;
        let mut right = [0u32; GRID_H];
        right[7] = 0b11_0000;
        let a = pose(left, [0.3, 0.4], [0.2, 0.1], 0.8, -0.2, [200, 10, 10]);
        let b = pose(right, [0.6, 0.7], [0.5, 0.3], 0.95, 0.3, [10, 10, 200]);
        assert!((pose_cost(&a, &b) - pose_cost(&b, &a)).abs() < 1e-6);
    }

    #[test]
    fn transparent_frames_describe_as_empty() {
        let rgba = vec![0u8; GRID_W * GRID_H * 4];
        let pose = describe_frame(&rgba);
        assert!(pose.is_empty());
        assert_eq!(pose, Pose::default());
    }

    #[test]
    #[should_panic(expected = "RGBA grid")]
    fn describe_frame_rejects_a_truncated_grid() {
        // A short buffer used to be walked with `get` and silently described as
        // a half-empty frame; it is a caller bug, not a pose.
        describe_frame(&vec![0u8; GRID_W * GRID_H * 4 - 4]);
    }

    #[test]
    fn pick_entry_stays_inside_the_window_and_skips_without_gain() {
        let mut mask = [0u32; GRID_H];
        mask[2] = 0b111;
        let target = pose(mask, [0.5, 0.5], [0.5, 0.3], 0.4, 0.0, [100, 100, 100]);
        // Close enough to frame 0 that it stays inside the window, but clearly
        // worse than the target.
        let shifted = pose(mask, [0.5, 0.56], [0.5, 0.3], 0.4, 0.0, [100, 100, 100]);
        // Far from frame 0, so the window ends before it.
        let mut drifting = [0u32; GRID_H];
        drifting[15] = 0b11_0000_0000_0000;
        let drifted = pose(drifting, [0.9, 0.9], [0.9, 0.9], 1.0, 0.5, [0, 0, 0]);

        // The drifting frame ends the window, so the perfect match after it is
        // not a candidate; the best frame inside the window wins instead.
        let window = vec![frame(0, shifted.clone()), frame(41, target.clone()), frame(82, drifted), frame(123, target.clone())];
        assert_eq!(pick_entry(&target, &window, ENTRY_MIN_GAIN), 1);

        // Frame 0 already matches perfectly, so no gain is worth skipping.
        let window = vec![frame(0, target.clone()), frame(41, target.clone())];
        assert_eq!(pick_entry(&target, &window, ENTRY_MIN_GAIN), 0);

        // A closer later frame wins.
        let window = vec![frame(0, shifted), frame(41, target.clone())];
        assert_eq!(pick_entry(&target, &window, ENTRY_MIN_GAIN), 1);

        // Empty windows fall back to frame 0.
        assert_eq!(pick_entry(&target, &[], ENTRY_MIN_GAIN), 0);
    }
}
