//! wgpu surface + egui renderer on the layer surface.
//!
//! The surface is built from raw `wl_display` / `wl_surface` pointers because
//! wgpu has no safe constructor for a Wayland surface outside winit. The unsafe
//! part lives in [`crate::ffi`]; this module only assembles the safe calls.
//!
//! Sizing contract with the rest of the crate: the swapchain is always
//! `CANVAS_W × CANVAS_H` physical pixels. Fractional scaling is expressed with a
//! `wp_viewport` destination rectangle set by `surface.rs` (protocol: buffer size
//! = surface size × scale, and `wl_surface` buffer scale stays 1), so the canvas
//! keeps its native pixel size at 1.0x, 1.5x and 2.0x alike.

use anyhow::{Context as _, Result, anyhow};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use pet_config::{CANVAS_H, CANVAS_W};
use wayland_client::Connection;
use wayland_client::protocol::wl_surface::WlSurface;
use wgpu::{
    CompositeAlphaMode, Device, Instance, InstanceDescriptor, PresentMode, Queue, RenderPassColorAttachment, RenderPassDescriptor, Surface,
    SurfaceConfiguration, SurfaceTargetUnsafe, TextureFormat, TextureUsages,
};

/// Everything needed to turn egui's tessellated output into a frame.
pub(crate) struct Wgpu {
    /// Declared before `instance`: fields drop in declaration order, so the
    /// surface is torn down while the instance that created it is still alive.
    /// The raw `wl_surface`/`wl_display` it points into are owned by
    /// `WaylandApp`, which drops its `Wgpu` before both.
    surface: Surface<'static>,
    /// Held for teardown order and for backends that keep per-instance state:
    /// wgpu's `Surface` only borrows the instance context, and the display
    /// handle passed at creation points into the Wayland connection.
    #[allow(dead_code)]
    instance: Instance,
    device: Device,
    queue: Queue,
    renderer: Renderer,
    format: TextureFormat,
    config: SurfaceConfiguration,
}

impl Wgpu {
    /// Creates the surface and the renderer for `wl_surface`.
    ///
    /// `wl_surface` and the connection it came from must outlive the returned
    /// value; `WaylandApp` declares the `gpu` field before the `LayerSurface`
    /// and its last `Connection` clone to guarantee that.
    pub(crate) fn new(conn: &Connection, wl_surface: &WlSurface) -> Result<Self> {
        let display = crate::ffi::DisplayPtr::new(conn)?;
        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(display)));

        let raw_window_handle = crate::ffi::window_handle(wl_surface)?;
        // SAFETY: `raw_window_handle` points at a live `wl_surface`, and the
        // instance carries the `wl_display` those handles belong to; wgpu
        // requires them to be identical, and both come from `conn`. The surface
        // is declared before the wgpu objects in `WaylandApp`, so it outlives
        // them.
        let surface = unsafe {
            instance
                .create_surface_unsafe(SurfaceTargetUnsafe::RawHandle { raw_display_handle: None, raw_window_handle })
                .context("create a wgpu surface for the layer surface")?
        };

        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { compatible_surface: Some(&surface), ..Default::default() }))
                .context("no Vulkan or GLES adapter accepts the layer surface")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).context("request a wgpu device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| format == &TextureFormat::Bgra8Unorm)
            .or_else(|| caps.formats.iter().copied().find(|format| format == &TextureFormat::Rgba8Unorm))
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| anyhow!("the surface reports no texture format"))?;
        let alpha_mode = pick_alpha_mode(&caps.alpha_modes);

        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: CANVAS_W,
            height: CANVAS_H,
            present_mode: PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, format, RendererOptions::default());

        log::info!("wgpu on Wayland: {:?} adapter, {format:?}, {alpha_mode:?} alpha, formats {:?}", adapter.get_info().backend, caps.formats.len());

        Ok(Self { surface, instance, device, queue, renderer, format, config })
    }

    /// Renders one egui frame and presents it.
    ///
    /// Texture uploads happen before the pass and frees after it, as
    /// `FullOutput::textures_delta` requires. `viewport` must already have been
    /// configured on the `wl_surface` by the caller: `Queue::present` attaches
    /// and commits in one go on Wayland, so anything set afterwards would only
    /// affect the next frame.
    pub(crate) fn draw(
        &mut self,
        paint_jobs: &[egui::ClippedPrimitive],
        screen: &ScreenDescriptor,
        textures: &mut egui::TexturesDelta,
    ) -> Result<()> {
        // Texture uploads do not touch the swapchain, so they are applied even
        // on frames that end up skipped. `TexturesDelta` asserts on drop that
        // every delta was handled, so the only correct exit is `clear()`.
        for (id, deltas) in &textures.set {
            for delta in deltas {
                self.renderer.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        textures.set.clear();

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.surface.configure(&self.device, &self.config);
                frame
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.free_textures(textures);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                self.free_textures(textures);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.rebuild_surface();
                self.free_textures(textures);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                log::error!("wgpu rejected the surface texture request");
                self.free_textures(textures);
                return Ok(());
            }
        };

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("whale-pet") });
        let user_cmds = self.renderer.update_buffers(&self.device, &self.queue, &mut encoder, paint_jobs, screen);
        {
            let attachment = RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            };
            let desc = RenderPassDescriptor { label: Some("whale-pet"), color_attachments: &[Some(attachment)], ..Default::default() };
            let mut pass = encoder.begin_render_pass(&desc).forget_lifetime();
            self.renderer.render(&mut pass, paint_jobs, screen);
        }

        self.queue.submit(user_cmds.into_iter().chain(std::iter::once(encoder.finish())));
        self.queue.present(frame);

        self.free_textures(textures);
        Ok(())
    }

    /// Releases the textures egui dropped this frame and empties the delta.
    fn free_textures(&mut self, textures: &mut egui::TexturesDelta) {
        for id in &textures.free {
            self.renderer.free_texture(id);
        }
        textures.free.clear();
    }

    /// Recovers from a lost swapchain.
    ///
    /// A truly lost surface cannot be rebuilt from here: the raw `wl_surface`
    /// belongs to `WaylandApp`, and `Surface<'static>` already erased the
    /// handle. In practice libwayland only reports `Lost` with a still-valid
    /// `wl_surface`, so reconfiguring reallocates the swapchain; if that is
    /// not enough the next frame reports `Validation` and is skipped rather
    /// than taking the pet down.
    fn rebuild_surface(&mut self) {
        log::warn!("wgpu surface lost; reconfiguring the swapchain");
        self.surface.configure(&self.device, &self.config);
    }
}

impl std::fmt::Debug for Wgpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wgpu").field("format", &self.format).finish_non_exhaustive()
    }
}

/// Prefers premultiplied alpha (what egui-wgpu's blending produces), then
/// `Inherit` (compositor decides), then opaque.
fn pick_alpha_mode(modes: &[CompositeAlphaMode]) -> CompositeAlphaMode {
    if modes.contains(&CompositeAlphaMode::PreMultiplied) {
        CompositeAlphaMode::PreMultiplied
    } else if modes.contains(&CompositeAlphaMode::Inherit) {
        CompositeAlphaMode::Inherit
    } else {
        log::warn!("the surface offers no alpha-capable composite mode; the pet will have an opaque background");
        modes.first().copied().unwrap_or(CompositeAlphaMode::Opaque)
    }
}
