//! Configuration for the whale-pet desktop app.
//!
//! The parsed subset mirrors what the upstream web client reads: animation
//! pools, weights, movement parameters, physics and the first pet's placement.
//! Unknown JSONC keys are ignored, so the upstream `assets/config.jsonc` can be
//! pointed at directly.
//!
//! # World units
//!
//! Upstream lays the pet out at `PET_REF_WIDTH` (462) CSS pixels wide no matter
//! how the window is scaled, so its distances, speeds and accelerations are in
//! "pet-relative" units. whale-pet always draws the canvas at 640x360 physical
//! pixels, which is [`WORLD_SCALE`] times larger. Every spatial constant from
//! upstream is therefore multiplied by that factor before use; time constants
//! and dimensionless ratios are not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// Canvas width of every animation clip, in pixels. Clips are authored at this
/// size and are drawn unscaled.
pub const CANVAS_W: u32 = 640;
/// Canvas height of every animation clip, in pixels.
pub const CANVAS_H: u32 = 360;

/// Upstream's reference pet width (`PET_REF_WIDTH` in `src/shared/constants.ts`).
pub const PET_REF_WIDTH: f32 = 462.0;
/// Canvas pixels per upstream layout unit.
pub const WORLD_SCALE: f32 = CANVAS_W as f32 / PET_REF_WIDTH;

/// Pointer hit box in canvas pixels (`HIT_BOX` in `src/shared/constants.ts`).
pub const HIT_BOX: HitBox = HitBox { x0: 200, y0: 50, x1: 440, y1: 335 };

/// Y of the pet's feet on the canvas (`FEET_Y`); the idle pose is shifted down
/// by [`BOTTOM_PAD`] so the feet rest on the window's bottom edge, and the
/// drag pose lifts the pet by that amount (matching the upstream desktop
/// sprite's stage `translateY(bottomPad)`).
pub const FEET_Y: i32 = 330;

/// Height of the stage offset below the pet's feet, in canvas pixels.
pub const BOTTOM_PAD: f32 = (CANVAS_H as i32 - FEET_Y) as f32;

/// How far the body may stick out past the window box on either side, in canvas
/// pixels: upstream `HIT_BOX.x0 / CANVAS_W * size`, i.e. the transparent side
/// margin of the clip. Throw and roam bounds keep the *body* on screen.
pub const SIDE_ALLOW: f32 = HIT_BOX.x0 as f32;

/// Pointer travel (logical pixels) before a press becomes a drag.
pub const DRAG_THRESHOLD: f32 = 5.0;

/// Q-squash depth and duration (`SQ_SQUASH`, `SQ_DURATION_MS` in `pet.ts`).
pub const SQ_SQUASH: f32 = 0.55;
pub const SQ_DURATION_SECS: f32 = 0.220;

/// Drag spring constants (`SPRING_K`, `SPRING_C` in `pet.ts`).
pub const SPRING_K: f32 = 200.0;
pub const SPRING_C: f32 = 30.0;

/// Cross-fade of the two video layers on a clip switch (`transition: opacity
/// .18s` in the web client).
pub const CROSSFADE_SECS: f32 = 0.18;

/// Subdirectory (below the asset root) that holds the `.webm` clips.
pub const WEBM_DIR: &str = "webm";
/// Scene configuration file inside the asset root.
pub const CONFIG_FILE: &str = "config.jsonc";
/// Font file shipped with the assets (upstream `assets/fonts/上首软糖体.ttf`).
pub const FONT_FILE: &str = "fonts/上首软糖体.ttf";
/// Environment variable that overrides the asset root.
pub const ASSET_DIR_ENV: &str = "WHALE_PET_ASSET_DIR";

/// A rectangle in canvas pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitBox {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl HitBox {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 as f32 && x <= self.x1 as f32 && y >= self.y0 as f32 && y <= self.y1 as f32
    }

    pub const fn width(&self) -> u32 {
        (self.x1 - self.x0) as u32
    }

    pub const fn height(&self) -> u32 {
        (self.y1 - self.y0) as u32
    }
}

