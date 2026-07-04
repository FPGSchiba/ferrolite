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
use wgpu::util::DeviceExt;

/// WGSL `mat3x3<f32>` uniform for the working→display tail transform. Column-major,
/// each column padded to 16 bytes. Generic (no photo concepts): the app supplies a
/// plain row-major 3×3.
///
/// `use_lut` selects the display tail in the shader: `0` = analytic sRGB
/// (`linear_to_srgb(m * lin)`, today's byte-identical path), `1` = sample the
/// generic 3D-LUT (`lut3d`/`lut_samp`) after shaper-encoding. `shaper_gamma` is
/// the shaper curve exponent applied before the LUT lookup.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayColorUniform {
    m: [[f32; 4]; 3],
    use_lut: u32,
    shaper_gamma: f32,
    lut_size: f32,
    _pad: f32,
}

/// Cube edge length of the display LUT texture. Mirrors
/// `ferrolite_color::DISPLAY_LUT_SIZE` — the two MUST match.
pub const LUT_SIZE: u32 = 33;

/// Pack a row-major 3×3 into WGSL column-major padded columns (`M * v == m · v`).
pub fn pack_display_matrix(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    [
        [m[0][0], m[1][0], m[2][0], 0.0],
        [m[0][1], m[1][1], m[2][1], 0.0],
        [m[0][2], m[1][2], m[2][2], 0.0],
    ]
}

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
    display_matrix: Arc<wgpu::Buffer>,
    lut_texture: Arc<wgpu::Texture>,
    lut_view: Arc<wgpu::TextureView>,
    lut_sampler: Arc<wgpu::Sampler>,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 8,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 9,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D3,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 10,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sparse_pipeline = mk(&sparse_bgl, "vs_main", "fs_sparse");

        let display_matrix = Arc::new(device.create_buffer_init(
            &wgpu::util::BufferInitDescriptor {
                label: Some("vt-display-matrix"),
                contents: bytemuck::bytes_of(&DisplayColorUniform {
                    m: pack_display_matrix([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
                    use_lut: 0,
                    shaper_gamma: 2.2,
                    lut_size: LUT_SIZE as f32,
                    _pad: 0.0,
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        ));

        // Generic 3D-LUT texture: allocated ONCE at fixed `LUT_SIZE`³ and reused for
        // every profile/image (GPU build-once, CLAUDE.md §2). `set_display_lut` only
        // `write_texture`s into this texture — the view stays stable so per-image
        // bind groups built in `view.rs` remain valid across LUT updates.
        let lut_texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vt-display-lut"),
            size: wgpu::Extent3d {
                width: LUT_SIZE,
                height: LUT_SIZE,
                depth_or_array_layers: LUT_SIZE,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }));
        let lut_view = Arc::new(lut_texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let lut_sampler = Arc::new(device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-display-lut-samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }));

        Self {
            target_format,
            sampler: Arc::new(sampler),
            display_matrix,
            lut_texture,
            lut_view,
            lut_sampler,
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

    /// The shared working→display matrix uniform buffer (bound at @8 by every
    /// variant). Cloned into per-image VT resources.
    pub fn display_matrix_buffer(&self) -> &Arc<wgpu::Buffer> {
        &self.display_matrix
    }

    /// Push a new working→display matrix (row-major 3×3). Call ONLY when the working
    /// space changes — never per frame, never per image. Cheap `write_buffer`.
    /// Switches the shader tail back to the analytic sRGB path (`use_lut = 0`).
    pub fn set_display_matrix(&self, queue: &wgpu::Queue, m: [[f32; 3]; 3]) {
        queue.write_buffer(
            &self.display_matrix,
            0,
            bytemuck::bytes_of(&DisplayColorUniform {
                m: pack_display_matrix(m),
                use_lut: 0,
                shaper_gamma: 2.2,
                lut_size: LUT_SIZE as f32,
                _pad: 0.0,
            }),
        );
    }

    /// The 3D-LUT texture view (bound @9 by every variant). Cloned into per-image VT resources.
    pub fn display_lut_view(&self) -> &Arc<wgpu::TextureView> {
        &self.lut_view
    }

    /// The LUT sampler (bound @10 by every variant).
    pub fn display_lut_sampler(&self) -> &Arc<wgpu::Sampler> {
        &self.lut_sampler
    }

    /// Upload a monitor LUT and switch the tail to the LUT path (`use_lut = 1`).
    /// `size` MUST equal `LUT_SIZE`; `rgba16f` is `size³` RGBA half-float texels.
    /// Call only when the profile / working space changes — never per frame/image.
    pub fn set_display_lut(
        &self,
        queue: &wgpu::Queue,
        size: u32,
        rgba16f: &[u16],
        shaper_gamma: f32,
    ) {
        debug_assert_eq!(size, LUT_SIZE, "display LUT size must match LUT_SIZE");
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.lut_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(rgba16f),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(size * 4 * 2), // 4 channels × 2 bytes (f16)
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: size,
            },
        );
        queue.write_buffer(
            &self.display_matrix,
            0,
            bytemuck::bytes_of(&DisplayColorUniform {
                m: pack_display_matrix([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
                use_lut: 1,
                shaper_gamma,
                lut_size: size as f32,
                _pad: 0.0,
            }),
        );
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

#[cfg(test)]
mod tests {
    use super::pack_display_matrix;

    #[test]
    fn pack_identity_columns() {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(
            pack_display_matrix(id),
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0]
            ]
        );
    }

    #[test]
    fn pack_transposes_rows_into_columns() {
        let m = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        assert_eq!(
            pack_display_matrix(m),
            [
                [1.0, 4.0, 7.0, 0.0],
                [2.0, 5.0, 8.0, 0.0],
                [3.0, 6.0, 9.0, 0.0]
            ]
        );
    }
}
