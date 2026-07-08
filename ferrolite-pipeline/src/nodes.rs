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
use crate::lens_gpu::{VignetteTexture, WarpGridTexture};
use crate::op::Geometry;
use crate::uniforms::{geometry_tile_uniform, GeometryUniform, LensUniform, VignetteUniform};

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
/// 1 = output storage texture, 2 = transform uniform, 3 = filtering sampler,
/// 4 = warp texture A (`rgba32float` `[rU,rV,gU,gV]`, non-filterable), 5 = warp
/// texture B (`rg32float` `[bU,bV]`, non-filterable), 6 = lens uniform. The two
/// warp textures are `textureLoad`-sampled (no sampler) with manual bilinear in
/// the shader — see `geometry.wgsl` and `lens_gpu.rs` for the rationale.
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
            // Warp textures are unfilterable f32 (device lacks FLOAT32_FILTERABLE);
            // sampled via textureLoad, so declared as non-filterable float.
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
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

/// The lens-warp resources every geometry node owns: the current warp grid
/// (default `identity`), its two `textureLoad`-only texture views built ONCE per
/// grid swap (never per frame — CLAUDE.md GPU rule), and a `LensUniform` buffer
/// (default `use_warp = 0`, i.e. the byte-identical no-correction path).
struct WarpBinding {
    warp: WarpGridTexture,
    a_view: wgpu::TextureView,
    b_view: wgpu::TextureView,
    lens_buf: wgpu::Buffer,
}

impl WarpBinding {
    /// Default identity warp + `use_warp = 0` lens uniform. Valid to bind before
    /// any bake completes; the shader skips the grid sample entirely.
    fn new(ctx: &GpuContext) -> Self {
        let warp = WarpGridTexture::identity(ctx);
        let a_view = warp.rg_ba_view();
        let b_view = warp.b_uv_view();
        let lens_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geometry-lens-uniform"),
            size: std::mem::size_of::<LensUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(
            &lens_buf,
            0,
            bytemuck::bytes_of(&LensUniform {
                dist_amount: 0.0,
                tca_amount: 0.0,
                vig_amount: 0.0,
                use_warp: 0,
            }),
        );
        Self {
            warp,
            a_view,
            b_view,
            lens_buf,
        }
    }

    /// Swap in a freshly baked warp grid (bake-time, infrequent). Rebuilds the
    /// cached texture views; callers must also recreate any cached bind group.
    fn set_warp(&mut self, warp: WarpGridTexture) {
        self.a_view = warp.rg_ba_view();
        self.b_view = warp.b_uv_view();
        self.warp = warp;
    }

    /// Overwrite the lens uniform (amounts + `use_warp`). Buffer write only — no
    /// view, bind group, or pipeline rebuild.
    fn set_lens_uniform(&self, ctx: &GpuContext, lens: LensUniform) {
        ctx.queue
            .write_buffer(&self.lens_buf, 0, bytemuck::bytes_of(&lens));
    }
}

/// Geometry compute pass (crop + rotate, optionally fused with the lens warp).
/// Output texture dims come from the uniform's `out_dims`, so it reallocates when
/// the crop changes.
pub(crate) struct GeometryNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    warp: RefCell<WarpBinding>,
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
        let warp = RefCell::new(WarpBinding::new(&ctx));
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            sampler,
            warp,
            params,
            out: RefCell::new(None),
        }
    }

    /// Swap in a freshly baked warp grid (bake-time; rebuilds cached views).
    pub(crate) fn set_warp(&self, warp: WarpGridTexture) {
        self.warp.borrow_mut().set_warp(warp);
    }

    /// Overwrite the lens uniform (amounts + `use_warp`); buffer write only.
    pub(crate) fn set_lens_uniform(&self, lens: LensUniform) {
        self.warp.borrow().set_lens_uniform(&self.ctx, lens);
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
        let warp = self.warp.borrow();
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
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&warp.a_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&warp.b_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: warp.lens_buf.as_entire_binding(),
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

/// Delegating `Node` impl so a `GeometryNode` can be shared via `Rc` — the
/// pipeline keeps a handle to drive `set_warp`/`set_lens_uniform` while a boxed
/// clone lives in the graph. Both point at the same node (interior mutability).
impl Node<PipelineImage> for Rc<GeometryNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        (**self).evaluate(inputs)
    }
}

/// Bind-group layout for the tone-curve pass: 0 = input texture,
/// 1 = output storage texture, 2 = 768-entry packed R/G/B LUT (read-only
/// storage buffer). Unchanged by the move from one shared LUT to three.
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

/// Tone-curve compute pass. Owns its (once-built) pipeline + a 768-entry
/// (3×256, R/G/B rows) LUT storage buffer; re-reads its LUT from a shared
/// `Cell` each evaluate.
pub(crate) struct CurveNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    lut_buf: wgpu::Buffer,
    lut: Rc<Cell<[[f32; 256]; 3]>>,
    out: RefCell<Option<PipelineImage>>,
}

impl CurveNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, lut: Rc<Cell<[[f32; 256]; 3]>>) -> Self {
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
            size: (std::mem::size_of::<f32>() * 768) as u64,
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
        // `[[f32; 256]; 3]: Pod` → 768 contiguous f32 (R,G,B rows).
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

/// Delegating `Node` impl so a `GeometryHeadNode` can be shared via `Rc` (see the
/// `Rc<GeometryNode>` impl above for the rationale).
impl Node<PipelineImage> for Rc<GeometryHeadNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        (**self).evaluate(inputs)
    }
}

/// The current tile request driving the geometry head (coord + active halo).
#[derive(Clone, Copy)]
pub(crate) struct TileRequest {
    pub coord: TileCoord,
    pub halo: u32,
}

