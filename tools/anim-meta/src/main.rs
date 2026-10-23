//! Offline pose metadata for pose-matched hard cuts.
//!
//! Decodes every `.webm` clip in an asset root and writes `anim-meta.json` next
//! to it: for each clip, the entry window (frames near the start that still
//! look like frame 0) and the last frame, as compact pose descriptors. The app
//! uses them to enter a clip at the frame that best matches the pose on screen
//! when the previous clip ends naturally.
//!
//! The decode path is `pet-decode`'s, the same one the app runs, so the
//! descriptors cannot drift between generation and playback. Frames are
//! downsampled to the descriptor grid with swscale's area filter — each cell is
//! the average of its 20x20 block — before `pet-model::describe_frame` sees
//! them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling::{context::Context as Scaler, flag::Flags};
use ffmpeg::util::frame::video::Video;
use ffmpeg_next as ffmpeg;
use pet_config::{ANIM_META_FILE, AnimMeta, AnimMetaDoc, ClipMeta, Frame, GRID_H, GRID_W, PetConfig, Pose, WEBM_DIR, clip_path, resolve_asset_dir};
use pet_decode::{RgbaFrame, decode_all};
use pet_model::{ENTRY_EPS, ENTRY_MAX_FRAMES, ENTRY_MIN_GAIN, describe_frame, pick_entry, pose_cost};

const HELP: &str = "\
anim-meta — generate pose metadata for whale-pet animation clips

USAGE:
    anim-meta [--asset-dir <DIR>]

OPTIONS:
    --asset-dir <DIR>   Asset root containing webm/ (default: $WHALE_PET_ASSET_DIR,
                        <exe>/assets, or the repo copy)
    -h, --help          Print this help

Writes <asset-dir>/anim-meta.json. The app loads it at startup and falls back to
entering every clip at frame 0 when it is missing.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("anim-meta: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let mut asset_dir = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--asset-dir" => asset_dir = Some(PathBuf::from(args.next().context("--asset-dir needs a value")?)),
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => bail!("unknown argument {other:?}; try --help"),
        }
    }
    let asset_dir = resolve_asset_dir(asset_dir)?;
    let clips = list_clips(&asset_dir)?;
    if clips.is_empty() {
        bail!("no .webm clips under {}", asset_dir.join(WEBM_DIR).display());
    }
    eprintln!("anim-meta: {} clips in {}", clips.len(), asset_dir.join(WEBM_DIR).display());

    let started = Instant::now();
    let mut meta = AnimMeta::default();
    let mut resizer = Resizer::new()?;
    for (index, name) in clips.iter().enumerate() {
        let clip = describe_clip(&clip_path(&asset_dir, name), &mut resizer).with_context(|| format!("describe {name:?}"))?;
        eprintln!("  [{:>3}/{}] {name}: {} frames, entry {} frames", index + 1, clips.len(), clip.frames, clip.entry.len());
        meta.clips.insert(name.clone(), clip);
    }
    let elapsed = started.elapsed();

    warn_unlisted(&asset_dir, &meta);
    report(&meta, &clips);
    write(&asset_dir, &meta)?;
    self_check(&asset_dir, &meta)?;
    let size = std::fs::metadata(asset_dir.join(ANIM_META_FILE)).map(|meta| meta.len()).unwrap_or(0);
    eprintln!("anim-meta: wrote {size} bytes to {} in {:.1}s", asset_dir.join(ANIM_META_FILE).display(), elapsed.as_secs_f32());
    Ok(())
}

