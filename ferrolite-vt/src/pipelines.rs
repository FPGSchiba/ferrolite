//! Cached, image-independent wgpu display pipelines. Built once per target
//! format (pre-warmed at startup) and reused for every image open, so opening
//! an image never pays a pipeline-compile cost on the UI thread.
//!
//! The bind-group-layout entries and vertex/fragment entry points for each
//! variant are MOVED verbatim from the four `view.rs` constructors (and the
//! shared `build_tiled_pipeline`/`build_sparse_pipeline` helpers). Nothing about
//! the layouts, pipeline state, or shader changes — only *where/when* the GPU
//! objects are created. Rendered output stays byte-identical (golden gate).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

/// The four display pipeline variants. Each owns its own bind-group layout and
/// fragment entry point; `Tiled` and `Streaming` are identical (both `fs_tiled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayVariant {
    Single,
    Tiled,
    Streaming,
    Sparse,
}

/// Cache of the reusable, image-independent GPU objects for all four display
/// variants: one shared shader module + sampler, and a `(BindGroupLayout,
/// RenderPipeline)` per variant. Build once via [`DisplayPipelines::new`] and
/// reuse across every image open.
pub struct DisplayPipelines {
    target_format: wgpu::TextureFormat,
    // wgpu 22 handles are not `Clone`, so the cache hands out cheap `Arc` clones
    // that the per-image VT resources hold for `prepare_*`/`draw_*`.
    sampler: Arc<wgpu::Sampler>,
    single: (Arc<wgpu::BindGroupLayout>, Arc<wgpu::RenderPipeline>),
    tiled: (Arc<wgpu::BindGroupLayout>, Arc<wgpu::RenderPipeline>),
    streaming: (Arc<wgpu::BindGroupLayout>, Arc<wgpu::RenderPipeline>),
    sparse: (Arc<wgpu::BindGroupLayout>, Arc<wgpu::RenderPipeline>),
}

impl DisplayPipelines {
    /// Build (compile) all four display pipelines for `target_format`. Call once
    /// (pre-warm); the result is reused for every image open.
    pub fn new(ctx: &GpuContext, target_format: wgpu::TextureFormat) -> Self {
        let device = &ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
        });
        // One shared filtering sampler (linear mag/min), as every variant used.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Build a render pipeline from a bind-group layout + vertex/fragment
        // entry points, against the shared shader and `target_format`.
        let mk = |bgl: &wgpu::BindGroupLayout, vs: &str, fs: &str| -> wgpu::RenderPipeline {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vt-pl"),
                bind_group_layouts: &[bgl],
                push_constant_ranges: &[],
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vt-pipeline"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: vs,
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: fs,
                    targets: &[Some(target_format.into())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };

        // --- Single (rung 1): tex@0, sampler@1, uniform@2; entry `fs_main`. ---
        let single_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vt-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let single_pipeline = mk(&single_bgl, "vs_main", "fs_main");

        // --- Tiled (rung 2) + Streaming (rung 3): identical bgl + `fs_tiled`.
        // binding 0 (`img_tex`) is intentionally omitted; sampler@1, uniform@2,
        // array-tex@3, slots@4, meta@5. ---
        let tiled_bgl = || {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("vt-tiled-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            })
        };
        let tiled_layout = tiled_bgl();
        let tiled_pipeline = mk(&tiled_layout, "vs_main", "fs_tiled");
        let streaming_layout = tiled_bgl();
        let streaming_pipeline = mk(&streaming_layout, "vs_main", "fs_tiled");

        // --- Sparse (rung 4): like tiled but slots@4 replaced by page-table@6
        // (Rg32Uint, non-filterable) + read-write feedback@7; entry `fs_sparse`. ---
        let sparse_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vt-sparse-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Page table: Rg32Uint texture, sampled via textureLoad (non-filterable).
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Feedback: read-write storage buffer of atomic<u32>.
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let sparse_pipeline = mk(&sparse_bgl, "vs_main", "fs_sparse");

        Self {
            target_format,
            sampler: Arc::new(sampler),
            single: (Arc::new(single_bgl), Arc::new(single_pipeline)),
            tiled: (Arc::new(tiled_layout), Arc::new(tiled_pipeline)),
            streaming: (Arc::new(streaming_layout), Arc::new(streaming_pipeline)),
            sparse: (Arc::new(sparse_bgl), Arc::new(sparse_pipeline)),
        }
    }

    /// The target color format these pipelines render to.
    pub fn target_format(&self) -> wgpu::TextureFormat {
        self.target_format
    }

    /// The shared filtering sampler used by every variant. Returns the `Arc` so
    /// callers can cheaply clone a handle to store in their per-image resources.
    pub fn sampler(&self) -> &Arc<wgpu::Sampler> {
        &self.sampler
    }

    /// The bind-group layout for `v` (used to build the per-image bind group).
    pub fn layout(&self, v: DisplayVariant) -> &Arc<wgpu::BindGroupLayout> {
        match v {
            DisplayVariant::Single => &self.single.0,
            DisplayVariant::Tiled => &self.tiled.0,
            DisplayVariant::Streaming => &self.streaming.0,
            DisplayVariant::Sparse => &self.sparse.0,
        }
    }

    /// The cached render pipeline for `v`.
    pub fn pipeline(&self, v: DisplayVariant) -> &Arc<wgpu::RenderPipeline> {
        match v {
            DisplayVariant::Single => &self.single.1,
            DisplayVariant::Tiled => &self.tiled.1,
            DisplayVariant::Streaming => &self.streaming.1,
            DisplayVariant::Sparse => &self.sparse.1,
        }
    }
}
