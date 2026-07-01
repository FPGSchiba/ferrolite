//! GPU edit nodes: `upload_source` (graph root upload), `SourceNode`,
//! and the generic `PointOpNode<U>` compute pass.

use ferrolite_gpu::{GpuContext, Node};
use ferrolite_image::{haloed_tile_extent, tile_pixel_origin, LinearRgbaF32, TileCoord};
use half::f16;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use wgpu::util::DeviceExt;

use crate::gpu_pyramid::GpuPyramidSource;
use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::op::Geometry;
use crate::uniforms::{geometry_tile_uniform, GeometryUniform};

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

/// One-shot camera/sRGB→working color pass: upload `src`, run a single
/// `color_matrix.wgsl` pass, return the working-space texture. Cheaper than a
/// full `EditPipeline` for the preview's initial color conversion (one upload,
/// one pass). Uses the shared shader cache (built once) via `PointOpNode`.
pub fn color_convert(
    ctx: std::sync::Arc<GpuContext>,
    src: &LinearRgbaF32,
    matrix: [[f32; 3]; 3],
) -> PipelineImage {
    let source = upload_source(&ctx, src);
    let params = std::rc::Rc::new(std::cell::Cell::new(crate::uniforms::color_matrix_uniform(
        matrix,
    )));
    let node = PointOpNode::new(
        ctx,
        include_str!("shaders/color_matrix.wgsl"),
        "preview-color-convert",
        params,
    );
    node.evaluate(&[&source])
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
    module: &wgpu::ShaderModule,
    label: &str,
) -> wgpu::ComputePipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module,
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
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        wgsl: &'static str,
        label: &str,
        params: Rc<Cell<U>>,
    ) -> Self {
        let bgl = point_op_bgl(&ctx.device);
        let module = ctx.shader_module(label, wgsl);
        let pipeline = point_op_pipeline(&ctx.device, &bgl, &module, label);
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
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
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

/// Bind-group layout for the geometry pass: 0 = input texture (filterable),
/// 1 = output storage texture, 2 = transform uniform, 3 = filtering sampler.
fn geometry_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("geometry-bgl"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// Geometry compute pass (crop + rotate). Output texture dims come from the
/// uniform's `out_dims`, so it reallocates when the crop changes.
pub(crate) struct GeometryNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    params: Rc<Cell<crate::uniforms::GeometryUniform>>,
    out: RefCell<Option<PipelineImage>>,
}

impl GeometryNode {
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        params: Rc<Cell<crate::uniforms::GeometryUniform>>,
    ) -> Self {
        let bgl = geometry_bgl(&ctx.device);
        let module = ctx.shader_module("geometry", include_str!("shaders/geometry.wgsl"));
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("geometry"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("geometry"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        let uniform_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geometry-uniform"),
            size: std::mem::size_of::<crate::uniforms::GeometryUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("geometry-samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            sampler,
            params,
            out: RefCell::new(None),
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("geometry-out"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PIPELINE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
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

impl Node<PipelineImage> for GeometryNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let u = self.params.get();
        let out_w = (u.out_dims[0] as u32).max(1);
        let out_h = (u.out_dims[1] as u32).max(1);
        let dst = self.ensure_out(out_w, out_h);

        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

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
                label: Some("geometry-bind"),
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
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("geometry-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(out_w.div_ceil(8), out_h.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        dst
    }
}

/// Bind-group layout for the tone-curve pass: 0 = input texture,
/// 1 = output storage texture, 2 = 256-entry LUT (read-only storage buffer).
fn curve_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("curve-bgl"),
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
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

/// Tone-curve compute pass. Owns its (once-built) pipeline + a 256-entry LUT
/// storage buffer; re-reads its LUT from a shared `Cell` each evaluate.
pub(crate) struct CurveNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    lut_buf: wgpu::Buffer,
    lut: Rc<Cell<[f32; 256]>>,
    out: RefCell<Option<PipelineImage>>,
}

impl CurveNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, lut: Rc<Cell<[f32; 256]>>) -> Self {
        let bgl = curve_bgl(&ctx.device);
        let module = ctx.shader_module("tone-curve", include_str!("shaders/tone_curve.wgsl"));
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("tone-curve"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("tone-curve"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        let lut_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tone-curve-lut"),
            size: (std::mem::size_of::<f32>() * 256) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            ctx,
            pipeline,
            bgl,
            lut_buf,
            lut,
            out: RefCell::new(None),
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("curve-out"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PIPELINE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
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

impl Node<PipelineImage> for CurveNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let dst = self.ensure_out(src.width, src.height);

        let lut = self.lut.get();
        // `[f32; 256]: Pod` via bytemuck's const-generic array impl.
        self.ctx
            .queue
            .write_buffer(&self.lut_buf, 0, bytemuck::bytes_of(&lut));

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
                label: Some("curve-bind"),
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
                        resource: self.lut_buf.as_entire_binding(),
                    },
                ],
            });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("curve-pass"),
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

/// The current tile request driving the geometry head (coord + active halo).
#[derive(Clone, Copy)]
pub(crate) struct TileRequest {
    pub coord: TileCoord,
    pub halo: u32,
}

/// Root node for the per-tile edit pipeline: samples the `GpuPyramidSource` LOD
/// for the current `TileRequest` through the geometry transform (geometry at the
/// head), producing a `(ext×ext)` haloed, geometrically-resampled tile in output
/// space. The color chain runs downstream of it.
pub(crate) struct GeometryHeadNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    source: Arc<GpuPyramidSource>,
    geometry: Geometry,
    request: Rc<Cell<TileRequest>>,
    out: RefCell<Option<PipelineImage>>,
}

impl GeometryHeadNode {
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        source: Arc<GpuPyramidSource>,
        geometry: Geometry,
        request: Rc<Cell<TileRequest>>,
    ) -> Self {
        let bgl = geometry_bgl(&ctx.device); // reuse the geometry pass bind layout
        let module = ctx.shader_module("geometry", include_str!("shaders/geometry.wgsl"));
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("geometry-head"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("geometry-head"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        let uniform_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geometry-head-uniform"),
            size: std::mem::size_of::<GeometryUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("geometry-head-samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            sampler,
            source,
            geometry,
            request,
            out: RefCell::new(None),
        }
    }

    fn ensure_out(&self, ext: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((ext, ext)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("geometry-head-out"),
                size: wgpu::Extent3d {
                    width: ext,
                    height: ext,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PIPELINE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            *out = Some(PipelineImage {
                texture: Arc::new(tex),
                width: ext,
                height: ext,
            });
        }
        out.as_ref().unwrap().clone()
    }
}

impl Node<PipelineImage> for GeometryHeadNode {
    fn evaluate(&self, _inputs: &[&PipelineImage]) -> PipelineImage {
        let req = self.request.get();
        let lod = req.coord.lod;
        let src = self.source.level(lod);
        let (sw, sh) = self.source.level_size(lod);
        let ext = haloed_tile_extent(req.halo);
        let dst = self.ensure_out(ext);

        // Haloed output-tile origin at this LOD (interior origin minus halo).
        let (ox, oy) = tile_pixel_origin(req.coord);
        let out_origin = (ox as f32 - req.halo as f32, oy as f32 - req.halo as f32);
        let u = geometry_tile_uniform(Some(self.geometry), sw, sh, out_origin, ext);
        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

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
                label: Some("geometry-head-bind"),
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
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("geometry-head-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(ext.div_ceil(8), ext.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        dst
    }
}
