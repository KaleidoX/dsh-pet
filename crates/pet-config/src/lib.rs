//! Configuration for the whale-pet desktop app.
//!
//! Only the subset used by the v1 feature set is parsed — animation pools,
//! `physics.throwPower` and `pets[0].position`. Unknown JSONC keys are ignored,
//! so the upstream `assets/config.jsonc` can be pointed at directly.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// Canvas width of every animation clip, in pixels. Clips are authored at this
/// size and are drawn unscaled.
pub const CANVAS_W: u32 = 640;
/// Canvas height of every animation clip, in pixels.
pub const CANVAS_H: u32 = 360;

/// Pointer hit box in canvas pixels (`HIT_BOX` in `src/shared/constants.ts`).
pub const HIT_BOX: HitBox = HitBox { x0: 200, y0: 50, x1: 440, y1: 335 };

/// Y of the pet's feet on the canvas (`FEET_Y`); the idle pose is shifted down
/// by `CANVAS_H - FEET_Y` so the feet rest on the window's bottom edge, and the
/// drag pose lifts the pet by that amount (matching the web client's stage
/// `translateY(bottomPad)`).
pub const FEET_Y: i32 = 330;

/// Vertical offset of the idle pose, in canvas pixels.
pub const IDLE_OFFSET_PX: f32 = (CANVAS_H as i32 - FEET_Y) as f32;

/// Pointer travel (logical pixels) before a press becomes a drag.
pub const DRAG_THRESHOLD: f32 = 5.0;

/// Q-squash depth and duration (`SQ_SQUASH`, `SQ_DURATION_MS` in `pet.ts`).
pub const SQ_SQUASH: f32 = 0.55;
pub const SQ_DURATION_SECS: f32 = 0.220;

/// Drag spring constants (`SPRING_K`, `SPRING_C` in `pet.ts`).
pub const SPRING_K: f32 = 200.0;
pub const SPRING_C: f32 = 30.0;

/// Subdirectory (below the asset root) that holds the `.webm` clips.
pub const WEBM_DIR: &str = "webm";
/// Scene configuration file inside the asset root.
pub const CONFIG_FILE: &str = "config.jsonc";
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

/// Parsed subset of `config.jsonc`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PetConfig {
    pub animations: Animations,
    pub physics: Physics,
    pub pets: Vec<PetEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Animations {
    pub idle: Vec<String>,
    pub turn: Vec<String>,
    pub drag: Vec<String>,
    pub clicks: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Physics {
    /// Drag spring force multiplier (`physics.throwPower`). Must be > 0.
    #[serde(rename = "throwPower")]
    pub throw_power: f32,
}

impl Default for Physics {
    fn default() -> Self {
        Self { throw_power: 1.0 }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PetEntry {
    pub id: String,
    pub name: Option<String>,
    /// Width in the upstream plugin's scaled layout. Unused here: v1 always
    /// draws the 640x360 canvas at its native size.
    pub size: Option<f32>,
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

    /// Spring force multiplier for dragging. Invalid values keep the default.
    pub fn throw_power(&self) -> f32 {
        let power = self.physics.throw_power;
        if power.is_finite() && power > 0.0 { power } else { Physics::default().throw_power }
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
