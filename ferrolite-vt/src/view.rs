//! VirtualTexture rung 1: the whole image as one `Rgba16Float` texture, sampled
//! by the display shader with a zoom/pan transform. Also the fallback path.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{tiles_per_level, LinearRgbaF32, TileCoord, TILE_SIZE};
use ferrolite_jobs::{JobHandle, JobSystem, Priority};
use half::f16;
use wgpu::util::DeviceExt;

use crate::pool::{SlotAllocator, TilePool, NOT_RESIDENT};
use crate::residency::{needed_tiles, ResidencySet};
use crate::{TileSource, ViewTransform};

/// Max LOD levels the `TileMeta` uniform can carry (matches `array<vec4<u32>, 8>` in WGSL).
const MAX_LEVELS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformUniform {
    zoom: f32,
    _pad0: f32,
    pan: [f32; 2],
    viewport: [f32; 2],
    image: [f32; 2],
}

/// `TileMeta` uniform: must match the WGSL `struct TileMeta` layout exactly.
/// `levels[lod] = (cols, flat_offset, _, _)`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileMetaUniform {
    level_count: u32,
    // WGSL `_pad: vec3<u32>` aligns to 16 and sits at offset 16, so `levels`
    // starts at offset 32. Use a full 32-byte header (7 padding u32s) to match.
    _pad: [u32; 7],
    levels: [[u32; 4]; MAX_LEVELS],
}

/// Rung-2 GPU resources: a 2-D-array texture holding every tile of every LOD,
/// a CPU-built `(lod,x,y) -> layer` slot table, the `TileMeta` uniform, plus the
/// bind-group layout and the `fs_tiled` pipeline.
pub struct TiledResources {
    array_view: wgpu::TextureView,
    slots_buf: wgpu::Buffer,
    meta_buf: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    image_dims: (u32, u32),
    // keep the texture alive (the view borrows from it conceptually)
    _array_tex: wgpu::Texture,
}

/// Rung-1 (single-texture) GPU resources.
struct SingleResources {
    texture: wgpu::Texture,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    image_dims: (u32, u32),
}

/// Per-level slot-table geometry: tiles-per-row and flat-slot offset for each LOD.
struct LevelLayout {
    cols: Vec<u32>,
    offsets: Vec<u32>,
    total_tiles: u32,
    level_count: u32,
}

/// Rung-3 streaming resources: a budget-limited physical `TilePool`, the CPU
/// residency brain (`ResidencySet` + `SlotAllocator`), a job-driven load channel,
/// and the GPU bind resources (slots buffer mirrors the per-frame `(lod,tile)->slot`
/// indirection used by the shader's coarse-LOD fallback).
struct StreamingResources {
    pool: TilePool,
    allocator: SlotAllocator,
    residency: ResidencySet,
    layout: LevelLayout,
    source: Arc<dyn TileSource + Send + Sync>,
    jobs: Arc<JobSystem>,
    tx: Sender<(TileCoord, LinearRgbaF32)>,
    rx: Receiver<(TileCoord, LinearRgbaF32)>,
    in_flight: HashMap<TileCoord, JobHandle>,
    // CPU mirror of the slot table (all NOT_RESIDENT until a tile is uploaded).
    slots: Vec<u32>,

    // GPU bind resources.
    array_view: wgpu::TextureView,
    slots_buf: wgpu::Buffer,
    meta_buf: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    image_dims: (u32, u32),
}

pub struct VirtualTexture {
    single: Option<SingleResources>,
    tiled: Option<TiledResources>,
    streaming: Option<StreamingResources>,
}

impl VirtualTexture {
    pub fn single_texture(
        ctx: &GpuContext,
        image: &LinearRgbaF32,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let device = &ctx.device;
        // f32 -> f16 RGBA.
        let texels: Vec<f16> = image.pixels.iter().map(|&v| f16::from_f32(v)).collect();
        let texture = device.create_texture_with_data(
            &ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("vt-single"),
                size: wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        );

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vt-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vt-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(target_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            single: Some(SingleResources {
                texture,
                bind_group_layout: bgl,
                sampler,
                pipeline,
                image_dims: (image.width, image.height),
            }),
            tiled: None,
            streaming: None,
        }
    }

