//! Generic GPU histogram compute pass over an `Rgba16Float` texture. Engine-tier:
//! it takes the working→display transform as a plain row-major `[[f32;3];3]` and
//! hardcodes the sRGB OETF (matching `display.wgsl`) — no photo concepts. Fills a
//! `256 × {R,G,B,luma}` bin buffer via atomics in display-referred space and reads
//! back only the ~4 KB buffer (never the image), per CLAUDE.md §1.

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::pipelines::pack_display_matrix;

pub const HIST_BINS: usize = 256;
pub const HIST_CHANNELS: usize = 4; // R, G, B, luma
pub const HIST_LEN: usize = HIST_BINS * HIST_CHANNELS; // 1024
const BINS_BYTES: u64 = (HIST_LEN * std::mem::size_of::<u32>()) as u64; // 4096

/// Quantize a display-referred `[0,1]` value to a `[0,255]` bin (round-to-nearest,
/// clamped). MUST match the WGSL `bin_index` in `histogram.wgsl`.
pub fn bin_index(v: f32) -> u32 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5).clamp(0.0, 255.0) as u32
}

/// Uniform for the histogram pass: the working→display 3×3 (WGSL column-major,
/// padded) + the image dims. 64 bytes: mat3x3 (48) + vec2<u32> (8, at offset 48)
/// + 8 pad, matching WGSL `struct Params { m: mat3x3<f32>, dims: vec2<u32> }`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HistParams {
    m: [[f32; 4]; 3],
    dims: [u32; 2],
    _pad: [u32; 2],
}

/// Once-built histogram compute pipeline + its 4 KB bin/staging buffers. Reused
/// for every recompute (CLAUDE.md: build pipelines once). `dispatch` submits its
/// own command buffer; `read_async` maps the staging buffer without blocking.
pub struct HistogramPipeline {
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    bins: wgpu::Buffer,
    staging: Arc<wgpu::Buffer>,
    params: wgpu::Buffer,
}

impl HistogramPipeline {
    pub fn new(ctx: &GpuContext) -> Self {
        let device = &ctx.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-histogram"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/histogram.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vt-histogram-bgl"),
            entries: &[
                // 0: source texture (read via textureLoad; non-filterable).
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // 1: atomic bins storage (read_write).
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(BINS_BYTES),
                    },
                    count: None,
                },
                // 2: params uniform (matrix + dims).
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<HistParams>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vt-histogram-layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vt-histogram"),
            layout: Some(&layout),
            module: &module,
            entry_point: "bin",
            compilation_options: Default::default(),
            cache: None,
        });
        let bins = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-histogram-bins"),
            size: BINS_BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let staging = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-histogram-staging"),
            size: BINS_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }));
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-histogram-params"),
            size: std::mem::size_of::<HistParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bgl,
            bins,
            staging,
            params,
        }
    }

    /// Zero the bins, bin `texture` (display-referred, via `display_matrix` +
    /// sRGB OETF), then copy the 4 KB bin buffer to the staging buffer. Submits
    /// its own command buffer. Call `read_async` afterwards to fetch the result.
    pub fn dispatch(
        &self,
        ctx: &GpuContext,
        texture: &wgpu::Texture,
        dims: (u32, u32),
        display_matrix: [[f32; 3]; 3],
    ) {
        let params = HistParams {
            m: pack_display_matrix(display_matrix),
            dims: [dims.0, dims.1],
            _pad: [0, 0],
        };
        ctx.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-histogram-bind"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.bins.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vt-histogram-enc"),
            });
        enc.clear_buffer(&self.bins, 0, None);
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vt-histogram-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(dims.0.div_ceil(8), dims.1.div_ceil(8), 1);
        }
        enc.copy_buffer_to_buffer(&self.bins, 0, &self.staging, 0, BINS_BYTES);
        ctx.queue.submit([enc.finish()]);
    }

    /// Map the staging buffer asynchronously and hand the 1024-entry bin vector to
    /// `on_ready` when the GPU work completes and the device is polled. Never
    /// blocks. The caller must keep the device polled (`Maintain::Poll`) until the
    /// callback fires (the app does this while a readback is in flight).
    pub fn read_async(&self, on_ready: impl FnOnce(Vec<u32>) + Send + 'static) {
        let staging = self.staging.clone();
        self.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |res| {
                if res.is_err() {
                    return;
                }
                let data = staging.slice(..).get_mapped_range();
                let bins: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&data).to_vec();
                drop(data);
                staging.unmap();
                on_ready(bins);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::bin_index;

    #[test]
    fn bin_index_maps_range_and_clamps() {
        assert_eq!(bin_index(0.0), 0);
        assert_eq!(bin_index(1.0), 255);
        assert_eq!(bin_index(-0.5), 0, "below range clamps to 0");
        assert_eq!(bin_index(2.0), 255, "above range clamps to 255");
        // Round-to-nearest: 0.5 * 255 = 127.5 -> 128.
        assert_eq!(bin_index(0.5), 128);
    }
}
