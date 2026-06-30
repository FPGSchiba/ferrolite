//! GPU edit nodes: `upload_source` (graph root upload), `SourceNode`,
//! and the generic `PointOpNode<U>` compute pass.

use ferrolite_gpu::{GpuContext, Node};
use ferrolite_image::LinearRgbaF32;
use half::f16;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use wgpu::util::DeviceExt;

use crate::image::{PipelineImage, PIPELINE_FORMAT};

/// Upload a display-linear `f32` image as an `Rgba16Float` GPU texture (the
/// pipeline source). Mirrors the VT's single-texture upload (f32 -> f16).
pub fn upload_source(ctx: &GpuContext, img: &LinearRgbaF32) -> PipelineImage {
    let texels: Vec<f16> = img.pixels.iter().map(|&v| f16::from_f32(v)).collect();
    let texture = ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("pipeline-source"),
            size: wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PIPELINE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&texels),
    );
    PipelineImage {
        texture: Arc::new(texture),
        width: img.width,
        height: img.height,
    }
}

/// Graph root: returns the pre-uploaded source image (ignores inputs).
pub(crate) struct SourceNode {
    image: PipelineImage,
}

impl SourceNode {
    pub(crate) fn new(ctx: &GpuContext, src: &LinearRgbaF32) -> Self {
        Self {
            image: upload_source(ctx, src),
        }
    }
}

impl Node<PipelineImage> for SourceNode {
    fn evaluate(&self, _inputs: &[&PipelineImage]) -> PipelineImage {
        self.image.clone()
    }
}

/// Bind-group layout shared by every point-op compute pass:
/// 0 = input texture, 1 = output storage texture, 2 = params uniform.
pub(crate) fn point_op_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("point-op-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: PIPELINE_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn point_op_pipeline(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    wgsl: &str,
    label: &str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module: &module,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    })
}

/// A single point-op compute pass. Owns its (once-built) pipeline + a reusable
/// output texture; reads its current params from a shared `Cell` each evaluate.
pub(crate) struct PointOpNode<U: bytemuck::Pod> {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    params: Rc<Cell<U>>,
    out: RefCell<Option<PipelineImage>>,
}

impl<U: bytemuck::Pod> PointOpNode<U> {
    pub(crate) fn new(ctx: Arc<GpuContext>, wgsl: &str, label: &str, params: Rc<Cell<U>>) -> Self {
        let bgl = point_op_bgl(&ctx.device);
        let pipeline = point_op_pipeline(&ctx.device, &bgl, wgsl, label);
        let uniform_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: std::mem::size_of::<U>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            params,
            out: RefCell::new(None),
        }
    }

    /// Allocate (or reuse) the output texture matching `(w,h)`.
    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("point-op-out"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PIPELINE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            *out = Some(PipelineImage {
                texture: Arc::new(tex),
                width: w,
                height: h,
            });
        }
        out.as_ref().unwrap().clone()
    }
}

impl<U: bytemuck::Pod> Node<PipelineImage> for PointOpNode<U> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let dst = self.ensure_out(src.width, src.height);

        // Current params -> uniform buffer.
        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&self.params.get()));

        let src_view = src
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("point-op-bind"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&dst_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniform_buf.as_entire_binding(),
                    },
                ],
            });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("point-op-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(src.width.div_ceil(8), src.height.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        dst
    }
}