/// Every clip name in `webm/`, sorted so the output does not depend on the
/// file system's ordering.
fn list_clips(asset_dir: &Path) -> Result<Vec<String>> {
    let dir = asset_dir.join(WEBM_DIR);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "webm")
            && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
        {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Decodes one clip and keeps only what the metadata needs: the entry window
/// and the last frame.
///
/// Frames between the entry window and the end are decoded (VP9 needs them)
/// but not even downsampled; the last one is held by value and described once.
fn describe_clip(path: &Path, resizer: &mut Resizer) -> Result<ClipMeta> {
    let mut entry: Vec<Frame> = Vec::with_capacity(ENTRY_MAX_FRAMES);
    let mut window_open = true;
    let mut last: Option<(RgbaFrame, u32)> = None;
    let mut last_t = 0;
    let mut error: Option<anyhow::Error> = None;

    let info = decode_all(path, |frame, pts_us| {
        let t = u32::try_from(pts_us).unwrap_or(u32::MAX);
        last_t = t;
        if window_open && entry.len() < ENTRY_MAX_FRAMES {
            match resizer.describe(&frame) {
                Ok(pose) => {
                    let inside = entry.first().is_none_or(|first| pose_cost(&first.pose, &pose) <= ENTRY_EPS);
                    if inside {
                        entry.push(Frame { t, pose });
                    } else {
                        // The clip has visibly started moving; the window is
                        // the prefix that still looks like frame 0.
                        window_open = false;
                    }
                }
                Err(err) => {
                    error = Some(err);
                    return false;
                }
            }
        }
        last = Some((frame, t));
        true
    })
    .with_context(|| format!("decode {}", path.display()))?;
    if let Some(err) = error {
        return Err(err);
    }

    let (last_frame, last_t) = last.context("clip decoded no frames")?;
    let last = Frame { t: last_t, pose: resizer.describe(&last_frame)? };
    if entry.is_empty() {
        // The first decoded frame always opens the window (its cost to itself
        // is 0), so an empty window means every frame failed to describe.
        bail!("no frame of the clip could be described");
    }
    let duration_us = info.duration.map_or(last_t, |duration| u32::try_from(duration.as_micros()).unwrap_or(u32::MAX));
    Ok(ClipMeta { frames: info.frames, duration_us, entry, last })
}

/// Downsamples native frames to the descriptor grid.
struct Resizer {
    scaler: Option<Scaler>,
    input: Video,
    rgba: Video,
    pixels: Vec<u8>,
}

impl Resizer {
    fn new() -> Result<Self> {
        ffmpeg::init()?;
        Ok(Self { scaler: None, input: Video::empty(), rgba: Video::empty(), pixels: Vec::new() })
    }

    fn describe(&mut self, frame: &RgbaFrame) -> Result<Pose> {
        let (width, height) = (frame.width as u32, frame.height as u32);
        if self.input.width() != width || self.input.height() != height {
            self.input = Video::new(Pixel::RGBA, width, height);
            self.scaler = None;
        }
        let stride = self.input.stride(0);
        for (row, line) in frame.pixels.chunks_exact(frame.width * 4).enumerate() {
            self.input.data_mut(0)[row * stride..row * stride + frame.width * 4].copy_from_slice(line);
        }
        let scaler = match self.scaler.as_mut() {
            Some(scaler) => scaler,
            None => self.scaler.insert(Scaler::get(Pixel::RGBA, width, height, Pixel::RGBA, GRID_W as u32, GRID_H as u32, Flags::AREA)?),
        };
        scaler.run(&self.input, &mut self.rgba)?;
        let stride = self.rgba.stride(0);
        let row_bytes = GRID_W * 4;
        self.pixels.clear();
        self.pixels.reserve(GRID_H * row_bytes);
        for row in 0..GRID_H {
            let start = row * stride;
            self.pixels.extend_from_slice(&self.rgba.data(0)[start..start + row_bytes]);
        }
        Ok(describe_frame(&self.pixels))
    }
}

/// Warns about clips the config references but the metadata does not cover.
fn warn_unlisted(asset_dir: &Path, meta: &AnimMeta) {
    let Ok(config) = PetConfig::load(asset_dir) else { return };
    let animations = &config.animations;
    let mut referenced = BTreeSet::new();
    for pool in [&animations.idle, &animations.turn, &animations.drag, &animations.clicks] {
        referenced.extend(pool.iter().cloned());
    }
    referenced.extend(animations.moves.actions.iter().map(|action| action.name.clone()));
    for category in &animations.categories {
        referenced.extend(category.actions.iter().cloned());
    }
    let missing = referenced.into_iter().filter(|name| !meta.clips.contains_key(name)).collect::<Vec<_>>();
    if !missing.is_empty() {
        eprintln!("anim-meta: warning: {} clip(s) referenced by config.jsonc have no metadata:", missing.len());
        for name in missing {
            eprintln!("  {name}");
        }
    }
}

/// Prints the distributions the thresholds were calibrated against.
fn report(meta: &AnimMeta, clips: &[String]) {
    let windows = meta.clips.values().map(|clip| clip.entry.len()).collect::<Vec<_>>();
    let at_cap = windows.iter().filter(|len| **len >= ENTRY_MAX_FRAMES).count();
    let mut sorted = windows.clone();
    sorted.sort_unstable();
    let at = |values: &[f32], q: f32| {
        if values.is_empty() { 0.0 } else { values[((values.len() - 1) as f32 * q) as usize] }
    };
    println!(
        "entry windows: {at_cap}/{} clips at the {ENTRY_MAX_FRAMES}-frame cap, min {}, median {}",
        windows.len(),
        sorted[0],
        sorted[sorted.len() / 2]
    );

    let mut costs = Vec::new();
    let mut gains = Vec::new();
    let mut skipped = 0;
    for from in clips {
        let Some(last) = meta.last_pose(from) else { continue };
        for to in clips {
            // A clip never follows itself; counting those pairs would
            // overstate the skip rate by the diagonal's share.
            if from == to {
                continue;
            }
            let Some(entry) = meta.entry(to) else { continue };
            let first = pose_cost(last, &entry[0].pose);
            costs.push(first);
            let index = pick_entry(last, entry, ENTRY_MIN_GAIN);
            if index > 0 {
                skipped += 1;
                gains.push(first - pose_cost(last, &entry[index].pose));
            }
        }
    }
    if costs.is_empty() {
        return;
    }
    costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    gains.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let total = costs.len();
    println!(
        "natural-end cost (last frame -> frame 0): p50 {:.3} p90 {:.3} p99 {:.3} max {:.3}",
        at(&costs, 0.5),
        at(&costs, 0.9),
        at(&costs, 0.99),
        costs[total - 1]
    );
    let mean_gain = if gains.is_empty() { 0.0 } else { gains.iter().sum::<f32>() / gains.len() as f32 };
    let p90_gain = if gains.is_empty() { 0.0 } else { at(&gains, 0.9) };
    println!(
        "entry choice: {total} transitions, {:.0}% skip frame 0 (threshold {ENTRY_MIN_GAIN}); mean gain when skipping {mean_gain:.3} (p90 {p90_gain:.3})",
        skipped as f32 / total as f32 * 100.0,
    );
}

/// Writes the document.
fn write(asset_dir: &Path, meta: &AnimMeta) -> Result<()> {
    let path = asset_dir.join(ANIM_META_FILE);
    let json = AnimMetaDoc::new(meta.clone()).to_json();
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))
}

