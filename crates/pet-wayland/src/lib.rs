//! Native Wayland host for whale-pet, built on `zwlr_layer_shell_v1`.
//!
//! Why a separate host instead of eframe: a `xdg_toplevel` under a Wayland
//! session cannot place itself, stay above other windows, or restrict its input
//! region — the three things a desktop pet needs. A layer surface gets all three
//! from the protocol: position from anchor + margin, always above the normal
//! window layer, and `wl_surface.set_input_region()` for click-through.
//!
//! The pet's behaviour lives in `pet-ui-core`; this crate only owns the window,
//! the input translation and the rendering path:
//!
//! ```text
//! wayland events ──► input.rs   (wl_pointer/wl_keyboard → egui events)
//!                        │
//!                        ├──► egui::Context::run_ui  ──► PetCore::ui
//!                        │            │
//!                        └──► surface.rs (anchor/margin/input region, viewport)
//!                                     │
//!                                     └──► wgpu.rs (egui-wgpu on the wl_surface)
//! ```
//!
//! Coordinate spaces, matching `pet-ui-core`'s contract:
//!
//! - The layer surface includes room for controls. PetScene offsets its fixed
//!   640×360 physical canvas inside that rectangle; egui uses logical points.
//! - `FrameInput::global` and `FrameOutput::move_to` are canvas (= physical)
//!   pixels; `surface.rs` converts them to margins when the window is dragged.
//!
//! Everything here is Linux-only. On other targets [`run`] returns an error and
//! the caller keeps using the eframe host, so a Windows cross build still
//! type-checks.

#[cfg(target_os = "linux")]
mod app;
#[cfg(target_os = "linux")]
mod ffi;
#[cfg(target_os = "linux")]
mod input;
#[cfg(target_os = "linux")]
mod surface;
#[cfg(target_os = "linux")]
mod wgpu;

use std::path::PathBuf;

use pet_config::PetConfig;

/// Runs the pet on a native Wayland layer surface until it exits.
///
/// Returns an error when the session has no usable layer shell (another
/// compositor, a missing protocol, no Vulkan/GLES device). The caller is
/// expected to fall back to the X11 path; nothing is left behind on failure.
#[cfg(target_os = "linux")]
pub fn run(asset_dir: PathBuf, config: PetConfig, stats: bool) -> anyhow::Result<()> {
    let conn = wayland_client::Connection::connect_to_env()?;
    let (globals, mut event_queue) = wayland_client::globals::registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor = smithay_client_toolkit::compositor::CompositorState::bind(&globals, &qh)?;
    let layer_shell = smithay_client_toolkit::shell::wlr_layer::LayerShell::bind(&globals, &qh)?;

    let mut event_loop = calloop::EventLoop::<app::WaylandApp>::try_new()?;
    let loop_handle = event_loop.handle();

    let mut app = app::WaylandApp::new(&conn, &globals, &qh, compositor, layer_shell, asset_dir, &config, stats)?;

    // The initial configure and the output geometry arrive as events; one
    // synchronous roundtrip gets both, so the first frame already has the right
    // size and anchor instead of flashing at scale 1.
    event_queue.roundtrip(&mut app)?;

    calloop_wayland_source::WaylandSource::new(conn, event_queue).insert(loop_handle)?;

    let mut repaint_at = std::time::Instant::now();
    loop {
        let timeout = repaint_at.saturating_duration_since(std::time::Instant::now());
        event_loop.dispatch(Some(timeout), &mut app)?;
        if app.should_exit() {
            break;
        }
        if app.dirty() || std::time::Instant::now() >= repaint_at {
            app.set_dirty(false);
            app.draw()?;
            repaint_at = std::time::Instant::now() + app.next_repaint();
        }
    }

    Ok(())
}

/// Non-Linux stub: keep the workspace building where the real host cannot exist.
#[cfg(not(target_os = "linux"))]
pub fn run(_asset_dir: PathBuf, _config: PetConfig, _stats: bool) -> anyhow::Result<()> {
    anyhow::bail!("the native Wayland host is only implemented on Linux")
}
