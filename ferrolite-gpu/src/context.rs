//! wgpu device handle wrapper. In the app it borrows eframe's device; for tests
//! it spins up a headless adapter (returning None when none is available so
//! GPU tests skip cleanly in headless CI). Engine-transferable.

use std::sync::Arc;

pub struct GpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
}

impl GpuContext {
    /// Borrow eframe's already-created device/queue (the app path).
    pub fn from_render_state(rs: &egui_wgpu::RenderState) -> Self {
        Self {
            device: rs.device.clone(),
            queue: rs.queue.clone(),
        }
    }

    /// Create a standalone headless context for tests. Returns `None` if no
    /// adapter is available (e.g. CI runners without a GPU) so callers skip.
    pub fn headless() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("ferrolite-headless"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .ok()?;
        Some(Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
        })
    }

    /// A `RENDER_ATTACHMENT | COPY_SRC` texture for offscreen rendering.
    pub fn render_target(&self, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ferrolite-render-target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Copy an `Rgba8` texture to the CPU (row-unpadded). For golden tests.
    pub fn read_rgba8(&self, tex: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
        let bpr_unpadded = w * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bpr_padded = bpr_unpadded.div_ceil(align) * align;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bpr_padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr_padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((bpr_unpadded * h) as usize);
        for row in 0..h {
            let start = (row * bpr_padded) as usize;
            out.extend_from_slice(&data[start..start + bpr_unpadded as usize]);
        }
        drop(data);
        buf.unmap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_context_can_make_and_read_a_render_target() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let tex = ctx.render_target(4, 4, wgpu::TextureFormat::Rgba8Unorm);
        // Clear it to opaque red via a render pass, then read it back.
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        ctx.queue.submit([enc.finish()]);
        let pixels = ctx.read_rgba8(&tex, 4, 4);
        assert_eq!(pixels.len(), 4 * 4 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]); // first texel red
    }
}