/// Reads the file back through the app's loader and compares it to what was
/// written.
fn self_check(asset_dir: &Path, meta: &AnimMeta) -> Result<()> {
    let loaded = AnimMeta::load(asset_dir).context("the metadata did not load back")?;
    if loaded.clips.len() != meta.clips.len() {
        bail!("read back {} clips, wrote {}", loaded.clips.len(), meta.clips.len());
    }
    for (name, clip) in &meta.clips {
        let back = loaded.clips.get(name).with_context(|| format!("clip {name:?} did not load back"))?;
        if back.frames != clip.frames || back.duration_us != clip.duration_us || back.entry.len() != clip.entry.len() {
            bail!("clip {name:?} did not survive the round trip");
        }
        for pair in back.entry.windows(2) {
            if pair[1].t < pair[0].t {
                bail!("clip {name:?} window is not ordered");
            }
        }
        if back.entry.len() > ENTRY_MAX_FRAMES || back.last.t < back.entry[back.entry.len() - 1].t {
            bail!("clip {name:?} has an inconsistent window");
        }
    }
    // The loaded descriptors must be the ones that were written, to the
    // quantisation the format allows.
    let first = meta.clips.values().next().and_then(|clip| clip.entry.first());
    if let Some(first) = first
        && let Some(loaded_first) = loaded.clips.values().next().and_then(|clip| clip.entry.first())
        && (pose_cost(&first.pose, &loaded_first.pose) > 1e-3 || first.t != loaded_first.t)
    {
        bail!("descriptors do not survive the round trip");
    }
    eprintln!("anim-meta: self-check passed");
    Ok(())
}
