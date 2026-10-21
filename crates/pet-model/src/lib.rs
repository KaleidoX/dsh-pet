//! Animation selection and motion math shared by the UI.
//!
//! The semantics mirror the upstream web client:
//! - clicks pick from the click pool without excluding the current clip
//!   (`pick(petAnims.clicks)` in `pet.ts`);
//! - every other transition picks from a pool while excluding the current
//!   clip, falling back to the full pool when the exclusion empties it;
//! - dragging and clicks play once, then fall back to the idle pool.

use pet_config::{PetConfig, SPRING_C, SPRING_K};

/// What the currently playing clip represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipRole {
    Idle,
    Drag,
    Click,
}

const DEFAULT_IDLE: &[&str] = &["待机呼吸休闲"];
const DEFAULT_DRAG: &[&str] = &["被鼠标拖拽悬空反馈"];
const DEFAULT_CLICKS: &[&str] = &["点击回应-开心跃动", "点击回应-害羞惊讶", "点击回应-傲娇生气", "点击回应-挠痒咯咯笑", "点击回应-元气挥手"];

/// Random animation pools, with the built-in defaults filling empty pools so
/// the app works without a usable `config.jsonc`.
#[derive(Debug, Clone)]
pub struct AnimPools {
    idle: Vec<String>,
    drag: Vec<String>,
    clicks: Vec<String>,
}

impl AnimPools {
    pub fn from_config(config: &PetConfig) -> Self {
        let animations = &config.animations;
        Self {
            idle: fill(&animations.idle, DEFAULT_IDLE),
            drag: fill(&animations.drag, DEFAULT_DRAG),
            clicks: fill(&animations.clicks, DEFAULT_CLICKS),
        }
    }

    /// Idle clip, avoiding `exclude` when the pool has an alternative.
    pub fn pick_idle(&self, exclude: Option<&str>) -> Option<String> {
        pick(&self.idle, exclude)
    }

    /// Clip played while the pet is held.
    pub fn pick_drag(&self) -> Option<String> {
        pick(&self.drag, None)
    }

    /// Clip played on a click; upstream does not exclude the current clip.
    pub fn pick_click(&self) -> Option<String> {
        pick(&self.clicks, None)
    }

    pub fn idle_count(&self) -> usize {
        self.idle.len()
    }
}

fn fill(pool: &[String], defaults: &[&str]) -> Vec<String> {
    if pool.is_empty() { defaults.iter().map(|name| (*name).to_owned()).collect() } else { pool.to_vec() }
}

fn pick(pool: &[String], exclude: Option<&str>) -> Option<String> {
    if pool.is_empty() {
        return None;
    }
    let filtered: Vec<&String> = pool.iter().filter(|name| Some(name.as_str()) != exclude).collect();
    let source: Vec<&String> = if filtered.is_empty() { pool.iter().collect() } else { filtered };
    let index = fastrand::usize(0..source.len());
    Some(source[index].clone())
}

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
    v + ((target - x) * SPRING_K - v * SPRING_C) * power * dt
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
