//! Apple hardware video decoding and Metal interop. Unsafe ownership is isolated here.
#![cfg(target_os = "macos")]

use anyhow::{Result, anyhow, bail};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{MTLTexture, MTLTextureType};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    ptr::NonNull,
    sync::Arc,
};

unsafe extern "C" {
    fn pet_decoder_open(path: *const c_char, duration: *mut f64, error: *mut c_char, len: usize) -> *mut c_void;
    fn pet_decoder_next(decoder: *mut c_void, buffer: *mut *mut c_void, pts: *mut f64, error: *mut c_char, len: usize) -> i32;
    fn pet_decoder_close(decoder: *mut c_void);
    fn pet_buffer_release(buffer: *mut c_void);
    fn pet_buffer_width(buffer: *mut c_void) -> usize;
    fn pet_buffer_height(buffer: *mut c_void) -> usize;
    fn pet_texture_cache(device: *mut c_void) -> *mut c_void;
    fn pet_texture(cache: *mut c_void, buffer: *mut c_void, texture: *mut *mut c_void) -> *mut c_void;
}

struct CfOwned(NonNull<c_void>);
// These handles exclusively own retained, immutable CVPixelBuffer/CVMetalTexture
// objects (or a thread-safe CVMetalTextureCache). No CPU mapping or mutation is exposed.
unsafe impl Send for CfOwned {}
unsafe impl Sync for CfOwned {}
impl Drop for CfOwned {
    fn drop(&mut self) {
        unsafe { pet_buffer_release(self.0.as_ptr()) }
    }
}

#[derive(Clone)]
pub struct PixelBuffer {
    owner: Arc<CfOwned>,
    pub width: u32,
    pub height: u32,
}

/// Confined to the decoder thread; every output buffer owns its pool lease.
pub struct Decoder {
    handle: NonNull<c_void>,
    pub duration: f64,
}
impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_encoded_bytes())?;
        let mut error = [0; 1024];
        let mut duration = 0.0;
        let handle = unsafe { pet_decoder_open(path.as_ptr(), &mut duration, error.as_mut_ptr(), error.len()) };
        let handle = NonNull::new(handle).ok_or_else(|| native_error(&error))?;
        log::info!("HEVC-alpha VideoToolbox hardware=true, IOSurface -> Metal: {}", path.to_string_lossy());
        Ok(Self { handle, duration })
    }
    pub fn next_frame(&mut self) -> Result<Option<(PixelBuffer, f64)>> {
        let mut error = [0; 1024];
        let mut buffer = std::ptr::null_mut();
        let mut pts = 0.0;
        let result = unsafe { pet_decoder_next(self.handle.as_ptr(), &mut buffer, &mut pts, error.as_mut_ptr(), error.len()) };
        match result {
            0 => Ok(None),
            1 => {
                let buffer = NonNull::new(buffer).ok_or_else(|| anyhow!("decoder returned a null buffer"))?;
                let owner = Arc::new(CfOwned(buffer));
                let (width, height) = unsafe { (pet_buffer_width(buffer.as_ptr()) as u32, pet_buffer_height(buffer.as_ptr()) as u32) };
                Ok(Some((PixelBuffer { owner, width, height }, pts)))
            }
            _ => Err(native_error(&error)),
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { pet_decoder_close(self.handle.as_ptr()) }
    }
}
fn native_error(error: &[c_char]) -> anyhow::Error {
    anyhow!("{}", unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy())
}

/// A persistent egui texture. Video buffers are imported without CPU pixel access;
/// a GPU pass converts BGRA to egui's RGBA8 texture format.
pub struct Renderer {
    state: egui_wgpu::RenderState,
    cache: CfOwned,
    pipeline: wgpu::ComputePipeline,
    output: Option<wgpu::Texture>,
    id: Option<egui::TextureId>,
}
impl Renderer {
    pub fn new(state: egui_wgpu::RenderState) -> Result<Self> {
        let device = &state.device;
        // The cache must use this exact wgpu device, never a separately created MTLDevice.
        let cache = unsafe {
            let metal = device.as_hal::<wgpu::hal::api::Metal>().ok_or_else(|| anyhow!("HEVC rendering requires the Metal backend"))?;
            pet_texture_cache(Retained::as_ptr(metal.raw_device()) as *mut c_void)
        };
        let cache = CfOwned(NonNull::new(cache).ok_or_else(|| anyhow!("cannot create CVMetalTextureCache"))?);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pet video conversion"),
            source: wgpu::ShaderSource::Wgsl(include_str!("video.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pet video conversion"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(Self { state, cache, pipeline, output: None, id: None })
    }
    pub fn id(&self) -> Option<egui::TextureId> {
        self.id
    }
    pub fn update(&mut self, frame: &PixelBuffer) -> Result<()> {
        let device = &self.state.device;
        let mut raw = std::ptr::null_mut();
        let owner = unsafe { pet_texture(self.cache.0.as_ptr(), frame.owner.0.as_ptr(), &mut raw) };
        let owner = CfOwned(NonNull::new(owner).ok_or_else(|| anyhow!("CVMetalTexture import failed"))?);
        if raw.is_null() {
            bail!("CVMetalTexture has no Metal texture");
        }
        let extent = wgpu::Extent3d { width: frame.width, height: frame.height, depth_or_array_layers: 1 };
        let desc = wgpu::TextureDescriptor {
            label: Some("HEVC IOSurface"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        // Decoder finished before publishing the buffer. The cache used this device;
        // dimensions/format match CVMetalTextureCreate. Retain both CV objects until
        // wgpu retires the HAL resource after its submitted GPU work completes.
        let input = unsafe {
            let raw = Retained::<ProtocolObject<dyn MTLTexture>>::retain(raw.cast()).ok_or_else(|| anyhow!("cannot retain Metal texture"))?;
            let buffer = frame.clone();
            let hal = wgpu::hal::metal::Device::texture_from_raw(
                raw,
                desc.format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent { width: frame.width, height: frame.height, depth: 1 },
                Some(Box::new(move || {
                    drop(owner);
                    drop(buffer);
                })),
            );
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(hal, &desc, wgpu::TextureUses::RESOURCE)
        };
        if self.output.as_ref().is_none_or(|t| t.size() != extent) {
            let output = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pet video RGBA"),
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                ..desc
            });
            let view = output.create_view(&Default::default());
            let mut renderer = self.state.renderer.write();
            if let Some(id) = self.id {
                renderer.update_egui_texture_from_wgpu_texture(device, &view, wgpu::FilterMode::Linear, id);
            } else {
                self.id = Some(renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear));
            }
            self.output = Some(output);
        }
        let input_view = input.create_view(&Default::default());
        let output_view = self.output.as_ref().unwrap().create_view(&Default::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pet video"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&input_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&output_view) },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(frame.width.div_ceil(8), frame.height.div_ceil(8), 1);
        }
        self.state.queue.submit([encoder.finish()]);
        Ok(())
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            self.state.renderer.write().free_texture(&id);
        }
    }
}
