//! whale-pet: a small always-on-top desktop pet rendered with egui.
//!
//! Usage:
//!   whale-pet [--asset-dir <dir>] [--stats] [--no-force-x11]
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
    --no-force-x11      Do not force the X11/XWayland backend on Linux
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
    let mut options = RunOptions { stats: false, force_x11: true };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--asset-dir" => {
                let value = args.next().with_context(|| "--asset-dir needs a directory")?;
                asset_dir = Some(PathBuf::from(value));
            }
            "--stats" => options.stats = true,
            "--no-force-x11" => options.force_x11 = false,
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => bail!("unknown argument {other:?} (try --help)"),
        }
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

    pet_ui_egui::run(asset_dir, config, options).map_err(|err| anyhow::anyhow!("eframe: {err}"))
}