    pub fn render(
        &self,
        ctx: &GpuContext,
        pass: &mut wgpu::RenderPass<'_>,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let single = self
            .single
            .as_ref()
            .expect("render called on a non-single VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [single.image_dims.0 as f32, single.image_dims.1 as f32],
        };
        let ubuf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vt-xf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let tview = single
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-bind"),
            layout: &single.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tview),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&single.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubuf.as_entire_binding(),
                },
            ],
        });
        pass.set_pipeline(&single.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Offscreen render to an `Rgba8Unorm` image (golden tests).
    pub fn render_to_image(
        ctx: &GpuContext,
        image: &LinearRgbaF32,
        view: &ViewTransform,
        viewport: (f32, f32),
        out_w: u32,
        out_h: u32,
    ) -> Vec<u8> {
        let vt = Self::single_texture(ctx, image, wgpu::TextureFormat::Rgba8Unorm);
        let target = ctx.render_target(out_w, out_h, wgpu::TextureFormat::Rgba8Unorm);
        let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vt-offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tview,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            vt.render(ctx, &mut pass, view, viewport);
        }
        ctx.queue.submit([enc.finish()]);
        ctx.read_rgba8(&target, out_w, out_h)
    }

    /// Rung 2: upload ALL tiles of ALL LOD levels into a `texture_2d_array`
    /// (one layer per `(lod, x, y)` slot), build the slot table + `TileMeta`,
    /// and create the `fs_tiled` pipeline. Everything stays resident.
    pub fn tiled_resident(
        ctx: &GpuContext,
        source: &dyn TileSource,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let device = &ctx.device;
        let (img_w, img_h) = source.level_size(0);
        let level_count = source.level_count().min(MAX_LEVELS as u32);

        // Per-level tile grid + flat slot offsets. cols/rows are derived from the
        // source's own level size (ceil-div TILE_SIZE) rather than the image's
        // pyramid math, so a source whose levels diverge still lays out correctly.
        let mut cols_per_level = Vec::with_capacity(level_count as usize);
        let mut offsets = Vec::with_capacity(level_count as usize);
        let mut total_tiles: u32 = 0;
        for lod in 0..level_count {
            let (lw, lh) = source.level_size(lod);
            let cols = lw.div_ceil(TILE_SIZE);
            let rows = lh.div_ceil(TILE_SIZE);
            cols_per_level.push(cols);
            offsets.push(total_tiles);
            total_tiles += cols * rows;
            // tiles_per_level is referenced to keep the documented contract visible;
            // it agrees with the ceil-div above for PyramidTileSource.
            debug_assert_eq!(tiles_per_level(img_w, img_h, lod), (cols, rows));
        }
        let total_tiles = total_tiles.max(1);

        // Create the array texture (one layer per slot) and upload each tile.
        let array_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vt-tiled-array"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: total_tiles,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Build the slot table: slots[offset + y*cols + x] = layer_index.
        let mut slots: Vec<u32> = vec![0u32; total_tiles as usize];
        let mut layer: u32 = 0;
        for lod in 0..level_count {
            let (lw, lh) = source.level_size(lod);
            let cols = lw.div_ceil(TILE_SIZE);
            let rows = lh.div_ceil(TILE_SIZE);
            let offset = offsets[lod as usize];
            for y in 0..rows {
                for x in 0..cols {
                    let tile = source.tile(TileCoord { lod, x, y });
                    let texels: Vec<f16> = tile.pixels.iter().map(|&v| f16::from_f32(v)).collect();
                    ctx.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &array_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: 0,
                                z: layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        bytemuck::cast_slice(&texels),
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(TILE_SIZE * 4 * 2), // RGBA * f16
                            rows_per_image: Some(TILE_SIZE),
                        },
                        wgpu::Extent3d {
                            width: TILE_SIZE,
                            height: TILE_SIZE,
                            depth_or_array_layers: 1,
                        },
                    );
                    slots[(offset + y * cols + x) as usize] = layer;
                    layer += 1;
                }
            }
        }

        let array_view = array_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("vt-tiled-array-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        // TileMeta uniform.
        let mut levels = [[0u32; 4]; MAX_LEVELS];
        for lod in 0..level_count as usize {
            levels[lod] = [cols_per_level[lod], offsets[lod], 0, 0];
        }
        let meta = TileMetaUniform {
            level_count,
            _pad: [0; 7],
            levels,
        };
        let meta_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-tile-meta"),
            contents: bytemuck::bytes_of(&meta),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let slots_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-slots"),
            contents: bytemuck::cast_slice(&slots),
            usage: wgpu::BufferUsages::STORAGE,
        });

        // Bind-group layout: 0-2 as rung 1, plus 3=array tex, 4=slots, 5=meta.
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vt-tiled-bgl"),
            entries: &[
                // binding 0 (`img_tex`) is declared in the shared module but not used
                // by `fs_tiled`, so it is intentionally omitted from this layout.
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
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vt-tiled-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vt-tiled-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_tiled",
                targets: &[Some(target_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-tiled-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let tiled = TiledResources {
            array_view,
            slots_buf,
            meta_buf,
            bind_group_layout: bgl,
            sampler,
            pipeline,
            image_dims: (img_w, img_h),
            _array_tex: array_tex,
        };

        Self {
            single: None,
            tiled: Some(tiled),
            streaming: None,
        }
    }

    /// Rung 2: bind the tiled resources and draw the full-screen triangle.
    pub fn render_tiled(
        &self,
        ctx: &GpuContext,
        pass: &mut wgpu::RenderPass<'_>,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let tiled = self
            .tiled
            .as_ref()
            .expect("render_tiled called on a non-tiled VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [tiled.image_dims.0 as f32, tiled.image_dims.1 as f32],
        };
        let ubuf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vt-tiled-xf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-tiled-bind"),
            layout: &tiled.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&tiled.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&tiled.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: tiled.slots_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: tiled.meta_buf.as_entire_binding(),
                },
            ],
        });
        pass.set_pipeline(&tiled.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Rung 2 offscreen render to an `Rgba8Unorm` image (golden tests).
    pub fn render_tiled_to_image(
        ctx: &GpuContext,
        source: &dyn TileSource,
        view: &ViewTransform,
        viewport: (f32, f32),
        out_w: u32,
        out_h: u32,
    ) -> Vec<u8> {
        let vt = Self::tiled_resident(ctx, source, wgpu::TextureFormat::Rgba8Unorm);
        let target = ctx.render_target(out_w, out_h, wgpu::TextureFormat::Rgba8Unorm);
        let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vt-tiled-offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tview,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            vt.render_tiled(ctx, &mut pass, view, viewport);
        }
        ctx.queue.submit([enc.finish()]);
        ctx.read_rgba8(&target, out_w, out_h)
    }

    /// Rung 3: a budget-limited streaming virtual texture. Allocates a physical
    /// `TilePool` of `budget_tiles` slots (NO tiles uploaded up front), builds the
    /// `(lod,tile)->slot` indirection (all `NOT_RESIDENT`) and the `fs_tiled`
    /// pipeline. Tiles are loaded on demand by `request_view` + `drain_loaded`.
    pub fn streaming(
        ctx: &GpuContext,
        source: Arc<dyn TileSource + Send + Sync>,
        jobs: Arc<JobSystem>,
        budget_tiles: u32,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let device = &ctx.device;
        let (img_w, img_h) = source.level_size(0);
        let level_count = source.level_count().min(MAX_LEVELS as u32);

        // Per-level slot-table geometry (drives both the shader's index math and
        // the CPU mirror). Derived from the source's own level sizes.
        let mut cols = Vec::with_capacity(level_count as usize);
        let mut offsets = Vec::with_capacity(level_count as usize);
        let mut total_tiles: u32 = 0;
        for lod in 0..level_count {
            let (lw, lh) = source.level_size(lod);
            let c = lw.div_ceil(TILE_SIZE);
            let r = lh.div_ceil(TILE_SIZE);
            cols.push(c);
            offsets.push(total_tiles);
            total_tiles += c * r;
        }
        let total_tiles = total_tiles.max(1);
        let layout = LevelLayout {
            cols,
            offsets,
            total_tiles,
            level_count,
        };

        let pool = TilePool::new(ctx, budget_tiles);
        let array_view = pool.texture().create_view(&wgpu::TextureViewDescriptor {
            label: Some("vt-stream-pool-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        // Slot table starts fully non-resident.
        let slots: Vec<u32> = vec![NOT_RESIDENT; total_tiles as usize];

        // TileMeta uniform (same layout as rung 2).
        let mut levels = [[0u32; 4]; MAX_LEVELS];
        for (lod, slot) in levels.iter_mut().enumerate().take(level_count as usize) {
            *slot = [layout.cols[lod], layout.offsets[lod], 0, 0];
        }
        let meta = TileMetaUniform {
            level_count,
            _pad: [0; 7],
            levels,
        };
        let meta_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-stream-meta"),
            contents: bytemuck::bytes_of(&meta),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        // Slots buffer needs COPY_DST so we can rewrite it each frame.
        let slots_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-stream-slots"),
            contents: bytemuck::cast_slice(&slots),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let (bind_group_layout, sampler, pipeline) =
            build_tiled_pipeline(device, target_format, "vt-stream");

        let (tx, rx) = std::sync::mpsc::channel();

        let streaming = StreamingResources {
            pool,
            allocator: SlotAllocator::new(budget_tiles),
            residency: ResidencySet::new(budget_tiles as usize),
            layout,
            source,
            jobs,
            tx,
            rx,
            in_flight: HashMap::new(),
            slots,
            array_view,
            slots_buf,
            meta_buf,
            bind_group_layout,
            sampler,
            pipeline,
            image_dims: (img_w, img_h),
        };

        Self {
            single: None,
            tiled: None,
            streaming: Some(streaming),
        }
    }

    /// Rung 3: reconcile the resident set with the current view. Computes the
    /// needed tiles, diffs against residency, frees evicted slots, cancels loads
    /// no longer needed, submits `Visible` jobs for newly-needed tiles, and drains
    /// any tiles that finished loading (uploading them to the pool on this thread).
    /// GPU access is single-threaded — call this from the main/render thread.
    pub fn request_view(
        &mut self,
        ctx: &GpuContext,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        // First absorb anything that finished since last frame.
        self.drain_loaded(ctx);

        let s = self
            .streaming
            .as_mut()
            .expect("request_view called on a non-streaming VirtualTexture");

        let needed = needed_tiles(
            s.image_dims,
            view,
            viewport,
            s.layout.level_count,
        );
        let (to_load, to_evict) = s.residency.diff(&needed);

        // Evict: free physical slots + drop residency + clear the slot-table entry.
        for t in &to_evict {
            s.allocator.free(*t);
            s.residency.forget(*t);
            if let Some(idx) = slot_index(&s.layout, *t) {
                s.slots[idx] = NOT_RESIDENT;
            }
        }

        // Cancel in-flight loads for tiles no longer needed.
        let stale: Vec<TileCoord> = s
            .in_flight
            .keys()
            .copied()
            .filter(|t| !needed.contains(t))
            .collect();
        for t in stale {
            if let Some(h) = s.in_flight.remove(&t) {
                h.cancel();
            }
        }

        // Submit loads for newly-needed tiles not already resident or in flight.
        for t in to_load {
            if s.residency.contains(t) || s.in_flight.contains_key(&t) {
                continue;
            }
            let tx = s.tx.clone();
            let source = Arc::clone(&s.source);
            let coord = t;
            let handle = s.jobs.submit(Priority::Visible, move |token| {
                if token.is_cancelled() {
                    return;
                }
                let tile = source.tile(coord);
                // Receiver gone (VT dropped) => ignore.
                let _ = tx.send((coord, tile));
            });
            s.in_flight.insert(t, handle);
        }

        // Push the (possibly updated) slot table to the GPU for this frame.
        ctx.queue
            .write_buffer(&s.slots_buf, 0, bytemuck::cast_slice(&s.slots));
    }

    /// Drain tiles that finished loading and make them resident: allocate a slot,
    /// upload pixels into the pool, touch residency (LRU), update the slot table.
    /// Returns the number of tiles made resident. Runs on the main thread.
    pub fn drain_loaded(&mut self, ctx: &GpuContext) -> usize {
        let s = match self.streaming.as_mut() {
            Some(s) => s,
            None => return 0,
        };
        let mut made_resident = 0;
        // Collect first so we don't hold the receiver borrow across mutation.
        let ready: Vec<(TileCoord, LinearRgbaF32)> = s.rx.try_iter().collect();
        for (coord, tile) in ready {
            s.in_flight.remove(&coord);
            // If a budget-driven eviction freed a slot, evict an LRU resident to
            // make room when the pool is full.
            let slot = match s.allocator.alloc(coord) {
                Some(slot) => slot,
                None => {
                    // Pool full: evict the LRU resident tile to free a slot.
                    if let Some(victim) = s.residency.lru() {
                        s.allocator.free(victim);
                        s.residency.forget(victim);
                        if let Some(idx) = slot_index(&s.layout, victim) {
                            s.slots[idx] = NOT_RESIDENT;
                        }
                    }
                    match s.allocator.alloc(coord) {
                        Some(slot) => slot,
                        None => continue, // still no room (capacity 0); drop tile
                    }
                }
            };
            s.pool.upload(ctx, slot, &tile);
            s.residency.touch(coord);
            if let Some(idx) = slot_index(&s.layout, coord) {
                s.slots[idx] = slot;
            }
            made_resident += 1;
        }
        if made_resident > 0 {
            ctx.queue
                .write_buffer(&s.slots_buf, 0, bytemuck::cast_slice(&s.slots));
        }
        made_resident
    }

    /// Rung 3: bind the streaming resources and draw the full-screen triangle.
    pub fn render_streaming(
        &self,
        ctx: &GpuContext,
        pass: &mut wgpu::RenderPass<'_>,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let s = self
            .streaming
            .as_ref()
            .expect("render_streaming called on a non-streaming VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [s.image_dims.0 as f32, s.image_dims.1 as f32],
        };
        let ubuf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vt-stream-xf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-stream-bind"),
            layout: &s.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&s.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&s.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: s.slots_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: s.meta_buf.as_entire_binding(),
                },
            ],
        });
        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Flat index into the slot table for `t`, or `None` if out of range (e.g. an LOD
/// the source does not carry).
fn slot_index(layout: &LevelLayout, t: TileCoord) -> Option<usize> {
    if t.lod >= layout.level_count {
        return None;
    }
    let cols = layout.cols[t.lod as usize];
    if t.x >= cols {
        return None;
    }
    let idx = layout.offsets[t.lod as usize] + t.y * cols + t.x;
    if idx >= layout.total_tiles {
        return None;
    }
    Some(idx as usize)
}

/// Build the shared `fs_tiled` bind-group layout + sampler + render pipeline used
/// by both the rung-2 (resident) and rung-3 (streaming) paths.
fn build_tiled_pipeline(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    label_prefix: &str,
) -> (wgpu::BindGroupLayout, wgpu::Sampler, wgpu::RenderPipeline) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vt-display"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(&format!("{label_prefix}-pl")),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!("{label_prefix}-pipeline")),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_tiled",
            targets: &[Some(target_format.into())],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(&format!("{label_prefix}-sampler")),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    (bgl, sampler, pipeline)
}
