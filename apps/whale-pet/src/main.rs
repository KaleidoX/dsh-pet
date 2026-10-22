//! whale-pet: a small always-on-top desktop pet rendered with egui.
//!
//! Usage:
//!   whale-pet [--asset-dir <dir>] [--stats] [--force-x11 | --force-wayland]
//!
//! On a Wayland session the pet runs natively on a layer surface; if that
//! fails it falls back to XWayland. `--force-x11` skips the native path,
//! `--force-wayland` makes a native failure fatal.
//!
//! The asset directory holds `config.jsonc` and a `webm/` folder with the
//! animation clips. It is resolved from `--asset-dir`, then
//! `WHALE_PET_ASSET_DIR`, then `<exe>/assets`, then the in-repo `dsh-pet/assets`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use pet_config::{PetConfig, resolve_asset_dir};
use pet_ui_egui::RunOptions;

const HELP: &str = "\
whale-pet — egui desktop pet

USAGE:
    whale-pet [OPTIONS]

OPTIONS:
    --asset-dir <DIR>   Asset root containing config.jsonc and webm/
                        (default: $WHALE_PET_ASSET_DIR, <exe>/assets, or the repo copy)
    --stats             Log fps, CPU and memory once per second (Linux)
    --force-x11         Always use X11/XWayland, even on a Wayland session
    --force-wayland     Require the native Wayland layer surface; fail instead of
                        falling back to XWayland (debugging)
    --no-force-x11      Accepted for compatibility; not forcing X11 is the default
    -h, --help          Print this help

CONTROLS:
    Drag the pet to move the window; click it for a reaction.
    Ctrl+Q quits.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("whale-pet: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut asset_dir: Option<PathBuf> = None;
    let mut stats = false;
    let mut force_x11 = false;
    let mut force_wayland = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--asset-dir" => {
                let value = args.next().with_context(|| "--asset-dir needs a directory")?;
                asset_dir = Some(PathBuf::from(value));
            }
            "--stats" => stats = true,
            "--force-x11" => force_x11 = true,
            "--force-wayland" => force_wayland = true,
            // Kept so existing launchers keep working: before the native
            // Wayland host existed, this turned off the default X11 forcing.
            // That is now the default.
            "--no-force-x11" => {}
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => bail!("unknown argument {other:?} (try --help)"),
        }
    }
    if force_x11 && force_wayland {
        bail!("--force-x11 and --force-wayland are mutually exclusive");
    }

    let asset_dir = resolve_asset_dir(asset_dir)?;
    let config = match PetConfig::load(&asset_dir) {
        Ok(config) => config,
        Err(err) => {
            log::warn!("cannot load config.jsonc from {}: {err:#}; using defaults", asset_dir.display());
            PetConfig::default()
        }
    };
    log::info!("asset directory {}", asset_dir.display());

    let mut fallback_to_x11 = false;
    #[cfg(target_os = "linux")]
    if !force_x11 && std::env::var_os("WAYLAND_DISPLAY").is_some() {
        match pet_wayland::run(asset_dir.clone(), config.clone(), stats) {
            Ok(()) => return Ok(()),
            Err(err) if force_wayland => return Err(err.context("native Wayland")),
            Err(err) => {
                // The native host may have failed for a reason that does not
                // affect winit (a missing layer shell, no Vulkan adapter), so
                // XWayland is attempted even though Wayland still looks
                // available. On failure eframe would otherwise pick Wayland
                // again and fail the same way.
                log::warn!("native Wayland failed ({err:#}); falling back to XWayland");
                fallback_to_x11 = true;
            }
        }
    }
    if force_wayland {
        bail!("--force-wayland was given but no Wayland session is available");
    }

    let force_x11 = force_x11 || fallback_to_x11;
    pet_ui_egui::run(asset_dir, config, RunOptions { stats, force_x11 }).map_err(|err| anyhow::anyhow!("eframe: {err}"))
}