/// The monitor work area the pet lives in: origin plus size, canvas pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorldArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WorldArea {
    pub fn right(self) -> i32 {
        self.x + self.width as i32
    }

    pub fn bottom(self) -> i32 {
        self.y + self.height as i32
    }
}

/// Parsed subset of `config.jsonc`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PetConfig {
    pub animations: Animations,
    pub physics: Physics,
    /// Parsed but unused: the app runs a single pet on a single monitor, where
    /// confining to the screen makes no difference.
    #[serde(rename = "confineToScreen")]
    pub confine_to_screen: bool,
    pub font: Font,
    /// Random-chain weights (percent). A pet entry may override them.
    #[serde(rename = "animationWeights")]
    pub animation_weights: AnimationWeights,
    pub pets: Vec<PetEntry>,
}

/// Weights of the random chain, in percent. What is left of 100 goes to a
/// weighted category action.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct AnimationWeights {
    pub idle: f32,
    pub turn: f32,
    #[serde(rename = "move")]
    pub walk: f32,
}

impl Default for AnimationWeights {
    fn default() -> Self {
        Self { idle: 10.0, turn: 5.0, walk: 5.0 }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Animations {
    pub idle: Vec<String>,
    pub turn: Vec<String>,
    pub drag: Vec<String>,
    pub clicks: Vec<String>,
    pub moves: Moves,
    pub categories: Vec<Category>,
    /// Event pools (balance / whisper / work status). Parsed so the menu can be
    /// told apart from dsh-driven clips, but never played: this app has no dsh
    /// integration.
    pub events: BTreeMap<String, Vec<EventSlot>>,
}

/// One event slot: a fixed clip name or a list of candidates.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum EventSlot {
    Fixed(String),
    Candidates(Vec<String>),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Moves {
    #[serde(rename = "default")]
    pub defaults: MoveParams,
    pub actions: Vec<MoveAction>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct MoveParams {
    #[serde(rename = "minDist")]
    pub min_dist: f32,
    #[serde(rename = "maxDist")]
    pub max_dist: f32,
    pub margin: f32,
    #[serde(rename = "leadSec")]
    pub lead_sec: f32,
    #[serde(rename = "tailSec")]
    pub tail_sec: f32,
}

impl Default for MoveParams {
    fn default() -> Self {
        Self { min_dist: 60.0, max_dist: 240.0, margin: 20.0, lead_sec: 2.0, tail_sec: 2.0 }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MoveAction {
    pub name: String,
    pub params: MoveParamsOverride,
}

/// Per-action overrides: missing keys fall back to [`Moves::defaults`], exactly
/// like `Object.assign({}, moves.default, action.params)` upstream.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct MoveParamsOverride {
    #[serde(rename = "minDist")]
    pub min_dist: Option<f32>,
    #[serde(rename = "maxDist")]
    pub max_dist: Option<f32>,
    pub margin: Option<f32>,
    #[serde(rename = "leadSec")]
    pub lead_sec: Option<f32>,
    #[serde(rename = "tailSec")]
    pub tail_sec: Option<f32>,
}

impl MoveParams {
    pub fn with_override(self, over: &MoveParamsOverride) -> Self {
        Self {
            min_dist: over.min_dist.unwrap_or(self.min_dist),
            max_dist: over.max_dist.unwrap_or(self.max_dist),
            margin: over.margin.unwrap_or(self.margin),
            lead_sec: over.lead_sec.unwrap_or(self.lead_sec),
            tail_sec: over.tail_sec.unwrap_or(self.tail_sec),
        }
    }

    /// Converts the spatial fields from the 462-wide reference space to canvas
    /// pixels. Durations are untouched.
    pub fn scaled(self, factor: f32) -> Self {
        Self {
            min_dist: self.min_dist * factor,
            max_dist: self.max_dist * factor,
            margin: self.margin * factor,
            lead_sec: self.lead_sec,
            tail_sec: self.tail_sec,
        }
    }
}

impl MoveAction {
    pub fn resolved(&self, defaults: MoveParams) -> MoveParams {
        defaults.with_override(&self.params)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Category {
    pub id: String,
    pub weight: f32,
    #[serde(rename = "noMirror")]
    pub no_mirror: bool,
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Physics {
    /// Gravity in upstream layout units per second squared.
    pub gravity: f32,
    /// Bounce restitution, 0..=1.
    pub restitution: f32,
    /// Ground friction per second.
    #[serde(rename = "groundFriction")]
    pub ground_friction: f32,
    #[serde(rename = "ceilingBounce")]
    pub ceiling_bounce: bool,
    /// Drag spring force multiplier. Must be > 0.
    #[serde(rename = "throwPower")]
    pub throw_power: f32,
    /// Parsed only: a single pet never collides with another.
    #[serde(rename = "petCollision")]
    pub pet_collision: bool,
}

impl Default for Physics {
    fn default() -> Self {
        Self { gravity: 1400.0, restitution: 0.78, ground_friction: 2.5, ceiling_bounce: true, throw_power: 1.0, pet_collision: false }
    }
}

impl Physics {
    /// Same values with out-of-range fields replaced by the defaults, after
    /// warning about each. The upstream client treats these as configuration
    /// errors and falls back the same way.
    pub fn sanitized(&self) -> Self {
        let defaults = Self::default();
        let mut out = self.clone();
        if !out.gravity.is_finite() || out.gravity < 0.0 {
            log::warn!("physics.gravity {} is invalid; using {}", out.gravity, defaults.gravity);
            out.gravity = defaults.gravity;
        }
        if !out.restitution.is_finite() || !(0.0..=1.0).contains(&out.restitution) {
            log::warn!("physics.restitution {} is invalid; using {}", out.restitution, defaults.restitution);
            out.restitution = defaults.restitution;
        }
        if !out.ground_friction.is_finite() || out.ground_friction < 0.0 {
            log::warn!("physics.groundFriction {} is invalid; using {}", out.ground_friction, defaults.ground_friction);
            out.ground_friction = defaults.ground_friction;
        }
        if !out.throw_power.is_finite() || out.throw_power <= 0.0 {
            log::warn!("physics.throwPower {} is invalid; using {}", out.throw_power, defaults.throw_power);
            out.throw_power = defaults.throw_power;
        }
        out
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Font {
    /// Preset id (`candy`, `yahei`, ...). Only `candy` ships with the assets;
    /// everything else falls back to the system stack.
    pub family: String,
    /// Bubble font size in logical pixels at the reference pet size.
    pub size: f32,
    /// Whether the font scales with the pet size. The pet size is fixed here,
    /// so this only matters in the formula's shape.
    #[serde(rename = "followPet")]
    pub follow_pet: bool,
}

impl Default for Font {
    fn default() -> Self {
        Self { family: "candy".to_owned(), size: 21.0, follow_pet: true }
    }
}

impl Font {
    /// Tooltip font size in logical pixels, mirroring `resolveFontPx` in
    /// `src/shared/fonts.ts` (`TIP_FONT_BASE_PX = 0.026 * 462`).
    pub fn tooltip_size(&self) -> f32 {
        let size = if self.size.is_finite() { self.size.clamp(4.0, 200.0) } else { 21.0 };
        0.026 * PET_REF_WIDTH * (size / 21.0)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PetEntry {
    pub id: String,
    pub name: Option<String>,
    /// Width in the upstream plugin's scaled layout. Ignored: the canvas is
    /// always drawn at 640x360 physical pixels (462 is the reference size all
    /// behaviour is normalised to).
    pub size: Option<f32>,
    /// Freeze the random chain: `turn` and `move` rolls have zero weight.
    #[serde(rename = "fixedEnabled")]
    pub fixed_enabled: bool,
    /// Per-pet override of the top-level weights (`pet.animationWeights ||
    /// cfg.animationWeights` upstream).
    #[serde(rename = "animationWeights")]
    pub animation_weights: Option<AnimationWeights>,
    pub position: Option<Position>,
}

/// Where the pet window sits on the primary monitor.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Position {
    pub corner: String,
    #[serde(rename = "marginX")]
    pub margin_x: i32,
    #[serde(rename = "marginY")]
    pub margin_y: i32,
}

impl Default for Position {
    fn default() -> Self {
        Self { corner: "top-right".to_owned(), margin_x: 24, margin_y: 100 }
    }
}

impl PetConfig {
    /// Loads `config.jsonc` from an asset root. Missing keys fall back to
    /// defaults; syntax errors are reported to the caller.
    pub fn load(asset_dir: &Path) -> Result<Self> {
        let path = asset_dir.join(CONFIG_FILE);
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let config = serde_json_lenient::from_str(&text).with_context(|| format!("parse {} (JSONC)", path.display()))?;
        Ok(config)
    }

    /// Placement of the first pet, or the upstream defaults when unspecified.
    pub fn position(&self) -> Position {
        self.pets.first().and_then(|pet| pet.position.clone()).unwrap_or_default()
    }

    /// Display name for the tooltip; falls back to the id.
    pub fn pet_name(&self) -> String {
        let pet = self.pets.first();
        pet.and_then(|pet| pet.name.clone()).filter(|name| !name.is_empty()).or_else(|| pet.map(|pet| pet.id.clone())).unwrap_or_default()
    }

    /// Whether the random chain may turn or walk.
    pub fn fixed(&self) -> bool {
        self.pets.first().is_some_and(|pet| pet.fixed_enabled)
    }

    /// Random-chain weights for the first pet: its own override, else the
    /// top-level block.
    pub fn weights(&self) -> AnimationWeights {
        self.pets.first().and_then(|pet| pet.animation_weights).unwrap_or(self.animation_weights)
    }

    /// Spring force multiplier for dragging. Invalid values keep the default.
    pub fn throw_power(&self) -> f32 {
        self.physics.sanitized().throw_power
    }

    /// Validated physics in upstream units. Callers scale spatial values with
    /// [`WORLD_SCALE`].
    pub fn physics(&self) -> Physics {
        self.physics.sanitized()
    }
}

/// Resolves the asset root: CLI flag, environment, `<exe>/assets`, then the
/// in-repo `dsh-pet/assets` for development builds.
pub fn resolve_asset_dir(cli: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = cli {
        ensure_asset_root(&dir, "--asset-dir")?;
        return Ok(dir);
    }
    if let Some(dir) = std::env::var_os(ASSET_DIR_ENV).map(PathBuf::from) {
        ensure_asset_root(&dir, ASSET_DIR_ENV)?;
        return Ok(dir);
    }

    let mut tried = Vec::new();

    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        let candidate = exe_dir.join("assets");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }

    let candidate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dsh-pet/assets");
    if candidate.is_dir() {
        return Ok(candidate);
    }
    tried.push(candidate);

    let listed = tried.iter().map(|path| format!("  {}", path.display())).collect::<Vec<_>>().join("\n");
    bail!("asset directory not found; pass --asset-dir <dir> or set {ASSET_DIR_ENV}. Tried:\n{listed}");
}

fn ensure_asset_root(dir: &Path, source: &str) -> Result<()> {
    if !dir.is_dir() {
        bail!("{source}: {} is not a directory", dir.display());
    }
    if !dir.join(WEBM_DIR).is_dir() {
        log::warn!("{} has no {WEBM_DIR}/ directory; animation playback will fail", dir.display());
    }
    Ok(())
}

/// Path of one animation clip inside the asset root.
pub fn clip_path(asset_dir: &Path, name: &str) -> PathBuf {
    asset_dir.join(WEBM_DIR).join(format!("{name}.webm"))
}