/// The current tile's output-space frame, written by `GeometryHeadNode` each
/// `evaluate` and read by the downstream `VignetteNode` (same graph evaluate, so
/// it is current). `origin` is the haloed tile's top-left in this LOD's output
/// pixel space; `full_dims` is the full output image size at this LOD. The
/// vignette pass uses these to compute its radius in whole-image coordinates so
/// the tiled render matches the whole-image one (no per-tile vignette grid).
/// Default `[0.0, 0.0]` is the whole-image sentinel (preview leaves it there).
#[derive(Clone, Copy, Default)]
pub(crate) struct TileFrame {
    pub origin: [f32; 2],
    pub full_dims: [f32; 2],
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
    warp: RefCell<WarpBinding>,
    source: Arc<GpuPyramidSource>,
    geometry: Geometry,
    request: Rc<Cell<TileRequest>>,
    frame: Rc<Cell<TileFrame>>,
    out: RefCell<Option<PipelineImage>>,
}

impl GeometryHeadNode {
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        source: Arc<GpuPyramidSource>,
        geometry: Geometry,
        request: Rc<Cell<TileRequest>>,
        frame: Rc<Cell<TileFrame>>,
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
        let warp = RefCell::new(WarpBinding::new(&ctx));
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            sampler,
            warp,
            source,
            geometry,
            request,
            frame,
            out: RefCell::new(None),
        }
    }

    /// Swap in a freshly baked warp grid (bake-time; rebuilds cached views).
    pub(crate) fn set_warp(&self, warp: WarpGridTexture) {
        self.warp.borrow_mut().set_warp(warp);
    }

    /// Overwrite the lens uniform (amounts + `use_warp`); buffer write only.
    pub(crate) fn set_lens_uniform(&self, lens: LensUniform) {
        self.warp.borrow().set_lens_uniform(&self.ctx, lens);
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
        // Publish this tile's output-space frame for the downstream vignette pass:
        // full output image dims at this LOD + the haloed tile origin, so the
        // vignette radius is measured in whole-image space (seamless across tiles).
        let (_, out_w, out_h) = crate::uniforms::geometry_uniform(Some(self.geometry), sw, sh);
        self.frame.set(TileFrame {
            origin: [out_origin.0, out_origin.1],
            full_dims: [out_w as f32, out_h as f32],
        });
        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        let src_view = src
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let warp = self.warp.borrow();
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
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&warp.a_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&warp.b_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: warp.lens_buf.as_entire_binding(),
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

/// Bind-group layout for the vignette gain pass: 0 = input texture, 1 = output
/// storage texture, 2 = `VignetteUniform`, 3 = gain LUT (`R32Float`, sampled via
/// `textureLoad` so declared non-filterable — no sampler).
fn vignette_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("vignette-bgl"),
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
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    })
}

/// Vignetting radial-gain compute pass (scene-linear point op). Owns its
/// once-built pipeline + a reusable output texture; holds a `VignetteTexture`
/// gain LUT (default identity) with a cached view rebuilt only when the LUT is
/// swapped, and a small uniform buffer written per evaluate. Default
/// `vig_amount = 0.0` → identity, so existing goldens are byte-identical.
pub(crate) struct VignetteNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    params: Rc<Cell<VignetteUniform>>,
    /// Shared with the `GeometryHeadNode` in the tiled pipeline: the head writes
    /// the current tile's output frame each evaluate and this node reads it to fill
    /// `full_dims`/`origin`. `None` on the whole-image preview path → those stay
    /// zero → the shader takes the byte-identical per-texture radius branch.
    frame: Option<Rc<Cell<TileFrame>>>,
    lut: RefCell<VignetteTexture>,
    lut_view: RefCell<wgpu::TextureView>,
    out: RefCell<Option<PipelineImage>>,
}

impl VignetteNode {
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        params: Rc<Cell<VignetteUniform>>,
        frame: Option<Rc<Cell<TileFrame>>>,
    ) -> Self {
        let bgl = vignette_bgl(&ctx.device);
        let module = ctx.shader_module("vignette", include_str!("shaders/vignette.wgsl"));
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vignette"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("vignette"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        let uniform_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vignette-uniform"),
            size: std::mem::size_of::<VignetteUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lut = VignetteTexture::identity(&ctx);
        let lut_view = RefCell::new(lut.view());
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            params,
            frame,
            lut: RefCell::new(lut),
            lut_view,
            out: RefCell::new(None),
        }
    }

    /// Swap in a freshly baked vignette gain LUT (bake-time; rebuilds the cached
    /// view). Callers should dirty the node so the next evaluate uses it.
    pub(crate) fn set_vignette(&self, lut: VignetteTexture) {
        *self.lut_view.borrow_mut() = lut.view();
        *self.lut.borrow_mut() = lut;
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("vignette-out"),
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

impl Node<PipelineImage> for VignetteNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let dst = self.ensure_out(src.width, src.height);

        // Merge the tiled full-image frame (if any) into the amount/manual params.
        // On the preview path `frame` is `None`, so `full_dims`/`origin` stay at the
        // params' defaults (zero) and the shader takes the whole-image branch.
        let mut u = self.params.get();
        if let Some(frame) = &self.frame {
            let f = frame.get();
            u.full_dims = f.full_dims;
            u.origin = f.origin;
        }
        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        let src_view = src
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let lut_view = self.lut_view.borrow();
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vignette-bind"),
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
                        resource: wgpu::BindingResource::TextureView(&lut_view),
                    },
                ],
            });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vignette-pass"),
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

/// Delegating `Node` impl so a `VignetteNode` can be shared via `Rc` (see the
/// `Rc<GeometryNode>` impl for the rationale).
impl Node<PipelineImage> for Rc<VignetteNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        (**self).evaluate(inputs)
    }
}
